//! OWE-719: 로그인 연결 상태와 사용량 조회 상태를 분리한다.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    #[default]
    Checking,
    Connected,
    NeedsLogin,
    NotInstalled,
    CheckFailed,
}

impl ConnectionStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Checking => "Checking",
            Self::Connected => "Connected",
            Self::NeedsLogin => "Login required",
            Self::NotInstalled => "Not installed",
            Self::CheckFailed => "Check failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConnection {
    pub account_id: String,
    pub status: ConnectionStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionsSnapshot {
    pub supported: bool,
    /// 구버전 호환용 필드. 새 데몬은 항상 true를 보낸다.
    pub direct_usage: bool,
    pub accounts: Vec<AccountConnection>,
}
