#![cfg(unix)]

use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::mpsc;
use std::thread;

struct FakeSsh {
    dir: PathBuf,
}

impl FakeSsh {
    fn new(tag: &str, pause_home: bool) -> Self {
        let dir = std::env::temp_dir().join(format!("m131-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("ssh");
        let pause = if pause_home {
            "touch \"$base/entered\"\nwhile [ ! -f \"$base/release\" ]; do sleep 0.02; done"
        } else {
            ""
        };
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
base=$(dirname "$0")
printf '%s\n' "$@" >> "$base/argv"
for arg in "$@"; do
  case "$arg" in /bin/sh\ -c\ *HOME*)
    {pause}
    printf '/Users/remote'
    exit 0;;
  esac
done
exit 17
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir }
    }

    fn manager(&self) -> HostManager {
        HostManager::with_ssh_program(self.dir.clone(), self.dir.join("ssh").into_os_string())
    }

    fn argv(&self) -> String {
        fs::read_to_string(self.dir.join("argv")).unwrap()
    }
}

impl Drop for FakeSsh {
    fn drop(&mut self) {
        fs::write(self.dir.join("release"), "").ok();
        // 실패 시에도 대기 중인 가짜 SSH를 풀고 다음 테스트의 임시 경로와 겹치지 않게 한다.
    }
}

fn spec(socket: Option<&str>) -> RemoteHostSpec {
    RemoteHostSpec {
        id: "studio".into(),
        label: "Studio".into(),
        address: "owen@studio.local".into(),
        port: Some(2222),
        identity_file: Some("/tmp/key with spaces".into()),
        remote_socket: socket.map(str::to_owned),
    }
}

#[test]
fn injected_ssh_handles_home_query_and_tunnel_with_same_options() {
    let fake = FakeSsh::new("home", false);
    let manager = fake.manager();
    manager.sync_specs(&[spec(None)]);
    assert!(manager.ensure_tunnel_socket("studio").is_err());
    let log = fake.argv();
    assert_eq!(
        log.lines().filter(|s| *s == "owen@studio.local").count(),
        2,
        "{log}"
    );
    assert_eq!(
        log.matches("-p\n2222\n-i\n/tmp/key with spaces\n").count(),
        2
    );
    assert!(log.contains(&format!("{}\n", crate::ssh::remote_query())));
    assert!(
        log.contains(":/Users/remote/Library/Application Support/dev.krill.app/run/krilld.sock\n")
    );
    assert_eq!(manager.status("studio"), HostStatus::Disconnected);
}

#[test]
fn explicit_socket_skips_home_query() {
    let fake = FakeSsh::new("explicit", false);
    let manager = fake.manager();
    manager.sync_specs(&[spec(Some("/tmp/remote socket"))]);
    assert!(manager.ensure_tunnel_socket("studio").is_err());
    let log = fake.argv();
    assert_eq!(
        log.lines().filter(|s| *s == "owen@studio.local").count(),
        1,
        "{log}"
    );
    assert!(log.contains(":/tmp/remote socket\n"));
    assert!(!log.contains("$HOME"));
}

