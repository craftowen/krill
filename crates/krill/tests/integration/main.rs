//! Consolidated integration tests.

mod account_provisioning;
mod attach_receipts;
#[cfg(unix)]
mod cli;
mod doctor;
mod drop;
mod hook;
#[cfg(unix)]
mod hook_install;
#[cfg(unix)]
mod import_orca;
#[cfg(unix)]
mod import_orca_sessions;
#[cfg(unix)]
mod log_routing;
mod reconnect;
#[cfg(unix)]
mod remote;
#[cfg(unix)]
mod remote_bridge;
mod set;
#[cfg(unix)]
mod skills;
mod status;
mod telemetry;
#[cfg(unix)]
mod wait;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_worktree;
mod worktree;
#[cfg(unix)]
mod worktree_remote_safety;
