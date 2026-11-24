//! Consolidated integration tests.

#[cfg(unix)]
mod detach;
#[cfg(unix)]
mod handoff_env;
#[cfg(unix)]
mod link;
mod size_budget;
#[cfg(target_os = "macos")]
mod stop_signals;
#[cfg(unix)]
mod teardown;
#[cfg(unix)]
mod test_logs;
#[cfg(windows)]
mod windows_session;
