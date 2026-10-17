//! OWE-701: 버전 게이트·기록 판정·슬롯 편집 회귀 테스트.

use super::*;

const EXE: &str = "/opt/krill/krill";

fn payload(five: u8, seven: u8) -> serde_json::Value {
    serde_json::json!({
        "session_id": "s1",
        "rate_limits": {
            "five_hour": {"used_percentage": five, "resets_at": "2099-01-01T00:00:00Z"},
            "seven_day": {"used_percentage": seven, "resets_at": "2099-01-08T00:00:00Z"},
        },
    })
}

#[test]
fn version_gate_rejects_only_known_old_releases() {
    assert!(supports_status_line(None));
    assert!(supports_status_line(Some("")));
    assert!(supports_status_line(Some("unknown")));
    assert!(supports_status_line(Some("2.1.301 (Claude Code)")));
    assert!(supports_status_line(Some("2.1.80-beta")));
    assert!(supports_status_line(Some("1.0.64")));
    assert!(!supports_status_line(Some("1.0.63")));
    assert!(!supports_status_line(Some("1.0.23")));
    assert_eq!(parse_version(""), None);
}

#[test]
fn parse_version_reads_dotted_triples() {
    assert_eq!(parse_version("2.1.301 (Claude Code)"), Some((2, 1, 301)));
    assert_eq!(parse_version("1.0.64"), Some((1, 0, 64)));
    // `v` 접두사는 버전 토큰으로 보지 않는다(Orca 파서와 같은 기준).
    assert_eq!(parse_version("v2.1.80"), None);
    assert_eq!(parse_version("2.1"), None);
    assert_eq!(parse_version("1.2.3.4"), None);
    assert_eq!(parse_version("abc"), None);
}

#[test]
fn rate_limits_missing_or_not_object_is_ignored() {
    assert!(extract_rate_limits(&serde_json::json!({})).is_none());
    assert!(extract_rate_limits(&serde_json::json!({"rate_limits": 1})).is_none());
    let payload = payload(10, 20);
    assert_eq!(extract_rate_limits(&payload), payload.get("rate_limits"));
}

#[test]
fn same_value_within_dedup_window_skips_rewrite() {
    let rate = payload(42, 67).get("rate_limits").cloned().unwrap();
    assert!(should_record(None, &rate, 1_000));
    let fresh = render_cache(None, &rate, 1_000);
    // 같은 값 + 5초 후: 재기록 안 함.
    assert!(!should_record(Some(&fresh), &rate, 1_005));
    // 같은 값 + 14초 후: 아직 안 함.
    assert!(!should_record(Some(&fresh), &rate, 1_014));
    // 같은 값 + 15초 후: 경계에서 다시 쓴다.
    assert!(should_record(Some(&fresh), &rate, 1_015));
    // 값이 바뀌면 즉시 쓴다.
    let other = payload(43, 67).get("rate_limits").cloned().unwrap();
    assert!(should_record(Some(&fresh), &other, 1_005));
    // 깨진 파일·미래 cached_at은 다시 쓴다.
    assert!(should_record(Some("not json"), &rate, 1_005));
    let future = render_cache(None, &rate, 2_000);
    assert!(should_record(Some(&future), &rate, 1_005));
}

