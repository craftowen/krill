//! MK-089: `krill telemetry status|enable|disable` 통합 테스트. 기본 꺼짐·동의 파일 왕복·
//! 환경 거부를 격리된 상태 디렉터리에서 확인한다. 네트워크를 쓰지 않는다.

#![expect(
    clippy::unwrap_used,
    reason = "통합 테스트 헬퍼(#[test] 밖이라 clippy.toml의 테스트 허용이 닿지 않는다)"
)]

use std::fs;
use std::process::{Command, Stdio};

const MK: &str = env!("CARGO_BIN_EXE_krill");

/// 거부 변수를 지워 테스트 환경을 고정한다(CI 러너의 `CI=true` 등이 새면 안 된다).
fn telemetry(state: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = Command::new(MK);
    cmd.arg("telemetry")
        .args(args)
        .env("KRILL_STATE_DIR", state);
    for var in [
        "DO_NOT_TRACK",
        "KRILL_TELEMETRY_DISABLED",
        "CI",
        "GITHUB_ACTIONS",
        "GITLAB_CI",
        "JENKINS_URL",
        "TEAMCITY_VERSION",
        "BUILDKITE",
        "CIRCLECI",
        "TRAVIS",
    ] {
        cmd.env_remove(var);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null()).output().unwrap()
}

fn state_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mk-telemetry-cli-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    dir
}

fn stdout(o: &std::process::Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

#[test]
fn default_is_off_and_status_is_read_only() {
    let state = state_dir();
    let out = telemetry(&state, &["status"], &[]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).contains("off"), "{out:?}");
    // `status`는 파일을 만들지 않는다(읽기 전용).
    assert!(!state.exists(), "status created {state:?}");
    fs::remove_dir_all(&state).ok();
}

#[test]
fn enable_disable_roundtrip() {
    let state = state_dir();
    let out = telemetry(&state, &["enable"], &[]);
    assert!(out.status.success(), "{out:?}");
    assert!(state.join("telemetry.enabled").is_file());
    let out = telemetry(&state, &["status"], &[]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).contains(" on"), "{out:?}");
    let out = telemetry(&state, &["disable"], &[]);
    assert!(out.status.success(), "{out:?}");
    let out = telemetry(&state, &["status"], &[]);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).contains("off"), "{out:?}");
    assert!(!state.join("telemetry.enabled").exists());
    fs::remove_dir_all(&state).ok();
}

#[test]
fn veto_env_forces_off() {
    let state = state_dir();
    let out = telemetry(&state, &["enable"], &[]);
    assert!(out.status.success(), "{out:?}");
    for var in ["DO_NOT_TRACK", "KRILL_TELEMETRY_DISABLED", "CI"] {
        let out = telemetry(&state, &["status"], &[(var, "1")]);
        assert!(out.status.success(), "{out:?}");
        let body = stdout(&out);
        assert!(body.contains("off"), "{out:?}");
        assert!(body.contains(var), "{out:?}");
    }
    fs::remove_dir_all(&state).ok();
}

#[test]
fn bad_args_fail_with_usage() {
    let state = state_dir();
    let out = telemetry(&state, &[], &[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let out = telemetry(&state, &["upload"], &[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    fs::remove_dir_all(&state).ok();
}
