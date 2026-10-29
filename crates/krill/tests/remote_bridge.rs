//! Windows 원격: 원격 셸이 POSIX 질의를 못 돌리면 `krill __bridge`로 원격 krilld에 붙는다.
//!
//! 가짜 ssh가 Windows sshd처럼 `/bin/sh` 질의를 실패시키고, `krill __bridge`(진짜 이 빌드의 krill)를 "원격"으로
//! 실행한다. 진짜 holder·krilld를 원격으로 띄워 Hello·세션 생성·입출력이 중계를 거쳐 오가는지 본다.
//!
//! 재현: `cargo nextest run -p krill --test remote_bridge`.

#![cfg_attr(unix, expect(clippy::unwrap_used, reason = "integration test helpers"))]
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::daemon::{client_hello, handshake};
use krill_proto::ipc::ClientRole;

const MK: &str = env!("CARGO_BIN_EXE_krill");
const WAIT: Duration = Duration::from_secs(10);
const HOST: &str = "win-pc";

/// 원격 krilld(있으면)와 가짜 Windows ssh.
struct WindowsRemote {
    holder: Option<Child>,
    krilld: Option<u32>,
    remote_dir: PathBuf,
    local_dir: PathBuf,
    ssh: PathBuf,
}

impl WindowsRemote {
    /// `bridge`: 가짜 원격이 `krill __bridge`에 답하는 방식(셸 조각, `$@`는 `__bridge` 인자).
    fn new(tag: &str, daemon: bool, bridge: &str) -> Self {
        let base = std::env::temp_dir().join(format!("kb-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&base).ok();
        let remote_dir = base.join("r");
        let local_dir = base.join("l");
        fs::create_dir_all(&remote_dir).unwrap();
        fs::create_dir_all(&local_dir).unwrap();
        let (holder, krilld) = if daemon {
            let bin = Path::new(MK).parent().unwrap();
            let socket = remote_dir.join("run/krilld.sock");
            let holder = Command::new(bin.join("krill-holder"))
                .arg(bin.join("krilld"))
                .arg(&socket)
                .env("KRILL_STATE_DIR", &remote_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + WAIT;
            let krilld = loop {
                if let Ok(c) = handshake(&socket, &client_hello(ClientRole::Cli, "bridge"), WAIT) {
                    break u32::try_from(c.server.instance_id).unwrap();
                }
                assert!(Instant::now() < deadline, "remote daemon startup timed out");
                thread::sleep(Duration::from_millis(20));
            };
            (Some(holder), Some(krilld))
        } else {
            (None, None)
        };
        // Windows sshd 흉내: 원격 명령(마지막 인자)이 `/bin/sh` 질의면 cmd.exe처럼 실패한다.
        let ssh = local_dir.join("win-ssh.sh");
        fs::write(
            &ssh,
            format!(
                "#!/bin/sh\n\
                 for a in \"$@\"; do last=\"$a\"; done\n\
                 echo \"$@\" >>'{log}'\n\
                 case \"$last\" in\n\
                   'krill __bridge'*) set -- ${{last#krill }}; export KRILL_STATE_DIR='{remote}'; {bridge} ;;\n\
                   *) echo 'The system cannot find the path specified.' >&2; exit 1 ;;\n\
                 esac\n",
                log = local_dir.join("ssh-args.log").display(),
                remote = remote_dir.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            holder,
            krilld,
            remote_dir,
            local_dir,
            ssh,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(MK)
            .args(args)
            .env("KRILL_STATE_DIR", &self.local_dir)
            .env("KRILL_SSH_BIN", &self.ssh)
            .env_remove("KRILL_REMOTE_SOCK")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn wait_text(&self, id: &str, text: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let out = self.ok(&["--host", HOST, "read", id]);
            if out.contains(text) {
                return out;
            }
            assert!(Instant::now() < deadline, "missing {text:?}: {out:?}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn ssh_argv(&self) -> String {
        fs::read_to_string(self.local_dir.join("ssh-args.log")).unwrap_or_default()
    }
}

impl Drop for WindowsRemote {
    fn drop(&mut self) {
        if let Some(mut holder) = self.holder.take() {
            holder.kill().ok();
            holder.wait().ok();
        }
        if let Some(pid) = self.krilld {
            Command::new("/bin/kill")
                .args(["-9", &pid.to_string()])
                .status()
                .ok();
        }
        fs::remove_dir_all(self.local_dir.parent().unwrap()).ok();
    }
}

#[test]
fn windows_remote_sessions_run_through_krill_bridge() {
    let r = WindowsRemote::new("ok", true, &format!("exec '{MK}' \"$@\""));
    let id = r.ok(&[
        "--host",
        HOST,
        "new",
        "--cwd",
        r.remote_dir.to_str().unwrap(),
        "--",
        "/bin/sh",
        "-c",
        "stty -echo; printf 'bridge-ready\\n'; IFS= read -r line; printf 'got:%s\\n' \"$line\"",
    ]);
    let id = id.trim().to_owned();
    r.wait_text(&id, "bridge-ready");
    r.ok(&["--host", HOST, "send", &id, "--enter", "--", "over-bridge"]);
    r.wait_text(&id, "got:over-bridge");
    assert!(r.ok(&["--host", HOST, "ls"]).contains(&id));

    let argv = r.ssh_argv();
    assert!(argv.contains("/bin/sh -c"), "POSIX query first: {argv}");
    assert!(
        argv.lines()
            .any(|l| l.contains("ControlMaster=yes") && l.ends_with("krill __bridge --probe")),
        "keepalive opens the control master: {argv}"
    );
    assert!(
        argv.lines()
            .any(|l| l.contains("ControlMaster=no") && l.ends_with("krill __bridge")),
        "connections reuse the control master: {argv}"
    );
    assert!(!argv.contains(" -L "), "no unix socket forwarding: {argv}");
    assert!(
        !r.local_dir.join("run/krilld.sock").exists(),
        "a local daemon was launched for --host"
    );
    assert!(
        fs::read_dir(r.local_dir.join("run")).map_or(true, |d| d
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with("fwd-"))),
        "forward socket leaked"
    );
}

#[test]
fn windows_remote_failures_name_the_cause() {
    let stopped = WindowsRemote::new("off", false, &format!("exec '{MK}' \"$@\""));
    let out = stopped.run(&["--host", HOST, "ls"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("Krill is not running on the remote host"),
        "{err}"
    );

    let old = WindowsRemote::new("old", false, "echo 'usage: krill ls/new' >&2; exit 2");
    let out = old.run(&["--host", HOST, "ls"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("too old"), "{err}");
}

/// foundation-02: 페이지 스냅샷을 모르는 원격(N-1) 데몬이 단일 프레임 상한 오류를 내면 `--host` CLI는
/// 로컬 상태로 대체하지 않고(원격→로컬 fallback 금지) 원격 데몬 갱신을 요구하는 명시 오류로 끝난다.
mod orchestration_frame_bound {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use krill_proto::control::{Reply, Request};
    use krill_proto::ipc::{ClientHello, FEATURES, PROTO_MAX, PROTO_MIN, ServerHello, read_frame};
    use krill_proto::orchestration::{
        FEAT_ORCH_SNAPSHOT_PAGE, OrchAction, OrchResponse, OrchestrationStore, frame_limit_error,
    };
    use krill_proto::screen::Msg as _;

    #[test]
    fn prior_remote_daemon_snapshot_overflow_never_falls_back_to_local_state() {
        let r = WindowsRemote::new("orch-old", false, &format!("exec '{MK}' \"$@\""));
        // 로컬에는 원격과 다른 상태가 있다 — 출력에 나타나면 안 된다.
        let mut local = OrchestrationStore::default();
        let run = local.create_run("local-only", None).unwrap();
        local
            .create_task(&run.id, "local-only-task", "", vec![])
            .unwrap();
        local.save(&r.local_dir).unwrap();
        fs::create_dir_all(r.remote_dir.join("run")).unwrap();
        let listener = Arc::new(UnixListener::bind(r.remote_dir.join("run/krilld.sock")).unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let server = {
            let listener = Arc::clone(&listener);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            thread::spawn(move || serve_prior(socket));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        let out = r.run(&["--host", HOST, "orchestration", "task-list", "--json"]);
        stop.store(true, Ordering::Release);
        server.join().unwrap();
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("update the remote krilld"), "{err}");
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("local-only-task"),
            "remote overflow must never read local state"
        );
    }

    /// `FEAT_ORCH_SNAPSHOT_PAGE`를 모르는 원격 데몬 흉내: Hello 뒤 모든 요청에 프레임 상한 명시 오류.
    /// `krill __bridge --probe`는 연결만 하고 읽지 않으므로 Hello 뒤 EOF까지 기다리다 끝난다.
    fn serve_prior(mut socket: UnixStream) {
        socket.set_nonblocking(false).ok();
        socket.set_read_timeout(Some(WAIT)).ok();
        let hello = ServerHello {
            proto_min: PROTO_MIN,
            proto_max: PROTO_MAX,
            features: FEATURES & !FEAT_ORCH_SNAPSHOT_PAGE,
            build: "prior-remote-fixture".into(),
            instance_id: 7,
        };
        if socket.write_all(&hello.encode().unwrap()).is_err() {
            return;
        }
        let Ok(Some((kind, body))) = read_frame(&mut socket) else {
            return;
        };
        ClientHello::decode(kind, &body).unwrap();
        while let Ok(Some((kind, body))) = read_frame(&mut socket) {
            assert!(matches!(
                Request::decode(kind, &body).unwrap(),
                Request::Orchestration(OrchAction::SnapshotV2)
            ));
            let reply = Reply::Orchestration(OrchResponse::Error(frame_limit_error(1_204_357)));
            socket.write_all(&reply.encode().unwrap()).unwrap();
        }
    }
}
