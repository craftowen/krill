//! MK-067: 앱이 panic하면 크래시 로그가 남는다(다음 실행의 진단 대화상자는 `crash::tests`). 실제 `krill-app` 바이너리를
//! `--crash-test`로 띄운다(창을 열기 전에 panic하므로 화면이 필요 없다).

use std::fs;
use std::process::Command;

#[test]
fn forced_panic_leaves_crash_log() {
    let state = std::env::temp_dir().join(format!("mk-crash-e2e-{}", std::process::id()));
    fs::remove_dir_all(&state).ok();
    let out = Command::new(env!("CARGO_BIN_EXE_krill-app"))
        .arg("--crash-test")
        .env("KRILL_STATE_DIR", &state)
        .env("KRILL_LOG_DIR", state.join("logs"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    // 기존 hook(stderr 출력)도 그대로 돈다.
    assert!(String::from_utf8_lossy(&out.stderr).contains("MK-067 crash test"));
    let logs = krill_proto::diag::pending(&state);
    assert_eq!(logs.len(), 1, "{logs:?}");
    let text = fs::read_to_string(&logs[0]).unwrap();
    assert!(text.contains("process: krill-app"), "{text}");
    assert!(text.contains("message: MK-067 crash test"), "{text}");
    assert!(
        text.replace('\\', "/")
            .contains("location: crates/krill-app/src/main.rs"),
        "{text}"
    );
    fs::remove_dir_all(&state).ok();
}
