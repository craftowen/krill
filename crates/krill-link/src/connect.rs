//! 원격 터널의 공용 타입과 플랫폼별 구현.
use iroh::endpoint::Connection;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(not(unix))]
#[path = "connect/unsupported.rs"]
mod unsupported;
#[cfg(not(unix))]
pub use unsupported::*;

/// 재연결 지수 백오프 초기 지연 (50ms).
pub const BACKOFF_INITIAL: Duration = Duration::from_millis(50);
/// 재연결 지수 백오프 최대 상한 (2,000ms).
pub const BACKOFF_MAX: Duration = Duration::from_millis(2_000);

/// 지수 백오프 지연 계산 (`50ms * 2^attempt`, 상한 `BACKOFF_MAX`).
pub fn reconnect_backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.min(10);
    let factor = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
    BACKOFF_INITIAL.saturating_mul(factor).min(BACKOFF_MAX)
}

/// 원격 호스트 터널 연결 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelState {
    Connected,
    Connecting,
    Disconnected,
}

/// Machine-readable cause for the most recent tunnel failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelErrorKind {
    Unauthorized,
    RateLimited,
    Network,
    Other,
}

/// 원격 호스트 네트워크 전송 경로 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    Direct,
    Relay,
    None,
}

/// 원격 호스트 터널 1개의 상태 스냅샷 (MK-131 앱 호스트 계층 및 `krill-link status --json` 공용).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostTunnelStatus {
    pub host_id: String,
    pub host_name: String,
    pub node_id: String,
    pub socket_path: PathBuf,
    pub state: TunnelState,
    pub route: RouteKind,
    pub remote_addr: Option<String>,
    pub rtt_ms: Option<f64>,
    pub reconnect_attempt: u32,
    pub updated_at_ms: u64,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_kind: Option<TunnelErrorKind>,
    #[serde(default)]
    pub streams: Vec<crate::resume::StreamStatus>,
}

/// `krill-link status --json` 전체 상태 보고서.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkStatusReport {
    pub node_id: String,
    pub relay_mode: String,
    pub serve_running: bool,
    pub allowed_devices_count: usize,
    pub hosts: Vec<HostTunnelStatus>,
    #[serde(default)]
    pub server_streams: Vec<crate::resume::StreamStatus>,
}

/// `krill-link connect` 실행 옵션.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub state_dir: PathBuf,
    pub host_id: String,
}

impl ConnectOptions {
    /// 상태 디렉터리와 호스트 식별자로 터널 옵션을 만든다.
    pub fn new(state_dir: PathBuf, host_id: impl Into<String>) -> Self {
        Self {
            state_dir,
            host_id: host_id.into(),
        }
    }
}

/// 활성 QUIC 연결에서 선택된 경로(`Direct` vs `Relay`), 원격 주소, RTT(ms)를 추출한다.
pub fn inspect_connection_route(conn: &Connection) -> (RouteKind, Option<String>, Option<f64>) {
    let paths = conn.paths();
    let selected = paths
        .iter()
        .find(iroh::endpoint::Path::is_selected)
        .or_else(|| paths.iter().next());
    let Some(path) = selected else {
        return (RouteKind::None, None, None);
    };
    let route = if path.is_ip() {
        RouteKind::Direct
    } else if path.is_relay() {
        RouteKind::Relay
    } else {
        RouteKind::None
    };
    let remote = Some(path.remote_addr().to_string());
    let rtt_ms = Some(path.rtt().as_secs_f64() * 1000.0);
    (route, remote, rtt_ms)
}
