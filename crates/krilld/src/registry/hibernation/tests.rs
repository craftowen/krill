use super::*;
use crate::holder::{Holder, Kept};
use crate::scope::{Peer, Scope};
use krill_proto::agent::Permission;
use krill_proto::control::{AgentLaunch, Create, Program, Reply};
use krill_proto::holder::{FromHolder, VERSION};
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

fn done(ns: u64) -> AgentStatus {
    AgentStatus {
        state: AgentState::Done,
        source: Some(Source::Hook),
        confidence: Confidence::High,
        agent: "codex".into(),
        since_unix_ns: ns,
        ..AgentStatus::UNKNOWN
    }
}

#[test]
fn cross_review_hibernation_final_check_never_waits_for_registry_state() {
    let reg = Registry::new(None, None);
    let reg = &reg;
    std::thread::scope(|scope| {
        // on_hook은 state를 쥐고 live를 기다릴 수 있다. 동면은 이미 live를 쥔 단계다.
        let state = lock(&reg.state);
        let (tx, rx) = std::sync::mpsc::channel();
        scope.spawn(move || {
            tx.send(reg.hibernation_group_live([SessionId(1)])).ok();
        });
        let result = rx.recv_timeout(Duration::from_millis(250));
        drop(state); // 실패한 구현도 회수해 테스트 프로세스에 교착을 남기지 않는다.
        assert_eq!(result, Ok(false));
    });
}

#[test]
fn thirty_minutes_requires_continuous_hook_done_and_resets_on_new_turn() {
    let mut c = Clock::default();
    let id = SessionId(1);
    let now = Instant::now();
    let mut s = done(1);
    assert!(!c.ready(id, &s, now, IDLE));
    assert!(!c.ready(
        id,
        &s,
        now + IDLE.saturating_sub(Duration::from_nanos(1)),
        IDLE
    ));
    assert!(c.ready(id, &s, now + IDLE, IDLE));
    for state in [
        AgentState::Waiting,
        AgentState::Blocked,
        AgentState::Working,
        AgentState::Unknown,
        AgentState::Exited,
    ] {
        s.state = state;
        assert!(!c.ready(id, &s, now + IDLE, IDLE));
        assert!(c.done.is_empty());
    }
    s = done(2);
    assert!(!c.ready(id, &s, now + IDLE, IDLE));
    assert!(c.ready(id, &s, now + IDLE * 2, IDLE));
    s.source = Some(Source::Screen);
    assert!(!c.ready(id, &s, now + IDLE * 3, IDLE));
}

