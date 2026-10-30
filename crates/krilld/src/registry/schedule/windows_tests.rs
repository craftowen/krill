//! daemon-03: Windows precheck runs through Git for Windows `sh.exe` with bounded, non-blocking pipes.
use super::*;

struct Temp(PathBuf);
impl Temp {
    #[expect(
        clippy::unwrap_used,
        reason = "Windows regression fixture must create its scratch directory"
    )]
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mk101-win-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&path).ok();
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn precheck_passes_exit_zero_and_reports_status_output_and_timeout() {
    if let Err(e) = precheck_shell() {
        eprintln!("skip: {e}");
        return;
    }
    let dir = Temp::new("precheck");

    // 1. A successful command passes (the audited HEAD skipped every Windows precheck).
    let ok = run_precheck_with_timeout(&dir.0, "exit 0", Duration::from_secs(10));
    assert!(ok.passed, "{ok:?}");
    assert_eq!(ok.status, Some(0));
    assert!(!ok.timed_out);
    assert_eq!(ok.error, None);

    // 2. A failing command is skipped with its exit status and captured output.
    let no = run_precheck_with_timeout(
        &dir.0,
        "printf 'nothing to do\\n'; exit 3",
        Duration::from_secs(10),
    );
    assert!(!no.passed, "{no:?}");
    assert_eq!(no.status, Some(3));
    assert_eq!(no.output.as_deref(), Some("nothing to do"));
    assert_eq!(no.error.as_deref(), Some("precheck exited with status 3"));

    // 3. A hanging command times out within the budget and its Job is torn down.
    let started = Instant::now();
    let hung = run_precheck_with_timeout(
        &dir.0,
        "printf 'before-hang\\n'; sleep 30",
        Duration::from_millis(500),
    );
    assert!(hung.timed_out, "{hung:?}");
    assert!(!hung.passed);
    assert_eq!(hung.status, None);
    assert_eq!(hung.output.as_deref(), Some("before-hang"));
    assert_eq!(hung.error.as_deref(), Some("precheck timed out"));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "timed-out precheck must return promptly: {:?}",
        started.elapsed()
    );
}
