//! exec 핸드오프 환경변수 이름은 구 holder와의 계약이다(mk→krill 개명 감사, docs/testing/audit-2026-10-17/rename.md).
//! 운영 중인 구 holder는 `MK_HOLDER_STATE`로 새 holder를 exec한다. 새 holder가 이 이름을 읽지 않으면
//! 넘겨받은 세션 fd를 버리고 빈 holder로 시작해 세션을 잃는다.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HOLDER: &str = env!("CARGO_BIN_EXE_krill-holder");

#[test]
fn legacy_handoff_env_is_read() {
    assert_handoff_env_is_read("MK_HOLDER_STATE");
}

/// 개명 도중 잠시 `KRILL_HOLDER_STATE`로 빌드된 운영 holder(2026-10-17 00:46 번들)도 있다. 그 위로 올라가도 세션을 잃지 않는다.
#[test]
fn interim_krill_handoff_env_is_read() {
    assert_handoff_env_is_read("KRILL_HOLDER_STATE");
}

#[expect(clippy::unwrap_used, clippy::panic, reason = "테스트 헬퍼")]
fn assert_handoff_env_is_read(name: &str) {
    let dir = std::env::temp_dir().join(format!("krill-handoff-env-{name}-{}", std::process::id()));
    // 깨진 상태를 넘기면 핸드오프를 읽는 holder는 곧바로 실패로 끝난다. 이름을 무시하면 새 holder로 계속 돈다.
    let mut child = Command::new(HOLDER)
        .arg("/usr/bin/true")
        .env(name, "not a handoff")
        .env("KRILL_LOG_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= end {
            child.kill().ok();
            child.wait().ok();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    std::fs::remove_dir_all(&dir).ok();
    let status = status.unwrap_or_else(|| panic!("{name}를 무시하고 빈 holder로 시작했다"));
    assert!(
        !status.success(),
        "깨진 핸드오프 상태를 거부해야 한다: {status}"
    );
}
