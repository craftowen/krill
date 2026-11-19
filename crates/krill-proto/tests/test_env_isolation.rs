//! Harness canary (F-01): tests run by nextest never see the real HOME or a Krill session's
//! state and agent config paths. Fails when the `isolate-env` setup script in
//! `.config/nextest.toml` is removed or stops covering these variables.
//! That script runs only on `cfg(unix)` (see the profile's `platform`), so this canary does too.
#![cfg(unix)]

use std::path::{Path, PathBuf};

#[test]
fn nextest_runs_tests_in_a_temporary_home() {
    if std::env::var_os("NEXTEST").is_none() {
        return; // `cargo test` has no setup scripts. check.sh and CI run this through nextest.
    }
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
    let temp = std::env::temp_dir();
    assert!(
        home.starts_with(&temp),
        "HOME {} is not a temporary directory under {}",
        home.display(),
        temp.display()
    );
    for key in [
        "KRILL_STATE_DIR",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "XDG_CONFIG_HOME",
    ] {
        if let Some(value) = std::env::var_os(key) {
            assert!(
                Path::new(&value).starts_with(&home),
                "{key}={} escapes the test HOME",
                Path::new(&value).display()
            );
        }
    }
}
