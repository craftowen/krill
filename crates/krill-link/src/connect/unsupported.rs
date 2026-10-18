//! Signature-compatible API until the Windows tunnel transport is implemented.
#![expect(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    reason = "preserve the native transport async signatures"
)]
use std::convert::Infallible;
use std::path::{Path, PathBuf};

use super::{ConnectOptions, HostTunnelStatus, LinkStatusReport};
use crate::{Error, unsupported_platform};

pub fn host_socket_path(_state_dir: &Path, _host_id: &str) -> Result<PathBuf, Error> {
    Err(unsupported_platform())
}

pub fn host_status_path(_state_dir: &Path, _host_id: &str) -> Result<PathBuf, Error> {
    Err(unsupported_platform())
}

pub fn host_status(_state_dir: &Path, _host_id: &str) -> Result<HostTunnelStatus, Error> {
    Err(unsupported_platform())
}

pub fn all_host_statuses(_state_dir: &Path) -> Result<Vec<HostTunnelStatus>, Error> {
    Err(unsupported_platform())
}

pub fn status_report(_state_dir: &Path) -> Result<LinkStatusReport, Error> {
    Err(unsupported_platform())
}

/// No instance can be constructed before the native tunnel exists.
#[derive(Debug)]
pub struct TunnelHandle {
    unavailable: Infallible,
}

impl TunnelHandle {
    pub fn socket_path(&self) -> &Path {
        match self.unavailable {}
    }

    pub fn status(&self) -> HostTunnelStatus {
        match self.unavailable {}
    }

    #[doc(hidden)]
    pub fn set_transport_offline(&self, _offline: bool) {
        match self.unavailable {}
    }

    pub async fn shutdown(self) {
        match self.unavailable {}
    }
}

pub async fn start_tunnel(_opts: ConnectOptions) -> Result<TunnelHandle, Error> {
    Err(unsupported_platform())
}
