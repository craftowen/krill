//! 실제 앱 백엔드 → 짝짓기 → 공유 connect → 사이드바 소스·세션 생성·회수.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration assertions and process fixture"
)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use krill_app::hosts::link::{LinkHosts, connection_info, host_id};
use krill_app::hosts::{HostId, HostManager, HostTarget};
use krill_app::link_ui::qr::QrMatrixSource;
use krill_app::link_ui::real::{RealLinkBackend, RealQrMatrix};
use krill_app::link_ui::{LinkBackend, PairFailure, PairOutcome};
use krill_app::{pane, sidebar};
use krill_proto::control::{Create, Program, Reply, Request};
use krill_proto::daemon::{Paths, client_hello, handshake};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use std::os::unix::ffi::OsStrExt as _;

const WAIT: Duration = Duration::from_secs(20);

struct ProbeDir(PathBuf);
impl Drop for ProbeDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn link_helper_resolves_in_dev_and_bundle_layouts() {
    if let Some(expected) = std::env::var_os("MK_LINK_EXPECTED_PATH") {
        assert_eq!(
            fs::canonicalize(krill_app::hosts::link::executable()).unwrap(),
            fs::canonicalize(PathBuf::from(expected)).unwrap()
        );
        return;
    }
    let directory = ProbeDir(PathBuf::from(format!(
        "/tmp/m133b-paths-{}",
        std::process::id()
    )));
    for (executable, helper) in [
        (
            directory.0.join("debug/krill-app"),
            directory.0.join("debug/krill-link"),
        ),
        (
            directory.0.join("Krill.app/Contents/MacOS/krill-app"),
            directory.0.join("Krill.app/Contents/Helpers/krill-link"),
        ),
    ] {
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::create_dir_all(helper.parent().unwrap()).unwrap();
        fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        fs::write(&helper, "path fixture").unwrap();
        let output = Command::new(executable)
            .args([
                "--exact",
                "link_helper_resolves_in_dev_and_bundle_layouts",
                "--nocapture",
            ])
            .env("MK_LINK_EXPECTED_PATH", &helper)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn until(mut predicate: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + WAIT;
    while !predicate() {
        assert!(Instant::now() < deadline, "{message}");
        thread::sleep(Duration::from_millis(25));
    }
}

struct Fixture {
    base: PathBuf,
    bins: PathBuf,
    holders: Vec<Child>,
    server: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let base = PathBuf::from(format!("/tmp/m133b-{}", std::process::id()));
        fs::remove_dir_all(&base).ok();
        fs::create_dir_all(&base).unwrap();
        let executable = std::env::current_exe().unwrap();
        let bins = executable.parent().unwrap().parent().unwrap().to_path_buf();
        for name in ["krill", "krill-holder", "krilld", "krill-link"] {
            assert!(
                bins.join(name).exists(),
                "build workspace binaries before integration tests"
            );
        }
        let mut fixture = Self {
            base,
            bins,
            holders: Vec::new(),
            server: None,
        };
        for tag in ["s", "c"] {
            let state = fixture.base.join(tag);
            fs::create_dir_all(&state).unwrap();
            krill_link::LinkConfig {
                relay: krill_link::RelaySetting::Off,
                server_name: Some(format!("Test Mac {tag}")),
                ..Default::default()
            }
            .save(&state)
            .unwrap();
            fixture.holders.push(
                Command::new(fixture.bins.join("krill-holder"))
                    .arg(fixture.bins.join("krilld"))
                    .arg(state.join("run/krilld.sock"))
                    .env("KRILL_STATE_DIR", &state)
                    .env("KRILL_LOG_DIR", fixture.base.join("logs"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            until(
                || {
                    handshake(
                        &state.join("run/krilld.sock"),
                        &client_hello(ClientRole::App, "mk133b"),
                        Duration::from_secs(2),
                    )
                    .is_ok()
                },
                "daemon startup",
            );
        }
        fixture.server = Some(
            Command::new(fixture.bins.join("krill-link"))
                .arg("--state-dir")
                .arg(fixture.base.join("s"))
                .arg("serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        until(
            || {
                krill_link::is_serve_running(&fixture.base.join("s"))
                    && krill_link::read_live_server_addr(&fixture.base.join("s")).is_some()
            },
            "link server startup",
        );
        fixture
    }

    fn local(&self, tag: &str) -> HostTarget {
        HostTarget::local(Paths {
            socket: self.base.join(tag).join("run/krilld.sock"),
            holder: self.bins.join("krill-holder"),
            krilld: self.bins.join("krilld"),
        })
    }

    fn backend(&self, tag: &str) -> RealLinkBackend {
        RealLinkBackend::new(self.base.join(tag), self.bins.join("krill-link"))
    }

    fn cli(&self, tag: &str, args: &[&str]) -> String {
        let result = Command::new(self.bins.join("krill"))
            .args(args)
            .env("KRILL_STATE_DIR", self.base.join(tag))
            .env("KRILL_LOG_DIR", self.base.join("logs"))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(server) = &mut self.server {
            server.kill().ok();
            server.wait().ok();
        }
        let mut pids = Vec::new();
        for tag in ["s", "c"] {
            if let Ok(mut connection) = handshake(
                &self.base.join(tag).join("run/krilld.sock"),
                &client_hello(ClientRole::Cli, "mk133b-cleanup"),
                Duration::from_secs(2),
            ) {
                pids.push(connection.server.instance_id);
                connection
                    .stream
                    .write_all(&Request::List.encode().unwrap())
                    .ok();
                if let Ok(Some((kind, body))) = ipc::read_frame(&mut connection.stream)
                    && let Ok(Reply::Sessions(sessions)) = Reply::decode(kind, &body)
                {
                    for session in sessions {
                        connection
                            .stream
                            .write_all(
                                &Request::Close {
                                    session: session.session,
                                }
                                .encode()
                                .unwrap(),
                            )
                            .ok();
                        ipc::read_frame(&mut connection.stream).ok();
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(150));
        for holder in &mut self.holders {
            holder.kill().ok();
            holder.wait().ok();
        }
        for pid in pids {
            Command::new("/bin/kill")
                .args(["-9", &pid.to_string()])
                .status()
                .ok();
        }
        fs::remove_dir_all(&self.base).ok();
    }
}

fn create(target: &HostTarget, cwd: &Path) -> krill_proto::SessionId {
    let request = Create {
        account: None,
        program: Program::Custom {
            program: b"/bin/sh".to_vec(),
            args: vec![
                b"-c".to_vec(),
                b"stty -echo; while IFS= read -r cmd; do eval \"$cmd\"; done".to_vec(),
            ],
        },
        cwd: cwd.as_os_str().as_bytes().to_vec(),
        cols: 80,
        rows: 24,
    };
    match pane::ensure_session_in(target, None, &request).unwrap() {
        pane::Found::Live(id, true) => id,
        other => panic!("expected a new live session: {other:?}"),
    }
}

#[test]
fn disconnect_remote_mac_stops_owned_tunnel_preserves_ptys_and_blocks_old_targets() {
    let fixture = Fixture::new();
    let server = fixture.backend("s");
    let client = fixture.backend("c");
    let remote_session = create(&fixture.local("s"), &fixture.base.join("s"));
    let local_session = create(&fixture.local("c"), &fixture.base.join("c"));
    let invite = server.create_invite().unwrap();
    let PairOutcome::Paired(paired) = client.pair(&invite.link, "Client Mac") else {
        panic!("pair failed");
    };
    let service = LinkHosts::shared(fixture.base.join("c"), fixture.bins.join("krill-link"));
    let manager = HostManager::new();
    manager.sync_external_hosts(service.registrations().unwrap());
    let host = HostId::Remote(host_id(&paired.id));
    let old_target = manager.target_for(&host, None).unwrap();
    assert!(
        sidebar::status_source(old_target.clone())
            .fetch()
            .unwrap()
            .sessions
            .iter()
            .any(|s| s.id == remote_session)
    );
    let pid = service.owned_pids()[0];
    assert!(client.disconnect_host(&paired.id).unwrap().is_empty());
    assert!(
        krill_link::load_hosts(&fixture.base.join("c"))
            .unwrap()
            .is_empty()
    );
    assert!(service.owned_pids().is_empty());
    assert!(service.registrations().unwrap().is_empty());
    assert!(
        old_target
            .connect(&client_hello(ClientRole::App, "removed"))
            .is_err()
    );
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        sidebar::status_source(fixture.local("s"))
            .fetch()
            .unwrap()
            .sessions
            .iter()
            .any(|s| s.id == remote_session),
        "remote PTY survives disconnect"
    );
    assert!(
        sidebar::status_source(fixture.local("c"))
            .fetch()
            .unwrap()
            .sessions
            .iter()
            .any(|s| s.id == local_session),
        "local PTY survives disconnect"
    );
    assert_eq!(
        server.devices().unwrap().len(),
        1,
        "outgoing removal does not revoke inbound permissions"
    );
    // 같은 설정 화면을 띄운 다른 창의 중복 제거도 성공한다.
    assert!(client.disconnect_host(&paired.id).unwrap().is_empty());
    let invite = server.create_invite().unwrap();
    let PairOutcome::Paired(repaired) = client.pair(&invite.link, "Client Mac") else {
        panic!("re-pair failed");
    };
    assert_eq!(repaired.id, paired.id);
    manager.sync_external_hosts(service.registrations().unwrap());
    let new_target = manager.target_for(&host, None).unwrap();
    assert!(
        sidebar::status_source(new_target.clone())
            .fetch()
            .unwrap()
            .sessions
            .iter()
            .any(|s| s.id == remote_session)
    );
    assert!(
        old_target
            .connect(&client_hello(ClientRole::App, "old-after-repair"))
            .is_err()
    );
    service.shutdown();
}

#[test]
fn settings_disconnect_does_not_kill_a_cli_owned_tunnel() {
    let mut fixture = Fixture::new();
    let server = fixture.backend("s");
    let client = fixture.backend("c");
    let remote_session = create(&fixture.local("s"), &fixture.base.join("s"));
    let invite = server.create_invite().unwrap();
    let PairOutcome::Paired(paired) = client.pair(&invite.link, "Client Mac") else {
        panic!("pair failed");
    };
    let helper = Command::new(fixture.bins.join("krill-link"))
        .arg("--state-dir")
        .arg(fixture.base.join("c"))
        .args(["connect", &paired.id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let helper_index = fixture.holders.len();
    fixture.holders.push(helper);
    until(
        || {
            connection_info(&fixture.base.join("c"), &paired.id)
                .is_ok_and(|s| s.status == krill_hosts::HostStatus::Connected)
        },
        "external tunnel startup",
    );
    let service = LinkHosts::shared(fixture.base.join("c"), fixture.bins.join("krill-link"));
    let manager = HostManager::new();
    manager.sync_external_hosts(service.registrations().unwrap());
    let target = manager
        .target_for(&HostId::Remote(host_id(&paired.id)), None)
        .unwrap();
    assert!(
        sidebar::status_source(target.clone())
            .fetch()
            .unwrap()
            .sessions
            .iter()
            .any(|s| s.id == remote_session)
    );
    assert!(service.owned_pids().is_empty());
    client.disconnect_host(&paired.id).unwrap();
    assert!(
        fixture.holders[helper_index].try_wait().unwrap().is_none(),
        "the app must not terminate a borrowed CLI process"
    );
    assert!(
        target
            .connect(&client_hello(ClientRole::App, "removed-external"))
            .is_err()
    );
    assert!(
        sidebar::status_source(fixture.local("s"))
            .fetch()
            .unwrap()
            .sessions
            .iter()
            .any(|s| s.id == remote_session)
    );
}

#[test]
fn real_backend_pairs_lists_creates_revokes_and_releases_shared_connect() {
    let fixture = Fixture::new();
    let server = fixture.backend("s");
    let client = fixture.backend("c");
    let server_target = fixture.local("s");
    let local_target = fixture.local("c");
    let existing = create(&server_target, &fixture.base.join("s"));
    let local = create(&local_target, &fixture.base.join("c"));
    let before = krill_app::link_ui::current_epoch_secs();
    let invite = server.create_invite().unwrap();
    assert!(
        (before + 599..=krill_app::link_ui::current_epoch_secs() + 600)
            .contains(&invite.expires_at)
    );
    let matrix = RealQrMatrix.encode(&invite.link).unwrap();
    let actual = krill_link::encode_qr(invite.link.as_bytes()).unwrap();
    assert_eq!(matrix.len(), actual.size());
    assert!(matrix.iter().enumerate().all(|(row, cells)| {
        cells
            .iter()
            .enumerate()
            .all(|(col, dark)| *dark == actual.get(row, col))
    }));
    assert_eq!(
        client.pair("invalid", "Client Mac"),
        PairOutcome::Failed(PairFailure::InvalidLink)
    );
    let PairOutcome::Paired(paired) = client.pair(&invite.link, "Client Mac") else {
        panic!("pair failed");
    };
    assert_eq!(paired.name, "Test Mac s");
    let devices = server.devices().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "Client Mac");
    assert_eq!(devices[0].last_seen_at, None);

    let service = LinkHosts::shared(fixture.base.join("c"), fixture.bins.join("krill-link"));
    let second_window = LinkHosts::shared(fixture.base.join("c"), fixture.bins.join("krill-link"));
    assert!(Arc::ptr_eq(&service, &second_window));
    let manager = HostManager::new();
    manager.sync_specs(&[krill_app::hosts::RemoteHostSpec {
        id: "ssh-example".into(),
        label: "SSH Mac".into(),
        address: "example.local".into(),
        port: None,
        identity_file: None,
        remote_socket: None,
    }]);
    manager.sync_external_hosts(service.registrations().unwrap());
    assert!(
        service.owned_pids().is_empty(),
        "registration and status must not start connect"
    );
    assert_eq!(manager.specs().len(), 2, "SSH and iroh coexist");
    let host = HostId::Remote(host_id(&paired.id));
    let target = manager.target_for(&host, None).unwrap();
    assert!(!target.allow_local_launch());
    let source = sidebar::status_source(target.clone());
    let remote = source.fetch().unwrap();
    assert!(remote.sessions.iter().any(|s| s.id == existing));
    assert_eq!(service.owned_pids().len(), 1);
    let pid = service.owned_pids()[0];
    let other_manager = HostManager::new();
    other_manager.sync_external_hosts(second_window.registrations().unwrap());
    let other_target = other_manager.target_for(&host, None).unwrap();
    let other_source = sidebar::status_source(other_target.clone());
    let source_copy = source.clone();
    let concurrent = thread::spawn(move || source_copy.fetch().unwrap());
    assert!(
        other_source
            .fetch()
            .unwrap()
            .sessions
            .iter()
            .any(|s| s.id == existing)
    );
    concurrent.join().unwrap();
    assert_eq!(service.owned_pids(), vec![pid]);
    let new = create(&target, &fixture.base.join("s"));
    assert_ne!(existing, new);
    assert!(
        sidebar::status_source(server_target)
            .fetch()
            .unwrap()
            .sessions
            .iter()
            .any(|s| s.id == new)
    );
    assert!(
        !sidebar::status_source(local_target)
            .fetch()
            .unwrap()
            .sessions
            .iter()
            .any(|s| s.id == new)
    );
    until(
        || {
            connection_info(&fixture.base.join("c"), &paired.id)
                .unwrap()
                .detail
                .is_some()
        },
        "route and RTT status",
    );

    assert!(server.revoke(&devices[0].id).unwrap().is_empty());
    until(
        || {
            connection_info(&fixture.base.join("c"), &paired.id)
                .unwrap()
                .error
                .is_some()
        },
        "revoke must have explicit public status evidence",
    );
    manager.sync_external_hosts(service.registrations().unwrap());
    assert!(
        service.owned_pids().is_empty(),
        "revoked transport must stop its owned process"
    );
    let err = target
        .connect(&client_hello(ClientRole::App, "revoked"))
        .unwrap_err();
    assert!(
        err.to_string()
            .contains(krill_app::i18n::link_ui::ERR_REVOKED.get()),
        "{err}"
    );
    fixture.cli(
        "c",
        &[
            "send",
            &local.0.to_string(),
            "--enter",
            "--",
            "printf '%s%s\\n' LOCAL_ STILL_ALIVE",
        ],
    );
    until(
        || {
            fixture
                .cli("c", &["read", &local.0.to_string()])
                .contains("LOCAL_STILL_ALIVE")
        },
        "local PTY survives remote revoke",
    );
    assert!(
        sidebar::status_source(fixture.local("s"))
            .fetch()
            .unwrap()
            .sessions
            .iter()
            .any(|s| s.id == new),
        "revoke must not kill the remote PTY"
    );
    drop((source, other_source, target, other_target));
    until(
        || service.owned_pids().is_empty(),
        "zero connect processes when last consumer releases",
    );
    until(
        || {
            !Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        },
        "owned child reaped",
    );

    // 새 초대는 과거 권한 해제 상태를 지우고 같은 호스트·세션으로 복귀시킨다.
    let invitation = server.create_invite().unwrap();
    let PairOutcome::Paired(repaired) = client.pair(&invitation.link, "Client Mac") else {
        panic!("re-pair failed");
    };
    assert_eq!(paired.id, repaired.id);
    manager.sync_external_hosts(service.registrations().unwrap());
    let resumed = manager.target_for(&host, None).unwrap();
    let resumed_source = sidebar::status_source(resumed.clone());
    let snapshot = resumed_source.fetch().unwrap();
    assert!(snapshot.sessions.iter().any(|session| session.id == new));
    assert_eq!(service.owned_pids().len(), 1);
    drop((resumed_source, resumed));
    until(
        || service.owned_pids().is_empty(),
        "release the repaired connection",
    );

    // 앱 종료 훅은 다른 창의 타깃이 남아 있어도 소유 자식을 정리하고 재기동을 막는다.
    let closing_target = other_manager.target_for(&host, None).unwrap();
    drop(
        closing_target
            .connect(&client_hello(ClientRole::App, "closing"))
            .unwrap(),
    );
    assert_eq!(service.owned_pids().len(), 1);
    for service in krill_app::hosts::link::begin_shutdown() {
        service.shutdown();
    }
    assert!(service.owned_pids().is_empty());
    assert!(
        closing_target
            .connect(&client_hello(ClientRole::App, "after-close"))
            .is_err()
    );
    assert!(service.owned_pids().is_empty());

    let json = Command::new(fixture.bins.join("krill-link"))
        .arg("--state-dir")
        .arg(fixture.base.join("s"))
        .args(["invite", "--no-qr", "--json"])
        .output()
        .unwrap();
    assert!(json.status.success());
    let json: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(json.get("qr_unicode").is_none());
    assert!(json.get("link").is_some());
}
