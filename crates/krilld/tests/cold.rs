//! MK-032: 실제 `krill-holder` + `krilld`로 콜드 복원. 세션 출력 → holder·krilld 동시 `kill -9`(재부팅과 같은 프로세스
//! 소실) → 같은 상태 디렉터리로 새 데몬 → 목록에는 없지만 `Request::Cold`로 마지막 화면과 스크롤백을 받는다.
//!
//! `krill-holder`는 `krilld` 옆(같은 target 디렉터리)에 있어야 한다: `cargo nextest run --workspace`.

#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use krill_proto::SessionId;
use krill_proto::control::{Cold, Create, Failure, Program, Reply, Request};
use krill_proto::daemon::client_hello;
use krill_proto::ipc::{self, ClientRole, ServerHello};
use krill_proto::screen::{Cell, Msg, Screen};
use rustix::process::{Pid, Signal, kill_process};

const KRILLD: &str = env!("CARGO_BIN_EXE_krilld");
const WAIT: Duration = Duration::from_secs(10);
/// 화면(24행)보다 많이 찍어 스크롤백이 생기게 한다.
const LINES: usize = 60;

fn pid(p: u32) -> Pid {
    Pid::from_raw(i32::try_from(p).unwrap()).unwrap()
}

/// holder와 그 krilld. 지우면 둘 다 `kill -9`(재부팅처럼 프로세스가 사라진다).
struct Daemon {
    holder: Child,
    krilld: u32,
    sock: PathBuf,
}

impl Daemon {
    fn start(dir: &Path) -> (Self, UnixStream) {
        let holder_exe = Path::new(KRILLD).parent().unwrap().join("krill-holder");
        assert!(
            holder_exe.is_file(),
            "krill-holder is not built next to krilld (run the whole workspace)"
        );
        let sock = dir.join("run/k.sock");
        let holder = Command::new(&holder_exe)
            .arg(KRILLD)
            .arg(&sock)
            .env("KRILL_STATE_DIR", dir)
            .env("CLAUDE_CONFIG_DIR", dir.join("claude"))
            .env("CODEX_HOME", dir.join("codex"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        let (c, hello) = loop {
            if let Some(c) = connect(&sock) {
                break c;
            }
            assert!(Instant::now() < deadline, "krilld did not come up");
            thread::sleep(Duration::from_millis(20));
        };
        let krilld = u32::try_from(hello.instance_id).unwrap();
        (
            Self {
                holder,
                krilld,
                sock,
            },
            c,
        )
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // holder부터: krilld가 먼저 죽으면 holder가 다시 띄워 Drop이 모르는 pid가 남는다(OWE-604).
        self.holder.kill().ok();
        self.holder.wait().ok();
        kill_process(pid(self.krilld), Signal::KILL).ok();
        // 다음 데몬이 같은 경로에 게시할 수 있게 krilld가 정말 사라질 때까지.
        let deadline = Instant::now() + WAIT;
        while rustix::process::test_kill_process(pid(self.krilld)).is_ok()
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(20));
        }
        fs::remove_file(&self.sock).ok();
    }
}

