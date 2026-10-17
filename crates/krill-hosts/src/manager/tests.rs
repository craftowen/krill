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
