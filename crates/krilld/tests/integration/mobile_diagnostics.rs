//! Browser diagnostics privacy, consent, queue and transport failure regression contracts.
#[test]
fn browser_diagnostics_runtime_contracts() {
    let result = std::process::Command::new("node")
        .arg("--test")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/mobile_diagnostics.cjs"
        ))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