#[test]
fn stalled_home_query_does_not_block_ui_state_or_other_hosts() {
    let fake = FakeSsh::new("stalled", true);
    let manager = fake.manager();
    manager.sync_specs(&[spec(None)]);
    let worker_manager = manager.clone();
    let worker = thread::spawn(move || worker_manager.ensure_tunnel_socket("studio"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fake.dir.join("entered").exists() {
        assert!(Instant::now() < deadline, "HOME query did not start");
        thread::sleep(Duration::from_millis(10));
    }

    let ui_manager = manager.clone();
    let (tx, rx) = mpsc::channel();
    let ui = thread::spawn(move || {
        assert_eq!(ui_manager.status("studio"), HostStatus::Connecting);
        assert_eq!(ui_manager.statuses().len(), 1);
        assert_eq!(ui_manager.specs().len(), 1);
        assert!(
            ui_manager
                .target_for(&HostId::Remote("studio".into()), None)
                .is_some()
        );
        let mut changed = spec(Some("/tmp/new.sock"));
        let mut other = changed.clone();
        other.id = "other".into();
        changed.port = Some(2223);
        ui_manager.sync_specs(&[changed, other]);
        assert_eq!(ui_manager.status("other"), HostStatus::Connecting);
        tx.send(()).unwrap();
    });
    let responsive = rx.recv_timeout(Duration::from_secs(2));
    fs::write(fake.dir.join("release"), "").unwrap();
    ui.join().unwrap();
    let err = worker.join().unwrap().unwrap_err();
    responsive.expect("UI state must remain accessible while HOME query is stalled");
    assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
    assert_eq!(manager.tunnel_starts("studio"), 0);
    assert_eq!(manager.specs()[0].port, Some(2223));
}

/// 터널은 사는데 원격 krilld에 닿지 못하면(ssh가 채널을 바로 닫음 → Hello EOF) 터널을 내려, 다음 시도에서
/// 원격 경로를 다시 찾고 데몬을 깨운다. "다시 연결"은 살아 있는 터널도 내린다.
#[test]
fn eof_through_live_tunnel_and_reconnect_now_rebuild_the_tunnel() {
    let dir = std::env::temp_dir().join(format!("m131-eof-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("ssh");
    // 가짜 ssh: `-L <local>:<remote>`의 local에서 받자마자 닫는다(원격 소켓이 없는 ssh 포워딩과 같은 관측).
    fs::write(
        &script,
        r#"#!/bin/sh
prev=""
for a in "$@"; do
  if [ "$prev" = "-L" ]; then fwd="$a"; fi
  prev="$a"
done
exec /usr/bin/perl -MIO::Socket::UNIX -e '$s = IO::Socket::UNIX->new(Local => $ARGV[0], Listen => 5) or exit 3; while ($c = $s->accept) { close $c }' "${fwd%%:*}"
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let manager = HostManager::with_ssh_program(dir.clone(), script.into_os_string());
    manager.sync_specs(&[spec(Some("/tmp/remote.sock"))]);
    let target = manager
        .target_for(&HostId::Remote("studio".into()), None)
        .unwrap();
    let hello = krill_proto::daemon::client_hello(krill_proto::ipc::ClientRole::App, "m131-eof");

    assert!(target.connect(&hello).is_err());
    assert_eq!(manager.tunnel_starts("studio"), 1);
    assert_eq!(manager.status("studio"), HostStatus::Disconnected);
    assert_eq!(
        manager.tunnel_pid("studio"),
        None,
        "EOF must drop the live tunnel"
    );

    thread::sleep(HostManager::backoff_delay(1) + Duration::from_millis(50));
    assert!(target.connect(&hello).is_err());
    assert_eq!(manager.tunnel_starts("studio"), 2, "next attempt rebuilds");

    // 터널을 살린 채로 "다시 연결": 백오프를 풀고 터널을 새로 만든다.
    manager.reconnect_now("studio");
    manager.ensure_tunnel_socket("studio").unwrap();
    assert!(manager.tunnel_pid("studio").is_some());
    manager.reconnect_now("studio");
    assert_eq!(manager.tunnel_pid("studio"), None);
    manager.ensure_tunnel_socket("studio").unwrap();
    assert_eq!(manager.tunnel_starts("studio"), 4);
    drop(manager);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn duplicate_host_id_alias_shares_the_canonical_slot() {
    let manager = HostManager::new();
    manager.sync_specs(&[spec(None)]);
    assert!(
        manager
            .target_for(&HostId::Remote("studio-alias".into()), None)
            .is_none()
    );
    manager.set_aliases(BTreeMap::from([("studio-alias".into(), "studio".into())]));
    assert!(
        manager
            .target_for(&HostId::Remote("studio-alias".into()), None)
            .is_some()
    );
    manager.kill_tunnel("studio-alias");
    assert_eq!(manager.status("studio"), HostStatus::Disconnected);
    assert_eq!(manager.statuses().len(), 1);
}

#[test]
fn manual_disconnect_blocks_retries_until_explicit_reconnect() {
    let fake = FakeSsh::new("manual-disconnect", false);
    let manager = fake.manager();
    let host = spec(Some("/tmp/remote.sock"));
    manager.sync_specs(std::slice::from_ref(&host));
    manager.disconnect_now("studio");
    manager.sync_specs(std::slice::from_ref(&host));
    let error = manager.ensure_tunnel_socket("studio").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
    assert_eq!(manager.status("studio"), HostStatus::Disconnected);
    assert_eq!(manager.tunnel_starts("studio"), 0);
    assert!(
        !fake.dir.join("argv").exists(),
        "disconnect must not invoke ssh"
    );
    manager.reconnect_now("studio");
    assert_eq!(manager.status("studio"), HostStatus::Connecting);
    assert!(manager.ensure_tunnel_socket("studio").is_err());
    assert!(
        fake.dir.join("argv").exists(),
        "reconnect allows a new tunnel attempt"
    );
}

/// 창마다 관리자가 따로 있어도 꺼진 기기 재시도 ssh는 프로세스에서 한 번만 돈다. '다시 연결'은 그 판정을 바로 푼다.
#[test]
fn offline_verdict_is_shared_across_managers_until_reconnect() {
    let fake = FakeSsh::new("offline-shared", false);
    let attempts = || {
        fake.argv()
            .lines()
            .filter(|s| *s == "owen@studio.local")
            .count()
    };
    let host = spec(Some("/tmp/remote.sock"));
    let first = fake.manager();
    let second = fake.manager();
    first.sync_specs(std::slice::from_ref(&host));
    second.sync_specs(std::slice::from_ref(&host));

    first.slot("studio").unwrap().lock().unwrap().failures = OFFLINE_AFTER_FAILURES;
    assert!(first.ensure_tunnel_socket("studio").is_err());
    assert_eq!(attempts(), 1);

    // 새 창은 첫 시도를 한다: 그사이 기기가 켜졌을 수 있다.
    assert!(second.ensure_tunnel_socket("studio").is_err());
    assert_eq!(
        attempts(),
        2,
        "a fresh window tries once before trusting the verdict"
    );
    second.slot("studio").unwrap().lock().unwrap().next_retry_at = None;
    let err = second.ensure_tunnel_socket("studio").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(
        attempts(),
        2,
        "after its own failure the window follows the shared verdict"
    );
    assert_eq!(second.status("studio"), HostStatus::Disconnected);
    assert!(second.ensure_tunnel_socket("studio").is_err());
    assert_eq!(attempts(), 2);

    second.reconnect_now("studio");
    assert!(second.ensure_tunnel_socket("studio").is_err());
    assert_eq!(attempts(), 3, "explicit reconnect retries right away");
}

#[test]
fn ssh_target_registration_cannot_follow_a_replaced_or_removed_host_spec() {
    let manager = HostManager::new();
    let mut first_spec = spec(Some("/tmp/first-daemon.sock"));
    let host = HostId::Remote(first_spec.id.clone());
    manager.sync_specs(&[first_spec.clone()]);
    let first = manager.target_for(&host, None).unwrap();
    assert!(first.is_current());
    assert_eq!(first, manager.target_for(&host, None).unwrap());
    first_spec.label = "Renamed only".into();
    manager.sync_specs(&[first_spec.clone()]);
    assert!(first.is_current());
    assert_eq!(first, manager.target_for(&host, None).unwrap());
    first_spec.port = Some(2223);
    manager.sync_specs(&[first_spec.clone()]);
    assert!(
        !first.is_current(),
        "old connector must be invalid before UI accepts a new target map"
    );
    let replacement = manager.target_for(&host, None).unwrap();
    assert!(replacement.is_current());
    assert_ne!(first, replacement);
    let hello = krill_proto::daemon::client_hello(
        krill_proto::ipc::ClientRole::App,
        "stale-target-fixture",
    );
    assert!(
        matches!(first.connect(&hello), Err(krill_proto::daemon::Error::Io(error)) if error.kind() == io::ErrorKind::Interrupted)
    );
    manager.sync_specs(&[]);
    assert!(!replacement.is_current());
    manager.sync_specs(&[first_spec]);
    let recreated = manager.target_for(&host, None).unwrap();
    assert!(recreated.is_current());
    assert_ne!(replacement, recreated);
    assert!(
        !replacement.is_current(),
        "same host ID must not revive a removed registration"
    );
}

#[test]
fn ssh_target_stays_equal_across_tunnel_start_drop_and_restart() {
    let dir = std::env::temp_dir().join(format!("krill-target-eq-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let script = dir.join("ssh");
    fs::write(
        &script,
        r#"#!/bin/sh
prev=""
for a in "$@"; do
  if [ "$prev" = "-L" ]; then fwd="$a"; fi
  prev="$a"
done
exec /usr/bin/perl -MIO::Socket::UNIX -e '$s = IO::Socket::UNIX->new(Local => $ARGV[0], Listen => 5) or exit 3; while ($c = $s->accept) { close $c }' "${fwd%%:*}"
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

    let manager = HostManager::with_ssh_program(dir.clone(), script.into_os_string());
    let mut host_spec = spec(Some("/tmp/remote.sock"));
    let host = HostId::Remote(host_spec.id.clone());
    manager.sync_specs(&[host_spec.clone()]);

    let before_tunnel = manager.target_for(&host, None).unwrap();
    let initial_epoch = before_tunnel.transport_epoch();
    let first_sock = manager.ensure_tunnel_socket("studio").unwrap();
    let during_first_tunnel = manager.target_for(&host, None).unwrap();
    assert_eq!(
        before_tunnel, during_first_tunnel,
        "starting a tunnel must not change HostTarget identity"
    );
    assert_ne!(
        before_tunnel.socket(),
        first_sock.as_path(),
        "ephemeral tunnel socket stays independent of HostTarget::socket"
    );

    manager.disconnect_now("studio");
    let after_drop = manager.target_for(&host, None).unwrap();
    let after_drop_epoch = after_drop.transport_epoch();
    assert_eq!(
        before_tunnel, after_drop,
        "tunnel disconnect must not toggle HostTarget identity"
    );
    assert!(
        after_drop_epoch > initial_epoch,
        "disconnecting a tunnel must advance transport_epoch for snapshot cache invalidation"
    );

    manager.reconnect_now("studio");
    let second_sock = manager.ensure_tunnel_socket("studio").unwrap();
    assert_ne!(first_sock, second_sock);
    let during_second_tunnel = manager.target_for(&host, None).unwrap();
    assert_eq!(
        before_tunnel, during_second_tunnel,
        "tunnel restart must keep the same HostTarget identity"
    );
    assert!(
        during_second_tunnel.transport_epoch() > after_drop_epoch,
        "reconnecting a tunnel must advance transport_epoch"
    );

    host_spec.port = Some(2224);
    manager.sync_specs(&[host_spec]);
    let replaced = manager.target_for(&host, None).unwrap();
    assert_ne!(
        before_tunnel, replaced,
        "changing SSH transport config must still replace HostTarget"
    );

    drop(manager);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn concurrent_connects_on_cloned_target_never_steal_attempt_tokens() {
    let dir = std::env::temp_dir().join(format!("krill-token-race-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();

    let hello_bytes = krill_proto::ipc::ServerHello {
        proto_min: 1,
        proto_max: 1,
        features: 0,
        build: "test".into(),
        instance_id: 2,
    }
    .encode()
    .unwrap();
    fs::write(dir.join("hello.bin"), &hello_bytes).unwrap();

    let script = dir.join("ssh");
    fs::write(
        &script,
        r#"#!/bin/sh
base=$(dirname "$0")
prev=""
for a in "$@"; do
  if [ "$prev" = "-L" ]; then fwd="$a"; fi
  prev="$a"
done
sock="${fwd%%:*}"
if [ ! -f "$base/second" ]; then
  : > "$base/second"
  /usr/bin/perl -MIO::Socket::UNIX -e '
    my ($sock, $base) = @ARGV;
    my $s = IO::Socket::UNIX->new(Local => $sock, Listen => 5) or exit 3;
    if (my $c = $s->accept) {
      open my $fh, ">", "$base/accepted1" or die;
      close $fh;
      while (!-f "$base/release1") { select(undef, undef, undef, 0.01); }
      close $c;
    }
  ' "$sock" "$base" </dev/null >/dev/null 2>&1 &
  while [ ! -f "$base/release1" ]; do sleep 0.02; done
  exit 0
fi
exec /usr/bin/perl -MIO::Socket::UNIX -e '
  my ($sock, $base) = @ARGV;
  my $s = IO::Socket::UNIX->new(Local => $sock, Listen => 5) or exit 3;
  while (my $c = $s->accept) {
    open my $fh, ">", "$base/accepted2" or die;
    close $fh;
    while (!-f "$base/release2") { select(undef, undef, undef, 0.01); }
    open my $in, "<", "$base/hello.bin" or die;
    binmode $in;
    local $/;
    my $hello = <$in>;
    close $in;
    print $c $hello;
    $c->flush;
    while (-e $sock) { select(undef, undef, undef, 0.05); }
    close $c;
  }
' "$sock" "$base"
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

    let manager = HostManager::with_ssh_program(dir.clone(), script.into_os_string());
    let host_spec = spec(Some("/tmp/remote.sock"));
    let host = HostId::Remote(host_spec.id.clone());
    manager.sync_specs(&[host_spec]);

    let target_a = manager.target_for(&host, None).unwrap();
    let target_b = target_a.clone();
    let client_hello =
        krill_proto::daemon::client_hello(krill_proto::ipc::ClientRole::App, "token-race");

    let hello_a = client_hello.clone();
    let thread_a = thread::spawn(move || target_a.connect(&hello_a));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dir.join("accepted1").exists() {
        assert!(Instant::now() < deadline, "first attempt did not connect");
        thread::sleep(Duration::from_millis(10));
    }

    // Start a newer tunnel generation on Thread B while Thread A is still stalled inside connect().
    manager.reconnect_now("studio");
    let hello_b = client_hello;
    let thread_b = thread::spawn(move || target_b.connect(&hello_b));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dir.join("accepted2").exists() {
        assert!(Instant::now() < deadline, "second attempt did not connect");
        thread::sleep(Duration::from_millis(10));
    }
    let pid2 = manager.tunnel_pid("studio").unwrap();

    // Thread A fails first; it must report only its own stale gen1 token without killing gen2.
    fs::write(dir.join("release1"), "").unwrap();
    assert!(thread_a.join().unwrap().is_err());

    fs::write(dir.join("release2"), "").unwrap();
    let stream_b = thread_b.join().unwrap();
    assert!(
        stream_b.is_ok(),
        "stale Thread A failure must not kill Thread B's newer tunnel: {stream_b:?}"
    );
    drop(stream_b);

    assert_eq!(manager.status("studio"), HostStatus::Connected);
    assert_eq!(manager.tunnel_pid("studio"), Some(pid2));

    drop(manager);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn stale_host_target_record_outcome_cannot_drop_replacement_tunnel() {
    let dir =
        std::env::temp_dir().join(format!("krill-r3p05-stale-outcome-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();

    let script = dir.join("ssh");
    fs::write(
        &script,
        r#"#!/bin/sh
prev=""
for a in "$@"; do
  if [ "$prev" = "-L" ]; then fwd="$a"; fi
  prev="$a"
done
exec /usr/bin/perl -MIO::Socket::UNIX -e '$s = IO::Socket::UNIX->new(Local => $ARGV[0], Listen => 5) or exit 3; sleep 60' "${fwd%%:*}"
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();

    let manager = HostManager::with_ssh_program(dir.clone(), script.into_os_string());
    let mut host_spec = spec(Some("/tmp/remote.sock"));
    let host = HostId::Remote(host_spec.id.clone());
    manager.sync_specs(&[host_spec.clone()]);

    let old_target = manager.target_for(&host, None).unwrap();
    let _sock1 = manager.ensure_tunnel_socket("studio").unwrap();
    old_target.record_outcome(true, None);
    assert_eq!(manager.status("studio"), HostStatus::Connected);

    // 1. Transport config change creates a new registration and replacement tunnel.
    host_spec.port = Some(2223);
    manager.sync_specs(&[host_spec.clone()]);
    let new_target = manager.target_for(&host, None).unwrap();
    let _sock2 = manager.ensure_tunnel_socket("studio").unwrap();
    new_target.record_outcome(true, None);
    let pid2 = manager.tunnel_pid("studio").unwrap();
    assert_eq!(manager.status("studio"), HostStatus::Connected);

    let broken = io::Error::new(io::ErrorKind::BrokenPipe, "old tunnel closed");
    old_target.record_outcome(false, Some(&broken));
    assert_eq!(
        manager.tunnel_pid("studio"),
        Some(pid2),
        "stale HostTarget::record_outcome must not tear down replacement tunnel"
    );
    assert_eq!(manager.status("studio"), HostStatus::Connected);

    // 2. Remove & re-add host creates a brand-new slot; old replacement target must not touch it.
    manager.sync_specs(&[]);
    manager.sync_specs(&[host_spec]);
    let recreated_target = manager.target_for(&host, None).unwrap();
    let _sock3 = manager.ensure_tunnel_socket("studio").unwrap();
    recreated_target.record_outcome(true, None);
    let pid3 = manager.tunnel_pid("studio").unwrap();

    let eof = io::Error::new(io::ErrorKind::UnexpectedEof, "removed slot EOF");
    new_target.record_outcome(false, Some(&eof));
    assert_eq!(
        manager.tunnel_pid("studio"),
        Some(pid3),
        "removed HostTarget::record_outcome must not tear down re-added host tunnel"
    );
    assert_eq!(manager.status("studio"), HostStatus::Connected);

    drop(manager);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn sync_specs_transport_change_keeps_manually_disconnected_host_disconnected() {
    let fake = FakeSsh::new("r3p08-suspended-edit", false);
    let manager = fake.manager();
    let mut host_spec = spec(Some("/tmp/remote.sock"));
    let host = HostId::Remote(host_spec.id.clone());
    manager.sync_specs(&[host_spec.clone()]);

    manager.disconnect_now("studio");
    assert_eq!(manager.status("studio"), HostStatus::Disconnected);

    host_spec.port = Some(2299);
    manager.sync_specs(&[host_spec]);
    assert_eq!(
        manager.status("studio"),
        HostStatus::Disconnected,
        "transport edit on a suspended host must not strand status in Connecting"
    );
    assert_eq!(
        manager.statuses().first().map(|(_, status)| *status),
        Some(HostStatus::Disconnected)
    );
    let target = manager.target_for(&host, None).unwrap();
    assert!(!target.is_current());

    manager.reconnect_now("studio");
    assert_eq!(manager.status("studio"), HostStatus::Connecting);
    assert!(target.is_current());
}
