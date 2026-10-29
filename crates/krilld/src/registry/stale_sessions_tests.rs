use super::*;
use krill_proto::control::{AgentState, Confidence, Source};
use krill_proto::hook::{EventId, Input};

#[test]
fn terminal_exit_overrides_stale_hook_status_without_hiding_live_done_sessions() {
    let reg = Registry::new(None, None);
    let peer = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let mut outputs = Vec::new();
    for id in [SessionId(1), SessionId(2), SessionId(3)] {
        let (master, output) = UnixStream::pair().unwrap();
        let session = Session::start(
            id,
            OwnedFd::from(master),
            FALLBACK_SIZE,
            Arc::clone(&reg.look),
            None,
        )
        .unwrap();
        lock(&reg.state).sessions.insert(
            id,
            Entry {
                session,
                pid: std::process::id(),
                cwd: PathBuf::new(),
                wrapped: false,
            },
        );
        outputs.push(output);
        reg.status
            .hook(&[(
                id,
                &HookEvent {
                    id: EventId { pid: 1, unix_ns: 1 },
                    ppid: 1,
                    agent: "codex".into(),
                    input: Input::Complete,
                    payload: br#"{"hook_event_name":"Stop"}"#.to_vec(),
                },
            )])
            .unwrap();
    }

    // EOF can be visible before the holder's Exited callback reaches the registry.
    drop(outputs.remove(0));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if lock(&reg.state)
            .sessions
            .get(&SessionId(1))
            .unwrap()
            .session
            .is_closed()
        {
            break;
        }
        assert!(Instant::now() < deadline, "PTY reader did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    // A late hook with a newer timestamp must not resurrect a holder-confirmed exit.
    reg.on_exited(SessionId(2), 0);
    reg.status
        .hook(&[(
            SessionId(2),
            &HookEvent {
                id: EventId {
                    pid: 2,
                    unix_ns: u64::MAX,
                },
                ppid: 1,
                agent: "codex".into(),
                input: Input::Complete,
                payload: br#"{"hook_event_name":"Stop"}"#.to_vec(),
            },
        )])
        .unwrap();

    let Reply::States(states) = reg.states(&peer) else {
        panic!("expected states");
    };
    for id in [SessionId(1), SessionId(2)] {
        let status = &states.iter().find(|s| s.session == id).unwrap().status;
        assert_eq!(status.state, AgentState::Exited, "session {id:?}");
        assert_eq!(status.source, Some(Source::Process));
        assert_eq!(status.confidence, Confidence::High);
        assert!(status.foreground.is_none());
        assert!(status.subagents.is_empty());
    }
    let done = &states
        .iter()
        .find(|s| s.session == SessionId(3))
        .unwrap()
        .status;
    assert_eq!(done.state, AgentState::Done);
    assert_eq!(done.source, Some(Source::Hook));
    // Read-only status reconciliation must not release PTYs or remove terminal history.
    assert_eq!(lock(&reg.state).sessions.len(), 3);
    assert!(!lock(&reg.state).exited.contains_key(&SessionId(1)));
}
