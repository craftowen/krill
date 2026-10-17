use super::*;
use std::os::unix::fs::PermissionsExt as _;

use krill_proto::agent::rules::{PUBLIC_KEY_FILE_NAME, RULES_FILE_NAME};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

/// 테스트용 서명 키와 서명 규칙 파일 바이트. 외부 서버 호출 없이 로컬에서 만든다.
fn fresh_key() -> Ed25519KeyPair {
    Ed25519KeyPair::from_pkcs8(
        Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .unwrap()
            .as_ref(),
    )
    .unwrap()
}
fn sign_bytes(key: &Ed25519KeyPair, payload: &str) -> (String, Vec<u8>) {
    use std::fmt::Write as _;
    let public: [u8; 32] = <[u8; 32]>::try_from(key.public_key().as_ref()).unwrap();
    let signature: [u8; 64] = <[u8; 64]>::try_from(key.sign(payload.as_bytes()).as_ref()).unwrap();
    let hex: String = public.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    });
    let bytes = krill_proto::agent::rules::pack(payload.as_bytes(), &signature).unwrap();
    (hex, bytes)
}

fn signed_fixture(payload: &str) -> (String, Vec<u8>) {
    sign_bytes(&fresh_key(), payload)
}

fn write_signed(dir: &std::path::Path, payload: &str) -> String {
    let (hex, bytes) = signed_fixture(payload);
    fs::write(dir.join(PUBLIC_KEY_FILE_NAME), format!("{hex}\n")).unwrap();
    fs::write(dir.join(RULES_FILE_NAME), &bytes).unwrap();
    hex
}

const SIGNED_PAYLOAD: &str = r#"
version = 1
permission = "yolo"

[screen]
blocked_phrases = ["trust this folder", "새 승인 문구"]

[agent.aider]
name = "Aider"
launch = ["aider"]
"#;

