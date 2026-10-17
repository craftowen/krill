//! OWE-712: 기존 정책·계정 행 바이트를 보존하는 선택 메시지 꼬리.
use super::{AutoSwitchEvent, ProviderKind, Window};
use serde::{Deserialize, Serialize};

/// 고급 정책 기본값은 기존 평가와 동치다. 초 단위이며 기존 전역·계정별 규칙을 존중한다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedPolicy {
    pub predict_exhaustion: bool,
    pub prefer_soonest_reset: bool,
    pub cooldown_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchRecord {
    pub sequence: u64,
    pub at: u64,
    pub event: AutoSwitchEvent,
    pub predicted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCooldown {
    pub provider: ProviderKind,
    pub cooldown_until: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageForecast {
    pub account_id: String,
    /// 창 이름(`5h` 또는 `7d`). 두 창의 속도를 섞지 않는다.
    pub window: String,
    pub percent_per_hour: u64,
    pub exhausts_at: u64,
    pub resets_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelWindow {
    pub account_id: String,
    pub model: String,
    pub seven_day: Window,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitExpiry {
    pub account_id: String,
    pub clears_at: u64,
}

/// 전체 메시지 마지막 꼬리. 목록 안의 기존 행에는 필드를 추가하지 않는다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountPolicyState {
    pub supported: bool,
    pub policy: AdvancedPolicy,
    pub cooldowns: Vec<ProviderCooldown>,
    pub history: Vec<SwitchRecord>,
    pub forecasts: Vec<UsageForecast>,
    pub model_windows: Vec<ModelWindow>,
    pub rate_limits: Vec<RateLimitExpiry>,
}