#[test]
fn render_cache_keeps_unknown_keys_and_records_cached_at() {
    let rate = payload(42, 67).get("rate_limits").cloned().unwrap();
    let text = render_cache(
        Some(r#"{"future": {"keep": true}, "rate_limits": {"old": 1}}"#),
        &rate,
        1_700_000_000,
    );
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value.get("future").and_then(|f| f.get("keep")),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(value.get("rate_limits"), Some(&rate));
    assert_eq!(
        value.get("cached_at").and_then(serde_json::Value::as_str),
        Some("1700000000")
    );
    // 데몬 읽기 경로 모양과 같다.
    assert_eq!(crate_read_five_hour(&text), Some(42));
}

/// 데몬 `parse_claude`와 같은 키로 읽는다(읽기 경로 호환 확인용 최소 재현).
fn crate_read_five_hour(text: &str) -> Option<u8> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let body = value.get("rate_limits").unwrap_or(&value);
    let window = ["five_hour", "5_hour"]
        .iter()
        .find_map(|key| body.get(*key))?;
    window
        .get("utilization")
        .or_else(|| window.get("used_percentage"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u8::try_from(n).ok())
}

#[test]
fn own_command_format_round_trips() {
    assert_eq!(
        classify_command(&install_command(EXE), EXE),
        OwnCommand::Bare
    );
    let user = "/usr/local/bin/my status 'line'.sh --flag";
    let chained = chain_command(EXE, user);
    assert_eq!(
        classify_command(&chained, EXE),
        OwnCommand::Chained(user.to_owned())
    );
    // 다른 바이너리 경로·미래 플래그는 구조가 아니면 사용자 소유로 보고 체인한다.
    // 깊이 가드가 재귀를 막는다.
    assert_eq!(
        classify_command("/other/krill statusline", EXE),
        OwnCommand::Foreign
    );
    assert_eq!(
        classify_command("/other/krill statusline --json", EXE),
        OwnCommand::Foreign
    );
    // 사용자 명령은 그대로 체인 대상이다.
    assert_eq!(
        classify_command("my-statusline.sh", EXE),
        OwnCommand::Foreign
    );
    // `echo krill statusline` 같은 우연한 언급은 사용자 소유다(구조 판별).
    assert_eq!(
        classify_command("echo krill statusline", EXE),
        OwnCommand::Foreign
    );
}

#[test]
fn slot_state_distinguishes_empty_managed_user() {
    assert_eq!(slot_state(None, EXE), SlotState::Empty);
    assert_eq!(
        slot_state(Some(&serde_json::json!({"padding": 1})), EXE),
        SlotState::Empty
    );
    assert_eq!(
        slot_state(
            Some(&serde_json::json!({"command": install_command(EXE)})),
            EXE
        ),
        SlotState::Managed
    );
    assert_eq!(
        slot_state(
            Some(&serde_json::json!({"command": chain_command(EXE, "user-cmd")})),
            EXE
        ),
        SlotState::Managed
    );
    assert_eq!(
        slot_state(Some(&serde_json::json!({"command": "user-cmd"})), EXE),
        SlotState::User
    );
}

#[test]
fn install_preserves_user_command_by_chaining_and_keeps_unknown_keys() {
    let before = r#"{"future": 7, "statusLine": {"command": "user-cmd", "padding": 2}}"#;
    let (after, state) = apply_install(Some(before), EXE).unwrap();
    assert_eq!(state, SlotState::User);
    let value: serde_json::Value = serde_json::from_str(&after).unwrap();
    assert_eq!(value.get("future"), Some(&serde_json::json!(7)));
    let slot = value.get("statusLine").unwrap();
    assert_eq!(
        slot.get("command").and_then(serde_json::Value::as_str),
        Some(chain_command(EXE, "user-cmd").as_str())
    );
    assert_eq!(slot.get("padding"), Some(&serde_json::json!(2)));
    assert_eq!(
        slot.get("type").and_then(serde_json::Value::as_str),
        Some("command")
    );
}

#[test]
fn install_into_empty_slot_sets_bare_command() {
    let (after, state) = apply_install(None, EXE).unwrap();
    assert_eq!(state, SlotState::Empty);
    let value: serde_json::Value = serde_json::from_str(&after).unwrap();
    assert_eq!(
        value
            .get("statusLine")
            .and_then(|slot| slot.get("command"))
            .and_then(serde_json::Value::as_str),
        Some(install_command(EXE).as_str())
    );
}

#[test]
fn install_leaves_managed_slot_byte_identical() {
    let before = format!(
        "{{\"statusLine\": {{\"type\": \"command\", \"command\": {}}}}}",
        serde_json::Value::String(chain_command(EXE, "user-cmd"))
    );
    let (after, state) = apply_install(Some(&before), EXE).unwrap();
    assert_eq!(state, SlotState::Managed);
    assert_eq!(after, before);
}

#[test]
fn install_rejects_broken_settings() {
    assert!(apply_install(Some("not json"), EXE).is_err());
    assert!(apply_install(Some("[1,2]"), EXE).is_err());
}

#[test]
fn uninstall_restores_before_and_removes_added_key() {
    // 빈 슬롯에 설치했다가 제거하면 파일 자체가 사라진다(설치 전 파일 없음).
    let (after, _) = apply_install(None, EXE).unwrap();
    assert!(
        apply_uninstall(Some(&after), None, &after, EXE)
            .unwrap()
            .is_none()
    );
    // 사용자 슬롯에 설치했다가 제거하면 원본이 돌아온다.
    let before = r#"{"statusLine": {"command": "user-cmd", "padding": 2}}"#;
    let (after, _) = apply_install(Some(before), EXE).unwrap();
    let restored = apply_uninstall(Some(&after), Some(before), &after, EXE)
        .unwrap()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&restored).unwrap();
    assert_eq!(
        value.get("statusLine").and_then(|slot| slot.get("command")),
        Some(&serde_json::Value::String("user-cmd".to_owned()))
    );
}

#[test]
fn uninstall_keeps_post_install_user_keys() {
    // 설치 전 파일이 없어도, 설치 후 사용자가 다른 키를 두면 파일은 두고 슬롯만 뺀다.
    let (after, _) = apply_install(None, EXE).unwrap();
    let mut current: serde_json::Value = serde_json::from_str(&after).unwrap();
    current["theme"] = serde_json::json!("dark");
    let current = serde_json::to_string_pretty(&current).unwrap() + "\n";
    let restored = apply_uninstall(Some(&current), None, &after, EXE)
        .unwrap()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&restored).unwrap();
    assert!(value.get("statusLine").is_none());
    assert_eq!(value.get("theme"), Some(&serde_json::json!("dark")));
}

#[test]
fn uninstall_preserves_user_owned_slot() {
    let before = r#"{"statusLine": {"command": "user-cmd"}}"#;
    let (after, _) = apply_install(Some(before), EXE).unwrap();
    // 사용자가 슬롯을 통째로 가져가면 보존하고 오류로 알린다.
    let tampered = serde_json::json!({"statusLine": {"command": "other-cmd"}}).to_string();
    assert!(apply_uninstall(Some(&tampered), Some(before), &after, EXE).is_err());
    // 사용자가 슬롯을 지웠으면 opt-out으로 보고 건드리지 않는다.
    assert!(
        apply_uninstall(Some("{}"), Some(before), &after, EXE)
            .unwrap()
            .is_none()
    );
}

// 임의 입력에 패닉하지 않는다(파서·슬롯 판정).
proptest::proptest! {
    #[test]
    fn arbitrary_inputs_do_not_panic(
        text in proptest::string::string_regex("[ -~]{0,128}").unwrap(),
        exe in proptest::string::string_regex("[ -~]{0,64}").unwrap(),
    ) {
        core::hint::black_box(parse_version(&text));
        core::hint::black_box(supports_status_line(Some(&text)));
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            core::hint::black_box(extract_rate_limits(&value));
            core::hint::black_box(slot_state(value.get("statusLine"), &exe));
            core::hint::black_box(classify_command(&text, &exe));
        }
    }
}