#[test]
fn timer_sends_terminate_only_for_safe_group_and_preserves_resume_marker() {
    let dir = std::env::temp_dir().join(format!("mk081-timer-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let (daemon_end, mut holder_end) = UnixStream::pair().unwrap();
    holder_end
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: std::process::id(),
        },
        FromHolder::Ready,
    ] {
        holder_end.write_all(&m.encode().unwrap()).unwrap();
    }
    let (holder, _link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    // 실제 프로세스 2개. holder 전송을 관측한 뒤 이 테스트가 띄운 PID만 종료한다.
    let mut processes: Vec<_> = (0..2)
        .map(|_| {
            Command::new("/bin/cat")
                .current_dir(&dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut users = Vec::new();
    let kept = processes
        .iter()
        .enumerate()
        .map(|(i, child)| {
            let pty = rustix_openpty::openpty(None, None).unwrap();
            users.push(pty.user);
            Kept {
                id: SessionId(u64::try_from(i + 1).unwrap()),
                pid: child.id(),
                status: None,
                master: pty.controller,
            }
        })
        .collect();
    let reg = Registry::new(Some((holder, kept)), Some(&dir));
    for id in [SessionId(1), SessionId(2)] {
        reg.agents.forget(id); // fixture의 재전달 veto를 새 spawn 관측으로 바꾼다.
        reg.agents
            .remember_session(
                id,
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Manual,
                },
                Some("conversation-81".into()),
                dir.as_os_str().as_encoded_bytes().to_vec(),
                None,
            )
            .unwrap();
        let e = krill_proto::hook::HookEvent {
            id: krill_proto::hook::EventId { pid: 9, unix_ns: 1 },
            agent: "codex".into(),
            ppid: 0,
            input: krill_proto::hook::Input::Complete,
            payload: br#"{"hook_event_name":"Stop"}"#.to_vec(),
        };
        let st = lock(&reg.state);
        reg.apply_agent_hook(&st, id, &e).unwrap();
    }
    let taps: Vec<_> = {
        let st = lock(&reg.state);
        st.sessions.values().map(|e| e.session.tap()).collect()
    };
    // 기본 꺼짐: 타이머를 호출해도 종료 표식이 생기지 않는다.
    reg.hibernate_idle();
    assert!(!reg.sleeping(SessionId(1)));
    // 한 pane이 foreground이면 묶음 전체가 살아 있어야 한다.
    let viewer = taps[1].watch();
    reg.hibernate_at(&mut Clock::default(), Instant::now(), Duration::ZERO);
    assert!(!reg.sleeping(SessionId(1)));
    drop(viewer);
    let client =
        crate::attach::roles::Client::new(taps[1].clone(), krill_proto::roles::Role::Observer)
            .unwrap();
    reg.hibernate_at(&mut Clock::default(), Instant::now(), Duration::ZERO);
    assert!(
        !reg.sleeping(SessionId(1)),
        "observer/raw/mobile connections also veto sleep"
    );
    drop(client);
    // 자식이 있는 루트(이 테스트 프로세스)를 관측하면 provider done보다 커널 근거가 우선한다.
    {
        let mut st = lock(&reg.state);
        for entry in st.sessions.values_mut() {
            entry.pid = std::process::id();
        }
    }
    reg.hibernate_at(&mut Clock::default(), Instant::now(), Duration::ZERO);
    assert!(
        !reg.sleeping(SessionId(1)),
        "live children veto the whole group"
    );
    {
        let mut st = lock(&reg.state);
        for (entry, child) in st.sessions.values_mut().zip(&processes) {
            entry.pid = child.id();
        }
    }
    // 완료 뒤 한 글자 입력은 새 완료 hook까지 영구 veto다(대기 시간만 늘려 지우지 않는다).
    taps[1].write_input(b"x", None).unwrap();
    reg.hibernate_at(&mut Clock::default(), Instant::now(), Duration::ZERO);
    assert!(!reg.sleeping(SessionId(1)));
    let fresh = crate::status::now_ns().saturating_add(1);
    {
        let event = krill_proto::hook::HookEvent {
            id: krill_proto::hook::EventId {
                pid: 9,
                unix_ns: fresh,
            },
            agent: "codex".into(),
            ppid: 0,
            input: krill_proto::hook::Input::Complete,
            payload: br#"{"hook_event_name":"Stop"}"#.to_vec(),
        };
        reg.apply_agent_hook(&lock(&reg.state), SessionId(2), &event)
            .unwrap();
    }
    // 안전한 전체 묶음: PTY fd와 재개 메타데이터를 보존하며 Terminate만 전송한다.
    reg.hibernate_at(&mut Clock::default(), Instant::now(), Duration::ZERO);
    for id in [SessionId(1), SessionId(2)] {
        assert!(reg.sleeping(id), "safe leaf process should hibernate");
        assert!(reg.agents.has(id));
        let mut header = [0; 4];
        holder_end.read_exact(&mut header).unwrap();
        let mut body = vec![0; usize::try_from(u32::from_le_bytes(header)).unwrap()];
        holder_end.read_exact(&mut body).unwrap();
        let (&[kind, fds], payload) = body.split_first_chunk::<2>().unwrap();
        assert!(
            matches!(ToHolder::decode(krill_proto::holder::Frame { kind, fds, body: payload }).unwrap(), ToHolder::Terminate { id: got, .. } if got == id)
        );
    }
    assert!(taps[0].write_input(b"late", None).is_err());
    for child in &mut processes {
        child.kill().unwrap();
        child.wait().unwrap();
    }
    for id in [SessionId(1), SessionId(2)] {
        reg.on_exited(id, 0);
    }
    assert!(reg.sleeping(SessionId(1)));
    let reopened = Registry::new(None, Some(&dir));
    assert!(
        reopened.sleeping(SessionId(1)),
        "marker survives daemon restart"
    );
    let peer = Peer {
        pid: None,
        scope: Scope::Restricted,
    };
    let create = Create {
        account: None,
        program: Program::Login,
        cwd: vec![],
        cols: 80,
        rows: 24,
    };
    assert_eq!(
        reg.wake_hibernated(&peer, SessionId(1), create),
        Err(krill_proto::control::Failure::Denied)
    );
    drop((users, reg, reopened));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unslept_session_is_never_automatically_restarted() {
    let reg = Registry::new(None, None);
    let peer = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let create = Create {
        account: None,
        program: Program::Login,
        cwd: vec![],
        cols: 80,
        rows: 24,
    };
    assert_eq!(
        reg.wake_hibernated(&peer, SessionId(9), create),
        Ok(Reply::Hibernation { session: None })
    );
}

/// K-22: 종료 송신에 실패하면 표시만 남기고 세션을 잠그지 않는다.
/// 수정 전에는 마커 파일이 먼저 만들어져 살아 있는 세션이 `sleeping`으로 잠겼다.
#[test]
fn terminate_send_failure_leaves_no_sleep_marker() {
    let dir = std::env::temp_dir().join(format!("mk081-sendfail-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let (daemon_end, holder_end) = UnixStream::pair().unwrap();
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: std::process::id(),
        },
        FromHolder::Ready,
    ] {
        (&holder_end).write_all(&m.encode().unwrap()).unwrap();
    }
    let (holder, _link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let mut child = Command::new("/bin/cat")
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let user = pty.user;
    let kept = vec![Kept {
        id: SessionId(1),
        pid: child.id(),
        status: None,
        master: pty.controller,
    }];
    let reg = Registry::new(Some((holder, kept)), Some(&dir));
    reg.agents.forget(SessionId(1));
    reg.agents
        .remember_session(
            SessionId(1),
            &AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Manual,
            },
            Some("conversation-81".into()),
            dir.as_os_str().as_encoded_bytes().to_vec(),
            None,
        )
        .unwrap();
    let e = krill_proto::hook::HookEvent {
        id: krill_proto::hook::EventId { pid: 9, unix_ns: 1 },
        agent: "codex".into(),
        ppid: 0,
        input: krill_proto::hook::Input::Complete,
        payload: br#"{"hook_event_name":"Stop"}"#.to_vec(),
    };
    reg.apply_agent_hook(&lock(&reg.state), SessionId(1), &e)
        .unwrap();
    // holder 링크를 끊는다. 이후 Terminate 송신은 실패한다.
    holder_end.shutdown(std::net::Shutdown::Both).unwrap();
    drop(holder_end);
    reg.hibernate_at(&mut Clock::default(), Instant::now(), Duration::ZERO);
    assert!(
        !reg.sleeping(SessionId(1)),
        "failed terminate must not lock a live session"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    drop((user, reg));
    fs::remove_dir_all(dir).unwrap();
}

/// OWE-698: 휴면 뒤 계정을 바꿔도 깨우기는 실행 당시 계정의 `CLAUDE_CONFIG_DIR`로 띄운다.
/// holder가 받은 Spawn env를 관측하고 실패로 답해 실제 프로세스는 띄우지 않는다.
#[test]
fn wake_after_account_switch_keeps_launch_profile() {
    use crate::agent::control::LaunchAccount;
    use crate::usage::accounts;
    use krill_proto::usage::ProviderKind;
    let dir = std::env::temp_dir().join(format!("owe698-wake-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(dir.join("hibernated")).unwrap();
    let claude_exe = dir.join("claude");
    fs::write(&claude_exe, "#!/bin/sh\nprintf '2.1.299\\n'\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&claude_exe, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        dir.join(krill_proto::agent::FILE_NAME),
        format!(
            "permission = \"manual\"\n[profiles.claude]\nname = \"Claude\"\nlaunch = [{:?}]\n",
            claude_exe.to_str().unwrap()
        ),
    )
    .unwrap();
    let home = accounts::default_home();
    let add = |label: &str| {
        let profile = dir.join(label);
        fs::create_dir_all(&profile).unwrap();
        let id = accounts::add_account(
            &home,
            Some(&dir),
            ProviderKind::Claude,
            label,
            None,
            profile.to_str().unwrap(),
        )
        .unwrap()
        .accounts
        .into_iter()
        .find(|a| a.label == label)
        .unwrap()
        .id;
        (id, profile.to_str().unwrap().to_owned())
    };
    let ((a, a_dir), (b, _)) = (add("A"), add("B"));
    accounts::switch_account(&home, Some(&dir), ProviderKind::Claude, &a).unwrap();

    let (daemon_end, mut holder_end) = UnixStream::pair().unwrap();
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: std::process::id(),
        },
        FromHolder::Ready,
    ] {
        holder_end.write_all(&m.encode().unwrap()).unwrap();
    }
    let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = Registry::new(Some((holder, Vec::new())), Some(&dir));
    let old = SessionId(5);
    reg.agents
        .remember_session(
            old,
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            Some("11111111-2222-4333-8444-555555555555".into()),
            dir.as_os_str().as_encoded_bytes().to_vec(),
            Some(LaunchAccount {
                id: a,
                env_key: "CLAUDE_CONFIG_DIR".into(),
                dir: a_dir.clone(),
            }),
        )
        .unwrap();
    fs::write(dir.join("hibernated").join("5"), b"").unwrap();
    accounts::switch_account(&home, Some(&dir), ProviderKind::Claude, &b).unwrap();

    let observed = std::thread::spawn(move || {
        let mut header = [0; 4];
        holder_end.read_exact(&mut header).unwrap();
        let mut body = vec![0; usize::try_from(u32::from_le_bytes(header)).unwrap()];
        holder_end.read_exact(&mut body).unwrap();
        let (&[kind, fds], payload) = body.split_first_chunk::<2>().unwrap();
        let frame = krill_proto::holder::Frame {
            kind,
            fds,
            body: payload,
        };
        let ToHolder::Spawn { req, env, .. } = ToHolder::decode(frame).unwrap() else {
            panic!("expected Spawn");
        };
        let failed = FromHolder::Failed {
            req,
            errno: libc::EIO,
        };
        holder_end.write_all(&failed.encode().unwrap()).unwrap();
        (env, holder_end)
    });
    let peer = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let create = Create {
        account: None,
        program: Program::Login,
        cwd: vec![],
        cols: 80,
        rows: 24,
    };
    let (env, holder_end) = std::thread::scope(|scope| {
        let holder = reg.holder.as_ref().unwrap();
        let pump = scope.spawn(move || holder.run(link, |_, _| {}));
        assert!(reg.wake_hibernated(&peer, old, create).is_err());
        let (env, holder_end) = observed.join().unwrap();
        // 링크를 닫아 `run`을 끝낸다.
        holder_end.shutdown(std::net::Shutdown::Both).unwrap();
        pump.join().unwrap();
        (env, holder_end)
    });
    let config: Vec<_> = env
        .iter()
        .filter(|(k, _)| k == b"CLAUDE_CONFIG_DIR")
        .map(|(_, v)| v.as_slice())
        .collect();
    assert_eq!(
        config,
        [a_dir.as_bytes()],
        "활성 계정 B가 아니라 실행 당시 A"
    );
    drop((holder_end, reg));
    fs::remove_dir_all(dir).unwrap();
}
