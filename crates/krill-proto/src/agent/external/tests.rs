use super::*;

const WORKING: &str = r#"{"pid": 19295, "id": "3c50e72c", "cwd": "/tmp/mk-owe-448", "kind": "background", "startedAt": 1792038480643, "sessionId": "3c50e72c-bb60-4cbf-a8f2-3d36af022bc9", "name": "reply with exactly: mk083-probe-ok", "status": "busy", "state": "working"}"#;
const BLOCKED: &str = r#"{"pid": 81156, "id": "fff7abcd", "cwd": "/tmp/mk-owe-448", "kind": "background", "startedAt": 1792038722121, "sessionId": "fff7abcd-7dd0-4f8b-94d6-0c0b25a350db", "name": "write hello to mk083-wait", "status": "waiting", "waitingFor": "permission prompt", "state": "blocked"}"#;
const STOPPED: &str = r#"{"id": "fff7abcd", "cwd": "/tmp/mk-owe-448", "kind": "background", "startedAt": 1792038478241, "sessionId": "fff7abcd-bb60-4cbf-a8f2-3d36af022bc9", "name": "mk083-probe-ok", "state": "done"}"#;
const INTERACTIVE: &str = r#"{"pid": 79077, "cwd": "/tmp/demo-app", "kind": "interactive", "startedAt": 1791884008505, "sessionId": "346e0791-b6f9-4889-b243-b39fdbe26bc5", "name": "demo-app-17", "status": "idle"}"#;

fn array(rows: &[&str]) -> String {
    format!("[{}]", rows.join(","))
}

#[test]
fn measured_shapes_map_to_sidebar_states() {
    let got = parse(&array(&[WORKING, BLOCKED, STOPPED, INTERACTIVE]));
    assert_eq!(got.len(), 3, "interactive는 대상이 아니다");
    assert_eq!(got[0].id, "3c50e72c");
    assert_eq!(got[0].pid, Some(19295));
    assert_eq!(got[0].agent_state(), AgentState::Working);
    assert_eq!(got[1].agent_state(), AgentState::Blocked);
    assert_eq!(got[1].waiting_for, "permission prompt");
    assert_eq!(got[2].pid, None, "stop 뒤 잔류형에 pid 없음이 정상");
    assert_eq!(got[2].status_raw, "");
    assert_eq!(got[2].agent_state(), AgentState::Done);
}

#[test]
fn unknown_strings_are_kept_and_map_to_unknown() {
    let row =
        r#"{"id": "abc123", "kind": "background", "state": "levitating", "status": "vibing"}"#;
    let got = parse(&array(&[row]));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].state_raw, "levitating");
    assert_eq!(got[0].agent_state(), AgentState::Unknown);
}

#[test]
fn broken_elements_do_not_kill_the_rest() {
    let got = parse(&array(&[
        WORKING,
        "42",
        r#"{"kind": "background"}"#,
        BLOCKED,
    ]));
    assert_eq!(got.len(), 2, "id 없는 background와 숫자는 건너뛴다");
    assert_eq!(got[0].id, "3c50e72c");
    assert_eq!(got[1].id, "fff7abcd");
}

#[test]
fn non_array_and_garbage_are_empty() {
    for text in ["", "{}", r#"{"kind": "background"}"#, "[[[]]]", "[{]"] {
        assert!(parse(text).is_empty(), "{text:?}");
    }
    assert!(parse(&"x".repeat(MAX_INPUT + 1)).is_empty());
}

#[test]
fn status_fallback_covers_stateless_rows() {
    let row = |status: &str| {
        parse(&array(&[&format!(
            r#"{{"id": "a1", "kind": "background", "status": "{status}"}}"#
        )]))[0]
            .agent_state()
    };
    assert_eq!(row("busy"), AgentState::Working);
    assert_eq!(row("waiting"), AgentState::Waiting);
    assert_eq!(row("idle"), AgentState::Done);
    assert_eq!(row(""), AgentState::Unknown);
}

#[test]
fn state_matching_ignores_case_and_whitespace() {
    let row = parse(&array(&[
        r#"{"id": "a1", "kind": "background", "state": "  BLOCKED "}"#,
    ]));
    assert_eq!(row[0].agent_state(), AgentState::Blocked);
}

proptest::proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(text in ".{0,8192}") {
        let got = parse(&text);
        proptest::prop_assert!(got.len() <= MAX_SESSIONS);
    }

    #[test]
    fn arbitrary_json_shaped_never_panic(
        pieces in proptest::collection::vec(
            proptest::string::string_regex("[a-z0-9_{}\\[\\]:, ]{0,32}").unwrap(),
            0..8,
        ),
    ) {
        let value = format!("[{}]", pieces.join(","));
        let got = parse(&value);
        proptest::prop_assert!(got.len() <= MAX_SESSIONS);
    }
}
