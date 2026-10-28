//! Explicit, isolated process fixture for error-reporting transport and release
//! panic checks. It requires `KRILL_STATE_DIR` and never enables reporting itself.

#![forbid(unsafe_code)]

use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use krill_proto::error_reporting::{self, Code, Event, Process, runs::RunGuard};

fn main() -> ExitCode {
    let Some(state) = std::env::var_os("KRILL_STATE_DIR").filter(|value| !value.is_empty()) else {
        return ExitCode::from(2);
    };
    let state = PathBuf::from(state);
    let Some(mode) = std::env::args().nth(1) else {
        return ExitCode::from(2);
    };
    if !matches!(mode.as_str(), "seed" | "panic") {
        return ExitCode::from(2);
    }
    krill_proto::diag::install_panic_hook("krill-app");
    let _run = RunGuard::start(&state, Process::App).ok().flatten();
    if mode == "panic" {
        forced_panic();
    }
    match seed(&state) {
        Ok(true) => {
            io::stdout().write_all(b"queued\n").ok();
            ExitCode::SUCCESS
        }
        Ok(false) => {
            io::stdout().write_all(b"not_queued\n").ok();
            ExitCode::SUCCESS
        }
        Err(_) => ExitCode::FAILURE,
    }
}

fn seed(state: &std::path::Path) -> io::Result<bool> {
    error_reporting::record_at(state, Event::new(Process::App, Code::UpdateFailed, None)?)
}

#[expect(
    clippy::panic,
    reason = "explicit isolated release-process fixture, matching the app's --crash-test path"
)]
fn forced_panic() {
    panic!("krill error-reporting release panic fixture");
}
