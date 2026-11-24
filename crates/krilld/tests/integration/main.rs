//! Consolidated integration tests.

#[cfg(unix)]
mod account_switch;
#[cfg(unix)]
mod attach;
#[cfg(unix)]
mod cold;
#[cfg(unix)]
mod core;
#[cfg(unix)]
mod holder;
#[cfg(unix)]
mod holder_link;
#[cfg(unix)]
mod input;
#[cfg(unix)]
mod machine_identity;
mod mobile_diagnostics;
#[cfg(unix)]
mod mobile_pwa;
#[cfg(unix)]
mod mobile_tailscale_serve;
#[cfg(unix)]
mod park;
#[cfg(unix)]
mod publish;
#[cfg(unix)]
mod rejoin;
#[cfg(unix)]
mod resources;
#[cfg(unix)]
mod roles;
#[cfg(unix)]
mod search;
#[cfg(unix)]
mod store;
#[cfg(unix)]
mod upgrade;
#[cfg(windows)]
mod windows_agent;
#[cfg(windows)]
mod windows_shell;
#[cfg(windows)]
mod windows_upgrade;
