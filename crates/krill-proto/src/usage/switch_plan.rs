//! 계정 전환 전 실행 중 세션 사전 진단([`super::FEAT_ACCOUNT_SWITCH_CONTROL`]).
use serde::{Deserialize, Serialize};

use super::ProviderKind;

/// 세션을 새 계정으로 옮길 수 없는 이유. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CantMoveReason {
    /// 재개할 대화 ID·트랜스크립트가 없다.
    NoSession,
    /// 세션이 쓰는 계정을 알 수 없다.
    UnknownAccount,
    /// Krill 관리·호스트 기본이 아닌 사용자 설정 폴더를 쓴다.
    OwnFolder,
    /// 같은 대화가 다른 살아 있는 패인에서도 열려 있다.
    SharedSession,
    /// 이미 계정 전환·교체가 진행 중이다.
    PendingHandoff,
}

/// 진단 결과 묶음. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSwitchGroup {
    OnPreviousAccount,
    Elsewhere,
    CantMove(CantMoveReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSwitchCandidate {
    pub session: crate::SessionId,
    pub cwd: String,
    pub agent: String,
    pub current_account_id: String,
    pub current_account_label: String,
    pub busy: bool,
    pub group: SessionSwitchGroup,
}

/// [`super::UsageRequest::InspectSessionSwitches`] 응답.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSwitchPlan {
    pub provider: ProviderKind,
    pub previous_account_id: Option<String>,
    pub target_account_id: String,
    pub candidates: Vec<SessionSwitchCandidate>,
}
