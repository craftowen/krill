use super::*;
use crate::control::{Reply, Request};
use crate::screen::Msg;

#[test]
fn history_payload_fixtures_and_control_roundtrips() {
    let cases = [
        (Action::Scan, vec![0]),
        (
            Action::Resume(Entry {
                agent: Agent::Codex,
                id: "id".into(),
                cwd: "/w".into(),
                modified: 7,
            }),
            vec![1, 1, 2, b'i', b'd', 2, b'/', b'w', 7],
        ),
    ];
    for (action, fixture) in cases {
        assert_eq!(postcard::to_allocvec(&action).unwrap(), fixture);
        assert_eq!(postcard::from_bytes::<Action>(&fixture).unwrap(), action);
        let req = Request::AgentHistory(action);
        let bytes = req.encode().unwrap();
        let (kind, body) = crate::ipc::read_frame(&mut &bytes[..]).unwrap().unwrap();
        assert_eq!(Request::decode(kind, &body).unwrap(), req);
        // 바깥 enum 변형 번호와 무관한 고정 payload.
        assert_eq!(body.get(1..).unwrap(), fixture);
    }
    let snapshot = Snapshot {
        entries: vec![],
        truncated: false,
        skipped: true,
    };
    for (response, fixture) in [
        (Response::Scanned(snapshot), vec![0, 0, 0, 1]),
        (
            Response::Resumed {
                session: crate::SessionId(7),
            },
            vec![1, 7],
        ),
    ] {
        assert_eq!(postcard::to_allocvec(&response).unwrap(), fixture);
        assert_eq!(
            postcard::from_bytes::<Response>(&fixture).unwrap(),
            response
        );
        let reply = Reply::AgentHistory(response);
        let bytes = reply.encode().unwrap();
        let (kind, body) = crate::ipc::read_frame(&mut &bytes[..]).unwrap().unwrap();
        assert_eq!(Reply::decode(kind, &body).unwrap(), reply);
        assert_eq!(body.get(1..).unwrap(), fixture);
    }
}

#[test]
fn history_decode_rejects_oversized_metadata_before_allocating() {
    let entry = Entry {
        agent: Agent::Claude,
        id: "x".repeat(MAX_TEXT + 1),
        cwd: "/w".into(),
        modified: 0,
    };
    assert!(postcard::from_bytes::<Entry>(&postcard::to_allocvec(&entry).unwrap()).is_err());
    let entry = Entry {
        id: "id".into(),
        ..entry
    };
    let data = Snapshot {
        entries: vec![entry; MAX_ENTRIES + 1],
        truncated: true,
        skipped: false,
    };
    assert!(postcard::from_bytes::<Snapshot>(&postcard::to_allocvec(&data).unwrap()).is_err());

    let oversized_query = SearchQuery {
        query: "q".repeat(MAX_TEXT + 1),
        ..SearchQuery::default()
    };
    assert!(
        postcard::from_bytes::<SearchQuery>(&postcard::to_allocvec(&oversized_query).unwrap())
            .is_err()
    );
    let oversized_paths = SearchQuery {
        paths: vec!["/w".into(); MAX_FILTER_ITEMS + 1],
        ..SearchQuery::default()
    };
    assert!(
        postcard::from_bytes::<SearchQuery>(&postcard::to_allocvec(&oversized_paths).unwrap())
            .is_err()
    );
}

