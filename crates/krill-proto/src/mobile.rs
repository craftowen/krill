//! M9 모바일 제어. variant는 끝에만 추가한다. 비밀은 Debug로 출력하지 않는다.
pub mod client;
pub mod qr;
use crate::screen::Msg;
use serde::{Deserialize, Serialize};

pub const FEAT_MOBILE: u64 = 1 << 56;
pub const MOBILE_REQ: u8 = 0x45;
pub const MOBILE_REPLY: u8 = 0x65;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectMode {
    /// Retired LAN discriminant (0). Decode for existing saved configurations only.
    Lan,
    /// Selects the direct backup QR; an authenticated relay remains active.
    Tailscale,
    /// Prefer the relay QR while keeping an available tailnet listener open.
    #[default]
    Tunnel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MobileRequest {
    MintPairingTicket {
        mode: ConnectMode,
        custom_address: Option<String>,
        rotate: bool,
    },
    Status,
    ListDevices,
    RevokeDevice {
        device_id: Option<String>,
        all: bool,
    },
    SetConnectMode {
        mode: ConnectMode,
        custom_address: Option<String>,
    },
    ConnectedDevices,
    CurrentPairingOffer,
    /// 인증된 모바일 워크플로 JSON. 기존 세션 IPC·오케스트레이션 저장소로 연결한다.
    Workflow(String),
    /// Cancel only the pending QR; paired devices and terminal sessions survive.
    CancelPairingTicket,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MobileReply {
    PairingOffer {
        url: String,
        /// Reachable direct IPv4 address, or empty when only relay is available.
        host: String,
        port: u16,
        expires_at_ms: u64,
        mode: ConnectMode,
        interfaces: Vec<NetworkInterface>,
    },
    GatewayStatus {
        listening: bool,
        bind_addrs: Vec<String>,
        mode: ConnectMode,
        tunnel_url: Option<String>,
        active_offer: bool,
        devices: Vec<PairedMobileDevice>,
    },
    Devices(Vec<PairedMobileDevice>),
    Revoked {
        count: usize,
    },
    Failed(String),
    ConnectedDevices(Vec<String>),
    Workflow(String),
}

impl std::fmt::Debug for MobileReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PairingOffer {
                expires_at_ms,
                mode,
                ..
            } => f
                .debug_struct("PairingOffer")
                .field("expires_at_ms", expires_at_ms)
                .field("mode", mode)
                .finish_non_exhaustive(),
            Self::GatewayStatus {
                listening, mode, ..
            } => f
                .debug_struct("GatewayStatus")
                .field("listening", listening)
                .field("mode", mode)
                .finish_non_exhaustive(),
            Self::Devices(v) => f.debug_tuple("Devices").field(&v.len()).finish(),
            Self::Revoked { count } => f.debug_struct("Revoked").field("count", count).finish(),
            Self::ConnectedDevices(ids) => {
                f.debug_tuple("ConnectedDevices").field(&ids.len()).finish()
            }
            Self::Workflow(_) => f.debug_tuple("Workflow").field(&"redacted").finish(),
            Self::Failed(e) => f.debug_tuple("Failed").field(e).finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub name: String,
    pub address: String,
    pub default_route: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedMobileDevice {
    pub device_id: String,
    pub label: String,
    pub key_fingerprint: String,
    /// 상태 파일 전용 검증자. 제어/상태 응답에서는 빈 문자열로 지운다.
    pub shared_secret_hash: String,
    pub created_at_ms: u64,
    pub last_seen_ms: u64,
    pub revoked_at_ms: Option<u64>,
}

impl std::fmt::Debug for PairedMobileDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairedMobileDevice")
            .field("device_id", &self.device_id)
            .field("revoked_at_ms", &self.revoked_at_ms)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingTicket {
    pub token: [u8; 32],
    pub secret: [u8; 32],
    pub expires_at_ms: u64,
}

impl std::fmt::Debug for PairingTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingTicket")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish_non_exhaustive()
    }
}

impl Msg for MobileRequest {
    const KIND: u8 = MOBILE_REQ;
}
impl Msg for MobileReply {
    const KIND: u8 = MOBILE_REPLY;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc;
    #[test]
    fn fixtures_and_roundtrips() {
        assert_eq!(postcard::to_allocvec(&ConnectMode::Lan).unwrap(), vec![0]);
        assert_eq!(
            postcard::to_allocvec(&ConnectMode::Tailscale).unwrap(),
            vec![1]
        );
        assert_eq!(ConnectMode::default(), ConnectMode::Tunnel);
        for (value, fixture) in [
            (
                MobileRequest::CancelPairingTicket,
                vec![2, 0, 0, 0, 0x45, 8],
            ),
            (MobileRequest::Status, vec![2, 0, 0, 0, 0x45, 1]),
            (MobileRequest::ListDevices, vec![2, 0, 0, 0, 0x45, 2]),
            (
                MobileRequest::Workflow("{}".into()),
                vec![5, 0, 0, 0, 0x45, 7, 2, b'{', b'}'],
            ),
            (
                MobileRequest::SetConnectMode {
                    mode: ConnectMode::Tunnel,
                    custom_address: None,
                },
                vec![4, 0, 0, 0, 0x45, 4, 2, 0],
            ),
        ] {
            assert_eq!(value.encode().unwrap(), fixture);
            let (k, b) = ipc::read_frame(&mut fixture.as_slice()).unwrap().unwrap();
            assert_eq!(MobileRequest::decode(k, &b).unwrap(), value);
        }
        let reply = MobileReply::Revoked { count: 2 };
        assert_eq!(reply.encode().unwrap(), vec![3, 0, 0, 0, 0x65, 3, 2]);
        let reply = MobileReply::Workflow("{}".into());
        let fixture = vec![5, 0, 0, 0, 0x65, 6, 2, b'{', b'}'];
        assert_eq!(reply.encode().unwrap(), fixture);
        assert_eq!(
            MobileReply::decode(MOBILE_REPLY, &fixture[5..]).unwrap(),
            reply
        );
        assert!(!format!("{reply:?}").contains("{}"));
    }
    proptest::proptest! {
        #[test]
        fn malformed_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256)) {
            MobileRequest::decode(MOBILE_REQ, &bytes).ok();
            MobileReply::decode(MOBILE_REPLY, &bytes).ok();
        }
    }
}
