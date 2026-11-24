//! Consolidated integration tests.

mod crash_e2e;
mod i18n;
mod i18n_lint;
#[cfg(unix)]
mod link_integration;
#[cfg(unix)]
mod updater_e2e;
#[cfg(windows)]
mod win_survival_e2e;
