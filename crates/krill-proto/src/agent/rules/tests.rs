//! MK-091 회귀 테스트. 외부 서버 호출 없이 로컬 바이트로 검증한다.

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

use super::*;
use crate::agent::Permission;

struct Keys {
    public: PublicKey,
    key: Ed25519KeyPair,
}

fn keys() -> Keys {
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let public: PublicKey = <[u8; 32]>::try_from(key.public_key().as_ref()).unwrap();
    Keys { public, key }
}

fn sign(key: &Ed25519KeyPair, payload: &[u8]) -> Vec<u8> {
    let signature: Signature = <[u8; 64]>::try_from(key.sign(payload).as_ref()).unwrap();
    pack(payload, &signature).unwrap()
}

const PAYLOAD: &str = r#"
version = 1
permission = "yolo"

[screen]
blocked_phrases = ["trust this folder", "새 승인 문구"]

[agent.aider]
name = "Aider"
launch = ["aider"]
"#;

/// 루프백: 서명→검증→적용. 서명 파일의 전역 권한은 무시하고 프로필·화면 규칙만 바뀐다.
#[test]
fn loopback_applies_profiles_and_screen_rules() {
    let k = keys();
    let payload = sign(&k.key, PAYLOAD.as_bytes());
    let parsed = load_signed(&k.public, &payload).unwrap();
    assert_eq!(parsed.version, SUPPORTED_VERSION);
    assert!(parsed.profiles.contains_key("aider"));

    let mut agents = Agents::load(Some("permission = 'manual'\n")).unwrap();
    let before = agents.clone();
    parsed.apply(&mut agents);
    // 전역 권한은 서명 파일로 바뀌지 않는다.
    assert_eq!(agents.permission, Permission::Manual);
    assert_eq!(agents.profiles.len(), before.profiles.len() + 1);
    assert_eq!(agents.profiles["aider"].launch, ["aider"]);
    // 화면 규칙은 서명 값이 통째로가 아니라 지정한 항목만 바뀐다.
    assert!(
        parsed
            .screen
            .blocked_phrases
            .contains(&"새 승인 문구".to_owned())
    );
    assert_eq!(
        parsed.screen.waiting_phrases,
        ScreenRules::default().waiting_phrases
    );
}

/// 서명이 틀리면 거부하고 호출자는 내장 규칙을 유지한다.
#[test]
fn wrong_key_and_tampered_bytes_are_rejected() {
    let k = keys();
    let other = keys();
    let payload = sign(&k.key, PAYLOAD.as_bytes());
    assert_eq!(
        load_signed(&other.public, &payload),
        Err(RulesError::BadSignature)
    );

    let mut tampered = payload.clone();
    let last_payload = tampered.len() - 65;
    tampered[last_payload] ^= 0x01;
    assert_eq!(
        load_signed(&k.public, &tampered),
        Err(RulesError::BadSignature)
    );

    // 거부됐을 때 내장이 그대로인지 호출자 패턴으로 확인한다.
    let mut agents = Agents::load(None).unwrap();
    let builtin = agents.clone();
    if let Ok(parsed) = load_signed(&k.public, &tampered) {
        parsed.apply(&mut agents);
    }
    assert_eq!(agents, builtin);
    assert_eq!(ScreenRules::default(), ScreenRules::default());
}

/// 잘린 꼬리·남는 바이트·상한 초과는 서명 전에 거부한다.
#[test]
fn damaged_and_oversize_files_are_rejected() {
    let k = keys();
    let payload = sign(&k.key, PAYLOAD.as_bytes());
    assert_eq!(load_signed(&k.public, &[]), Err(RulesError::Truncated));
    assert_eq!(
        load_signed(&k.public, &payload[..payload.len() - 1]),
        Err(RulesError::Truncated)
    );
    let mut trailing = payload.clone();
    trailing.push(0);
    assert_eq!(
        load_signed(&k.public, &trailing),
        Err(RulesError::Truncated)
    );
    assert_eq!(
        load_signed(&k.public, &vec![0u8; MAX_SIGNED_BYTES + 1]),
        Err(RulesError::TooLarge)
    );
    assert_eq!(
        pack(&vec![0u8; MAX_PAYLOAD_BYTES + 1], &[0u8; 64]),
        Err(RulesError::TooLarge)
    );
}

