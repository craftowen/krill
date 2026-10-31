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
