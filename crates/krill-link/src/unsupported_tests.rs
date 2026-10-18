//! Compile and exercise the unsupported transport contract on the native host too.
use crate::{ConnectOptions, Error, HostTunnelStatus, LinkStatusReport, ServeOptions};
use std::io::ErrorKind;
#[cfg(not(target_os = "macos"))]
use std::path::Path;

#[expect(
    dead_code,
    reason = "unconstructible handles preserve the platform API"
)]
#[path = "connect/unsupported.rs"]
mod connect;
#[expect(
    dead_code,
    reason = "unconstructible handles preserve the platform API"
)]
#[path = "serve/unsupported.rs"]
mod serve;

fn assert_unsupported<T>(result: Result<T, Error>) {
    assert!(matches!(result, Err(Error::Io(e)) if e.kind() == ErrorKind::Unsupported));
}

#[tokio::test]
async fn unsupported_transport_does_not_create_state_or_claim_a_live_service() {
    let state = std::env::temp_dir().join(format!("krill-link-unsupported-{}", std::process::id()));
    assert!(!state.exists());
    assert!(!serve::is_serve_running(&state));
    assert!(serve::read_live_server_addr(&state).is_none());
    assert_unsupported(connect::host_socket_path(&state, "home"));
    assert_unsupported(connect::host_status_path(&state, "home"));
    assert_unsupported(connect::host_status(&state, "home"));
    assert_unsupported(connect::all_host_statuses(&state));
    assert_unsupported(connect::status_report(&state));
    assert_unsupported(connect::start_tunnel(ConnectOptions::new(state.clone(), "home")).await);
    assert_unsupported(serve::start_server(ServeOptions::new(state.clone())).await);
    assert_unsupported(serve::revoke_device_command(&state, "home").await);
    serve::raise_nofile_limit();
    assert!(!state.exists());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_launch_agent_does_not_write_plists() {
    let state = Path::new("unused-state");
    assert_unsupported(crate::enable_launch_agent(Path::new("krill-link"), state));
    assert_unsupported(crate::disable_launch_agent(state));
}

#[cfg(not(unix))]
#[tokio::test]
async fn unsupported_pairing_does_not_create_keys() {
    let state = Path::new("unused-state");
    assert_unsupported(crate::create_invite_command(state, None).await);
    assert_unsupported(crate::pair_with_server(state, "krill://pair?", "device").await);
}
