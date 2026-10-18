#![forbid(unsafe_code)]
//! `krill-link`: iroh QUIC 기반 Krill 원격 연결 크레이트 (MK-133a / OWE-669).
//!
//! 비개발자가 초대 링크(`krill://pair?...`) 하나만 붙여넣어 다른 맥(예: 집 맥미니)의
//! Krill 세션(`krilld.sock`)에 어디서든 안전하게 연결하도록 한다.
//!
//! # 핵심 불변식
//! - **세션 보존**: `krilld`·`krill-holder`·`krill-proto` 와이어 프로토콜을 수정하지 않으며,
//!   원격 연결 실패가 로컬 PTY나 세션을 절대 건드리지 않는다.
//! - **비밀 보호**: 기기 비밀키(`link/secret.key`)와 초대 토큰(`link/invites.toml`에는 SHA-256 해시만 저장)은
//!   0600 원자적 쓰기로 보관하며, `Debug`·로그·오류 메시지에 절대 원문을 남기지 않는다.
//! - **Resumable stream v2**: random stream IDs, byte offsets, bounded replay and
//!   destination-write ACKs preserve both Unix connections across QUIC outages.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

pub mod config;
pub mod connect;
pub mod identity;
pub mod launchd;
pub mod messages;
pub mod pair;
pub mod qr;
pub mod resume;
pub use resume::{StreamId, StreamPhase, StreamStatus};
pub mod serve;
pub mod store;

pub use config::{CONFIG_REL_PATH, LinkConfig, RelaySetting};
pub use connect::{
    BACKOFF_INITIAL, BACKOFF_MAX, ConnectOptions, HostTunnelStatus, LinkStatusReport, RouteKind,
    TunnelErrorKind, TunnelHandle, TunnelState, all_host_statuses, host_socket_path, host_status,
    host_status_path, inspect_connection_route, reconnect_backoff_delay, start_tunnel,
    status_report,
};
pub use identity::{DeviceIdentity, SECRET_KEY_REL_PATH};
pub use launchd::{
    LINK_LABEL, disable_launch_agent, enable_launch_agent, isolated_state_dir,
    launch_agent_plist_path, link_label, link_plist,
};
pub use pair::{
    DEFAULT_MAX_ATTEMPTS, DEFAULT_MAX_FAILURES, DEFAULT_RATE_WINDOW, InviteOutput,
    LEGACY_PAIR_URI_PREFIX, PAIR_URI_PREFIX, PairLink, PairRateLimiter, PairWireRequest,
    PairWireResponse, create_invite_command, decode_addr_ticket, encode_addr_ticket,
    generate_invite_for_addr, pair_with_server,
};
pub use qr::{QrMatrix, encode_qr, render_qr_unicode};
pub use serve::{
    ServeOptions, ServerHandle, is_serve_running, raise_nofile_limit, read_live_server_addr,
    revoke_device_command, start_server,
};
pub use store::{
    AllowedDevice, DEFAULT_INVITE_TTL, DEVICES_REL_PATH, HOST_ID_MAX_LEN, HOSTS_REL_PATH,
    INVITES_REL_PATH, InviteRecord, InviteToken, InviteVerifyError, RemoteHost, find_host,
    load_allowed_node_set, load_devices, load_hosts, remove_device, save_devices, save_hosts,
    upsert_device, upsert_host, validate_host_id, verify_and_consume_invite,
};

/// 데이터 터널 ALPN 식별자 (`b"minikrill/link/2"`).
pub const ALPN_LINK: &[u8] = b"minikrill/link/2";

/// 기기 짝짓기 ALPN 식별자 (`b"minikrill/pair/1"`).
pub const ALPN_PAIR: &[u8] = b"minikrill/pair/1";

/// Legacy v1 preamble (v2 rejects the old ALPN instead of silently losing resume).
pub const STREAM_INIT_BYTE: u8 = 0x00;

/// Legacy v1 success response.
pub const STREAM_READY_BYTE: u8 = 0x00;

/// Legacy v1 upstream error response.
pub const STREAM_ERR_UPSTREAM: u8 = 0x01;

/// 상태 디렉터리를 결정한다 (`override_dir` → `KRILL_STATE_DIR` → `~/Library/Application Support/dev.krill.app`(이관 전이면 옛 `dev.minikrill.app`)).
pub fn resolve_state_dir(override_dir: Option<&Path>) -> Result<PathBuf, Error> {
    if let Some(d) = override_dir {
        return Ok(d.to_path_buf());
    }
    krill_proto::daemon::state_dir()
        .ok_or_else(|| Error::Io(io::Error::new(io::ErrorKind::NotFound, "HOME is not set")))
}

/// `krill-link` 오류 타입 (CONVENTIONS P8: 소문자 시작, 끝 마침표 없음, 비밀키·토큰 원문 미포함).
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidKey(&'static str),
    InvalidConfig(String),
    InvalidPairLink(&'static str),
    InvalidHostId(String),
    PathTooLong(PathBuf),
    DeviceNotFound(String),
    HostNotFound(String),
    PairRejected(String),
    QrPayloadTooLarge(usize),
    Launchd(String),
    Transport(String),
    Protocol(&'static str),
    Timeout(&'static str),
    Crypto(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::InvalidKey(msg) => write!(f, "invalid secret key: {msg}"),
            Self::InvalidConfig(msg) => write!(f, "invalid config: {msg}"),
            Self::InvalidPairLink(msg) => write!(f, "invalid pair link: {msg}"),
            Self::InvalidHostId(id) => write!(f, "invalid host id: {id:?}"),
            Self::PathTooLong(p) => write!(
                f,
                "unix socket path exceeds {} bytes: {}",
                krill_proto::daemon::SUN_PATH_MAX.saturating_sub(1),
                p.display()
            ),
            Self::DeviceNotFound(id) => write!(f, "device not found: {id}"),
            Self::HostNotFound(id) => write!(f, "remote host not found: {id}"),
            Self::PairRejected(reason) => write!(f, "pairing rejected: {reason}"),
            Self::QrPayloadTooLarge(len) => {
                write!(f, "qr payload too large ({len} bytes)")
            }
            Self::Launchd(msg) => write!(f, "launchd error: {msg}"),
            Self::Transport(msg) => write!(f, "transport error: {msg}"),
            Self::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Self::Timeout(msg) => write!(f, "timeout: {msg}"),
            Self::Crypto(msg) => write!(f, "crypto error: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// Platform operations fail explicitly until a native transport is implemented.
#[cfg(any(test, not(target_os = "macos")))]
pub(crate) fn unsupported_platform() -> Error {
    Error::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "krill-link is not supported on this platform",
    ))
}

#[cfg(test)]
#[cfg(unix)]
mod unsupported_tests;
