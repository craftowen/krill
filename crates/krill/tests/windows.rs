//! Windows 컴파일 단계의 명령 노출·미지원 hook 계약.
#![cfg(windows)]

use std::process::Command;

const KRILL: &str = env!("CARGO_BIN_EXE_krill");

#[test]
fn help_hides_computer_commands() {
    let output = Command::new(KRILL).arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(!help.contains("krill computer"));
}

#[test]
fn unsupported_hook_preserves_agent_response_and_success() {
    for (agent, event, expected) in [
        ("claude", "PreToolUse", "{}"),
        ("agy", "PreToolUse", r#"{"decision":"ask"}"#),
        ("antigravity", "Stop", r#"{"decision":""}"#),
        ("agy", "SessionStart", "{}"),
    ] {
        let output = Command::new(KRILL)
            .args(["hook", agent, event])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("not supported")
        );
    }
}

#[test]
fn unsupported_hook_uses_event_environment_and_argument_precedence() {
    for (args, expected) in [
        (vec!["hook", "agy"], r#"{"decision":"ask"}"#),
        (vec!["hook", "agy", "Stop"], r#"{"decision":""}"#),
    ] {
        let output = Command::new(KRILL)
            .args(args)
            .env("MK_HOOK_EVENT", "PreToolUse")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
}
