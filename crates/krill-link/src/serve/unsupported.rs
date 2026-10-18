//! Signature-compatible API until the Windows server transport is implemented.
#![expect(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    reason = "preserve the native transport async signatures"
)]
use std::convert::Infallible;
use std::path::Path;
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId};

use super::ServeOptions;
use crate::{AllowedDevice, Error, unsupported_platform};

pub fn raise_nofile_limit() {}

pub fn is_serve_running(_state_dir: &Path) -> bool {
    false
}

pub fn read_live_server_addr(_state_dir: &Path) -> Option<EndpointAddr> {
    None
}

/// No instance can be constructed before the native server exists.
#[derive(Debug)]
pub struct ServerHandle {
    unavailable: Infallible,
}

impl ServerHandle {
    pub fn endpoint_addr(&self) -> EndpointAddr {
        match self.unavailable {}
    }

    pub fn node_id(&self) -> EndpointId {
        match self.unavailable {}
    }

    pub fn create_invite(&self, _ttl: Duration) -> Result<crate::InviteOutput, Error> {
        match self.unavailable {}
    }

    pub async fn revoke_device(&self, _target: &str) -> Result<AllowedDevice, Error> {
        match self.unavailable {}
    }

    pub fn stream_statuses(&self) -> Vec<crate::StreamStatus> {
        match self.unavailable {}
    }

    pub async fn shutdown(self) {
        match self.unavailable {}
    }
}

pub async fn start_server(_opts: ServeOptions) -> Result<ServerHandle, Error> {
    Err(unsupported_platform())
}

pub async fn revoke_device_command(
    _state_dir: &Path,
    _target: &str,
) -> Result<AllowedDevice, Error> {
    Err(unsupported_platform())
}
