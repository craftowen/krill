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
    pub const fn label_ko(self) -> &'static str {
        match self {
            Self::Checking => "확인 중",
            Self::Connected => "연결됨",
            Self::NeedsLogin => "로그인 필요",
            Self::NotInstalled => "설치 안 됨",
            Self::CheckFailed => "확인 실패",
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
    pub direct_usage: bool,
    pub accounts: Vec<AccountConnection>,
}
