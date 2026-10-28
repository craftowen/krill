//! Provider-specific meters and running-session account handoffs. Optional IPC tail.

use serde::{Deserialize, Serialize};

use super::{AutoSwitchMetric, ProviderKind, Snapshot, Window};
use crate::SessionId;

/// 리셋 크레딧 미터 키. 팝업·설정은 이 키로 남은 소진 가능 횟수를 읽는다.
/// Codex는 `rate_limit_reset_credits.available_count`, Claude는 주간 세션 리셋(`juniper_tide`)
/// 제공 여부와 프로모션 grant(`cedar_ember`) 사용 가능 합을 같은 키로 담는다.
pub const RESET_CREDITS_METER_KEY: &str = "reset_credits";
/// 리셋 크레딧 미터 라벨. [`crate::accounts::display::meter_label`]이 현지화한다.
pub const RESET_CREDITS_METER_LABEL: &str = "Reset credits";
/// 사용권 묶음별 만료 미터 접두사. value는 epoch 초 또는 미제공 시 `unknown`이다.
/// 기존 value 미터를 써 구 피어와도 와이어 형식을 유지한다.
pub const RESET_CREDITS_EXPIRY_METER_PREFIX: &str = "reset_credits_expiry_";

/// 제공자가 보고한 사용권 묶음의 남은 횟수·만료 시각만 전달한다.
#[must_use]
pub fn reset_credit_expiry_meter(index: usize, count: u64, expires_at: Option<i64>) -> UsageMeter {
    UsageMeter {
        key: format!("{RESET_CREDITS_EXPIRY_METER_PREFIX}{index}"),
        label: format!("Reset credit expiry · {count}"),
        window: None,
        value: Some(expires_at.map_or_else(|| "unknown".into(), |at| at.to_string())),
    }
}

/// A meter actually reported by a provider. Missing meters are never invented as 0%.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageMeter {
    pub key: String,
    pub label: String,
    pub window: Option<Window>,
    /// Non-percentage values, for example a credit balance. Contains no credentials.
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountMeters {
    pub account_id: String,
    pub plan: Option<String>,
    pub meters: Vec<UsageMeter>,
}

impl AccountMeters {
    fn meter_effective(meter: &UsageMeter, now_secs: i64) -> Option<u8> {
        let window = meter.window.as_ref()?;
        let expired = window
            .resets_at
            .as_deref()
            .and_then(crate::time::parse_reset_epoch)
            .is_some_and(|at| now_secs >= at);
        (!expired).then_some(window.percent.min(100))
    }

    /// Most constrained reported pool, excluding reset windows and non-percentage balances.
    #[must_use]
    pub fn effective_max(&self, now_secs: i64) -> Option<u8> {
        self.meters
            .iter()
            .filter_map(|meter| Self::meter_effective(meter, now_secs))
            .max()
    }

    /// 프로바이더·전환 기준 창(`metric`)에 맞는 유효 사용률을 계산한다.
    ///
    /// Antigravity의 기본(`Max`/`FiveHour`/`SevenDay`)은 Gemini 그룹만 반영하여
    /// 보조 `Claude and GPT models` 한도 소진이 Gemini 세션을 전환시키지 않게 한다.
    #[must_use]
    pub fn effective_for_metric(
        &self,
        provider: ProviderKind,
        metric: AutoSwitchMetric,
        usage: &Snapshot,
        now_secs: i64,
    ) -> Option<u8> {
        let max_where = |pred: &dyn Fn(&UsageMeter) -> bool| -> Option<u8> {
            self.meters
                .iter()
                .filter(|m| pred(m))
                .filter_map(|m| Self::meter_effective(m, now_secs))
                .max()
        };
        let is_agy_gemini = |m: &UsageMeter| m.label.to_ascii_lowercase().contains("gemini");
        let is_agy_claude_gpt = |m: &UsageMeter| {
            let lower = m.label.to_ascii_lowercase();
            lower.contains("claude") || lower.contains("gpt")
        };
        let has_window_suffix = |m: &UsageMeter, win: &str| {
            let lower = m.label.to_ascii_lowercase();
            lower == win || lower.ends_with(&format!("· {win}")) || m.key == win
        };

        match (provider, metric) {
            (ProviderKind::Antigravity, AutoSwitchMetric::Max) => {
                max_where(&is_agy_gemini).or_else(|| metric.effective_percent(usage, now_secs))
            }
            (ProviderKind::Antigravity, AutoSwitchMetric::FiveHour) => {
                max_where(&|m| is_agy_gemini(m) && has_window_suffix(m, "5h"))
                    .or_else(|| metric.effective_percent(usage, now_secs))
            }
            (ProviderKind::Antigravity, AutoSwitchMetric::SevenDay) => {
                max_where(&|m| is_agy_gemini(m) && has_window_suffix(m, "7d"))
                    .or_else(|| metric.effective_percent(usage, now_secs))
            }
            (_, AutoSwitchMetric::AntigravityClaudeGptMax) => max_where(&is_agy_claude_gpt),
            (_, AutoSwitchMetric::AntigravityClaudeGptFiveHour) => {
                max_where(&|m| is_agy_claude_gpt(m) && has_window_suffix(m, "5h"))
            }
            (_, AutoSwitchMetric::AntigravityClaudeGptSevenDay) => {
                max_where(&|m| is_agy_claude_gpt(m) && has_window_suffix(m, "7d"))
            }
            (_, AutoSwitchMetric::AntigravityAllMax) => self
                .effective_max(now_secs)
                .or_else(|| AutoSwitchMetric::Max.effective_percent(usage, now_secs)),
            (_, AutoSwitchMetric::ClaudeSonnetSevenDay) => max_where(&|m| {
                m.key == "seven_day_sonnet" || m.label.to_ascii_lowercase().contains("sonnet")
            }),
            (_, AutoSwitchMetric::ClaudeOpusSevenDay) => max_where(&|m| {
                m.key == "seven_day_opus" || m.label.to_ascii_lowercase().contains("opus")
            }),
            (_, AutoSwitchMetric::CodexCodeReview) => max_where(&|m| {
                m.key.starts_with("review_") || m.label.eq_ignore_ascii_case("code review")
            }),
            (_, AutoSwitchMetric::OpenCodeThirtyDay) => {
                max_where(&|m| m.key == "monthly" || has_window_suffix(m, "30d"))
            }
            (_, AutoSwitchMetric::Max) => self
                .effective_max(now_secs)
                .or_else(|| metric.effective_percent(usage, now_secs)),
            (_, AutoSwitchMetric::FiveHour | AutoSwitchMetric::SevenDay) => {
                metric.effective_percent(usage, now_secs)
            }
        }
    }
}

/// The old terminal remains available until its replacement has been created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHandoff {
    pub from: SessionId,
    pub to: Option<SessionId>,
    pub account_id: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDetails {
    pub supported: bool,
    pub accounts: Vec<AccountMeters>,
    pub handoffs: Vec<SessionHandoff>,
}
