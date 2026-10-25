//! MK-045: all MVP commands against real holder/krilld with no application process.

#![cfg_attr(unix, expect(clippy::unwrap_used, reason = "integration test helpers"))]
#![cfg(unix)]

use std::fs::{self, File};
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::control::{Reply, Request};
use krill_proto::daemon::{client_hello, handshake};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::termios::{Winsize, tcgetattr, tcsetwinsize};

const MK: &str = env!("CARGO_BIN_EXE_krill");
const WAIT: Duration = Duration::from_secs(10);

struct Daemon {
    holder: Child,
    krilld: u32,
    dir: PathBuf,
}
impl Daemon {
    fn new(tag: &str) -> Self {
        Self::configured(tag, None)
    }
    fn configured(tag: &str, config: Option<&str>) -> Self {
        Self::configured_schedules(tag, config, None)
    }
    fn configured_schedules(tag: &str, config: Option<&str>, schedules: Option<&str>) -> Self {
        Self::configured_schedules_home(tag, config, schedules, false)
    }
    fn configured_schedules_home(
        tag: &str,
        config: Option<&str>,
        schedules: Option<&str>,
        isolated_home: bool,
    ) -> Self {
        let dir = std::env::temp_dir().join(format!("mk045-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        // 데몬은 그룹 쓰기 가능한 상태 디렉터리를 거부한다. Ubuntu 기본 umask(002)에서도 실제 설치(0700)와 같게 만든다.
        fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        if let Some(config) = config {
            fs::write(dir.join("history.toml"), config).unwrap();
        }
        if let Some(schedules) = schedules {
            fs::write(dir.join("schedules.toml"), schedules).unwrap();
        }
        let bin = Path::new(MK).parent().unwrap();
        let socket = dir.join("run/krilld.sock");
        let holder = Command::new(bin.join("krill-holder"))
            .arg(bin.join("krilld"))
            .arg(&socket)
            .env("KRILL_CLOUDFLARED_BIN", dir.join("cloudflared-test"))
            // 외부 릴레이·공개 PWA 배포 상태에 의존하지 않는 격리된 모바일 게이트웨이.
            .env("KRILL_MOBILE_ORIGIN", "http://127.0.0.1:0")
            .env("KRILL_STATE_DIR", &dir)
            .envs(
                [
                    "HOME",
                    "CLAUDE_CONFIG_DIR",
                    "CODEX_HOME",
                    "ANTIGRAVITY_CLI_HOME",
                    "AGY_HOME",
                    "OPENCODE_CONFIG_DIR",
                    "OPENCODE_DATA_DIR",
                    "XDG_DATA_HOME",
                ]
                .into_iter()
                .filter(|_| isolated_home)
                .map(|name| (name, dir.join("empty-home"))),
            )
            .env_remove("KRILL_SESSION_ID")
            .env_remove("MK_SESSION_ID")
            .env_remove("KRILL_PANE_ID")
            .env_remove("MK_PANE_ID")
            .env_remove("KRILL_ORCH_DEPTH")
            .env_remove("MK_WORKER_DEPTH")
            .env_remove("KRILL_ORCH_MAX_DEPTH")
            .env_remove("MK_MAX_WORKER_DEPTH")
            .env_remove("KRILL_WORKTREES_DIR")
            .env_remove("KRILL_ORCA_DIR")
            .env_remove("KRILL_COMPUTER_OSASCRIPT_BIN")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        let krilld = loop {
            if let Ok(c) = handshake(&socket, &client_hello(ClientRole::Cli, "mk045"), WAIT) {
                break u32::try_from(c.server.instance_id).unwrap();
            }
            assert!(Instant::now() < deadline, "daemon startup timed out");
            thread::sleep(Duration::from_millis(20));
        };
        Self {
            holder,
            krilld,
            dir,
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(MK)
            .args(args)
            .env("KRILL_STATE_DIR", &self.dir)
            .env_remove("KRILL_SESSION_ID")
            .env_remove("MK_SESSION_ID")
            .env_remove("KRILL_PANE_ID")
            .env_remove("MK_PANE_ID")
            .env_remove("KRILL_ORCH_DEPTH")
            .env_remove("MK_WORKER_DEPTH")
            .env_remove("KRILL_ORCH_MAX_DEPTH")
            .env_remove("MK_MAX_WORKER_DEPTH")
            .env_remove("KRILL_WORKTREES_DIR")
            .env_remove("KRILL_ORCA_DIR")
            .env_remove("KRILL_COMPUTER_OSASCRIPT_BIN")
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
            let out = self.ok(&["read", id]);
            if out.contains(text) {
                return out;
            }
            assert!(Instant::now() < deadline, "missing {text:?}: {out:?}");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

/// `krill ls` 출력에서 세션 ID 존재 여부를 첫 칼럼 정확 매칭으로 판정한다.
/// 부분문자열(`contains("1\t")`)은 다른 행의 PID 끝자리와 오탐한다
/// (예: 남은 세션 행 `2\t12341\t...`는 `"1\t"`을 포함한다).
fn ls_has_session(ls: &str, id: &str) -> bool {
    ls.lines()
        .skip(1)
        .filter_map(|line| line.split('\t').next())
        .any(|first| first == id)
}
impl Drop for Daemon {
    fn drop(&mut self) {
        // Close our sessions first so a failing test never leaves a live shell behind.
        if let Ok(mut c) = handshake(
            &self.dir.join("run/krilld.sock"),
            &client_hello(ClientRole::Cli, "cleanup"),
            WAIT,
        ) {
            c.stream.write_all(&Request::List.encode().unwrap()).ok();
            if let Ok(Some((kind, body))) = ipc::read_frame(&mut c.stream)
                && let Ok(Reply::Sessions(sessions)) = Reply::decode(kind, &body)
            {
                for info in sessions {
                    c.stream
                        .write_all(
                            &Request::Close {
                                session: info.session,
                            }
                            .encode()
                            .unwrap(),
                        )
                        .ok();
                    ipc::read_frame(&mut c.stream).ok();
                }
                thread::sleep(Duration::from_millis(150));
            }
        }
        self.holder.kill().ok();
        self.holder.wait().ok();
        Command::new("/bin/kill")
            .args(["-9", &self.krilld.to_string()])
            .status()
            .ok();
        fs::remove_dir_all(&self.dir).ok();
    }
}

#[test]
fn commands_work_without_app_and_enforce_session_scope() {
    let d = Daemon::new("commands");
    assert!(d.ok(&["doctor"]).contains("daemon     ok"));
    let id = d.ok(&[
        "new",
        "--cwd",
        d.dir.to_str().unwrap(),
        "--",
        "/bin/sh",
        "-c",
        "stty -echo; printf 'ready\\n'; IFS= read -r line; printf 'received:%s\\n' \"$line\"; pwd",
    ]);
    let id = id.trim();
    d.wait_text(id, "ready");
    assert!(ls_has_session(&d.ok(&["ls"]), id));
    assert_eq!(
        d.run(&["wait", id, "--for", "done", "--timeout", "0"])
            .status
            .code(),
        Some(2)
    );
    assert!(
        d.ok(&["wait", id, "--for", "unknown", "--timeout", "0"])
            .contains("unknown")
    );

    let rejected = d.dir.join("rejected");
    let child_out = d.dir.join("child-out");
    let attack = format!(
        "KRILL_STATE_DIR='{dir}' '{krill}' send {id} --enter hijack >'{rej}' 2>&1; \
         printf '%s' $? >'{rej}.status'; \
         cid=$(KRILL_STATE_DIR='{dir}' '{krill}' new -- /bin/sh -c 'stty -echo; printf child-ready\\\\n; IFS= read -r l; printf child-got:%s\\\\n \"$l\"') && \
         KRILL_STATE_DIR='{dir}' '{krill}' wait \"$cid\" --for pattern:child-ready --timeout 5 && \
         KRILL_STATE_DIR='{dir}' '{krill}' send \"$cid\" --enter -- from-creator && \
         KRILL_STATE_DIR='{dir}' '{krill}' wait \"$cid\" --timeout 5 && \
         KRILL_STATE_DIR='{dir}' '{krill}' read \"$cid\" >'{cout}' 2>&1; \
         printf '%s' $? >'{cout}.status'",
        dir = d.dir.display(),
        krill = MK,
        id = id,
        rej = rejected.display(),
        cout = child_out.display(),
    );
    let attacker = d.ok(&["new", "--", "/bin/sh", "-c", &attack]);
    d.ok(&["wait", attacker.trim(), "--timeout", "10"]);
    assert_eq!(
        fs::read_to_string(rejected.with_extension("status")).unwrap(),
        "1"
    );
    assert!(
        fs::read_to_string(&rejected)
            .unwrap()
            .contains("no such session")
    );
    assert!(!d.ok(&["read", id]).contains("hijack"));
    assert_eq!(
        fs::read_to_string(child_out.with_extension("status")).unwrap(),
        "0",
        "child output: {}",
        fs::read_to_string(&child_out).unwrap_or_default()
    );
    assert!(
        fs::read_to_string(&child_out)
            .unwrap()
            .contains("child-got:from-creator")
    );

    d.ok(&["send", id, "--enter", "--", "안녕"]);
    d.ok(&["wait", id, "--timeout", "5"]);
    let text = d.wait_text(id, "received:안녕");
    assert!(text.contains(d.dir.to_str().unwrap()));
    assert!(d.ok(&["ls"]).contains("exited\texited"));
    assert_eq!(d.run(&["read", "999999"]).status.code(), Some(1));
    assert_eq!(d.run(&["send", id, "late"]).status.code(), Some(1));
    assert_eq!(d.run(&["new", "--cols", "0"]).status.code(), Some(2));
}

fn host() -> (OwnedFd, OwnedFd) {
    let pty = rustix_openpty::openpty(None, None).unwrap();
    tcsetwinsize(
        &pty.controller,
        Winsize {
            ws_col: 90,
            ws_row: 27,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    (pty.controller, pty.user)
}

fn receive_until(fd: &OwnedFd, bytes: &mut Vec<u8>, text: &str) {
    let deadline = Instant::now() + WAIT;
    while !String::from_utf8_lossy(bytes).contains(text) {
        assert!(
            Instant::now() < deadline,
            "missing {text:?}: {:?}",
            String::from_utf8_lossy(bytes)
        );
        let mut fds = [PollFd::new(fd, PollFlags::IN)];
        poll(
            &mut fds,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: 100_000_000,
            }),
        )
        .unwrap();
        if !fds[0].revents().is_empty() {
            let mut buf = [0; 8192];
            let n = rustix::io::read(fd, &mut buf).unwrap();
            bytes.extend_from_slice(&buf[..n]);
        }
    }
}

#[test]
fn attach_uses_a_normal_terminal_and_detach_preserves_session() {
    let d = Daemon::new("attach");
    let id = d.ok(&["new", "--", "/bin/sh", "-c", "stty -echo; printf 'before-attach\\n'; IFS= read -r line; printf 'terminal:%s\\n' \"$line\"; stty size; while IFS= read -r line; do [ \"$line\" = done ] && break; printf 'resized:%s:%s\\n' \"$line\" \"$(stty size)\"; done; IFS= read -r line; printf 'after:%s\\n' \"$line\"; stty size"]);
    let id = id.trim();
    d.wait_text(id, "before-attach");
    let (master, slave) = host();
    let before = tcgetattr(&master).unwrap();
    let mut attach = Command::new(MK)
        .args(["attach", id])
        .env("KRILL_STATE_DIR", &d.dir)
        .stdin(File::from(slave.try_clone().unwrap()))
        .stdout(File::from(slave))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut bytes = Vec::new();
    receive_until(&master, &mut bytes, "before-attach");
    rustix::io::write(&master, b"from-terminal\r").unwrap();
    receive_until(&master, &mut bytes, "terminal:from-terminal");
    receive_until(&master, &mut bytes, "27 90");
    // Dynamic host resize is propagated and preserved across subsequent key input on the same attach stream.
    tcsetwinsize(
        &master,
        Winsize {
            ws_col: 100,
            ws_row: 30,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        rustix::io::write(&master, b"probe\r").unwrap();
        receive_until(&master, &mut bytes, "resized:probe:");
        let screen = d.ok(&["read", id]);
        if screen.contains("resized:probe:30 100") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "attach key input reverted or never applied resize: {screen}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    rustix::io::write(&master, b"after-key\r").unwrap();
    receive_until(&master, &mut bytes, "resized:after-key:30 100");
    rustix::io::write(&master, b"done\r\x1d").unwrap();
    assert!(attach.wait().unwrap().success());
    assert_eq!(
        format!("{:?}", tcgetattr(&master).unwrap()),
        format!("{before:?}"),
        "host termios was not restored"
    );
    d.ok(&["send", id, "--enter", "still-alive"]);
    d.ok(&["wait", id, "--timeout", "5"]);
    let final_screen = d.wait_text(id, "after:still-alive");
    assert!(final_screen.contains("30 100"), "{final_screen}");
    assert_eq!(
        String::from_utf8_lossy(&bytes)
            .matches("terminal:from-terminal")
            .count(),
        1
    );
}

#[test]
fn history_clear_preserves_live_session_and_restarts_recording() {
    let d = Daemon::new("history");
    let id = d.ok(&["new", "--", "/bin/sh", "-c",
        "stty -echo; printf 'old-secret'; IFS= read -r line; printf 'new-output'; IFS= read -r line"]);
    let id = id.trim();
    d.wait_text(id, "old-secret");
    // PTY append follows screen publication on the reader thread.
    let deadline = Instant::now() + WAIT;
    let session = d.dir.join("sessions").join(format!("{id}.noindex"));
    while fs::read_dir(&session).unwrap().next().is_none() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert!(d.ok(&["history", "clear"]).contains("history cleared"));
    assert!(!d.dir.join("sessions").exists());
    assert!(d.ok(&["read", id]).contains("old-secret"));
    d.ok(&["history", "clear"]);
    d.ok(&["send", id, "--enter", "go"]);
    d.wait_text(id, "new-output");
    assert!(ls_has_session(&d.ok(&["ls"]), id));
    // A session descendant cannot erase all sessions' history.
    let report = d.dir.join("denied.status");
    let script = format!(
        "KRILL_STATE_DIR='{}' '{}' history clear; printf '%s' $? > '{}'",
        d.dir.display(),
        MK,
        report.display()
    );
    let attacker = d.ok(&["new", "--", "/bin/sh", "-c", &script]);
    d.ok(&["wait", attacker.trim(), "--timeout", "5"]);
    assert_eq!(fs::read_to_string(report).unwrap(), "1");
    assert!(session.exists());
    assert_eq!(d.run(&["history"]).status.code(), Some(2));
    d.ok(&["history", "clear"]);
    assert!(!d.dir.join("sessions").exists());
}

#[test]
fn history_disabled_has_no_disk_recording_through_real_pty() {
    let d = Daemon::configured("history-disabled", Some("recording = false"));
    let id = d.ok(&["new", "--", "/bin/sh", "-c", "printf 'private-output'"]);
    d.ok(&["wait", id.trim(), "--timeout", "5"]);
    assert!(d.ok(&["read", id.trim()]).contains("private-output"));
    assert!(!d.dir.join("sessions").exists());
    d.ok(&["history", "clear"]);
    assert!(!d.dir.join("sessions").exists());
}

/// MK-055: 사용자 전역 설정·실제 유료 CLI 없이 `Program::Agent` 경로를 실제 PTY로 검증한다.
#[test]
fn agent_profile_runs_in_non_git_folder_with_literal_path() {
    let d = Daemon::new("folder-agent");
    let folder = d.dir.join("한글 folder 'quotes' ;$name");
    fs::create_dir(&folder).unwrap();
    assert!(!folder.join(".git").exists());
    fs::write(d.dir.join("agents.toml"), r#"
[agent.folder_probe]
name = "Folder probe"
launch = ["/bin/sh", "-c", "printf 'folder-agent-ready\\n'; /bin/pwd -P; IFS= read -r line; printf 'agent-received:%s\\n' \"$line\""]
"#).unwrap();
    let id = d.ok(&[
        "new",
        "--cols",
        "240",
        "--cwd",
        folder.to_str().unwrap(),
        "--agent",
        "folder_probe",
    ]);
    let id = id.trim();
    let text = d.wait_text(id, "folder-agent-ready");
    let expected = fs::canonicalize(&folder).unwrap();
    let text = if text.contains(expected.to_str().unwrap()) {
        text
    } else {
        d.wait_text(id, expected.to_str().unwrap())
    };
    assert!(text.contains(expected.to_str().unwrap()), "{text}");
    d.ok(&["send", id, "--enter", "folder-input"]);
    d.ok(&["wait", id, "--timeout", "5"]);
    d.wait_text(id, "agent-received:folder-input");
    assert!(!folder.join(".git").exists());
}

#[test]
fn creator_can_read_child_after_daemon_sigkill() {
    let mut d = Daemon::new("creator-restart");
    let script = format!(
        r#"stty -echo
cid=$(KRILL_STATE_DIR='{dir}' '{MK}' new -- /bin/sh -c 'stty -echo; IFS= read -r _; printf child-alive; exec sleep 600')
printf 'child-created\n'
IFS= read -r _
KRILL_STATE_DIR='{dir}' '{MK}' send "$cid" --enter ping &&
KRILL_STATE_DIR='{dir}' '{MK}' wait "$cid" --for pattern:child-alive --timeout 5 &&
KRILL_STATE_DIR='{dir}' '{MK}' read "$cid" >'{dir}/child-read' 2>'{dir}/child-error'
printf '%s' $? >'{dir}/child-status'
printf 'child-read-finished\n'
exec sleep 600"#,
        dir = d.dir.display()
    );
    let parent = d.ok(&["new", "--", "/bin/sh", "-c", &script]);
    let parent = parent.trim();
    d.wait_text(parent, "child-created");
    Command::new("/bin/kill")
        .args(["-9", &d.krilld.to_string()])
        .status()
        .unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(c) = handshake(
            &d.dir.join("run/krilld.sock"),
            &client_hello(ClientRole::Cli, "restart"),
            WAIT,
        ) {
            let next = u32::try_from(c.server.instance_id).unwrap();
            if next != d.krilld {
                d.krilld = next;
                break;
            }
        }
        assert!(Instant::now() < deadline, "daemon did not restart");
        thread::sleep(Duration::from_millis(20));
    }
    d.ok(&["send", parent, "--enter", "read-child"]);
    d.wait_text(parent, "child-read-finished");
    assert_eq!(
        fs::read_to_string(d.dir.join("child-status")).unwrap(),
        "0",
        "{}",
        fs::read_to_string(d.dir.join("child-error")).unwrap_or_default()
    );
    assert!(
        fs::read_to_string(d.dir.join("child-read"))
            .unwrap()
            .contains("child-alive")
    );
}

#[test]
fn resume_rejects_a_running_session_without_spawning_another_process() {
    use krill_proto::control::{Create, Failure, Program};
    let d = Daemon::new("resume-running");
    let id = d.ok(&[
        "new",
        "--",
        "/bin/sh",
        "-c",
        "printf resume-ready; exec sleep 600",
    ]);
    let id = id.trim();
    d.wait_text(id, "resume-ready");
    let mut c = handshake(
        &d.dir.join("run/krilld.sock"),
        &client_hello(ClientRole::Cli, "resume"),
        WAIT,
    )
    .unwrap();
    let req = Request::Resume {
        session: krill_proto::SessionId(id.parse().unwrap()),
        create: Create {
            account: None,
            program: Program::Custom {
                program: b"/bin/sleep".to_vec(),
                args: vec![b"600".to_vec()],
            },
            cwd: vec![],
            cols: 80,
            rows: 24,
        },
    };
    c.stream.write_all(&req.encode().unwrap()).unwrap();
    let (kind, body) = ipc::read_frame(&mut c.stream).unwrap().unwrap();
    assert_eq!(
        Reply::decode(kind, &body).unwrap(),
        Reply::Failed(Failure::BadRequest)
    );
    c.stream
        .write_all(&Request::List.encode().unwrap())
        .unwrap();
    let (kind, body) = ipc::read_frame(&mut c.stream).unwrap().unwrap();
    assert!(
        matches!(Reply::decode(kind, &body).unwrap(), Reply::Sessions(sessions) if sessions.len() == 1)
    );
    assert!(d.ok(&["read", id]).contains("resume-ready"));
}

#[test]
fn scheduled_prompts_run_without_app_and_do_not_replay_after_daemon_restart() {
    use std::collections::HashSet;
    use std::os::unix::fs::PermissionsExt as _;

    let config = r#"[[schedule]]
id = "literal"
cron = "* * * * *"
cwd = "/tmp"
command = ["/bin/sh", "-c", "printf '%s\\n' \"$1\"; sleep 5", "schedule"]
prompt = "한글 $(literal); 'quote'"
"#;
    let mut d = Daemon::configured_schedules("schedule", None, Some(config));
    let deadline = Instant::now() + WAIT;
    let session = loop {
        let runs = d.ok(&["schedule", "runs"]);
        let rows: Vec<serde_json::Value> = runs
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        if let Some(session) = rows
            .iter()
            .find(|r| r["phase"] == "spawned")
            .and_then(|r| r["session"].as_u64())
        {
            break session.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "scheduled process not spawned: {runs}"
        );
        thread::sleep(Duration::from_millis(25));
    };
    d.wait_text(&session, "한글 $(literal); 'quote'");
    assert!(d.ok(&["schedule", "list"]).contains("literal"));
    let before = fs::read(d.dir.join("schedules.toml")).unwrap();
    assert!(
        !d.run(&[
            "schedule",
            "add",
            "bad",
            "--cron",
            "*/0 * * * *",
            "--prompt",
            "bad",
            "--",
            "/bin/echo"
        ])
        .status
        .success()
    );
    assert_eq!(fs::read(d.dir.join("schedules.toml")).unwrap(), before);
    d.ok(&[
        "schedule",
        "add",
        "future",
        "--cron",
        "0 0 31 2 *",
        "--prompt",
        "never",
        "--",
        "/bin/echo",
    ]);
    assert!(d.ok(&["schedule", "list"]).contains("future"));
    assert_eq!(
        fs::metadata(d.dir.join("schedules.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    d.ok(&["schedule", "rm", "future"]);
    assert!(!d.ok(&["schedule", "list"]).contains("future"));

    let old = d.krilld;
    Command::new("/bin/kill")
        .args(["-9", &old.to_string()])
        .status()
        .unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(c) = handshake(
            &d.dir.join("run/krilld.sock"),
            &client_hello(ClientRole::Cli, "mk101-restart"),
            Duration::from_millis(100),
        ) {
            let pid = u32::try_from(c.server.instance_id).unwrap();
            if pid != old {
                d.krilld = pid;
                break;
            }
        }
        assert!(Instant::now() < deadline, "daemon did not restart");
        thread::sleep(Duration::from_millis(25));
    }
    thread::sleep(Duration::from_millis(1200));
    let mut claimed = HashSet::new();
    let runs = d.ok(&["schedule", "runs"]);
    for line in runs.lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        if row["phase"] == "started" {
            assert!(
                claimed.insert((row["id"].clone(), row["minute"].clone())),
                "duplicate claim: {runs}"
            );
        }
    }
    d.ok(&["schedule", "rm", "literal"]);
    assert!(!d.ok(&["schedule", "list"]).contains("literal"));
}

/// 내장 `krill` 스킬 문서가 안내하는 세션 명령을 그대로 실제 krilld에 돌린다.
#[test]
fn krill_skill_session_commands_match_cli() {
    let skill = krill_proto::orchestration::BUILTIN_SKILLS
        .iter()
        .find(|s| s.name == "krill")
        .unwrap();
    let documented = |cmd: &str| assert!(skill.content.contains(cmd), "{cmd}");
    let d = Daemon::new("skill-doc");
    let id = d.ok(&[
        "new",
        "--cwd",
        d.dir.to_str().unwrap(),
        "--",
        "/bin/sh",
        "-c",
        "stty -echo; printf 'ready\\n'; IFS= read -r line; printf 'got:%s\\n' \"$line\"; exec sleep 60",
    ]);
    let id = id.trim();
    d.wait_text(id, "ready");

    documented("krill ls --json");
    let ls: serde_json::Value = serde_json::from_str(&d.ok(&["ls", "--json"])).unwrap();
    assert!(
        ls.as_array()
            .unwrap()
            .iter()
            .any(
                |s| s["id"].as_u64().map(|n| n.to_string()).as_deref() == Some(id)
                    && s["process"] == "running"
            ),
        "{ls}"
    );

    documented("krill send <session_id> --text \"<text>\" --enter");
    d.ok(&["send", id, "--text", "hello skill", "--enter"]);
    d.wait_text(id, "got:hello skill");

    documented("krill read <session_id> --lines 80");
    assert!(
        d.ok(&["read", id, "--lines", "80"])
            .contains("got:hello skill")
    );
    assert!(d.ok(&["read", id, "--lines", "1"]).lines().count() <= 1);

    documented("krill wait <session_id> --for idle --timeout 30");
    // 평범한 셸은 상태를 몰라 idle에 닿지 않는다 — 파서를 통과해 시간 초과(2)로 끝나야 한다.
    let wait = d.run(&["wait", id, "--for", "idle", "--timeout", "0"]);
    assert_eq!(wait.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&wait.stderr).contains("wait timed out"));

    documented("krill kill <session_id>");
    d.ok(&["kill", id]);
    assert!(!ls_has_session(&d.ok(&["ls"]), id));
}

#[test]
fn session_checkpoint_references_and_release_preserve_the_selected_worktree_and_output() {
    let d = Daemon::new("m8-session-context");
    let repo = d.dir.join("repo");
    let sub = repo.join("nested");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(&sub).unwrap();
    let key = fs::canonicalize(&repo)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let id = d.ok(&["new", "--cwd", sub.to_str().unwrap(), "--", "/bin/sh", "-c",
        "stty -echo; printf 'archive-marker\\n'; while IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done"]);
    let id = id.trim();
    d.wait_text(id, "archive-marker");
    let checkpoint = d.ok(&[
        "set",
        "--session",
        id,
        "--comment",
        "selected session",
        "--json",
    ]);
    let checkpoint: serde_json::Value = serde_json::from_str(&checkpoint).unwrap();
    assert_eq!(checkpoint["worktree"], key);
    let refs = d.ok(&[
        "reference",
        "add",
        "https://example.com/session",
        "--session",
        id,
        "--json",
    ]);
    let refs: serde_json::Value = serde_json::from_str(&refs).unwrap();
    assert_eq!(refs["worktree"], key);
    let before = fs::read(d.dir.join("worktree-comments.toml")).unwrap();
    for args in [
        vec!["set", "--session", "999999", "--comment", "wrong"],
        vec!["reference", "add", "wrong", "--session", "999999"],
    ] {
        assert_eq!(d.run(&args).status.code(), Some(1));
        assert_eq!(
            fs::read(d.dir.join("worktree-comments.toml")).unwrap(),
            before
        );
    }

    let run: serde_json::Value = serde_json::from_str(&d.ok(&[
        "orchestration",
        "run-create",
        "--title",
        "Archive",
        "--json",
    ]))
    .unwrap();
    let run = run["id"].as_str().unwrap();
    let task: serde_json::Value = serde_json::from_str(&d.ok(&[
        "orchestration",
        "task-create",
        "--run",
        run,
        "--title",
        "Work",
        "--json",
    ]))
    .unwrap();
    let task = task["id"].as_str().unwrap();
    let dispatch: serde_json::Value = serde_json::from_str(&d.ok(&[
        "orchestration",
        "worker-start",
        "--run",
        run,
        "--task",
        task,
        "--worker",
        "archive-worker",
        "--session",
        id,
        "--prompt",
        "Work",
        "--json",
    ]))
    .unwrap();
    let dispatch = dispatch["id"].as_str().unwrap();
    let before_sessions = d.ok(&["ls", "--json"]);
    let before_store = fs::read(d.dir.join("orchestration.json")).unwrap();
    assert!(
        !d.run(&[
            "orchestration",
            "worker-start",
            "--run",
            run,
            "--task",
            task,
            "--worker",
            "replacement",
            "--prompt",
            "retry"
        ])
        .status
        .success()
    );
    assert_eq!(d.ok(&["ls", "--json"]), before_sessions);
    assert_eq!(
        fs::read(d.dir.join("orchestration.json")).unwrap(),
        before_store
    );
    d.ok(&["send", id, "--text", "still-alive", "--enter"]);
    d.wait_text(id, "got:still-alive");
    d.ok(&[
        "orchestration",
        "send",
        "--from",
        "archive-worker",
        "--to",
        "coordinator",
        "--type",
        "worker_done",
        "--dispatch",
        dispatch,
        "--summary",
        "Result summary",
    ]);
    let released: serde_json::Value =
        serde_json::from_str(&d.ok(&["orchestration", "worker-release", dispatch, "--json"]))
            .unwrap();
    assert_eq!(released["summary"], "Result summary");
    assert!(
        released["archived_output"]
            .as_str()
            .unwrap()
            .contains("archive-marker")
    );
    assert!(!ls_has_session(&d.ok(&["ls"]), id));
    let output: serde_json::Value =
        serde_json::from_str(&d.ok(&["orchestration", "worker-read", dispatch, "--json"])).unwrap();
    assert!(
        output["output"]
            .as_str()
            .unwrap()
            .contains("archive-marker")
    );
    d.ok(&["orchestration", "worker-release", dispatch]);
}

#[test]
fn kill_and_close_wait_for_termination_including_signal_escalation() {
    let d = Daemon::new("close-wait");
    for command in ["kill", "close"] {
        let id = d.ok(&[
            "new",
            "--",
            "/bin/sh",
            "-c",
            "trap '' HUP TERM; printf 'ignore-signals-ready\\n'; while :; do sleep 0.1; done",
        ]);
        let id = id.trim();
        d.wait_text(id, "ignore-signals-ready");
        d.ok(&[command, id]);
        assert!(!ls_has_session(&d.ok(&["ls"]), id));
    }
}

#[test]
fn worker_start_validates_run_and_task_before_spawn_and_reports_delivery_errors() {
    let d = Daemon::new("worker-preflight");
    let run: serde_json::Value = serde_json::from_str(&d.ok(&[
        "orchestration",
        "run-create",
        "--title",
        "Preflight",
        "--json",
    ]))
    .unwrap();
    let run = run["id"].as_str().unwrap();
    let before_sessions = d.ok(&["ls", "--json"]);
    let before_store = fs::read(d.dir.join("orchestration.json")).unwrap();

    // 잘못된 Run/Task는 에이전트 스폰 시도보다 먼저 거부해야 한다.
    // 존재하지 않는 프로필을 지정해 스폰 오류가 선검증 오류를 가리는 회귀도 잡는다.
    for (run_id, task, expected) in [
        ("missing-run", None, "run not found"),
        (run, Some("missing-task"), "task not found"),
    ] {
        let mut args = vec![
            "orchestration",
            "worker-start",
            "--run",
            run_id,
            "--worker",
            "invalid",
            "--agent",
            "missing-preflight-agent",
            "--prompt",
            "Work",
        ];
        if let Some(task) = task {
            args.extend(["--task", task]);
        }
        let out = d.run(&args);
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(expected),
            "{out:?}"
        );
        assert_eq!(d.ok(&["ls", "--json"]), before_sessions);
        assert_eq!(
            fs::read(d.dir.join("orchestration.json")).unwrap(),
            before_store
        );
    }

    let spawn = d.run(&[
        "orchestration",
        "worker-start",
        "--run",
        run,
        "--worker",
        "spawn-failure",
        "--agent",
        "missing-preflight-agent",
        "--prompt",
        "Work",
    ]);
    assert_eq!(spawn.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&spawn.stderr)
            .contains("spawn missing-preflight-agent worker session")
    );
    assert_eq!(d.ok(&["ls", "--json"]), before_sessions);
    assert_eq!(
        fs::read(d.dir.join("orchestration.json")).unwrap(),
        before_store
    );

    // 존재하지 않는 세션을 지정하면 고스트 Active dispatch를 남기지 않고 시작 전에 거부한다(C1).
    // 예전에는 dispatch를 Active로 기록한 뒤 전송 실패로 끝내 stop/release가 영구 실패하는 쐐기가 됐다.
    let delivered = d.run(&[
        "orchestration",
        "worker-start",
        "--run",
        run,
        "--worker",
        "delivery-failure",
        "--session",
        "999999",
        "--prompt",
        "Work",
        "--json",
    ]);
    assert_eq!(delivered.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&delivered.stderr).contains("session 999999 not found"),
        "{:?}",
        delivered.stderr
    );
    assert!(
        delivered.stdout.is_empty(),
        "ghost dispatch must not be printed: {:?}",
        delivered.stdout
    );
    assert_eq!(
        fs::read(d.dir.join("orchestration.json")).unwrap(),
        before_store
    );
}

/// `worker-start`는 TUI가 프롬프트를 띄운 뒤에 지시문을 넣고, 부팅 중 죽은 에이전트는 실패로 알린다.
/// `worker-stop`은 상태만 바꾸지 않고 krilld 세션을 닫는다.
#[test]
fn orchestration_worker_start_waits_for_tui_and_stop_closes_session() {
    let d = Daemon::new("orch-worker");
    fs::write(d.dir.join("agents.toml"), r#"
[agent.slow_tui]
name = "Slow TUI"
launch = ["/bin/bash", "-c", "stty -echo; sleep 1; while IFS= read -r -t 1 junk; do printf 'early-input\n'; done; printf 'Slow TUI\n> '; IFS= read -r line; printf 'agent-received:%.20s\n' \"$line\"; exec sleep 60"]

[agent.broken]
name = "Broken"
launch = ["/bin/sh", "-c", "exit 3"]
"#).unwrap();
    let cwd = d.dir.to_str().unwrap();
    let run = d.ok(&["orchestration", "run-create", "--title", "R", "--json"]);
    let run: serde_json::Value = serde_json::from_str(&run).unwrap();
    let run = run["id"].as_str().unwrap();

    let started = d.ok(&[
        "orchestration",
        "worker-start",
        "--run",
        run,
        "--worker",
        "w1",
        "--agent",
        "slow_tui",
        "--cwd",
        cwd,
        "--prompt",
        "do it",
        "--json",
    ]);
    let started: serde_json::Value = serde_json::from_str(&started).unwrap();
    let sid = started["session_id"].as_u64().unwrap().to_string();
    // 부팅(약 2초) 중에 들어온 입력은 가짜 TUI가 버리고 `early-input`을 찍는다.
    let text = d.wait_text(&sid, "agent-received:");
    assert!(!text.contains("early-input"), "{text}");

    let broken = d.run(&[
        "orchestration",
        "worker-start",
        "--run",
        run,
        "--worker",
        "w2",
        "--agent",
        "broken",
        "--cwd",
        cwd,
        "--prompt",
        "do it",
    ]);
    assert!(!broken.status.success());
    assert!(
        String::from_utf8_lossy(&broken.stderr).contains("exited during startup"),
        "{}",
        String::from_utf8_lossy(&broken.stderr)
    );

    d.ok(&["orchestration", "worker-stop", "w1"]);
    assert!(!ls_has_session(&d.ok(&["ls"]), &sid));
}

/// 세션이 먼저 사라져도 `worker-stop`/`worker-release`는 '이미 종료됨'으로 정산한다(C1).
/// 예전에는 capture 실패 지점에서 매번 실패해 고스트 Active dispatch가 영구히 남았다.
#[test]
fn orchestration_worker_stop_settles_after_session_killed() {
    let d = Daemon::new("orch-stop-gone");
    fs::write(
        d.dir.join("agents.toml"),
        r#"
[agent.owned_fixture]
name = "Owned worker fixture"
launch = ["/bin/sh", "-c", "printf 'fixture-ready\\n> '; IFS= read -r line; exec sleep 30"]
"#,
    )
    .unwrap();
    let run = d.ok(&["orchestration", "run-create", "--title", "R", "--json"]);
    let run: serde_json::Value = serde_json::from_str(&run).unwrap();
    let run = run["id"].as_str().unwrap();
    for (worker, release) in [("w-gone-stop", false), ("w-gone-release", true)] {
        let started = d.ok(&[
            "orchestration",
            "worker-start",
            "--run",
            run,
            "--worker",
            worker,
            "--agent",
            "owned_fixture",
            "--cwd",
            d.dir.to_str().unwrap(),
            "--prompt",
            "do it",
            "--json",
        ]);
        let started: serde_json::Value = serde_json::from_str(&started).unwrap();
        let sid = started["session_id"].as_u64().unwrap().to_string();
        if release {
            d.ok(&[
                "orchestration",
                "send",
                "--from",
                worker,
                "--type",
                "worker_done",
                "--run",
                run,
                "--dispatch",
                started["id"].as_str().unwrap(),
                "--outcome",
                "succeeded",
                "--summary",
                "finished",
            ]);
        }
        d.ok(&["kill", &sid]);
        let sub = if release {
            "worker-release"
        } else {
            "worker-stop"
        };
        d.ok(&["orchestration", sub, worker]);
        let show = d.ok(&["orchestration", "worker-show", worker]);
        assert!(
            show.contains(if release { "released" } else { "stopped" }),
            "{show}"
        );
    }
}

#[test]
fn orchestration_borrowed_session_survives_stop_rejection_and_abandon() {
    let d = Daemon::new("orch-borrowed");
    let run: serde_json::Value = serde_json::from_str(&d.ok(&[
        "orchestration",
        "run-create",
        "--title",
        "borrowed",
        "--json",
    ]))
    .unwrap();
    let sid = d.ok(&["new", "--", "/bin/sleep", "30"]);
    let sid = sid.trim();
    d.ok(&[
        "orchestration",
        "worker-start",
        "--run",
        run["id"].as_str().unwrap(),
        "--worker",
        "borrowed",
        "--session",
        sid,
        "--no-preamble",
        "--prompt",
        "inspect",
    ]);
    let rejected = d.run(&["orchestration", "worker-stop", "borrowed"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("borrowed"));
    assert!(ls_has_session(&d.ok(&["ls"]), sid));
    d.ok(&["orchestration", "worker-abandon", "borrowed"]);
    assert!(ls_has_session(&d.ok(&["ls"]), sid));
    d.ok(&["kill", sid]);
}

/// agy-shaped PTY work uses Krill's guide, absolute CLI, Unicode questions, and settlement.
/// This exercises the real holder/daemon protocol with a fixture, not a live model.
#[test]
fn orchestration_agy_full_and_minimal_workers_read_guide_ask_and_report() {
    use std::os::unix::fs::PermissionsExt as _;
    let d = Daemon::configured_schedules_home("agy-skills", None, None, true);
    let scripts = d.dir.join("agy fixture with spaces");
    fs::create_dir_all(&scripts).unwrap();
    let executable = scripts.join("agy");
    fs::write(&executable, include_str!("fixtures/krill-agy-worker.py")).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        d.dir.join("agents.toml"),
        format!(
            "[agent.agy]\nname = \"AGY protocol fixture\"\nlaunch = [{}]\nyolo_args = []\n",
            serde_json::to_string(&executable.display().to_string()).unwrap(),
        ),
    )
    .unwrap();
    let run: serde_json::Value = serde_json::from_str(&d.ok(&[
        "orchestration",
        "run-create",
        "--title",
        "AGY workflow",
        "--json",
    ]))
    .unwrap();
    let run = run["id"].as_str().unwrap();
    for mode in ["full", "minimal"] {
        let task: serde_json::Value = serde_json::from_str(&d.ok(&[
            "orchestration",
            "task-create",
            "--run",
            run,
            "--title",
            mode,
            "--json",
        ]))
        .unwrap();
        let task_id = task["id"].as_str().unwrap();
        let dispatch: serde_json::Value = serde_json::from_str(&d.ok(&[
            "orchestration",
            "worker-start",
            "--run",
            run,
            "--task",
            task_id,
            "--worker",
            &format!("agy-{mode}"),
            "--agent",
            "agy",
            "--preamble",
            mode,
            "--cwd",
            scripts.to_str().unwrap(),
            "--prompt",
            "한글 작업\n검증 근거를 남기세요",
            "--json",
        ]))
        .unwrap();
        let deadline = Instant::now() + WAIT;
        let question = loop {
            let mail: serde_json::Value = serde_json::from_str(&d.ok(&[
                "orchestration",
                "inbox",
                "--to",
                "coordinator",
                "--run",
                run,
                "--json",
            ]))
            .unwrap();
            if let Some(question) = mail
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["msg_type"] == "question" && m["from"] == format!("agy-{mode}"))
            {
                break question["id"].as_str().unwrap().to_owned();
            }
            assert!(Instant::now() < deadline, "agy fixture did not ask: {mail}");
            thread::sleep(Duration::from_millis(20));
        };
        d.ok(&[
            "orchestration",
            "reply",
            &question,
            "--body",
            "검증 후 진행\n범위 유지",
            "--json",
        ]);
        let sid = dispatch["session_id"].as_u64().unwrap().to_string();
        d.wait_text(&sid, "agy-fixture-completion-accepted");
        let task_state: serde_json::Value =
            serde_json::from_str(&d.ok(&["orchestration", "task-list", "--run", run, "--json"]))
                .unwrap();
        assert!(
            task_state
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == task_id && t["status"] == "completed")
        );
        let dispatch_id = dispatch["id"].as_str().unwrap();
        let report: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(scripts.join(format!("agy-evidence-{dispatch_id}.json"))).unwrap(),
        )
        .unwrap();
        assert!(report["prompt"].as_str().unwrap().contains("한글 작업"));
        for root in krill_proto::orchestration::skill_directories(&d.dir.join("empty-home"), "agy")
        {
            assert!(
                !root.exists(),
                "isolated daemon wrote host skills: {root:?}"
            );
        }
        d.ok(&["orchestration", "worker-release", dispatch_id, "--json"]);
        assert!(!ls_has_session(&d.ok(&["ls"]), &sid));
    }
}

#[test]
fn mobile_cli_pair_list_revoke_and_tunnel_contract() {
    use std::os::unix::fs::PermissionsExt;
    let d = Daemon::new("mobile");
    assert_eq!(d.ok(&["mobile", "devices", "--json"]).trim(), "[]");
    assert!(d.ok(&["mobile", "--help"]).contains("krill mobile"));
    assert!(
        Command::new(env!("CARGO_BIN_EXE_krill"))
            .args(["mobile", "--help"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(d.run(&["mobile", "revoke"]).status.code(), Some(2));
    let offer: serde_json::Value =
        serde_json::from_str(&d.ok(&["mobile", "pair", "--address", "127.0.0.1:0", "--json"]))
            .unwrap();
    let url = offer
        .get("PairingOffer")
        .unwrap()
        .get("url")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(url.starts_with("http://127.0.0.1:"));
    assert!(url.contains("/#pair=") && url.contains("&key="));
    assert!(!url.split('#').next().unwrap().contains("key="));
    d.ok(&["mobile", "revoke", "--all", "--json"]);
    let script = d.dir.join("cloudflared-test");
    fs::write(
        &script,
        "#!/bin/sh\n: > \"${0%/*}/cloudflared-started\"\nprintf 'https://cli-mobile.trycloudflare.com\\n' >&2\nexec /bin/sleep 120\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    // 릴레이가 없으면 오류를 반환하고, 설치된 퀵터널로 폴백하지 않는다.
    let out = d.run(&["mobile", "pair", "--tunnel", "--json"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("모바일 릴레이에 연결하지 못했습니다"));
    assert!(out.stdout.is_empty());
    let status: krill_proto::mobile::MobileReply =
        serde_json::from_str(&d.ok(&["mobile", "serve"])).unwrap();
    assert!(matches!(
        status,
        krill_proto::mobile::MobileReply::GatewayStatus {
            mode: krill_proto::mobile::ConnectMode::Tunnel,
            tunnel_url: None,
            ..
        }
    ));
    assert!(!d.dir.join("cloudflared-started").exists());
    d.ok(&["mobile", "revoke", "--all"]);
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "macOS 전용: krill computer(접근성 API)"
)]
fn computer_cli_subcommands_and_skill_work_offline() {
    let dir = std::env::temp_dir().join(format!("m13-computer-contract-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("krill-bin");
    fs::copy(MK, &bin).unwrap();
    let run = |args: &[&str]| {
        Command::new(&bin)
            .args(args)
            .env("KRILL_STATE_DIR", &dir)
            .env("KRILL_TEST_OFFLINE", "1")
            .output()
            .unwrap()
    };
    let ok = |args: &[&str]| {
        let out = run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let tree = serde_json::json!({"role":"AXWindow","title":"Fixture","bounds":{"x":0,"y":0,"width":400,"height":300},
    "children":[
        {"role":"AXTextField","title":"Input","value":"initial","focused":true,
         "bounds":{"x":10,"y":20,"width":200,"height":30},"actions":["AXConfirm"]},
        {"role":"AXButton","title":"Save","bounds":{"x":220,"y":20,"width":60,"height":30},"actions":["AXPress"]}
    ]});
    let initial = serde_json::json!({"clipboard":"private clipboard secret","clipboardRestoreCount":0,
        "apps":[{"name":"Fixture","pid":7777,"isActive":true,"windows":[{
            "id":"101","title":"Fixture","bounds":{"x":0,"y":0,"width":400,"height":300},"root":tree}]}]});
    let path = dir.join("computer-mock.json");
    let save =
        |value: &serde_json::Value| fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    save(&initial);
    assert!(
        !run(&[
            "computer",
            "click",
            "--app",
            "Fixture",
            "--element",
            "1",
            "--json"
        ])
        .status
        .success()
    );
    let skill = run(&["skills", "get", "computer-use"]);
    assert!(skill.status.success());
    assert!(String::from_utf8_lossy(&skill.stdout).contains("get-app-state"));
    let capabilities = ok(&["computer", "capabilities", "--json"]);
    assert_eq!(capabilities["contract"], "computer/2");
    assert_eq!(capabilities["batchSupported"], false);
    assert!(
        capabilities["guideHash"]
            .as_str()
            .unwrap()
            .starts_with("fnv1a64:")
    );
    let permissions = ok(&["computer", "permissions", "--json"]);
    assert!(permissions["supported"].as_bool().unwrap());
    let apps = ok(&["computer", "list-apps", "--json"]);
    assert!(
        apps["apps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|app| app["pid"] == 7777)
    );
    let windows = ok(&["computer", "list-windows", "--app", "Fixture", "--json"]);
    assert_eq!(windows["windows"][0]["id"], "101");
    let snapshot = ok(&[
        "computer",
        "get-app-state",
        "--app",
        "Fixture",
        "--no-screenshot",
        "--json",
    ]);
    let id = snapshot["snapshotId"].as_str().unwrap();
    let snapshot_again = ok(&[
        "computer",
        "get-app-state",
        "--app",
        "Fixture",
        "--no-screenshot",
        "--json",
    ]);
    assert_ne!(snapshot["snapshotId"], snapshot_again["snapshotId"]);
    let long = format!("{}tail", "가".repeat(900));
    let set = ok(&[
        "computer",
        "set-value",
        "--snapshot",
        id,
        "--element",
        "1",
        "--value",
        &long,
        "--no-screenshot",
        "--json",
    ]);
    assert_eq!(set["verification"]["status"], "verified");
    assert_eq!(set["delivery"], "submitted");
    assert_eq!(set["observation"]["status"], "observed");
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        stored["apps"][0]["windows"][0]["root"]["children"][0]["value"],
        long
    );
    let paste = ok(&[
        "computer",
        "paste-text",
        "--element",
        "1",
        "--text",
        " pasted",
        "--no-screenshot",
        "--json",
    ]);
    assert_eq!(paste["clipboard"]["preserved"], true);
    assert_eq!(paste["clipboard"]["restored"], true);
    assert_eq!(paste["cleanup"], "completed");
    assert!(paste["clipboard"].get("previousText").is_none());
    assert!(
        !serde_json::to_string(&paste)
            .unwrap()
            .contains("private clipboard secret")
    );
    let click = ok(&[
        "computer",
        "click",
        "--element",
        "2",
        "--no-screenshot",
        "--json",
    ]);
    assert_eq!(click["verification"]["status"], "unverified");
    assert_eq!(click["verification"]["retried"], false);
    assert_eq!(click["delivery"], "submitted");
    assert!(
        !run(&["computer", "click", "--x", "999", "--y", "999", "--json"])
            .status
            .success()
    );

    // Keep the legacy public actions and aliases executable through the real CLI.
    // Each command gets fresh immutable evidence, so this is not stale-index reuse.
    for action in [
        vec![
            "perform-secondary-action",
            "--element-index",
            "1",
            "--action",
            "AXConfirm",
        ],
        vec!["type-text", "--element-index", "1", "--text", "typed"],
        vec!["press-key", "--key", "return", "--repeat", "2"],
        vec!["hotkey", "--key", "cmd+shift+p"],
        vec![
            "scroll",
            "--element-index",
            "2",
            "--direction",
            "down",
            "--pages",
            "1",
        ],
        vec![
            "drag",
            "--from-element-index",
            "1",
            "--to-element-index",
            "2",
        ],
        vec!["click", "--x", "250", "--y", "35", "--mouse-button", "left"],
    ] {
        save(&initial);
        ok(&[
            "computer",
            "get-app-state",
            "--app",
            "Fixture",
            "--format",
            "full",
            "--json",
        ]);
        let mut arguments = vec!["computer"];
        arguments.extend(action);
        arguments.extend(["--no-screenshot", "--json"]);
        let result = ok(&arguments);
        assert_eq!(result["delivery"], "submitted", "{arguments:?}: {result}");
        assert_eq!(result["verification"]["status"], "unverified");
        assert_eq!(result["verification"]["retried"], false);
    }

    // Each rejected stale/ambiguous ref must leave the effect journal untouched.
    for adversary in ["reorder", "ambiguous", "window_replaced", "disabled"] {
        save(&initial);
        let before = ok(&[
            "computer",
            "get-app-state",
            "--app",
            "Fixture",
            "--no-screenshot",
            "--json",
        ]);
        let token = before["snapshotId"].as_str().unwrap();
        let mut changed = initial.clone();
        let children = changed["apps"][0]["windows"][0]["root"]["children"]
            .as_array_mut()
            .unwrap();
        match adversary {
            "reorder" => children.swap(0, 1),
            "ambiguous" => children.push(children[0].clone()),
            "disabled" => children[0]["enabled"] = serde_json::json!(false),
            _ => changed["apps"][0]["windows"][0]["id"] = serde_json::json!("102"),
        }
        save(&changed);
        let out = run(&[
            "computer",
            "click",
            "--snapshot",
            token,
            "--element",
            "1",
            "--no-screenshot",
            "--json",
        ]);
        assert!(
            !out.status.success(),
            "{adversary}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let after: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(after["lastAction"].is_null(), "{adversary} mutated target");
    }
    let mut host = initial.clone();
    host["apps"][0]["name"] = serde_json::json!("krill-app");
    host["apps"][0]["isHidden"] = serde_json::json!(true);
    save(&host);
    assert!(
        !run(&[
            "computer",
            "get-app-state",
            "--app",
            "krill-app",
            "--restore-window",
            "--json"
        ])
        .status
        .success()
    );
    let host_after: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(host_after["apps"][0]["isHidden"], true);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn search_literal_help_queries_an_isolated_daemon() {
    let d = Daemon::configured_schedules_home("search-literal-help", None, None, true);
    let out = d.ok(&["search", "help", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(report["hits"], serde_json::json!([]));
}