/// 이 소켓 경로를 인자로 가진 holder 프로세스가 0개인지 확인한다(OWE-604).
/// `Daemon`을 명시 drop한 뒤 부른다(Drop 안에서는 단언하지 않는다).
fn assert_no_holders(sock: &Path) {
    let out = std::process::Command::new("/usr/bin/pgrep")
        .arg("-f")
        .arg(sock)
        .output()
        .unwrap();
    let mut holders = Vec::new();
    for line in String::from_utf8(out.stdout).unwrap().lines() {
        let pid: u32 = line.trim().parse().unwrap();
        let cmd = std::process::Command::new("/bin/ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let cmd = String::from_utf8(cmd.stdout).unwrap();
        if cmd.contains("krill-holder") {
            holders.push(format!("{pid}: {}", cmd.trim()));
        }
    }
    assert!(
        holders.is_empty(),
        "residual holder on {}: {holders:?}",
        sock.display()
    );
}

fn connect(sock: &Path) -> Option<(UnixStream, ServerHello)> {
    let mut s = UnixStream::connect(sock).ok()?;
    s.set_read_timeout(Some(WAIT)).unwrap();
    let (k, b) = ipc::read_frame(&mut s).ok()??;
    let hello = ServerHello::decode(k, &b).unwrap();
    std::io::Write::write_all(
        &mut s,
        &client_hello(ClientRole::App, "it").encode().unwrap(),
    )
    .unwrap();
    Some((s, hello))
}

fn request(c: &mut UnixStream, r: &Request) -> Reply {
    std::io::Write::write_all(c, &r.encode().unwrap()).unwrap();
    let (k, b) = ipc::read_frame(c).unwrap().unwrap();
    Reply::decode(k, &b).unwrap()
}

fn text(row: &[Cell]) -> String {
    row.iter()
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// 콜드 화면의 (화면 행, 스크롤백 행) 평문.
fn cold(c: &mut UnixStream, session: SessionId) -> Option<(Vec<String>, Vec<String>)> {
    let Reply::Cold(Cold { screen, history }) = request(c, &Request::Cold { session }) else {
        return None;
    };
    let mut s = Screen::default();
    s.apply(screen).unwrap();
    Some((
        s.rows.iter().map(|r| text(r)).collect(),
        history.iter().map(|r| text(r)).collect(),
    ))
}

#[test]
fn cold_view_after_holder_and_krilld_lost() {
    let dir = env::temp_dir().join(format!("mk032-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();

    let (daemon, mut c) = Daemon::start(&dir);
    let script =
        format!("for i in $(seq 1 {LINES}); do echo \"cold $i\"; done; echo done; sleep 60");
    let create = Request::Create(Create {
        account: None,
        program: Program::Custom {
            program: b"/bin/sh".to_vec(),
            args: vec![b"-c".to_vec(), script.into_bytes()],
        },
        cwd: Vec::new(),
        cols: 80,
        rows: 24,
    });
    let Reply::Created { session, .. } = request(&mut c, &create) else {
        panic!("create failed");
    };
    // 살아 있는 세션도 디스크 기록으로 답한다. 마지막 줄이 보이면 디스크에 다 들어간 것이다.
    let deadline = Instant::now() + WAIT;
    while !cold(&mut c, session).is_some_and(|(rows, _)| rows.iter().any(|r| r == "done")) {
        assert!(Instant::now() < deadline, "output never reached disk");
        thread::sleep(Duration::from_millis(50));
    }

    // 재부팅: holder와 krilld가 함께 사라진다(세션 프로세스도 master를 잃고 끝난다).
    drop((c, daemon));
    let (daemon, mut c) = Daemon::start(&dir);
    let Reply::Sessions(list) = request(&mut c, &Request::List) else {
        panic!("list failed");
    };
    assert!(list.is_empty(), "no process survives: {list:?}");

    let (rows, history) = cold(&mut c, session).expect("cold view");
    let want = |i: usize| format!("cold {i}");
    assert!(rows.contains(&want(LINES)), "last screen: {rows:?}");
    assert!(rows.contains(&"done".to_owned()));
    assert!(history.contains(&want(1)), "scrollback: {history:?}");
    // 스크롤백은 오래된 행부터, 화면 위에서 끊김 없이 이어진다.
    let top = rows.iter().position(|r| r.starts_with("cold ")).unwrap();
    let joined: Vec<_> = history.iter().chain(&rows[top..]).collect();
    let numbered: Vec<_> = joined
        .iter()
        .filter_map(|r| r.strip_prefix("cold ")?.parse::<usize>().ok())
        .collect();
    assert_eq!(numbered, (1..=LINES).collect::<Vec<_>>());

    // 기록이 없는 세션은 NoSession. 새 세션은 옛 id를 다시 쓰지 않는다(다시 시작해도 콜드 기록이 남는다).
    assert_eq!(
        request(
            &mut c,
            &Request::Cold {
                session: SessionId(9_999)
            }
        ),
        Reply::Failed(Failure::NoSession)
    );
    let relaunch = Request::Create(Create {
        account: None,
        program: Program::Custom {
            program: b"/bin/sh".to_vec(),
            args: vec![b"-c".to_vec(), b"sleep 60".to_vec()],
        },
        cwd: Vec::new(),
        cols: 80,
        rows: 24,
    });
    let Reply::Created { session: new, .. } = request(&mut c, &relaunch) else {
        panic!("relaunch failed");
    };
    assert_ne!(new, session);
    assert!(cold(&mut c, session).is_some());

    let sock = daemon.sock.clone();
    drop((c, daemon));
    assert_no_holders(&sock);
    fs::remove_dir_all(&dir).ok();
}

/// MK-052: 데몬 강제 종료·재기동 뒤 `Request::Resume`이 Claude 사전 발급 `--session-id`를 `--resume=<uuid>`로,
/// hook이 기록한 Codex `session_id`를 `codex resume <id>`로(종료 세션 Restart 경로 포함) 같은 작업 폴더에서 이어받는다.
#[test]
fn resume_claude_and_codex_after_daemon_crash_and_exit() {
    use std::os::unix::fs::PermissionsExt as _;

    use krill_proto::hook::{EventId, HookEvent, Input};

    let dir = env::temp_dir().join(format!("mk052-e2e-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let work = dir.join("work");
    let bin = dir.join("bin");
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(&bin).unwrap();

    let claude_bin = bin.join("claude");
    fs::write(
        &claude_bin,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '2.1.299 (Claude Code)'; exit 0; fi\necho \"CLAUDE_ARGS:$*\"\necho \"CLAUDE_PWD:$(pwd)\"\nsleep 60\n",
    )
    .unwrap();
    fs::set_permissions(&claude_bin, fs::Permissions::from_mode(0o700)).unwrap();

    let codex_bin = bin.join("codex");
    fs::write(
        &codex_bin,
        "#!/bin/sh\necho \"CODEX_ARGS:$*\"\necho \"CODEX_PWD:$(pwd)\"\nsleep 1\n",
    )
    .unwrap();
    fs::set_permissions(&codex_bin, fs::Permissions::from_mode(0o700)).unwrap();

    fs::write(
        dir.join("agents.toml"),
        format!(
            "permission = 'yolo'\n[agent.claude]\nname = 'Claude'\nlaunch = ['{}']\nyolo_args = ['--dangerously-skip-permissions']\n[agent.codex]\nname = 'Codex'\nlaunch = ['{}']\nyolo_args = ['--dangerously-bypass-approvals-and-sandbox']\n",
            claude_bin.display(),
            codex_bin.display(),
        ),
    )
    .unwrap();

    let (daemon, mut c) = Daemon::start(&dir);
    let claude_create = Create {
        account: None,
        program: Program::Agent {
            id: "claude".into(),
        },
        cwd: work.as_os_str().as_encoded_bytes().to_vec(),
        cols: 120,
        rows: 24,
    };
    let Reply::Created {
        session: claude_old,
        ..
    } = request(&mut c, &Request::Create(claude_create.clone()))
    else {
        panic!("claude create failed");
    };
    let wait_line = |c: &mut UnixStream, s: SessionId, needle: &str| -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some((rows, _)) = cold(c, s) {
                let joined = rows.join("\n");
                if joined.contains(needle) {
                    return joined;
                }
            }
            assert!(Instant::now() < deadline, "timed out waiting for {needle}");
            thread::sleep(Duration::from_millis(50));
        }
    };
    let first_out = wait_line(&mut c, claude_old, "--session-id ");
    let sid = first_out
        .split("--session-id ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(sid.len(), 36, "expected UUIDv4: {sid}");

    // Codex 세션 생성 → hook 이벤트로 session_id 기록 → 프로세스 종료(Restart 칩 경로).
    let codex_create = Create {
        account: None,
        program: Program::Agent { id: "codex".into() },
        cwd: work.as_os_str().as_encoded_bytes().to_vec(),
        cols: 120,
        rows: 24,
    };
    let Reply::Created {
        session: codex_old,
        pid: codex_pid,
    } = request(&mut c, &Request::Create(codex_create.clone()))
    else {
        panic!("codex create failed");
    };
    wait_line(&mut c, codex_old, "CODEX_ARGS:");
    let (mut hook_conn, _) = connect(&daemon.sock).unwrap();
    let ev = HookEvent {
        id: EventId {
            pid: codex_pid,
            unix_ns: 1,
        },
        agent: "codex".into(),
        ppid: codex_pid,
        input: Input::Complete,
        payload: br#"{"hook_event_name":"SessionStart","session_id":"codex-conv-42"}"#.to_vec(),
    };
    std::io::Write::write_all(&mut hook_conn, &ev.encode().unwrap()).unwrap();
    let _ = ipc::read_frame(&mut hook_conn).unwrap();

    // 살아 있는 데몬에서 종료된 codex 세션을 Resume(Restart 칩).
    let deadline = Instant::now() + WAIT;
    while let Reply::Sessions(list) = request(&mut c, &Request::List) {
        if list
            .iter()
            .any(|i| i.session == codex_old && i.status.is_some())
        {
            break;
        }
        assert!(Instant::now() < deadline, "codex did not exit");
        thread::sleep(Duration::from_millis(50));
    }
    let Reply::Resumed {
        session: codex_resumed,
        ..
    } = request(
        &mut c,
        &Request::Resume {
            session: codex_old,
            create: codex_create.clone(),
        },
    )
    else {
        panic!("codex live resume failed");
    };
    assert_ne!(codex_resumed, codex_old);
    let codex_out = wait_line(&mut c, codex_resumed, "resume codex-conv-42");
    assert!(
        codex_out.contains("--dangerously-bypass-approvals-and-sandbox resume codex-conv-42"),
        "{codex_out}"
    );

    // 데몬 강제 종료·재기동 뒤 콜드 복원 → Claude 세션을 Resume.
    drop((c, hook_conn, daemon));
    let (daemon, mut c) = Daemon::start(&dir);
    assert!(cold(&mut c, claude_old).is_some());
    let Reply::Resumed {
        session: claude_resumed,
        ..
    } = request(
        &mut c,
        &Request::Resume {
            session: claude_old,
            create: Create {
                account: None,
                program: Program::Login,
                cwd: Vec::new(),
                cols: 120,
                rows: 24,
            },
        },
    )
    else {
        panic!("claude cold resume failed");
    };
    assert_ne!(claude_resumed, claude_old);
    let resumed_out = wait_line(&mut c, claude_resumed, &format!("--resume={sid}"));
    assert!(
        resumed_out.contains("--dangerously-skip-permissions"),
        "{resumed_out}"
    );
    assert!(resumed_out.contains("--settings "), "{resumed_out}");

    let sock = daemon.sock.clone();
    drop((c, daemon));
    assert_no_holders(&sock);
    fs::remove_dir_all(&dir).ok();
}

/// MK-081: 실제 holder가 종료를 확인한 뒤 재개 API는 저장된 대화·cwd·권한을 자동 복원한다.
/// 시간·안전 게이트는 `registry::hibernation` 단위 테스트의 가짜 시계로 별도 재현한다.
#[test]
fn hibernation_auto_resume_keeps_conversation_and_is_idempotent() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    let dir = env::temp_dir().join(format!("mk081-wake-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(dir.join("bin")).unwrap();
    let dir = dir.canonicalize().unwrap();
    let fake = dir.join("bin/codex");
    fs::write(
        &fake,
        "#!/bin/sh\nprintf 'MK081_ARGS:%s MK081_CWD:%s\\n' \"$*\" \"$PWD\"\nexec /bin/cat\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        dir.join("agents.toml"),
        format!(
            "permission = 'yolo'\n[agent.codex]\nname = 'fixture'\nlaunch = ['{}']\nyolo_args = ['--fixture-yolo']\n",
            fake.display()
        ),
    )
    .unwrap();
    let (daemon, mut c) = Daemon::start(&dir);
    let create = Create {
        account: None,
        program: Program::Agent { id: "codex".into() },
        cwd: dir.as_os_str().as_encoded_bytes().to_vec(),
        cols: 200,
        rows: 24,
    };
    let Reply::Created {
        session: old,
        pid: old_pid,
    } = request(&mut c, &Request::Create(create.clone()))
    else {
        panic!("create");
    };
    let mut hook = connect(&daemon.sock).unwrap().0;
    let event = krill_proto::hook::HookEvent {
        id: krill_proto::hook::EventId {
            pid: old_pid,
            unix_ns: 1,
        },
        agent: "codex".into(),
        ppid: old_pid,
        input: krill_proto::hook::Input::Complete,
        payload: br#"{"hook_event_name":"Stop","session_id":"mk081-conversation"}"#.to_vec(),
    };
    hook.write_all(&event.encode().unwrap()).unwrap();
    ipc::read_frame(&mut hook).unwrap().unwrap();
    fs::create_dir_all(dir.join("hibernated")).unwrap();
    fs::write(dir.join("hibernated").join(old.0.to_string()), b"").unwrap();
    // 에이전트는 대화형 로그인 셸(`-l -i -c`)을 거쳐 뜨고 대화형 zsh는 SIGTERM을 무시한다. 셸 기동 중에
    // 보내면 신호가 사라져 세션이 끝나지 않으므로(고부하에서 기동이 길어져 재현, OWE-745) fixture가 뜬 뒤에 보낸다.
    let deadline = Instant::now() + WAIT;
    while !cold(&mut c, old).is_some_and(|(rows, _)| rows.iter().any(|r| r.contains("MK081_ARGS")))
    {
        assert!(Instant::now() < deadline, "fixture never started");
        thread::sleep(Duration::from_millis(20));
    }
    kill_process(pid(old_pid), Signal::TERM).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        if let Reply::Sessions(rows) = request(&mut c, &Request::List)
            && rows.iter().any(|r| r.session == old && r.status.is_some())
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let wake = Request::Hibernation {
        session: old,
        create: Create {
            account: None,
            program: Program::Login,
            cwd: vec![],
            cols: 200,
            rows: 24,
        },
    };
    let Reply::Hibernation { session: Some(new) } = request(&mut c, &wake) else {
        panic!("wake");
    };
    assert_ne!(old, new);
    assert_eq!(
        request(&mut c, &wake),
        Reply::Hibernation { session: Some(new) }
    );
    loop {
        if let Some((rows, _)) = cold(&mut c, new) {
            let text = rows.join("\n");
            if text.contains("resume mk081-conversation") {
                assert!(text.contains("--fixture-yolo"));
                assert!(text.contains(&format!("MK081_CWD:{}", dir.display())));
                break;
            }
        }
        assert!(Instant::now() < deadline, "resume output not found");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!dir.join("hibernated").join(old.0.to_string()).exists());
    let sock = daemon.sock.clone();
    drop((hook, c, daemon));
    assert_no_holders(&sock);
    fs::remove_dir_all(dir).unwrap();
}

/// MK-082: 과거 transcript 스캔 → 두 CLI 재개 → 저장한 대화 ID·cwd 검증. 실제 에이전트 대신
/// 인자를 찍고 종료하는 fixture만 실행하며 사용자 설정·인증·네트워크를 건드리지 않는다.
#[test]
fn agent_history_scans_on_request_and_resumes_both_agents() {
    use krill_proto::agent_history::{Action, Agent, Response};
    use krill_proto::control::CaptureFormat;
    use std::os::unix::fs::PermissionsExt as _;

    let dir = env::temp_dir().join(format!("mk082-e2e-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let work = dir.join("work 한글");
    fs::create_dir_all(&work).unwrap();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for name in ["claude", "codex"] {
        let exe = bin.join(name);
        fs::write(&exe, "#!/bin/sh\nif [ \"$1\" = --version ]; then echo '2.1.299 (Claude Code)'; exit 0; fi\nfor a in \"$@\"; do printf 'ARG:%s\\n' \"$a\"; done\nprintf 'PWD:%s\\n' \"$PWD\"\nprintf 'CONFIG:%s%s\\n' \"$CODEX_HOME\" \"$CLAUDE_CONFIG_DIR\"\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(dir.join("agents.toml"), format!(
        "[agent.claude]\nname='Claude'\nlaunch=['{}']\n[agent.codex]\nname='Codex'\nlaunch=['{}']\n",
        bin.join("claude").display(), bin.join("codex").display())).unwrap();
    let c = dir.join("claude/projects/project");
    let x = dir.join("codex/sessions/2026/10/15");
    fs::create_dir_all(&c).unwrap();
    fs::create_dir_all(&x).unwrap();
    let sid = "11111111-2222-4333-8444-555555555555";
    fs::write(
        c.join("main.jsonl"),
        serde_json::json!({"sessionId":sid,"cwd":work}).to_string(),
    )
    .unwrap();
    fs::write(
        x.join("rollout-main.jsonl"),
        serde_json::json!({"type":"session_meta","payload":{"id":sid,"cwd":work}}).to_string(),
    )
    .unwrap();
    let (daemon, mut stream) = Daemon::start(&dir);
    assert_eq!(
        request(&mut stream, &Request::List),
        Reply::Sessions(vec![])
    );
    let Reply::AgentHistory(Response::Scanned(data)) =
        request(&mut stream, &Request::AgentHistory(Action::Scan))
    else {
        panic!("scan failed");
    };
    assert_eq!(data.entries.len(), 2);
    assert!(!data.skipped && !data.truncated);
    for entry in data.entries {
        let agent = entry.agent;
        let Reply::AgentHistory(Response::Resumed { session }) =
            request(&mut stream, &Request::AgentHistory(Action::Resume(entry)))
        else {
            panic!("resume failed");
        };
        let needle = match agent {
            Agent::Claude => format!("ARG:--resume={sid}"),
            Agent::Codex => format!("ARG:{sid}"),
        };
        let deadline = Instant::now() + WAIT;
        loop {
            if let Reply::Captured { bytes, .. } = request(
                &mut stream,
                &Request::Capture {
                    session,
                    format: CaptureFormat::Plain,
                },
            ) {
                let text = String::from_utf8_lossy(&bytes).replace('\n', "");
                if text.contains(&needle) && text.contains("PWD:") && text.contains("CONFIG:") {
                    // 80열 화면 캡처는 줄바꿈 경계의 공백을 다듬으므로 한글 경로의 공백은 따로 확인한다.
                    assert!(text.contains("work") && text.contains("한글"), "{text}");
                    assert!(
                        text.contains(&format!("CONFIG:{}", dir.join(agent.profile()).display())),
                        "{text}"
                    );
                    if agent == Agent::Codex {
                        assert!(text.contains("ARG:resume"), "{text}");
                    }
                    break;
                }
            }
            assert!(Instant::now() < deadline, "resume output missing: {needle}");
            thread::sleep(Duration::from_millis(20));
        }
        let metadata: serde_json::Value = serde_json::from_slice(
            &fs::read(dir.join(format!("agent-launches/{}.json", session.0))).unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["session_id"], sid);
        assert_eq!(metadata["profile"], agent.profile());
        request(&mut stream, &Request::Close { session });
    }
    drop(stream);
    drop(daemon);
    fs::remove_dir_all(dir).ok();
}
