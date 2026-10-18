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
        let dir = std::env::temp_dir().join(format!("mk045-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
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
            .env("KRILL_STATE_DIR", &dir)
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

/// 내장 `minikrill` 스킬 문서가 안내하는 세션 명령을 그대로 실제 krilld에 돌린다.
#[test]
fn minikrill_skill_session_commands_match_cli() {
    let skill = krill_proto::orchestration::BUILTIN_SKILLS
        .iter()
        .find(|s| s.name == "minikrill")
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
    let run = d.ok(&["orchestration", "run-create", "--title", "R", "--json"]);
    let run: serde_json::Value = serde_json::from_str(&run).unwrap();
    let run = run["id"].as_str().unwrap();
    for (worker, release) in [("w-gone-stop", false), ("w-gone-release", true)] {
        let sid = d.ok(&["new", "--", "/bin/sleep", "30"]);
        let sid = sid.trim().to_owned();
        d.ok(&[
            "orchestration",
            "worker-start",
            "--run",
            run,
            "--worker",
            worker,
            "--session",
            &sid,
            "--prompt",
            "do it",
        ]);
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
    assert!(url.contains("#key="));
    assert!(!url.split('#').next().unwrap().contains("key="));
    d.ok(&["mobile", "revoke", "--all", "--json"]);
    let script = d.dir.join("cloudflared-test");
    fs::write(
        &script,
        "#!/bin/sh\nprintf 'https://cli-mobile.trycloudflare.com\\n' >&2\nexec /bin/sleep 30\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let offer: serde_json::Value =
        serde_json::from_str(&d.ok(&["mobile", "pair", "--tunnel", "--json"])).unwrap();
    let url = offer
        .get("PairingOffer")
        .unwrap()
        .get("url")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(url.starts_with("https://cli-mobile.trycloudflare.com/?pair="));
    assert!(url.contains("#key="));
    let status = d.ok(&["mobile", "serve"]);
    assert!(status.contains("cli-mobile.trycloudflare.com"));
    d.ok(&["mobile", "revoke", "--all"]);
}

#[test]
#[cfg(target_os = "macos")]
fn computer_cli_subcommands_and_skill_work_offline() {
    let dir = std::env::temp_dir().join(format!("mk187-computer-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();

    // 공유 CARGO_TARGET_DIR 환경에서 다른 워커가 debug/krill을 덮어써도
    // 현재 워크트리의 deps/krill-* 바이너리를 고정 복사해 사용한다.
    let bin_path = {
        let candidate_supports_computer = |p: &Path| -> bool {
            Command::new(p)
                .args(["computer", "capabilities", "--json"])
                .env("KRILL_STATE_DIR", &dir)
                .env("KRILL_TEST_OFFLINE", "1")
                .output()
                .is_ok_and(|out| {
                    out.status.success()
                        && String::from_utf8_lossy(&out.stdout)
                            .contains("\"performSecondaryAction\"")
                })
        };
        let mk_path = PathBuf::from(MK);
        let chosen = if candidate_supports_computer(&mk_path) {
            mk_path
        } else {
            let deps = mk_path.parent().unwrap().join("deps");
            let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&deps)
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("krill-") && !n.contains('.'))
                })
                .filter_map(|p| {
                    let mtime = p.metadata().ok()?.modified().ok()?;
                    Some((mtime, p))
                })
                .collect();
            candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
            candidates
                .into_iter()
                .map(|(_, p)| p)
                .find(|p| candidate_supports_computer(p))
                .unwrap_or(mk_path)
        };
        let pinned = dir.join("krill-bin");
        fs::copy(&chosen, &pinned).unwrap();
        pinned
    };

    let run_cli = |args: &[&str]| -> Output {
        Command::new(&bin_path)
            .args(args)
            .env("KRILL_STATE_DIR", &dir)
            .env("HOME", &dir)
            .env("KRILL_TEST_OFFLINE", "1")
            .output()
            .unwrap()
    };
    let ok_cli = |args: &[&str]| -> String {
        let out = run_cli(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };

    // 1. computer-use built-in skill list/get/install
    let skills_json: serde_json::Value =
        serde_json::from_str(&ok_cli(&["skills", "list", "--json"])).unwrap();
    assert!(
        skills_json
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "computer-use")
    );
    let skill_md = ok_cli(&["skills", "get", "computer-use"]);
    assert!(skill_md.contains("krill computer get-app-state"));
    assert!(skill_md.contains("krill computer paste-text"));

    // 2. Write mock environment for deterministic offline testing of all 14 subcommands
    fs::write(
        dir.join("computer-mock.json"),
        r#"{
  "accessibilityGranted": true,
  "screenshotsGranted": true,
  "clipboard": "original-clipboard-value",
  "clipboardRestoreCount": 0,
  "apps": [
    {
      "name": "Calculator",
      "bundleId": "com.apple.calculator",
      "pid": 4242,
      "isActive": true,
      "isHidden": false,
      "windows": [
        {
          "id": "101",
          "title": "Calculator",
          "bounds": { "x": 100.0, "y": 100.0, "width": 400.0, "height": 300.0 },
          "isFocused": true,
          "isMain": true,
          "isMinimized": false,
          "root": {
            "role": "AXWindow",
            "title": "Calculator",
            "bounds": { "x": 100.0, "y": 100.0, "width": 400.0, "height": 300.0 },
            "enabled": true,
            "focused": false,
            "selected": false,
            "actions": [],
            "children": [
              {
                "role": "AXTextField",
                "title": "Input",
                "value": "0",
                "bounds": { "x": 120.0, "y": 130.0, "width": 200.0, "height": 30.0 },
                "enabled": true,
                "focused": true,
                "selected": false,
                "actions": ["AXConfirm"],
                "children": []
              },
              {
                "role": "AXButton",
                "title": "Equals",
                "bounds": { "x": 340.0, "y": 130.0, "width": 50.0, "height": 30.0 },
                "enabled": true,
                "focused": false,
                "selected": false,
                "actions": ["AXPress", "AXShowMenu"],
                "children": []
              }
            ]
          }
        }
      ]
    }
  ]
}"#,
    )
    .unwrap();

    let caps: serde_json::Value =
        serde_json::from_str(&ok_cli(&["computer", "capabilities", "--json"])).unwrap();
    assert_eq!(caps["supported"], true);
    assert_eq!(caps["actions"].as_array().unwrap().len(), 12);

    let perms: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "permissions",
        "--id",
        "accessibility",
        "--json",
    ]))
    .unwrap();
    assert_eq!(perms["allGranted"], true);
    assert_eq!(perms["permissions"][0]["id"], "accessibility");

    let apps: serde_json::Value =
        serde_json::from_str(&ok_cli(&["computer", "list-apps", "--json"])).unwrap();
    assert_eq!(apps["apps"][0]["name"], "Calculator");

    let wins: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "list-windows",
        "--app",
        "Calculator",
        "--json",
    ]))
    .unwrap();
    assert_eq!(wins["windows"][0]["id"], "101");

    let state: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "get-app-state",
        "--app",
        "Calculator",
        "--json",
    ]))
    .unwrap();
    assert_eq!(state["interactiveCount"], 2);
    assert_eq!(state["focusedElementId"], 1);
    assert!(
        state["treeText"]
            .as_str()
            .unwrap()
            .contains("[#1] AXTextField")
    );

    let set_val: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "set-value",
        "--element",
        "1",
        "--value",
        "123",
        "--json",
    ]))
    .unwrap();
    assert_eq!(set_val["verification"]["status"], "verified");
    assert_eq!(set_val["appState"]["elements"][0]["value"], "123");

    let typed: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "type-text",
        "--element",
        "1",
        "--text",
        "+45",
        "--json",
    ]))
    .unwrap();
    assert_eq!(typed["appState"]["elements"][0]["value"], "123+45");

    let pasted: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "paste-text",
        "--element",
        "1",
        "--text",
        "+6",
        "--json",
    ]))
    .unwrap();
    assert_eq!(pasted["clipboard"]["preserved"], true);
    assert_eq!(pasted["clipboard"]["restored"], true);
    assert_eq!(
        pasted["clipboard"]["previousText"],
        "original-clipboard-value"
    );
    assert_eq!(pasted["appState"]["elements"][0]["value"], "123+45+6");

    let clicked: serde_json::Value =
        serde_json::from_str(&ok_cli(&["computer", "click", "--element", "2", "--json"])).unwrap();
    assert_eq!(clicked["verification"]["status"], "verified");
    assert_eq!(clicked["appState"]["focusedElementId"], 2);

    let secondary: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "perform-secondary-action",
        "--element",
        "2",
        "--action",
        "AXShowMenu",
        "--json",
    ]))
    .unwrap();
    assert_eq!(secondary["action"], "performSecondaryAction");

    let scrolled: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "scroll",
        "--element",
        "1",
        "--delta-y",
        "-120",
        "--json",
    ]))
    .unwrap();
    assert_eq!(scrolled["action"], "scroll");

    let dragged: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "drag",
        "--from-element",
        "1",
        "--to-element",
        "2",
        "--json",
    ]))
    .unwrap();
    assert_eq!(dragged["action"], "drag");

    let key_res: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "press-key",
        "--key",
        "Return",
        "--modifiers",
        "shift",
        "--json",
    ]))
    .unwrap();
    assert_eq!(key_res["action"], "pressKey");

    let hotkey_res: serde_json::Value = serde_json::from_str(&ok_cli(&[
        "computer",
        "hotkey",
        "--keys",
        "Command+Shift+P",
        "--json",
    ]))
    .unwrap();
    assert_eq!(hotkey_res["action"], "hotkey");

    // 3. Live mode verification (without computer-mock.json) using fake osascript binary (OWE-746)
    fs::remove_file(dir.join("computer-mock.json")).unwrap();
    fs::remove_file(dir.join("computer-state.json")).ok();

    let fake_osascript = dir.join("fake-osascript.sh");
    let action_log = dir.join("jxa-actions.jsonl");
    let value_file = dir.join("live-value.txt");
    fs::write(&value_file, "initial-doc2").unwrap();
    fs::write(
        &fake_osascript,
        format!(
            r#"#!/bin/sh
SCRIPT="$4"
shift 4
case "$SCRIPT" in
  *"runningApplications"*)
    printf '[{{"name":"TextEdit","bundleId":"com.apple.TextEdit","pid":7777,"isActive":true,"isHidden":false}}]\n'
    ;;
  *"CGWindowListCopyWindowInfo"*)
    printf '[{{"id":"501","index":1,"title":"Doc 1","appName":"TextEdit","bundleId":"com.apple.TextEdit","pid":7777,"bounds":{{"x":10.0,"y":10.0,"width":500.0,"height":400.0}},"isFocused":true,"isMain":true,"isMinimized":false}},{{"id":"502","index":2,"title":"Doc 2","appName":"TextEdit","bundleId":"com.apple.TextEdit","pid":7777,"bounds":{{"x":600.0,"y":10.0,"width":500.0,"height":400.0}},"isFocused":false,"isMain":false,"isMinimized":false}}]\n'
    ;;
  *"selectTargetWindow"* )
    case "$SCRIPT" in
      *"executeRequest"*)
        REQ="$1"
        printf '%s\n' "$REQ" >> "{log}"
        case "$REQ" in
          *'"kind":"setValue"'*)
            printf 'abc' > "{val}"
            ;;
        esac
        if [ -n "$FAKE_JXA_FAIL" ]; then
          printf '{{"ok":false,"status":"unverified","reason":"simulated live failure","note":null}}\n'
        else
          printf '{{"ok":true,"status":"verified","reason":null,"note":"simulated live dispatch"}}\n'
        fi
        ;;
      *)
        WIN_ID="$4"
        WIN_IDX="$5"
        WIN_TITLE="$6"
        CUR_VAL="$(cat "{val}")"
        if [ "$WIN_IDX" = "2" ] || [ "$WIN_ID" = "502" ]; then
          printf '{{"role":"AXWindow","title":"%s","bounds":{{"x":600.0,"y":10.0,"width":500.0,"height":400.0}},"enabled":true,"focused":true,"selected":false,"actions":[],"children":[{{"role":"AXTextArea","title":"SecondWindowEditor","value":"%s","bounds":{{"x":620.0,"y":40.0,"width":400.0,"height":300.0}},"enabled":true,"focused":true,"selected":false,"actions":["AXPress"],"children":[]}}]}}\n' "$WIN_TITLE" "$CUR_VAL"
        else
          printf '{{"role":"AXWindow","title":"Doc 1","bounds":{{"x":10.0,"y":10.0,"width":500.0,"height":400.0}},"enabled":true,"focused":true,"selected":false,"actions":[],"children":[{{"role":"AXTextArea","title":"FirstWindowEditor","value":"%s","bounds":{{"x":20.0,"y":40.0,"width":400.0,"height":300.0}},"enabled":true,"focused":true,"selected":false,"actions":["AXPress"],"children":[]}}]}}\n' "$CUR_VAL"
        fi
        ;;
    esac
    ;;
