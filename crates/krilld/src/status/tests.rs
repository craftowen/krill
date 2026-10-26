use std::fmt::Write as _;
use std::time::Instant;

use krill_proto::hook::{Input, append_spool};

use super::*;
use crate::usage::accounts;

const S1: SessionId = SessionId(1);

pub(super) fn event(pid: u32, unix_ns: u64, agent: &str, payload: &str) -> HookEvent {
    HookEvent {
        id: EventId { pid, unix_ns },
        agent: agent.to_owned(),
        ppid: 1,
        input: Input::Complete,
        payload: payload.as_bytes().to_vec(),
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mk039-{}-{name}", std::process::id()));
    fs::remove_dir_all(&d).ok();
    fs::create_dir_all(&d).unwrap();
    d
}

/// 이벤트를 하나씩 반영하며 `<payload> => <상태>/<출처>/<신뢰도> since=<ns> (반영 여부)` 한 줄씩.
fn replay(st: &Statuses, events: &[HookEvent]) -> String {
    let mut out = String::new();
    for e in events {
        let n = st.hook(&[(S1, e)]).unwrap();
        let s = st.get(S1);
        writeln!(
            out,
            "{} => {:?}/{:?}/{:?} since={} {}",
            String::from_utf8_lossy(&e.payload),
            s.state,
            s.source,
            s.confidence,
            s.since_unix_ns,
            if n == 1 { "applied" } else { "ignored" },
        )
        .unwrap();
    }
    out
}

/// Claude 한 턴(권한 요청·질문 포함) + 중복·늦은 이벤트·모르는 이벤트. 이 표가 바뀌면 정규화가 바뀐 것이다.
#[test]
fn claude_sequence_snapshot() {
    let ev = |n: u64, p: &str| event(100 + u32::try_from(n).unwrap(), n * 10, "claude", p);
    let start = ev(
        1,
        r#"{"hook_event_name":"SessionStart","source":"startup"}"#,
    );
    let events = [
        start.clone(),
        ev(2, r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#),
        ev(3, r#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#),
        ev(
            4,
            r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#,
        ),
        ev(
            5,
            r#"{"hook_event_name":"Notification","notification_type":"permission_prompt"}"#,
        ),
        ev(6, r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#),
        ev(
            7,
            r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion"}"#,
        ),
        ev(
            8,
            r#"{"hook_event_name":"PostToolUse","tool_name":"AskUserQuestion"}"#,
        ),
        ev(9, r#"{"hook_event_name":"SubagentStart","agent_id":"a1"}"#),
        ev(
            10,
            r#"{"hook_event_name":"Stop","last_assistant_message":"ok"}"#,
        ),
        ev(
            11,
            r#"{"hook_event_name":"Notification","notification_type":"idle_prompt"}"#,
        ),
        // 소켓과 spool 양쪽으로 온 같은 Stop, 늦게 수거한 오래된 SessionStart, JSON이 아닌 payload.
        ev(
            10,
            r#"{"hook_event_name":"Stop","last_assistant_message":"ok"}"#,
        ),
        start,
        ev(12, "not json"),
        ev(13, r#"{"hook_event_name":"SessionEnd","reason":"exit"}"#),
    ];
    let st = Statuses::open(None);
    assert_eq!(
        replay(&st, &events),
        r#"{"hook_event_name":"SessionStart","source":"startup"} => Done/Some(Hook)/High since=10 applied
{"hook_event_name":"UserPromptSubmit","prompt":"hi"} => Working/Some(Hook)/High since=20 applied
{"hook_event_name":"PreToolUse","tool_name":"Bash"} => Working/Some(Hook)/High since=20 applied
{"hook_event_name":"PermissionRequest","tool_name":"Bash"} => Waiting/Some(Hook)/High since=40 applied
{"hook_event_name":"Notification","notification_type":"permission_prompt"} => Waiting/Some(Hook)/High since=40 applied
{"hook_event_name":"PostToolUse","tool_name":"Bash"} => Working/Some(Hook)/High since=60 applied
{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion"} => Waiting/Some(Hook)/High since=70 applied
{"hook_event_name":"PostToolUse","tool_name":"AskUserQuestion"} => Working/Some(Hook)/High since=80 applied
{"hook_event_name":"SubagentStart","agent_id":"a1"} => Working/Some(Hook)/High since=80 ignored
{"hook_event_name":"Stop","last_assistant_message":"ok"} => Done/Some(Hook)/High since=100 applied
{"hook_event_name":"Notification","notification_type":"idle_prompt"} => Done/Some(Hook)/High since=100 ignored
{"hook_event_name":"Stop","last_assistant_message":"ok"} => Done/Some(Hook)/High since=100 ignored
{"hook_event_name":"SessionStart","source":"startup"} => Done/Some(Hook)/High since=100 ignored
not json => Done/Some(Hook)/High since=100 ignored
{"hook_event_name":"SessionEnd","reason":"exit"} => Exited/Some(Hook)/High since=130 applied
"#
    );
    assert_eq!(st.get(S1).agent, "claude");
}

/// Codex: 질문 도구 `request_user_input`, 인터럽트로 끝난 턴.
#[test]
fn codex_sequence_snapshot() {
    let ev = |n: u64, p: &str| event(200 + u32::try_from(n).unwrap(), n * 10, "codex", p);
    let events = [
        ev(1, r#"{"hook_event_name":"SessionStart"}"#),
        ev(2, r#"{"hook_event_name":"UserPromptSubmit"}"#),
        ev(
            3,
            r#"{"hook_event_name":"PreToolUse","tool_name":"request_user_input"}"#,
        ),
        ev(
            4,
            r#"{"hook_event_name":"PostToolUse","tool_name":"request_user_input"}"#,
        ),
        ev(5, r#"{"hook_event_name":"PermissionRequest"}"#),
        ev(6, r#"{"hook_event_name":"Interrupt"}"#),
    ];
    let st = Statuses::open(None);
    assert_eq!(
        replay(&st, &events),
        r#"{"hook_event_name":"SessionStart"} => Done/Some(Hook)/High since=10 applied
{"hook_event_name":"UserPromptSubmit"} => Working/Some(Hook)/High since=20 applied
{"hook_event_name":"PreToolUse","tool_name":"request_user_input"} => Waiting/Some(Hook)/High since=30 applied
{"hook_event_name":"PostToolUse","tool_name":"request_user_input"} => Working/Some(Hook)/High since=40 applied
{"hook_event_name":"PermissionRequest"} => Waiting/Some(Hook)/High since=50 applied
{"hook_event_name":"Interrupt"} => Done/Some(Hook)/High since=60 applied
"#
    );
}

/// Cursor 네이티브 이벤트(OWE-503). 설치기가 거는 이벤트 전부.
#[test]
fn cursor_sequence_snapshot() {
    let ev = |n: u64, a: &str, p: &str| event(300 + u32::try_from(n).unwrap(), n * 10, a, p);
    let events = [
        ev(1, "cursor", r#"{"hook_event_name":"sessionStart"}"#),
        ev(2, "cursor", r#"{"hook_event_name":"beforeSubmitPrompt"}"#),
        ev(3, "cursor", r#"{"hook_event_name":"afterShellExecution"}"#),
        ev(4, "cursor", r#"{"hook_event_name":"afterMCPExecution"}"#),
        ev(5, "cursor", r#"{"hook_event_name":"stop"}"#),
        ev(6, "cursor", r#"{"hook_event_name":"sessionEnd"}"#),
    ];
    let st = Statuses::open(None);
    assert_eq!(
        replay(&st, &events),
        r#"{"hook_event_name":"sessionStart"} => Done/Some(Hook)/High since=10 applied
{"hook_event_name":"beforeSubmitPrompt"} => Working/Some(Hook)/High since=20 applied
{"hook_event_name":"afterShellExecution"} => Working/Some(Hook)/High since=20 applied
{"hook_event_name":"afterMCPExecution"} => Working/Some(Hook)/High since=20 applied
{"hook_event_name":"stop"} => Done/Some(Hook)/High since=50 applied
{"hook_event_name":"sessionEnd"} => Exited/Some(Hook)/High since=60 applied
"#
    );
    assert_eq!(st.get(S1).agent, "cursor");
}

#[test]
fn agy_sequence_snapshot() {
    let ev = |n: u64, p: &str| event(350 + u32::try_from(n).unwrap(), n * 10, "agy", p);
    let events = [
        ev(1, r#"{"hook_event_name":"PreInvocation"}"#),
        ev(
            2,
            r#"{"hook_event_name":"PreToolUse","tool_name":"ask_question"}"#,
        ),
        ev(
            3,
            r#"{"hook_event_name":"PostToolUse","tool_name":"ask_question"}"#,
        ),
        ev(
            4,
            r#"{"hook_event_name":"PreToolUse","tool_name":"ask_permission"}"#,
        ),
        ev(
            5,
            r#"{"hook_event_name":"PostToolUse","tool_name":"ask_permission"}"#,
        ),
        ev(6, r#"{"hook_event_name":"PostInvocation"}"#),
    ];
    let st = Statuses::open(None);
    assert_eq!(
        replay(&st, &events),
        r#"{"hook_event_name":"PreInvocation"} => Working/Some(Hook)/High since=10 applied
{"hook_event_name":"PreToolUse","tool_name":"ask_question"} => Waiting/Some(Hook)/High since=20 applied
{"hook_event_name":"PostToolUse","tool_name":"ask_question"} => Working/Some(Hook)/High since=30 applied
{"hook_event_name":"PreToolUse","tool_name":"ask_permission"} => Waiting/Some(Hook)/High since=40 applied
{"hook_event_name":"PostToolUse","tool_name":"ask_permission"} => Working/Some(Hook)/High since=50 applied
{"hook_event_name":"PostInvocation"} => Done/Some(Hook)/High since=60 applied
"#
    );
    assert_eq!(st.get(S1).agent, "agy");
}

/// 프로세스 종료는 hook 상태를 이기고, 그 전에 시작한 hook 이벤트는 더 바꾸지 못한다.
#[test]
fn process_exit_wins_over_older_hooks() {
    let st = Statuses::open(None);
    assert_eq!(st.get(S1), AgentStatus::UNKNOWN);
    st.hook(&[(
        S1,
        &event(1, 10, "claude", r#"{"hook_event_name":"UserPromptSubmit"}"#),
    )])
    .unwrap();
    st.exited(S1);
    let s = st.get(S1);
    assert_eq!(
        (s.state, s.source),
        (AgentState::Exited, Some(Source::Process))
    );
    let late = event(2, 20, "claude", r#"{"hook_event_name":"Stop"}"#);
    assert_eq!(st.hook(&[(S1, &late)]).unwrap(), 0);
    assert_eq!(st.get(S1).state, AgentState::Exited);
    st.remove(S1);
    assert_eq!(st.get(S1), AgentStatus::UNKNOWN);
}

/// krilld 재기동: 저장본에서 상태와 중복 제거 기준이 이어진다. 사라진 세션은 버린다.
#[test]
fn survives_reopen_and_dedups_after_it() {
    let dir = tmp("reopen");
    let path = dir.join(FILE_NAME);
    let stop = event(7, 70, "claude", r#"{"hook_event_name":"Stop"}"#);
    {
        let st = Statuses::open(Some(path.clone()));
        st.hook(&[(S1, &stop)]).unwrap();
        st.hook(&[(
            SessionId(2),
            &event(8, 80, "codex", r#"{"hook_event_name":"UserPromptSubmit"}"#),
        )])
        .unwrap();
    }
    let st = Statuses::open(Some(path.clone()));
    let before = st.get(S1);
    assert_eq!(before.state, AgentState::Done);
    assert_eq!(
        st.hook(&[(S1, &stop)]).unwrap(),
        0,
        "duplicate after restart"
    );
    st.retain(&[S1]);
    assert_eq!(st.get(SessionId(2)), AgentStatus::UNKNOWN);
    assert_eq!(Statuses::open(Some(path)).get(S1), before);
    fs::remove_dir_all(&dir).ok();
}

/// 저장 형식 고정 바이트: 바뀌면 이전 krilld의 저장본을 못 읽는다(버전을 올려야 한다).
#[test]
fn file_fixture_v1() {
    let st = Statuses::open(None);
    st.hook(&[(
        S1,
        &event(5, 1000, "claude", r#"{"hook_event_name":"Stop"}"#),
    )])
    .unwrap();
    let bytes = encode(&lock(&st.map)).unwrap();
    let hex: String = bytes.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    });
    assert_eq!(hex, "0101010301000206636c61756465e8070105e807e807");
    assert_eq!(decode(&bytes).unwrap(), *lock(&st.map));
    // 다른 버전·손상은 빈 저장소.
    assert!(decode(&[2, 0]).is_none());
    assert!(decode(&bytes[..bytes.len() - 1]).is_none());
    assert!(decode(&[]).is_none());
}

/// spool 수거: 시각 순으로 넘기고 비운다. 잘린 꼬리 앞까지는 읽는다. 없는 파일은 아무것도 안 한다.
#[test]
fn take_spool_sorts_and_truncates() {
    let dir = tmp("spool");
    let path = dir.join(hook::SPOOL_FILE);
    take_spool(&path, |_| panic!("no spool file")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    for e in [
        event(2, 20, "claude", r#"{"hook_event_name":"Stop"}"#),
        event(1, 10, "claude", r#"{"hook_event_name":"SessionStart"}"#),
    ] {
        append_spool(&path, &e.encode().unwrap(), deadline).unwrap();
    }
    append_spool(&path, &[2, 0, 0, 0, hook::kind::EVENT, 0xff], deadline).unwrap();
    let stop = event(4, 40, "claude", r#"{"hook_event_name":"Stop"}"#);
    append_spool(&path, &stop.encode().unwrap(), deadline).unwrap();
    let mut torn = event(3, 30, "claude", "{}").encode().unwrap();
    torn.pop();
    append_spool(&path, &torn, deadline).unwrap();
    let mut got = Vec::new();
    take_spool(&path, |v| {
        got = v;
        Ok(())
    })
    .unwrap();
    assert_eq!(
        got.iter().map(|e| e.id.unix_ns).collect::<Vec<_>>(),
        [10, 20, 40]
    );
    assert_eq!(fs::metadata(&path).unwrap().len(), 0);
    take_spool(&path, |_| panic!("empty spool")).unwrap();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn equal_timestamp_ids_are_deduplicated_after_reopen() {
    let dir = tmp("same-time");
    let path = dir.join(FILE_NAME);
    let start = event(1, 10, "claude", r#"{"hook_event_name":"SessionStart"}"#);
    let prompt = event(2, 10, "claude", r#"{"hook_event_name":"UserPromptSubmit"}"#);
    let st = Statuses::open(Some(path.clone()));
    assert_eq!(st.hook(&[(S1, &start), (S1, &prompt)]).unwrap(), 2);
    let st = Statuses::open(Some(path));
    assert_eq!(st.hook(&[(S1, &start), (S1, &prompt)]).unwrap(), 0);
    assert_eq!(st.get(S1).state, AgentState::Working);
    fs::remove_dir_all(dir).unwrap();
}

/// 저장 실패 후에는 spool도 중복 제거 기준도 움직이지 않아 다음 수거가 이벤트를 반영한다.
#[test]
fn failed_save_preserves_spool_for_retry() {
    let dir = tmp("save-failure");
    let path = dir.join(FILE_NAME);
    let spool = dir.join(hook::SPOOL_FILE);
    let e = event(1, 10, "claude", r#"{"hook_event_name":"SessionStart"}"#);
    let frame = e.encode().unwrap();
    append_spool(&spool, &frame, Instant::now() + Duration::from_secs(1)).unwrap();
    let st = Statuses::open(Some(path.clone()));
    // 임시 파일 자리를 디렉터리로 막아 쓰기 실패를 결정적으로 만든다.
    let blocked = with_suffix(&path, ".tmp");
    fs::create_dir(&blocked).unwrap();
    let collect = |events: Vec<HookEvent>| {
        let found: Vec<_> = events.iter().map(|e| (S1, e)).collect();
        st.hook(&found).map(|_| ())
    };
    assert!(take_spool(&spool, collect).is_err());
    assert_eq!(fs::read(&spool).unwrap(), frame);
    assert_eq!(st.get(S1), AgentStatus::UNKNOWN);
    fs::remove_dir(blocked).unwrap();
    take_spool(&spool, collect).unwrap();
    assert_eq!(fs::metadata(&spool).unwrap().len(), 0);
    let saved = Statuses::open(Some(path)).get(S1);
    assert_eq!(
        (saved.state, saved.source),
        (AgentState::Done, Some(Source::Hook))
    );
    fs::remove_dir_all(dir).unwrap();
}

/// 저장은 끝났지만 spool 비우기 전에 재기동하면 같은 이벤트를 읽어도 상태가 되돌아가지 않는다.
#[test]
fn restart_before_spool_truncation_replays_once() {
    let dir = tmp("before-truncation");
    let path = dir.join(FILE_NAME);
    let spool = dir.join(hook::SPOOL_FILE);
    let e = event(1, 10, "claude", r#"{"hook_event_name":"Stop"}"#);
    append_spool(
        &spool,
        &e.encode().unwrap(),
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    let st = Statuses::open(Some(path.clone()));
    assert!(
        take_spool(&spool, |events| {
            assert_eq!(st.hook(&[(S1, &events[0])]).unwrap(), 1);
            Err(io::Error::other("injected crash before truncation"))
        })
        .is_err()
    );
    let st = Statuses::open(Some(path));
    take_spool(&spool, |events| {
        assert_eq!(st.hook(&[(S1, &events[0])]).unwrap(), 0);
        Ok(())
    })
    .unwrap();
    assert_eq!(st.get(S1).state, AgentState::Done);
    assert_eq!(fs::metadata(&spool).unwrap().len(), 0);
    fs::remove_dir_all(dir).unwrap();
}

proptest::proptest! {
    #[test]
    fn malformed_state_and_payload_never_panic(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..4096)) {
        let _ = decode(&bytes);
        let _ = normalize(&bytes);
        let _ = stop_failure_rate_limited(&bytes);
        let _ = agy_rate_limited(&bytes);
    }
}

/// OWE-710: 문서·바이너리 실측의 대표 페이로드는 한도로 감지하고, 한도여도 상태는 `Done`이다.
#[test]
fn stop_failure_rate_limit_detects_documented_payload() {
    use krill_proto::control::AgentState;
    let rate = br#"{"hook_event_name":"StopFailure","error":"rate_limit","error_details":"429 Too Many Requests","last_assistant_message":"API Error: Rate limit reached"}"#;
    assert!(stop_failure_rate_limited(rate));
    assert_eq!(normalize(rate), Some(AgentState::Done));
    for p in [
        r#"{"hook_event_name":"StopFailure","error":"server_error"}"#,
        r#"{"hook_event_name":"StopFailure","error":"overloaded"}"#,
        r#"{"hook_event_name":"StopFailure","error":"unknown"}"#,
        r#"{"hook_event_name":"StopFailure"}"#,
        r#"{"hook_event_name":"Stop","error":"rate_limit"}"#,
        "not json",
    ] {
        assert!(!stop_failure_rate_limited(p.as_bytes()), "{p}");
    }
}

/// OWE-710용 임시 홈: Claude 계정 High·Low + 사용량 캐시, 활성은 High.
fn rate_limit_home(tag: &str, high_five: u8, low_five: u8) -> (PathBuf, PathBuf) {
    use crate::usage::CLAUDE_CACHE;
    use krill_proto::usage::{AutoSwitchPolicy, ProviderKind};
    let root = std::env::temp_dir().join(format!("krilld-stop-fail-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    accounts::set_auto_switch(
        &home,
        Some(&state),
        &AutoSwitchPolicy {
            enabled: true,
            ..AutoSwitchPolicy::default()
        },
    )
    .unwrap();
    for (label, five) in [("High", high_five), ("Low", low_five)] {
        let profile = home.join(format!("claude-{label}"));
        accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Claude,
            label,
            None,
            &profile.display().to_string(),
        )
        .unwrap();
        fs::write(
            profile.join(CLAUDE_CACHE),
            format!(r#"{{"five_hour":{{"utilization":{five}}}}}"#),
        )
        .unwrap();
    }
    accounts::switch_account(&home, Some(&state), ProviderKind::Claude, "claude-high").unwrap();
    (home, state)
}

fn active_claude(home: &Path, state: &Path) -> Option<String> {
    accounts::load_file(home, Some(state))
        .active
        .get("claude")
        .cloned()
}

const RATE_PAYLOAD: &str = r#"{"hook_event_name":"StopFailure","error":"rate_limit"}"#;

#[test]
fn stop_failure_respects_disabled_rate_limit_switch_with_threshold_cache() {
    let (home, state) = rate_limit_home("rate-disabled", 95, 10);
    let mut file = accounts::load_file(&home, Some(&state));
    file.auto_switch.switch_on_rate_limit = false;
    accounts::save_file(&home, Some(&state), &file).unwrap();
    let switched =
        auto_switch_on_rate_limit(&home, Some(&state), "claude", RATE_PAYLOAD.as_bytes()).unwrap();
    assert!(switched.is_empty());
    assert_eq!(active_claude(&home, &state).as_deref(), Some("claude-high"));
    fs::remove_dir_all(home.parent().unwrap()).ok();
}

/// OWE-710: 한도 `StopFailure`가 오면 즉시 자동전환을 평가해 여유 계정으로 전환한다(다음 실행부터 적용).
#[test]
fn rate_limit_stop_failure_triggers_immediate_auto_switch() {
    let (home, state) = rate_limit_home("switch", 95, 10);
    let switched =
        auto_switch_on_rate_limit(&home, Some(&state), "claude", RATE_PAYLOAD.as_bytes())
            .expect("claude rate_limit evaluates");
    assert_eq!(switched.len(), 1);
    assert_eq!(switched[0].to_id, "claude-low");
    assert_eq!(active_claude(&home, &state).as_deref(), Some("claude-low"));
    // 전환 뒤에는 같은 페이로드가 와도 왕복하지 않는다.
    let again = auto_switch_on_rate_limit(&home, Some(&state), "claude", RATE_PAYLOAD.as_bytes())
        .expect("claude rate_limit evaluates");
    assert!(again.is_empty());
    fs::remove_dir_all(home.parent().unwrap()).ok();
}

/// OWE-710: 한도 외 오류·Claude 외 에이전트·비JSON은 평가하지 않는다(활성 유지).
#[test]
fn non_rate_limit_stop_failure_does_not_trigger() {
    let (home, state) = rate_limit_home("ignore", 95, 10);
    for (agent, payload) in [
        (
            "claude",
            r#"{"hook_event_name":"StopFailure","error":"server_error"}"#,
        ),
        ("claude", r#"{"hook_event_name":"StopFailure"}"#),
        ("claude", r#"{"hook_event_name":"Stop"}"#),
        ("claude", "not json"),
        ("codex", RATE_PAYLOAD),
        ("cursor", RATE_PAYLOAD),
        ("", RATE_PAYLOAD),
        // OWE-901: 에이전트·봉투가 어긋나면 평가하지 않는다.
        ("agy", RATE_PAYLOAD),
        (
            "claude",
            r#"{"hook_event_name":"Stop","error":"rate_limit"}"#,
        ),
    ] {
        assert!(
            auto_switch_on_rate_limit(&home, Some(&state), agent, payload.as_bytes()).is_none(),
            "{agent}/{payload}"
        );
    }
    assert_eq!(active_claude(&home, &state).as_deref(), Some("claude-high"));
    fs::remove_dir_all(home.parent().unwrap()).ok();
}

/// OWE-901용 임시 홈: agy 계정 High·Low + 사용량 파일, 활성은 High.
fn agy_rate_limit_home(tag: &str, high_five: u8, low_five: u8) -> (PathBuf, PathBuf) {
    use krill_proto::usage::{AutoSwitchPolicy, ProviderKind};
    let root = std::env::temp_dir().join(format!("krilld-agy-fail-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    accounts::set_auto_switch(
        &home,
        Some(&state),
        &AutoSwitchPolicy {
            enabled: true,
            ..AutoSwitchPolicy::default()
        },
    )
    .unwrap();
    for (label, five) in [("High", high_five), ("Low", low_five)] {
        let profile = home.join(format!("agy-{label}"));
        accounts::add_account(
            &home,
            Some(&state),
            ProviderKind::Antigravity,
            label,
            None,
            &profile.display().to_string(),
        )
        .unwrap();
        fs::write(
            profile.join("usage.json"),
            format!(r#"{{"five_hour":{{"utilization":{five}}}}}"#),
        )
        .unwrap();
    }
    accounts::switch_account(&home, Some(&state), ProviderKind::Antigravity, "agy-high").unwrap();
    (home, state)
}

fn active_agy(home: &Path, state: &Path) -> Option<String> {
    accounts::load_file(home, Some(state))
        .active
        .get("agy")
        .cloned()
}

/// OWE-901: agy 한도 봉투(`Stop`·`PostInvocation` + `error: "rate_limit"`)만 감지한다.
#[test]
fn agy_rate_limit_detects_only_exact_limit_envelope() {
    for p in [
        r#"{"hook_event_name":"Stop","error":"rate_limit"}"#,
        r#"{"hook_event_name":"PostInvocation","error":"rate_limit"}"#,
        r#"{"hookEventName":"Stop","error":"rate_limit"}"#,
    ] {
        assert!(agy_rate_limited(p.as_bytes()), "{p}");
    }
    for p in [
        r#"{"hook_event_name":"StopFailure","error":"rate_limit"}"#,
        r#"{"hook_event_name":"Stop","error":"server_error"}"#,
        r#"{"hook_event_name":"Stop"}"#,
        r#"{"hook_event_name":"PreInvocation","error":"rate_limit"}"#,
        r#"{"hook_event_name":"Stop","error":"Rate_Limit"}"#,
        "not json",
    ] {
        assert!(!agy_rate_limited(p.as_bytes()), "{p}");
    }
    // Claude 감지기와 겹치지 않는다.
    assert!(!stop_failure_rate_limited(
        br#"{"hook_event_name":"Stop","error":"rate_limit"}"#
    ));
}

/// OWE-901: agy 한도 봉투가 오면 즉시 자동전환을 평가해 여유 계정으로 전환한다.
#[test]
fn agy_limit_envelope_triggers_immediate_auto_switch() {
    let (home, state) = agy_rate_limit_home("switch", 100, 10);
    let payload = r#"{"hook_event_name":"Stop","error":"rate_limit"}"#;
    let switched = auto_switch_on_rate_limit(&home, Some(&state), "agy", payload.as_bytes())
        .expect("agy rate_limit evaluates");
    assert_eq!(switched.len(), 1);
    assert_eq!(switched[0].to_id, "agy-low");
    assert_eq!(active_agy(&home, &state).as_deref(), Some("agy-low"));
    fs::remove_dir_all(home.parent().unwrap()).ok();
}
/// OWE-710: 기존 정책 존중 — 자동전환 꺼짐·히스테리시스 근소 우위면 평가만 하고 전환하지 않는다.
#[test]
fn rate_limit_trigger_respects_existing_policy() {
    use krill_proto::usage::AutoSwitchPolicy;
    // 꺼짐.
    let (home, state) = rate_limit_home("off", 95, 10);
    accounts::set_auto_switch(
        &home,
        Some(&state),
        &AutoSwitchPolicy {
            enabled: false,
            threshold_percent: 90,
            switch_on_rate_limit: true,
            rules: Vec::new(),
        },
    )
    .unwrap();
    // 수동 선택 복원(set_auto_switch 끝의 평가는 꺼져 있어 덮지 않는다).
    accounts::switch_account(
        &home,
        Some(&state),
        krill_proto::usage::ProviderKind::Claude,
        "claude-high",
    )
    .unwrap();
    let evaluated =
        auto_switch_on_rate_limit(&home, Some(&state), "claude", RATE_PAYLOAD.as_bytes())
            .expect("claude rate_limit evaluates");
    assert!(evaluated.is_empty());
    assert_eq!(active_claude(&home, &state).as_deref(), Some("claude-high"));
    fs::remove_dir_all(home.parent().unwrap()).ok();
    // 근소 우위(5%p 미만): 왕복 방지 히스테리시스가 전환을 막는다.
    let (home, state) = rate_limit_home("close", 95, 93);
    let evaluated =
        auto_switch_on_rate_limit(&home, Some(&state), "claude", RATE_PAYLOAD.as_bytes())
            .expect("claude rate_limit evaluates");
    assert!(evaluated.is_empty());
    assert_eq!(active_claude(&home, &state).as_deref(), Some("claude-high"));
    fs::remove_dir_all(home.parent().unwrap()).ok();
}

/// OWE-872: 전경 프로세스 그룹이 루트 셸과 같거나 종료된 세션이면 `None`,
/// 다른 pgrp에서 실행 중인 명령이면 `process_name`으로 이름을 채운다.
#[test]
fn foreground_process_distinguishes_root_shell_and_child_command() {
    assert_eq!(session_foreground(std::process::id(), None, true), None);
    assert_eq!(
        foreground_process(100, Some(100), false, |_| panic!("root shell skips lookup")),
        None
    );
    assert_eq!(
        foreground_process(100, None, false, |_| unreachable!()),
        None
    );
    assert_eq!(
        foreground_process(100, Some(0), false, |_| unreachable!()),
        None
    );
    assert_eq!(
        foreground_process(100, Some(200), true, |_| Some("npm".into())),
        None
    );
    assert_eq!(
        foreground_process(100, Some(200), false, |pid| {
            assert_eq!(pid, 200);
            Some("npm\n".into())
        }),
        Some("npm".into())
    );
    assert_eq!(
        foreground_process(100, Some(200), false, |_| Some("   ".into())),
        None
    );
    assert_eq!(foreground_process(100, Some(200), false, |_| None), None);

    // 현재 프로세스 PID를 자식 pgrp처럼 조회해 OS process_name 연동도 확인한다.
    let me = std::process::id();
    let resolved = foreground_process(me.wrapping_add(1), Some(me), false, |pid| {
        crate::sys::process_name(pid).ok()
    });
    assert!(resolved.is_some_and(|name| !name.is_empty()));
}

#[test]
fn status_reads_continue_while_a_durable_writer_is_serialized() {
    let statuses = std::sync::Arc::new(Statuses::open(None));
    statuses
        .hook(&[(
            S1,
            &event(1, 10, "codex", r#"{"hook_event_name":"UserPromptSubmit"}"#),
        )])
        .unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let writer = lock(&statuses.commit);
    let reader = std::sync::Arc::clone(&statuses);
    let thread = std::thread::spawn(move || sent.send(reader.get(S1)).unwrap());
    let observed = received.recv_timeout(Duration::from_secs(1));
    drop(writer);
    thread.join().unwrap();
    assert_eq!(observed.unwrap().state, AgentState::Working);
}

/// 네이티브 설치 Claude Code(`…/claude/versions/<버전>`)는 짧은 이름이 버전 번호라도 `claude`다.
#[cfg(unix)]
#[test]
fn process_name_maps_versioned_claude_binary() {
    let root = std::env::temp_dir().join(format!("krill-claude-ver-{}", std::process::id()));
    // 경로가 다르면 버전 같은 이름도 그대로 둔다.
    for (dir, want) in [("claude/versions", "claude"), ("other/versions", "2.1.309")] {
        let dir = root.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("2.1.309");
        std::fs::copy("/bin/sleep", &exe).unwrap();
        let mut child = std::process::Command::new(&exe).arg("5").spawn().unwrap();
        let name = crate::sys::process_name(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(name.unwrap(), want);
    }
    std::fs::remove_dir_all(&root).unwrap();
}