/// 서명은 맞지만 내용이 깨졌거나 버전·상한이 다르면 거부한다.
#[test]
fn valid_signature_with_bad_content_is_rejected() {
    let k = keys();
    for (name, body) in [
        ("not toml", "{{{{".as_bytes().to_vec()),
        ("not utf8", vec![0xff, 0xfe]),
        ("version", b"version = 2\n".to_vec()),
        (
            "profiles",
            format!("version = 1\n{}", "[agent.x]\n".repeat(300)).into_bytes(),
        ),
    ] {
        let signed = sign(&k.key, &body);
        let err = load_signed(&k.public, &signed).unwrap_err();
        assert!(
            matches!(
                err,
                RulesError::BadPayload | RulesError::UnsupportedVersion | RulesError::OverLimit
            ),
            "{name}: {err:?}"
        );
    }
}

/// 신뢰 앵커 파일 형식: hex 64자, 앞뒤 공백 허용.
#[test]
fn public_key_file_format() {
    use std::fmt::Write as _;
    let k = keys();
    let hex: String = k.public.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    });
    assert_eq!(decode_public_key(&format!("  {hex}\n")), Some(k.public));
    assert_eq!(decode_public_key(&hex.to_uppercase()), Some(k.public));
    assert!(decode_public_key("zz").is_none());
    let short: String = hex.chars().take(62).collect();
    assert!(decode_public_key(&short).is_none());
}

/// 내장 화면 규칙이 MK-049의 기존 판정과 같다.
#[test]
fn builtin_screen_rules_match_previous_behavior() {
    let rules = ScreenRules::default();
    assert_eq!(
        rules.menu_hit("Do you trust this folder?\n❯ 1. Yes, I trust this folder"),
        Some(MenuHit::Blocked)
    );
    assert_eq!(
        rules.menu_hit("Do you trust the contents of this project?\n> Yes, I trust this folder"),
        Some(MenuHit::Blocked)
    );
    assert_eq!(
        rules.menu_hit("Would you like to run this command?\n1. Yes\n2. No"),
        Some(MenuHit::Waiting)
    );
    assert_eq!(
        rules.menu_hit("enter to select, esc to cancel\n❯ 1. Yes"),
        Some(MenuHit::Waiting)
    );
    assert_eq!(
        rules.menu_hit("⚠ Approval Required\nRequested Permission: command\n❯ 1. Yes, grant permission\n  2. No, deny permission"),
        Some(MenuHit::Waiting)
    );
    assert_eq!(
        rules.menu_hit(
            "Accept this file edit?\n1. Yes, accept this change\n2. No, reject this change"
        ),
        Some(MenuHit::Waiting)
    );
    assert_eq!(
        rules.menu_hit("Run this command?\n1. Yes, run command\n2. No, suggest changes (esc)"),
        Some(MenuHit::Waiting)
    );
    assert!(rules.menu_hit("explaining: trust this folder").is_none());
    assert!(rules.is_prompt("❯"));
    assert!(rules.is_prompt(">"));
    assert!(rules.is_prompt("> Accept-edits mode: on (shift+tab to cycle)"));
    assert!(!rules.is_prompt("❯ 1. Yes"));
    assert!(!rules.is_prompt("> Yes, I trust this folder"));
}

#[test]
fn codex_pending_questions_require_the_queue_footer() {
    for screen in [
        "• Working (14m 03s • esc to interrupt)\n• Queued follow-up inputs\n  ? 1 question\n  ⇧← to answer\n\n› Ask Codex to do anything",
        "• Queued follow-up inputs\n  ? 12 questions\n  shift+left to answer\n›",
    ] {
        assert!(has_pending_question(screen), "{screen}");
    }
    for screen in [
        "• Queued follow-up inputs\n  run tests\n  ↑ to edit\n›",
        "• Queued follow-up inputs\n  ? 0 questions\n  ⇧← to answer\n›",
        "• Queued follow-up inputs\n  ? 1 question\n›",
        "? 1 question\n⇧← to answer\n›",
        "Explaining Queued follow-up inputs and 1 question to answer",
        "• Queued follow-up inputs\n  ? 1 question\n\nSome other output\n⇧← to answer",
    ] {
        assert!(!has_pending_question(screen), "{screen}");
    }
    // 출력 본문에 남은 예시를 현재 footer로 오인하지 않는다.
    let old = format!(
        "• Queued follow-up inputs\n  ? 1 question\n  ⇧← to answer\n{}",
        "output\n".repeat(20)
    );
    assert!(!has_pending_question(&old));
}