#[test]
fn vault_search_and_resume_roundtrip_and_agent_parsing() {
    for (input, expected, cmd) in [
        ("claude", VaultAgent::Claude, "claude --resume s1"),
        ("CODEX", VaultAgent::Codex, "codex resume s1"),
        ("agy", VaultAgent::Agy, "agy --conversation s1"),
        ("antigravity", VaultAgent::Agy, "agy --conversation s1"),
        ("opencode", VaultAgent::OpenCode, "opencode --session s1"),
    ] {
        let parsed = VaultAgent::parse(input).unwrap();
        assert_eq!(parsed, expected);
        assert_eq!(parsed.resume_command("s1"), cmd);
    }
    assert!(VaultAgent::parse("unknown").is_none());

    let query = Action::Search(SearchQuery {
        query: "panic".into(),
        agents: vec![VaultAgent::Claude, VaultAgent::Agy],
        paths: vec!["/tmp/work".into()],
        limit: 20,
    });
    let req = Request::AgentHistory(query);
    let bytes = req.encode().unwrap();
    let (kind, body) = crate::ipc::read_frame(&mut &bytes[..]).unwrap().unwrap();
    assert_eq!(Request::decode(kind, &body).unwrap(), req);

    let hit = VaultHit {
        agent: VaultAgent::OpenCode,
        id: "ses_123".into(),
        cwd: "/tmp/work".into(),
        modified: 42,
        title: "Fix panic".into(),
        summary: "Resolved issue".into(),
        snippet: "no panic in krill-holder".into(),
        role: "assistant".into(),
        score: 120,
    };
    let resume = Action::ResumeVault(hit.target());
    let req = Request::AgentHistory(resume);
    let bytes = req.encode().unwrap();
    let (kind, body) = crate::ipc::read_frame(&mut &bytes[..]).unwrap().unwrap();
    assert_eq!(Request::decode(kind, &body).unwrap(), req);

    let searched = Response::Searched(SearchSnapshot {
        hits: vec![hit],
        truncated: false,
        skipped: false,
    });
    let reply = Reply::AgentHistory(searched);
    let bytes = reply.encode().unwrap();
    let (kind, body) = crate::ipc::read_frame(&mut &bytes[..]).unwrap().unwrap();
    assert_eq!(Reply::decode(kind, &body).unwrap(), reply);

    let poll_req = Request::AgentHistory(Action::ChatPoll {
        session: crate::SessionId(42),
        offset: 1024,
    });
    let bytes = poll_req.encode().unwrap();
    let (kind, body) = crate::ipc::read_frame(&mut &bytes[..]).unwrap().unwrap();
    assert_eq!(Request::decode(kind, &body).unwrap(), poll_req);

    let poll_resp = Reply::AgentHistory(Response::ChatPolled(ChatPollSnapshot {
        agent: "claude".into(),
        session_id: "sid-42".into(),
        transcript_path: "/tmp/sid-42.jsonl".into(),
        next_offset: 2048,
        reset: false,
        bound: true,
        items: vec![ChatItem {
            kind: ChatItemKind::Question,
            text: "Choose mode".into(),
            title: "Mode".into(),
            tool_name: "AskUserQuestion".into(),
            tool_id: "toolu_1".into(),
            file_path: String::new(),
            is_error: false,
            multi_select: false,
            options: vec![ChatQuestionOption {
                label: "Fast".into(),
                description: "Low latency".into(),
            }],
            at: String::new(),
        }],
    }));
    let bytes = poll_resp.encode().unwrap();
    let (kind, body) = crate::ipc::read_frame(&mut &bytes[..]).unwrap().unwrap();
    assert_eq!(Reply::decode(kind, &body).unwrap(), poll_resp);

    // 모바일 전용 시각은 postcard 와이어에 실리지 않는다(버전이 다른 앱·데몬 호환).
    let item = ChatItem {
        kind: ChatItemKind::User,
        text: "hi".into(),
        ..ChatItem::default()
    };
    let timed = ChatItem {
        at: "2026-10-24T03:48:10.000Z".into(),
        ..item.clone()
    };
    assert_eq!(
        postcard::to_allocvec(&timed).unwrap(),
        postcard::to_allocvec(&item).unwrap()
    );
}

proptest::proptest! {
    #[test]
    fn arbitrary_history_payloads_never_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
        postcard::from_bytes::<Action>(&bytes).ok();
        postcard::from_bytes::<Response>(&bytes).ok();
    }
}
