//! Consolidated integration tests.

mod config_roundtrip;
mod runtime_verify;
mod socket_routes;
#[cfg(unix)]
mod test_env_isolation;
mod workspace_policy;
