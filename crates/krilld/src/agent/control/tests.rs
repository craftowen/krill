use super::*;
use std::os::unix::fs::PermissionsExt as _;

use krill_proto::agent::rules::{PUBLIC_KEY_FILE_NAME, RULES_FILE_NAME};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

#[test]
fn switch_identity_merge_keeps_late_hook_and_source_account_across_restart() {
    let dir = std::env::temp_dir().join(format!("switch-identity-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    let c = Control::new(Some(&dir));
    let session = SessionId(1);
    let mut discovered = StoredLaunch {
        profile: "codex".into(),
        permission: Permission::Manual,
        session_id: Some("exact".into()),
        transcript_path: Some("/tmp/exact.jsonl".into()),
        cwd: b"/project".to_vec(),
        hibernation_blocked: true,
        account: Some(LaunchAccount {
            id: "source".into(),
            env_key: "CODEX_HOME".into(),
            dir: "/source".into(),
        }),
    };
    let first = c.bind_switch_identity(session, &discovered).unwrap();
    assert_eq!(first, discovered);
    c.record_hook_session(session, br#"{"hook_event_name":"SessionStart","source":"clear","session_id":"rotated","transcript_path":"/tmp/rotated.jsonl"}"#);
    let current = c.stored(session).unwrap().unwrap();
    assert_eq!(current.session_id.as_deref(), Some("rotated"));
    assert!(c.bind_switch_identity(session, &discovered).is_err());
    assert_eq!(c.stored(session).unwrap().unwrap(), current);
    discovered.session_id = Some("rotated".into());
    let merged = c.bind_switch_identity(session, &discovered).unwrap();
    assert_eq!(
        merged.transcript_path.as_deref(),
        Some("/tmp/rotated.jsonl")
    );
    assert_eq!(
        Control::new(Some(&dir)).stored(session).unwrap().unwrap(),
        merged
    );
    let agy_session = SessionId(2);
    let db_path = Path::new("/tmp/profile/.gemini/antigravity-cli/conversations/conv-1.db");
    let jsonl_path = Path::new(
        "/tmp/profile/.gemini/antigravity-cli/brain/conv-1/.system_generated/logs/transcript.jsonl",
    );
    assert!(valid_transcript_path(db_path.to_str().unwrap()));
    assert!(!valid_transcript_path("/tmp/other/conv-1.db"));
    c.remember_session(
        agy_session,
        &AgentLaunch {
            profile: "agy".into(),
            permission: Permission::Yolo,
        },
        None,
        b"/project".to_vec(),
        None,
    )
    .unwrap();
    c.bind_discovered_session(agy_session, "conv-1", Some(db_path));
    assert_eq!(
        c.stored(agy_session)
            .unwrap()
            .unwrap()
            .transcript_path
            .as_deref(),
        db_path.to_str()
    );
    c.bind_discovered_session(agy_session, "conv-1", Some(jsonl_path));
    assert_eq!(
        c.stored(agy_session)
            .unwrap()
            .unwrap()
            .transcript_path
            .as_deref(),
        jsonl_path.to_str()
    );
    fs::remove_dir_all(dir).unwrap();
}

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
    fs::write(dir.join(FILE_NAME), "permission = 'manual'\n").unwrap();
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
    assert_eq!(agents.permission, Permission::Yolo);
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
    assert_eq!(c.settings(None).unwrap().permission, Permission::Yolo);
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
    // 이벤트 이름 없는 훅은 묶인 대화를 바꾸지 못하므로 정당한 회전(SessionStart clear)으로 바꾼다.
    c.record_hook_session(
        SessionId(1),
        br#"{"hook_event_name":"SessionStart","source":"clear","session_id":"_new-ID_123"}"#,
    );
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

/// OWE-929 / MK-233: 서브에이전트·보조(`claude -p`) 외래 훅은 부모 `session_id`를 덮어쓰지 않고
/// 외래 훅으로 판정하되, `/clear`·`resume`·`compact` 회전은 허용한다(#30067, #29429, #28966).
#[test]
fn subagent_and_foreign_hooks_do_not_clobber_parent_session_id() {
    let dir = std::env::temp_dir().join(format!("owe929-subagent-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let c = Control::new(Some(&dir));
    let launch = AgentLaunch {
        profile: "claude".into(),
        permission: Permission::Manual,
    };
    let id = SessionId(29);
    c.remember_session(id, &launch, None, vec![], None).unwrap();

    // 최초 부모 SessionStart는 session_id를 바인딩하고 외래 훅이 아니다.
    let parent_start =
        br#"{"hook_event_name":"SessionStart","source":"startup","session_id":"parent-sid-1"}"#;
    assert!(!c.is_subagent_or_foreign_hook(id, parent_start));
    assert!(!c.record_hook_session(id, parent_start));
    assert_eq!(
        c.stored(id).unwrap().unwrap().session_id.as_deref(),
        Some("parent-sid-1")
    );

    // 명시적 subagent/teammate 페이로드는 같은 session_id여도 외래(서브에이전트)로 판정한다.
    for sub in [
        br#"{"hook_event_name":"UserPromptSubmit","session_id":"parent-sid-1","agent_id":"sub-1","prompt":"sub task"}"#.as_slice(),
        br#"{"hook_event_name":"Stop","session_id":"parent-sid-1","subagent_id":"sub-1"}"#,
        br#"{"hook_event_name":"Stop","session_id":"child-sid","parent_session_id":"parent-sid-1"}"#,
        br#"{"hook_event_name":"UserPromptSubmit","session_id":"child-sid","is_subagent":true}"#,
    ] {
        assert!(c.is_subagent_or_foreign_hook(id, sub));
        assert!(c.record_hook_session(id, sub));
        assert_eq!(
            c.stored(id).unwrap().unwrap().session_id.as_deref(),
            Some("parent-sid-1")
        );
    }

    // 같은 터미널 안에서 실행된 보조 `claude -p` (다른 session_id, startup/Stop/UserPromptSubmit)도 부모를 덮어쓰지 않는다.
    for aux in [
        br#"{"hook_event_name":"SessionStart","source":"startup","session_id":"aux-sid-99"}"#.as_slice(),
        br#"{"hook_event_name":"UserPromptSubmit","session_id":"aux-sid-99","prompt":"commit msg"}"#,
        br#"{"hook_event_name":"Stop","session_id":"aux-sid-99"}"#,
    ] {
        assert!(c.is_subagent_or_foreign_hook(id, aux));
        assert!(c.record_hook_session(id, aux));
        assert_eq!(
            c.stored(id).unwrap().unwrap().session_id.as_deref(),
            Some("parent-sid-1")
        );
    }

    // 반면 `/clear`·`compact`·`resume`으로 인한 부모 세션 ID 회전은 정상 반영한다.
    let clear_rotate =
        br#"{"hook_event_name":"SessionStart","source":"clear","session_id":"parent-sid-2"}"#;
    assert!(!c.is_subagent_or_foreign_hook(id, clear_rotate));
    assert!(!c.record_hook_session(id, clear_rotate));
    assert_eq!(
        c.stored(id).unwrap().unwrap().session_id.as_deref(),
        Some("parent-sid-2")
    );
    fs::remove_dir_all(dir).unwrap();
}

/// OWE-944 / MK-241: 훅 payload의 `transcript_path`를 기록하고, 로그인 셸에서 직접 실행된 에이전트도
/// 첫 훅 수신 시 `StoredLaunch`에 자동 바인딩하되 외래/서브에이전트 훅은 부모 경로를 덮어쓰지 않는다.
#[test]
fn hook_transcript_path_and_login_shell_auto_binding_work() {
    let dir = std::env::temp_dir().join(format!("owe944-bind-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let c = Control::new(Some(&dir));

    // 1) 비정상(malformed) 페이로드가 바인딩 전 로그인 셸에 오더라도 StoredLaunch를 만들지 않는다.
    let login_id = SessionId(77);
    assert!(!c.has(login_id));
    c.record_hook_session_with_context(login_id, b"malformed", Some("claude"), None);
    assert!(!c.has(login_id));

    // 2) 로그인 셸(StoredLaunch 없음)에서 수동 실행된 claude가 첫 훅을 보내면 자동 바인딩된다.
    let payload = br#"{"hook_event_name":"SessionStart","session_id":"login-sid-1","transcript_path":"/Users/test/.claude/projects/demo/login-sid-1.jsonl","cwd":"/Users/test/demo"}"#;
    assert!(!c.record_hook_session_with_context(
        login_id,
        payload,
        Some("claude-code"),
        Some(b"/Users/test/fallback")
    ));
    let stored = c.stored(login_id).unwrap().unwrap();
    assert_eq!(stored.profile, "claude");
    assert_eq!(stored.session_id.as_deref(), Some("login-sid-1"));
    assert_eq!(
        stored.transcript_path.as_deref(),
        Some("/Users/test/.claude/projects/demo/login-sid-1.jsonl")
    );
    assert_eq!(stored.cwd, b"/Users/test/demo");
    assert!(stored.hibernation_blocked);
    assert!(c.hibernation_blocked(login_id));

    // 3) 외래 보조 훅이나 상대 경로·비허용 확장자·부모 디렉터리(`..`) 탈출 경로는 부모의 transcript_path를 덮어쓰지 않는다.
    let foreign = br#"{"hook_event_name":"UserPromptSubmit","session_id":"aux-sid-2","transcript_path":"/Users/test/.claude/projects/demo/aux.jsonl"}"#;
    assert!(c.record_hook_session_with_context(login_id, foreign, Some("claude"), None));
    assert_eq!(
        c.stored(login_id)
            .unwrap()
            .unwrap()
            .transcript_path
            .as_deref(),
        Some("/Users/test/.claude/projects/demo/login-sid-1.jsonl")
    );
    for bad_path in [
        "relative/path.jsonl",
        "/etc/passwd",
        "/Users/test/.claude/../escape.jsonl",
        "/Users/test/.claude/note.txt",
    ] {
        let bad = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "login-sid-1",
            "transcript_path": bad_path,
        }))
        .unwrap();
        c.record_hook_session_with_context(login_id, &bad, Some("claude"), None);
        assert_eq!(
            c.stored(login_id)
                .unwrap()
                .unwrap()
                .transcript_path
                .as_deref(),
            Some("/Users/test/.claude/projects/demo/login-sid-1.jsonl"),
            "{bad_path}"
        );
    }

    // 4) `/clear` 등으로 session_id가 합법 회전될 때 새 transcript_path가 없으면 이전 파일 경로를 비운다.
    let rotated =
        br#"{"hook_event_name":"SessionStart","source":"clear","session_id":"login-sid-2"}"#;
    assert!(!c.record_hook_session_with_context(login_id, rotated, Some("claude"), None));
    let after_rotate = c.stored(login_id).unwrap().unwrap();
    assert_eq!(after_rotate.session_id.as_deref(), Some("login-sid-2"));
    assert_eq!(after_rotate.transcript_path, None);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hooks_without_event_name_cannot_replace_bound_conversation_identity() {
    let dir = std::env::temp_dir().join(format!("account-bound-hook-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    let control = Control::new(Some(&dir));
    for (session, sid) in [
        (SessionId(1), "conversation-one"),
        (SessionId(2), "conversation-two"),
    ] {
        control
            .remember_session(
                session,
                &AgentLaunch {
                    profile: "codex".into(),
                    permission: Permission::Yolo,
                },
                Some(sid.into()),
                b"/same/project".to_vec(),
                None,
            )
            .unwrap();
        let foreign =
            br#"{"conversation_id":"unrelated","transcript_path":"/tmp/unrelated.jsonl"}"#;
        assert!(control.is_subagent_or_foreign_hook(session, foreign));
        assert!(control.record_hook_session(session, foreign));
        let stored = control.stored(session).unwrap().unwrap();
        assert_eq!(stored.session_id.as_deref(), Some(sid));
        assert_eq!(stored.transcript_path, None);
        let same = serde_json::to_vec(&serde_json::json!({"conversation_id": sid})).unwrap();
        assert!(!control.is_subagent_or_foreign_hook(session, &same));
        assert!(!control.record_hook_session(session, &same));
    }
    let reloaded = Control::new(Some(&dir));
    assert_eq!(
        reloaded
            .stored(SessionId(1))
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("conversation-one")
    );
    assert_eq!(
        reloaded
            .stored(SessionId(2))
            .unwrap()
            .unwrap()
            .session_id
            .as_deref(),
        Some("conversation-two")
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn account_resume_requires_an_observed_parent_hook_not_launch_metadata() {
    let dir = std::env::temp_dir().join(format!("kas-observed-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let control = Control::new(Some(&dir));
    let session = SessionId(17);
    control.watch_account_resume(session);
    control
        .remember_session(
            session,
            &AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Yolo,
            },
            Some("expected".into()),
            Vec::new(),
            None,
        )
        .unwrap();
    assert!(!control.account_resume_observed(session, "expected"));
    for payload in [
        r#"{"hook_event_name":"SessionStart","session_id":"other"}"#,
        r#"{"hook_event_name":"SessionStart","session_id":"expected","agent_id":"child"}"#,
        r#"{"hook_event_name":"SessionEnd","session_id":"expected"}"#,
        r#"{"session_id":"expected"}"#,
    ] {
        control.record_hook_session(session, payload.as_bytes());
        assert!(!control.account_resume_observed(session, "expected"));
    }
    control.record_hook_session(
        session,
        br#"{"hook_event_name":"SessionStart","session_id":"expected"}"#,
    );
    assert!(control.account_resume_observed(session, "expected"));
    for payload in [
        r#"{"hookEventName":"SessionStart","session_id":"expected"}"#,
        r#"{"event":"PreInvocation","conversation_id":"expected"}"#,
    ] {
        control.watch_account_resume(session);
        control.record_hook_session(session, payload.as_bytes());
        assert!(control.account_resume_observed(session, "expected"));
    }
    control.finish_account_resume(session);
    assert!(!control.account_resume_observed(session, "expected"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cached_signed_rules_fail_closed_after_public_key_removal() {
    let dir = std::env::temp_dir().join(format!("rules-cache-delete-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    write_signed(&dir, SIGNED_PAYLOAD);
    let control = Control::new(Some(&dir));
    assert!(control.agents().unwrap().profiles.contains_key("aider"));
    fs::remove_file(dir.join(PUBLIC_KEY_FILE_NAME)).unwrap();
    assert!(!control.agents().unwrap().profiles.contains_key("aider"));
    assert_eq!(control.screen_rules(), ScreenRules::default());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn another_providers_resume_hook_cannot_replace_a_bound_conversation() {
    let dir = std::env::temp_dir().join(format!("account-provider-hook-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let control = Control::new(Some(&dir));
    let session = SessionId(39374);
    control
        .remember_session(
            session,
            &AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Manual,
            },
            Some("codex-parent".into()),
            b"/parent".to_vec(),
            Some(LaunchAccount {
                id: "codex-original".into(),
                env_key: "CODEX_HOME".into(),
                dir: String::new(),
            }),
        )
        .unwrap();
    let before = control.stored(session).unwrap().unwrap();
    control.watch_account_resume(session);
    for id in ["claude-child", "codex-parent"] {
        let payload = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "SessionStart", "source": "resume",
            "session_id": id, "transcript_path": "/foreign/projects/demo/claude-child.jsonl"
        }))
        .unwrap();
        assert!(control.record_hook_session_with_context(
            session,
            &payload,
            Some("claude-code"),
            None
        ));
        assert_eq!(control.stored(session).unwrap().unwrap(), before);
        assert!(!control.account_resume_observed(session, id));
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn retiring_binding_rejects_hooks_until_retirement_finishes_or_is_cancelled() {
    let dir = std::env::temp_dir().join(format!("account-retiring-hook-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let control = Control::new(Some(&dir));
    let session = SessionId(1);
    control
        .remember_session(
            session,
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            Some("original".into()),
            Vec::new(),
            None,
        )
        .unwrap();
    let before = control.stored(session).unwrap().unwrap();
    let retiring = control.retire_account_binding(session);
    let payload = br#"{"hook_event_name":"SessionStart","source":"resume","session_id":"other"}"#;
    assert!(control.record_foreground_hook_session(session, payload, "codex", None));
    assert_eq!(control.stored(session).unwrap().unwrap(), before);
    drop(retiring);
    assert!(!control.record_foreground_hook_session(session, payload, "codex", None));
    assert_eq!(control.stored(session).unwrap().unwrap().profile, "codex");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn first_foreground_hook_keeps_the_selected_launch_account_and_permission() {
    let dir = std::env::temp_dir().join(format!("account-first-hook-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let control = Control::new(Some(&dir));
    let session = SessionId(1);
    let account = LaunchAccount {
        id: "codex-selected".into(),
        env_key: "CODEX_HOME".into(),
        dir: String::new(),
    };
    control
        .remember_session(
            session,
            &AgentLaunch {
                profile: "codex".into(),
                permission: Permission::Yolo,
            },
            None,
            b"/launch".to_vec(),
            Some(account.clone()),
        )
        .unwrap();
    assert!(!control.record_foreground_hook_session(
        session,
        br#"{"hook_event_name":"SessionStart","source":"startup","session_id":"first"}"#,
        "codex",
        None
    ));
    let stored = control.stored(session).unwrap().unwrap();
    assert_eq!(stored.session_id.as_deref(), Some("first"));
    assert_eq!(stored.account.as_ref(), Some(&account));
    assert_eq!(stored.permission, Permission::Yolo);
    assert_eq!(stored.cwd, b"/launch");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn verified_foreground_session_replaces_stale_identity_as_one_binding() {
    let dir = std::env::temp_dir().join(format!("account-foreground-hook-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let control = Control::new(Some(&dir));
    for (number, provider, source) in [(1, "claude", "startup"), (2, "codex", "resume")] {
        let session = SessionId(number);
        control
            .remember_session(
                session,
                &AgentLaunch {
                    profile: "claude".into(),
                    permission: Permission::Yolo,
                },
                Some("previous-conversation".into()),
                b"/old".to_vec(),
                Some(LaunchAccount {
                    id: "claude-original".into(),
                    env_key: "CLAUDE_CONFIG_DIR".into(),
                    dir: String::new(),
                }),
            )
            .unwrap();
        let payload = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "SessionStart", "source": source, "session_id": "current-conversation",
            "transcript_path": "/current/transcript.jsonl", "cwd": "/current"
        })).unwrap();
        assert!(!control.record_foreground_hook_session(session, &payload, provider, None));
        let stored = control.stored(session).unwrap().unwrap();
        assert_eq!(stored.profile, provider);
        assert_eq!(stored.session_id.as_deref(), Some("current-conversation"));
        assert_eq!(
            stored.transcript_path.as_deref(),
            Some("/current/transcript.jsonl")
        );
        assert_eq!(stored.cwd, b"/current");
        assert_eq!(stored.account, None);
        assert_eq!(stored.permission, Permission::Manual);
        assert!(stored.hibernation_blocked);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn foreground_rotation_preserves_account_and_unexpected_resume_cannot_confirm_handoff() {
    let dir = std::env::temp_dir().join(format!(
        "account-foreground-rotation-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let control = Control::new(Some(&dir));
    let session = SessionId(1);
    let account = LaunchAccount {
        id: "claude-original".into(),
        env_key: "CLAUDE_CONFIG_DIR".into(),
        dir: String::new(),
    };
    control
        .remember_session(
            session,
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Yolo,
            },
            Some("original".into()),
            b"/original".to_vec(),
            Some(account.clone()),
        )
        .unwrap();
    let rotated = br#"{"hook_event_name":"SessionStart","source":"clear","session_id":"rotated"}"#;
    assert!(!control.record_foreground_hook_session(session, rotated, "claude", None));
    let expected = control.stored(session).unwrap().unwrap();
    assert_eq!(expected.account.as_ref(), Some(&account));
    assert_eq!(expected.permission, Permission::Yolo);
    control.watch_account_resume(session);
    for (source, provider) in [
        ("startup", "claude"),
        ("resume", "claude"),
        ("resume", "codex"),
    ] {
        let payload = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "SessionStart", "source": source, "session_id": "unexpected",
            "transcript_path": "/other/transcript.jsonl"
        }))
        .unwrap();
        assert!(control.record_foreground_hook_session(session, &payload, provider, None));
        assert_eq!(control.stored(session).unwrap().unwrap(), expected);
        assert!(!control.account_resume_observed(session, "unexpected"));
    }
    assert!(!control.record_foreground_hook_session(
        session,
        br#"{"hook_event_name":"SessionStart","source":"resume","session_id":"rotated"}"#,
        "claude",
        None
    ));
    assert!(control.account_resume_observed(session, "rotated"));
    fs::remove_dir_all(dir).unwrap();
}