esac
"#,
            log = action_log.display(),
            val = value_file.display(),
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake_osascript, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let ok_live = |args: &[&str]| -> serde_json::Value {
        let out = Command::new(&bin_path)
            .args(args)
            .env("KRILL_STATE_DIR", &dir)
            .env("HOME", &dir)
            .env("KRILL_TEST_OFFLINE", "1")
            .env("KRILL_COMPUTER_OSASCRIPT_BIN", &fake_osascript)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_str(&String::from_utf8(out.stdout).unwrap()).unwrap()
    };

    // Scenario 2: snapshot --window-index 2 queries Window 2's AX tree instead of wins[0]
    let snap2 = ok_live(&[
        "computer",
        "snapshot",
        "--app",
        "TextEdit",
        "--window-index",
        "2",
        "--json",
    ]);
    assert_eq!(snap2["window"]["id"], "502");
    assert_eq!(snap2["window"]["index"], 2);
    assert_eq!(snap2["elements"][0]["label"], "SecondWindowEditor");

    // Scenario 1 & 3: live actions with --ref @e1, --dy, and --app pid:<PID>
    let live_click = ok_live(&[
        "computer", "click", "--app", "TextEdit", "--ref", "@e1", "--json",
    ]);
    assert_eq!(live_click["ok"], true);
    assert_eq!(live_click["verification"]["status"], "verified");

    let live_pid_click = ok_live(&[
        "computer", "click", "--app", "pid:7777", "--x", "100", "--y", "100", "--json",
    ]);
    assert_eq!(live_pid_click["ok"], true);
    assert_eq!(live_pid_click["appState"]["app"]["name"], "TextEdit");

    let live_type = ok_live(&[
        "computer",
        "type-text",
        "--app",
        "TextEdit",
        "--text",
        "hello",
        "--json",
    ]);
    assert_eq!(live_type["ok"], true);

    let live_key = ok_live(&[
        "computer",
        "press-key",
        "--app",
        "TextEdit",
        "--key",
        "Return",
        "--json",
    ]);
    assert_eq!(live_key["ok"], true);

    let live_hotkey = ok_live(&[
        "computer", "hotkey", "--app", "TextEdit", "--keys", "cmd+a", "--json",
    ]);
    assert_eq!(live_hotkey["ok"], true);

    let live_scroll = ok_live(&[
        "computer", "scroll", "--app", "TextEdit", "--dy", "-120", "--json",
    ]);
    assert_eq!(live_scroll["ok"], true);

    let live_drag = ok_live(&[
        "computer", "drag", "--app", "TextEdit", "--from-x", "10", "--from-y", "10", "--to-x",
        "50", "--to-y", "50", "--json",
    ]);
    assert_eq!(live_drag["ok"], true);

    let live_set = ok_live(&[
        "computer",
        "set-value",
        "--app",
        "TextEdit",
        "--ref",
        "@e1",
        "--value",
        "abc",
        "--json",
    ]);
    assert_eq!(live_set["ok"], true);
    assert_eq!(live_set["verification"]["status"], "verified");

    // Verify logged JXA payloads resolved pid:7777 -> appName "TextEdit" & pid 7777
    let logged = fs::read_to_string(&action_log).unwrap();
    let payloads: Vec<serde_json::Value> = logged
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(payloads.len(), 8);
    assert!(
        payloads
            .iter()
            .all(|p| p["appName"] == "TextEdit" && p["pid"] == 7777)
    );
    assert_eq!(payloads[0]["kind"], "click");
    assert_eq!(payloads[0]["targetRef"], 1);
    assert_eq!(payloads[1]["kind"], "click");
    assert_eq!(payloads[2]["kind"], "typeText");
    assert_eq!(payloads[3]["kind"], "pressKey");
    assert_eq!(payloads[4]["kind"], "hotkey");
    assert_eq!(payloads[5]["kind"], "scroll");
    assert_eq!(payloads[6]["kind"], "drag");
    assert_eq!(payloads[7]["kind"], "setValue");

    // Verify failed/unverified live JXA outcome is reported honestly as ok: false, status: "unverified"
    let fail_out = Command::new(&bin_path)
        .args([
            "computer", "click", "--app", "TextEdit", "--ref", "@e1", "--json",
        ])
        .env("KRILL_STATE_DIR", &dir)
        .env("HOME", &dir)
        .env("KRILL_TEST_OFFLINE", "1")
        .env("KRILL_COMPUTER_OSASCRIPT_BIN", &fake_osascript)
        .env("FAKE_JXA_FAIL", "1")
        .output()
        .unwrap();
    assert!(fail_out.status.success());
    let fail_json: serde_json::Value = serde_json::from_slice(&fail_out.stdout).unwrap();
    assert_eq!(fail_json["ok"], false);
    assert_eq!(fail_json["verification"]["status"], "unverified");

    // Scenario 4: oversized --text-stdin stream (> 65536 bytes) is rejected
    let mut child = Command::new(&bin_path)
        .args([
            "computer",
            "type-text",
            "--app",
            "TextEdit",
            "--text-stdin",
            "--json",
        ])
        .env("KRILL_STATE_DIR", &dir)
        .env("HOME", &dir)
        .env("KRILL_TEST_OFFLINE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&vec![b'a'; 70_000]).ok();
    }
    let big_out = child.wait_with_output().unwrap();
    assert_eq!(big_out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&big_out.stderr).contains("exceeds maximum length"),
        "{}",
        String::from_utf8_lossy(&big_out.stderr)
    );

    // Scenario 5 (OWE-775): real /usr/bin/osascript JXA execution for list-apps and list-windows --app pid:<N>
    if cfg!(target_os = "macos") && Path::new("/usr/bin/osascript").exists() {
        let apps_out = Command::new(&bin_path)
            .args(["computer", "list-apps", "--json"])
            .env("KRILL_STATE_DIR", &dir)
            .env("HOME", &dir)
            .env("KRILL_TEST_OFFLINE", "1")
            .env_remove("KRILL_COMPUTER_OSASCRIPT_BIN")
            .output()
            .unwrap();
        assert!(
            apps_out.status.success(),
            "real list-apps failed: {}",
            String::from_utf8_lossy(&apps_out.stderr)
        );
        let apps_json: serde_json::Value = serde_json::from_slice(&apps_out.stdout).unwrap();
        let apps_arr = apps_json["apps"].as_array().unwrap();
        if let Some(first_app) = apps_arr.first() {
            let pid = first_app["pid"].as_u64().unwrap();
            let pid_sel = format!("pid:{pid}");
            let wins_out = Command::new(&bin_path)
                .args(["computer", "list-windows", "--app", &pid_sel, "--json"])
                .env("KRILL_STATE_DIR", &dir)
                .env("HOME", &dir)
                .env("KRILL_TEST_OFFLINE", "1")
                .env_remove("KRILL_COMPUTER_OSASCRIPT_BIN")
                .output()
                .unwrap();
            assert!(
                wins_out.status.success(),
                "real list-windows --app {pid_sel} failed: {}",
                String::from_utf8_lossy(&wins_out.stderr)
            );
            let wins_json: serde_json::Value = serde_json::from_slice(&wins_out.stdout).unwrap();
            assert!(wins_json["windows"].is_array());
        }
    }

    fs::remove_dir_all(&dir).ok();
}