/// 서명 규칙이 재시작 없이 반영되고, 서명 파일의 전역 권한은 무시된다.
#[test]
fn signed_rules_apply_without_restart_and_ignore_permission() {
    let dir = std::env::temp_dir().join(format!("mk091-signed-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let c = Control::new(Some(&dir));
    assert!(!c.agents().unwrap().profiles.contains_key("aider"));

    write_signed(&dir, SIGNED_PAYLOAD);
    let agents = c.agents().unwrap();
    assert_eq!(agents.profiles["aider"].launch, ["aider"]);
    assert_eq!(agents.permission, Permission::Manual);
    assert!(
        c.screen_rules()
            .blocked_phrases
            .contains(&"새 승인 문구".to_owned())
    );

    // 같은 Control(재시작 없음)에서 서명 파일을 교체하면 다음 조회부터 바뀐다.
    let key = fresh_key();
    let (hex, first) = sign_bytes(&key, SIGNED_PAYLOAD);
    fs::write(dir.join(PUBLIC_KEY_FILE_NAME), format!("{hex}\n")).unwrap();
    fs::write(dir.join(RULES_FILE_NAME), &first).unwrap();
    assert!(c.agents().unwrap().profiles.contains_key("aider"));
    let rotated = SIGNED_PAYLOAD.replace("[agent.aider]", "[agent.pear]");
    let (_, second) = sign_bytes(&key, &rotated);
    fs::write(dir.join(RULES_FILE_NAME), &second).unwrap();
    let agents = c.agents().unwrap();
    assert!(!agents.profiles.contains_key("aider"));
    assert!(agents.profiles.contains_key("pear"));
    fs::remove_dir_all(&dir).unwrap();
}

/// 서명이 틀리거나 파일이 손상됐으면 거부하고 내장 규칙을 유지한다.
#[test]
fn broken_signature_keeps_builtin_rules() {
    let dir = std::env::temp_dir().join(format!("mk091-reject-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let (hex, mut bytes) = signed_fixture(SIGNED_PAYLOAD);
    fs::write(dir.join(PUBLIC_KEY_FILE_NAME), format!("{hex}\n")).unwrap();
    fs::write(dir.join(RULES_FILE_NAME), &bytes).unwrap();
    let c = Control::new(Some(&dir));
    assert!(c.agents().unwrap().profiles.contains_key("aider"));

    // 페이로드 1바이트를 뒤집으면 서명이 틀려 내장으로 돌아간다.
    let last_payload = bytes.len() - 65;
    bytes[last_payload] ^= 0x01;
    fs::write(dir.join(RULES_FILE_NAME), &bytes).unwrap();
    let agents = c.agents().unwrap();
    assert!(!agents.profiles.contains_key("aider"));
    assert_eq!(agents.permission, Permission::Manual);
    assert_eq!(c.screen_rules(), ScreenRules::default());

    // 잘린 파일도 마찬가지다.
    fs::write(dir.join(RULES_FILE_NAME), &bytes[..bytes.len() - 10]).unwrap();
    assert!(!c.agents().unwrap().profiles.contains_key("aider"));
    fs::remove_dir_all(&dir).unwrap();
}

/// 같은 id는 사용자 파일이 서명 규칙보다 우선한다.
#[test]
fn user_file_wins_over_signed_rules() {
    let dir = std::env::temp_dir().join(format!("mk091-user-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(FILE_NAME),
        "[agent.aider]\nname = 'Mine'\nlaunch = ['mine']\n",
    )
    .unwrap();
    write_signed(&dir, SIGNED_PAYLOAD);
    let c = Control::new(Some(&dir));
    assert_eq!(c.agents().unwrap().profiles["aider"].launch, ["mine"]);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn permission_and_launch_survive_restart_without_changing_profiles() {
    let dir = std::env::temp_dir().join(format!("mk488-settings-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(FILE_NAME);
    fs::write(&path, "extra = 'kept'\n[agent.fixture]\nname = 'fixture'\nlaunch = ['/bin/echo', 'hi']\nyolo_args = ['--yes']\n").unwrap();
    let c = Control::new(Some(&dir));
    assert_eq!(c.settings(None).unwrap().permission, Permission::Manual);
    assert_eq!(
        c.settings(Some(Permission::Yolo)).unwrap().permission,
        Permission::Yolo
    );
    let launch = AgentLaunch {
        profile: "fixture".into(),
        permission: Permission::Yolo,
    };
    c.remember(SessionId(1), &launch).unwrap();
    c.settings(Some(Permission::Manual)).unwrap();
    let restarted = Control::new(Some(&dir));
    assert_eq!(
        restarted.settings(None).unwrap().permission,
        Permission::Manual
    );
    assert_eq!(restarted.launch(SessionId(1)).unwrap(), Some(launch));
    assert_eq!(
        restarted.agents().unwrap().profiles["fixture"].launch,
        ["/bin/echo", "hi"]
    );
    assert!(fs::read_to_string(&path).unwrap().contains("kept"));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(c.launch_path(SessionId(1)).unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    c.forget(SessionId(1));
    assert_eq!(c.launch(SessionId(1)).unwrap(), None);
    fs::write(&path, "broken = [").unwrap();
    assert!(c.settings(Some(Permission::Yolo)).is_err());
    assert!(c.agents().is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "broken = [");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stored_session_id_and_hook_payload_persist_across_restart() {
    let dir = std::env::temp_dir().join(format!("mk052-stored-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let c = Control::new(Some(&dir));
    let launch = AgentLaunch {
        profile: "codex".into(),
        permission: Permission::Yolo,
    };
    c.remember_session(SessionId(3), &launch, None, b"/tmp/work".to_vec(), None)
        .unwrap();
    assert!(c.has(SessionId(3)));
    c.record_hook_session(
        SessionId(3),
        br#"{"hook_event_name":"SessionStart","session_id":"0197-codex-sid"}"#,
    );
    let restarted = Control::new(Some(&dir));
    assert!(restarted.has(SessionId(3)));
    let stored = restarted.stored(SessionId(3)).unwrap().unwrap();
    assert_eq!(stored.profile, "codex");
    assert_eq!(stored.permission, Permission::Yolo);
    assert_eq!(stored.session_id.as_deref(), Some("0197-codex-sid"));
    assert_eq!(stored.cwd, b"/tmp/work");
    restarted.forget(SessionId(3));
    assert!(!restarted.has(SessionId(3)));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hook_session_id_rejects_options_and_non_identifier_bytes() {
    let dir = std::env::temp_dir().join(format!("mk595-hook-id-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let c = Control::new(Some(&dir));
    let launch = AgentLaunch {
        profile: "codex".into(),
        permission: Permission::Manual,
    };
    c.remember_session(
        SessionId(1),
        &launch,
        Some("valid_ID-9".into()),
        vec![],
        None,
    )
    .unwrap();
    for id in [
        "-option", "--help", "a b", "a/b", "a.b", "a\n", "한글", "", "a=1", "a\0b",
    ] {
        let payload = serde_json::to_vec(&serde_json::json!({"session_id": id})).unwrap();
        c.record_hook_session(SessionId(1), &payload);
        assert_eq!(
            c.stored(SessionId(1))
                .unwrap()
                .unwrap()
                .session_id
                .as_deref(),
            Some("valid_ID-9"),
            "{id:?}"
        );
    }
    c.record_hook_session(SessionId(1), br#"{"session_id":"_new-ID_123"}"#);
    assert_eq!(
        c.stored(SessionId(1))
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("_new-ID_123")
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn roster_use_and_unknown_old_metadata_permanently_veto_hibernation() {
    let dir = std::env::temp_dir().join(format!("mk081-roster-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(dir.join("agent-launches")).unwrap();
    let control = Control::new(Some(&dir));
    let id = SessionId(81);
    fs::write(
        control.launch_path(id).unwrap(),
        br#"{"profile":"codex","permission":"manual","session_id":"sid"}"#,
    )
    .unwrap();
    assert!(control.stored(id).unwrap().unwrap().hibernation_blocked);
    for payload in [
        br#"{"hook_event_name":"PreToolUse","tool_name":"spawn_agent"}"#.as_slice(),
        br#"{"hook_event_name":"PreToolUse","tool_name":"Agent"}"#,
        br#"{"hook_event_name":"SubagentStart","agent_id":"child"}"#,
        b"malformed",
    ] {
        control
            .remember_session(
                id,
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Manual,
                },
                Some("sid".into()),
                vec![],
                None,
            )
            .unwrap();
        control.record_hook_session(id, payload);
        control.record_hook_session(id, br#"{"hook_event_name":"Stop","session_id":"sid"}"#);
        assert!(control.stored(id).unwrap().unwrap().hibernation_blocked);
        assert!(
            Control::new(Some(&dir))
                .stored(id)
                .unwrap()
                .unwrap()
                .hibernation_blocked
        );
        assert!(control.hibernation_blocked(id));
    }
    control.forget(id);
    fs::remove_dir_all(dir).unwrap();
}

/// OWE-698: 실행 계정이 hook 갱신·재시작을 지나도 남고, 계정 필드 없는 구 기록도 그대로 읽힌다.
#[test]
fn launch_account_persists_and_old_records_still_read() {
    let dir = std::env::temp_dir().join(format!("owe698-stored-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(dir.join("agent-launches")).unwrap();
    let c = Control::new(Some(&dir));
    let launch = AgentLaunch {
        profile: "claude".into(),
        permission: Permission::Manual,
    };
    let account = LaunchAccount {
        id: "claude-a".into(),
        env_key: "CLAUDE_CONFIG_DIR".into(),
        dir: "/profiles/a".into(),
    };
    c.remember_session(SessionId(4), &launch, None, vec![], Some(account.clone()))
        .unwrap();
    c.record_hook_session(
        SessionId(4),
        br#"{"hook_event_name":"SessionStart","session_id":"sid-698"}"#,
    );
    let stored = Control::new(Some(&dir))
        .stored(SessionId(4))
        .unwrap()
        .unwrap();
    assert_eq!(stored.session_id.as_deref(), Some("sid-698"));
    assert_eq!(stored.account, Some(account));

    fs::write(
        dir.join("agent-launches").join("5.json"),
        br#"{"profile":"claude","permission":"manual","session_id":"old-sid"}"#,
    )
    .unwrap();
    let old = c.stored(SessionId(5)).unwrap().unwrap();
    assert_eq!(old.session_id.as_deref(), Some("old-sid"));
    assert_eq!(old.account, None);
    fs::remove_dir_all(dir).unwrap();
}
