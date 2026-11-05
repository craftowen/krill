//! 2026-11-05: 저장소 밖 벤치(`KRILL_STATE_DIR`만 임시, `HOME`은 실제)가 운영 로그 `krill.log`를 2분에 1MB씩 밀어냈다.
//! 테스트 표식을 지운 실제 실행처럼 띄워, 격리된 상태 폴더의 로그가 그 폴더에 남는지 본다.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MK: &str = env!("CARGO_BIN_EXE_krill");

fn krill(home: &Path, state: &Path, args: &[&str]) {
    let mut command = Command::new(MK);
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|key| key == "NEXTEST" || key.starts_with("CARGO_BIN_EXE_"))
        {
            command.env_remove(key);
        }
    }
    // 데몬 없는 `doctor --no-start`는 비0으로 끝난다. 여기서는 로그가 어디에 남는지만 본다.
    command
        .args(args)
        .env_remove("KRILL_LOG_DIR")
        .env("HOME", home)
        .env("KRILL_STATE_DIR", state)
        .env("KRILL_ERROR_REPORTING_DISABLED", "1")
        .output()
        .unwrap();
}

fn fixture(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("krill-log-routing-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    (root, home)
}

fn production_logs(home: &Path) -> PathBuf {
    home.join(if cfg!(target_os = "linux") {
        ".local/state/krill/logs"
    } else {
        "Library/Logs/Krill"
    })
}

#[test]
fn isolated_state_keeps_logs_beside_the_state() {
    let (root, home) = fixture("isolated");
    let state = root.join("state");
    krill(&home, &state, &["doctor", "--json", "--no-start"]);
    assert!(state.join("logs/krill.log").is_file());
    assert!(!production_logs(&home).exists());
    fs::remove_dir_all(root).ok();
}

/// Krill 세션 셸은 기본 경로를 `KRILL_STATE_DIR`로 내보낸다. 값이 있다는 것만으로 격리로 보지 않고,
/// `krill-log`가 따로 들고 있는 기본 경로가 `krill-proto`와 어긋나지 않는지도 함께 고정한다.
#[test]
fn default_state_path_from_a_session_shell_keeps_the_production_logs() {
    let (root, home) = fixture("session");
    let state = krill_proto::daemon::state_dir_in(&home);
    krill(&home, &state, &["doctor", "--json", "--no-start"]);
    assert!(production_logs(&home).join("krill.log").is_file());
    assert!(!state.join("logs").exists());
    fs::remove_dir_all(root).ok();
}
