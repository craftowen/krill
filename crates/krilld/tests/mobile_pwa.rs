//! 비보안 HTTP를 가정한 독립 JS 구현과 실제 데몬 간 모바일 워크플로 E2E.
#[test]
fn non_secure_pwa_crypto_and_workflows() {
    let result = std::process::Command::new("node")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mobile_pwa.cjs"))
        .arg(env!("CARGO_BIN_EXE_krilld"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
