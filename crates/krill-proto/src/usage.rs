//! 사용량·레이트리밋 조회 및 프로바이더 다계정·자동전환 제어(MK-084·MK-107·MK-108, T1#32).
//! Hello에서 [`FEAT_USAGE`]를 협상한 뒤 전용 연결에서 요청 하나·응답 하나.
//! 기본 창은 N-1 호환용 5시간·주간 요약이다. 선택 꼬리는 프로바이더가 보고한 모든 사용량 항목을 담는다.

use serde::{Deserialize, Serialize};

use crate::ipc::{self, Error};
use crate::screen::Msg;

/// 이 모듈의 kind를 쓸 수 있다는 feature 비트(ARCHITECTURE §4.3 등록표).
pub const FEAT_USAGE: u64 = 1 << 17;
/// Provider-specific meters and running-session account handoff results.
pub const FEAT_PROVIDER_DETAILS: u64 = 1 << 15;
mod details;
pub use details::{
    AccountMeters, ProviderDetails, RESET_CREDITS_METER_KEY, RESET_CREDITS_METER_LABEL,
    SessionHandoff, UsageMeter,
};
/// Create 계정 선택·고급 정책·이력·모델별 창(OWE-712 지휘자 배정).
pub const FEAT_ACCOUNT_POLICY: u64 = 1 << 61;
/// OWE-719: 연결 상태·읽기 전용 직접 사용량 설정 꼬리.
pub const FEAT_ACCOUNT_CONNECTION: u64 = 1 << 62;
mod connections;
pub use connections::{AccountConnection, ConnectionStatus, ConnectionsSnapshot};
/// Accounts replies may include the appended Antigravity provider variant.
pub const FEAT_USAGE_AGY: u64 = 1 << 53;
/// Account-specific auto-switch rule requests and reply extension.
pub const FEAT_USAGE_RULES: u64 = 1 << 54;
/// Per-account usage status tail (`AccountStatusInfo`, OWE-703 1단계).
/// `AccountsSnapshot` 꼬리의 rules 뒤에 붙는다.
pub const FEAT_USAGE_STATUS: u64 = 1 << 57;
/// `AccountStatus::RateLimited` 구분 수신(OWE-703 2단계 지시). 없으면 한도 도달을
/// 기존 표현(`Error`)으로 폴백한다.
pub const FEAT_USAGE_RATELIMIT: u64 = 1 << 60;
/// 계정 라벨 변경·데이터 삭제·고아 프로필 정리 요청과 [`UsageReply::Orphans`](OWE-704·705).
/// 협상하지 않은 데몬에는 보내지 않는다(구 데몬은 모르는 variant를 `BadRequest`로 거부).
pub const FEAT_USAGE_MANAGE: u64 = 1 << 59;
/// Codex banked rate-limit reset 크레딧 소진 요청·응답. 협상하지 않은 데몬에는 보내지 않는다
/// (구 데몬은 모르는 variant를 `BadRequest`로 거부한다).
pub const FEAT_USAGE_RESET: u64 = 1 << 49;
/// 수동 '사용량 새로고침' [`UsageRequest::RefreshUsage`]. 협상하지 않은 데몬에는 `ListAccounts`를 보낸다.
pub const FEAT_USAGE_REFRESH: u64 = 1 << 20;
/// 상태바 경고 기준 사용률(MK-084 완료 조건: 80% 경고). krilld와 UI가 같은 값을 쓴다.
pub const WARN_PERCENT: u8 = 80;
/// 상태바·계정 카드 위험 색 기준 사용률(OWE-708). [`WARN_PERCENT`] 이상은 경고, 이 값 이상은 위험.
pub const CRITICAL_PERCENT: u8 = 90;
/// 전역·계정별 자동전환 임계치 프리셋(OWE-708). 두 편집기가 같은 목록을 쓴다.
pub const AUTO_SWITCH_PRESETS: [u8; 5] = [70, 80, 85, 90, 95];
/// 기본 자동전환 임계치(MK-108: 90%).
pub const DEFAULT_AUTO_SWITCH_PERCENT: u8 = 90;
/// 자동전환 히스테리시스(K-14 왕복 방지). 후보는 활성 계정보다 이만큼(%p) 더 여유로워야 전환한다.
pub const AUTO_SWITCH_HYSTERESIS: u8 = 5;
/// 자동전환 임계값 범위(문서 규범 1~100, K-27). krilld·krill-app 평가 클램프가 공유한다.
pub const AUTO_SWITCH_THRESHOLD_MIN: u8 = 1;
/// 자동전환 임계값 범위(문서 규범 1~100, K-27). krilld·krill-app 평가 클램프가 공유한다.
pub const AUTO_SWITCH_THRESHOLD_MAX: u8 = 100;

pub mod kind {
    /// 클라이언트 → krilld: [`super::UsageRequest`].
    pub const USAGE_REQ: u8 = 0x41;
    /// krilld → 클라이언트: [`super::UsageReply`].
    pub const USAGE_REPLY: u8 = 0x61;
}

/// 한 창의 사용률. `None`이면 모름(파일 없음·깨짐·창 없음).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// 0~100.
    pub percent: u8,
    /// 리셋 시각(원문 그대로: ISO 문자열 또는 epoch 초). 표시용이다.
    pub resets_at: Option<String>,
}

/// 5시간·주간 사용률.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

impl Snapshot {
    /// 둘 다 모르면 참.
    pub fn is_empty(&self) -> bool {
        self.five_hour.is_none() && self.seven_day.is_none()
    }

    /// 창 하나라도 [`WARN_PERCENT`] 이상이면 참(상태바 경고).
    pub fn warn(&self) -> bool {
        self.max_percent() >= WARN_PERCENT
    }

    /// 5h·7d 중 더 높은 사용률(모르면 0). 모름을 0%로 보여주면 안 되는 곳(표시·판정)은
    /// [`AutoSwitchMetric::effective_percent`]·[`format_window_percent_ko`]를 쓴다.
    pub fn max_percent(&self) -> u8 {
        [self.five_hour.as_ref(), self.seven_day.as_ref()]
            .into_iter()
            .flatten()
            .map(|w| w.percent)
            .max()
            .unwrap_or(0)
    }
}

/// AI 코딩 에이전트 프로바이더 종류(MK-107). variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ProviderKind {
    Claude,
    Codex,
    /// Deprecated wire tombstone. Never create or use; keep its postcard position for N-1 peers.
    Gemini,
    OpenCode,
    Antigravity,
}

impl ProviderKind {
    /// Live providers only; `Gemini` remains solely as a postcard wire tombstone.
    pub const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::Antigravity, Self::OpenCode];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "",
            Self::OpenCode => "opencode",
            Self::Antigravity => "agy",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "removed",
            Self::OpenCode => "OpenCode",
            Self::Antigravity => "Antigravity",
        }
    }

    /// 에이전트 실행 시 프로필 격리에 쓰는 환경 변수 이름.
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CONFIG_DIR",
            Self::Codex => "CODEX_HOME",
            Self::Gemini => "",
            Self::OpenCode => "OPENCODE_CONFIG_DIR",
            Self::Antigravity => "ANTIGRAVITY_CLI_HOME",
        }
    }

    /// Environment overrides that would bypass the chosen account's credential authority.
    pub const fn credential_overrides(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &[
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "ANTHROPIC_BASE_URL",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
                "CLAUDECODE",
                "CLAUDE_CODE_ENTRYPOINT",
            ],
            Self::Codex => &["OPENAI_API_KEY", "CODEX_API_KEY", "OPENAI_BASE_URL"],
            Self::OpenCode => &[
                "OPENCODE_AUTH_CONTENT",
                "OPENCODE_DB",
                "OPENCODE_CONFIG_DIR",
                "OPENCODE_API_KEY",
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "GOOGLE_API_KEY",
                "GEMINI_API_KEY",
            ],
            Self::Antigravity | Self::Gemini => &[],
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claudecode" => Some(Self::Claude),
            "codex" | "openai" => Some(Self::Codex),
            "opencode" | "open-code" => Some(Self::OpenCode),
            "agy" | "antigravity" | "antigravity-cli" => Some(Self::Antigravity),
            _ => None,
        }
    }

    /// 실행 환경 변수 이름(`env_var`)에서 프로바이더를 푼다.
    /// 실행 당시 계정 기록(`LaunchAccount.env_key`) → 한도 기록 연결용(OWE-703 2단계 지시).
    #[must_use]
    pub fn from_env_var(name: &str) -> Option<Self> {
        match name.trim() {
            "CLAUDE_CONFIG_DIR" => Some(Self::Claude),
            "CODEX_HOME" => Some(Self::Codex),
            "OPENCODE_CONFIG_DIR" => Some(Self::OpenCode),
            "ANTIGRAVITY_CLI_HOME" => Some(Self::Antigravity),
            _ => None,
        }
    }
}

/// Antigravity CLI(`agy -p /usage --output-format json`) 사용량 봉투를 파싱해
/// 5시간(`5h`)·주간(`weekly`) 중 가장 제약이 큰 버킷으로 [`Snapshot`]을 만든다.
#[must_use]
pub fn parse_antigravity_usage(s: &str) -> Snapshot {
    let v = serde_json::from_str::<serde_json::Value>(s)
        .ok()
        .or_else(|| {
            s.lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .find(|v| {
                    v.pointer("/command/name")
                        .and_then(serde_json::Value::as_str)
                        == Some("usage")
                })
        });
    let Some(v) = v else {
        return Snapshot::default();
    };
    if v.get("status")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| s != "SUCCESS")
    {
        return Snapshot::default();
    }
    if let Some(cmd) = v.get("command")
        && let Some(name) = cmd.get("name").and_then(serde_json::Value::as_str)
        && name != "usage"
    {
        return Snapshot::default();
    }
    let groups = v
        .get("command")
        .and_then(|c| c.get("data"))
        .and_then(|d| d.get("groups"))
        .or_else(|| v.get("data").and_then(|d| d.get("groups")))
        .or_else(|| v.get("groups"))
        .and_then(serde_json::Value::as_array);
    let Some(groups) = groups else {
        return Snapshot::default();
    };
    let mut five_hour: Option<Window> = None;
    let mut seven_day: Option<Window> = None;

    for group in groups {
        let Some(buckets) = group.get("buckets").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for bucket in buckets {
            if bucket
                .get("disabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            let Some(win_kind) = bucket.get("window").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(rem) = bucket
                .get("remaining_fraction")
                .and_then(serde_json::Value::as_f64)
            else {
                continue;
            };
            if !rem.is_finite() {
                continue;
            }
            #[expect(
                clippy::as_conversions,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "0~100으로 clamp·round한 뒤라 안전"
            )]
            let used_pct = ((1.0 - rem.clamp(0.0, 1.0)) * 100.0).round() as u8;
            let resets_at = match bucket.get("reset_time").or_else(|| bucket.get("resets_at")) {
                Some(serde_json::Value::String(ts)) if !ts.trim().is_empty() => {
                    Some(ts.trim().to_owned())
                }
                Some(serde_json::Value::Number(n)) => Some(n.to_string()),
                _ => None,
            };
            let candidate = Window {
                percent: used_pct.min(100),
                resets_at,
            };
            let slot = match win_kind {
                "5h" | "five_hour" | "5_hour" => &mut five_hour,
                "weekly" | "7d" | "seven_day" | "7_day" => &mut seven_day,
                _ => continue,
            };
            let replace = match slot.as_ref() {
                None => true,
                Some(cur) if candidate.percent > cur.percent => true,
                Some(cur) if candidate.percent == cur.percent => {
                    match (&candidate.resets_at, &cur.resets_at) {
                        (Some(a), Some(b)) => a < b,
                        (Some(_), None) => true,
                        _ => false,
                    }
                }
                Some(_) => false,
            };
            if replace {
                *slot = Some(candidate);
            }
        }
    }

    Snapshot {
        five_hour,
        seven_day,
    }
}

/// 프로바이더 계정 한 개의 상태·사용량(MK-107).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub id: String,
    pub provider: ProviderKind,
    pub label: String,
    pub email: Option<String>,
    pub profile_dir: String,
    pub active: bool,
    pub host_default: bool,
    pub stale: bool,
    pub usage: Snapshot,
}

/// 계정 사용량 상태(OWE-703 1단계, P1-H). 모름은 0%가 아니다.
/// variant는 끝에만 붙인다. 와이어에서는 [`AccountsSnapshot::statuses`] 꼬리로
/// 오가며, 구버전 피어는 꼬리를 무시해 모름으로 본다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    /// 사용량 파일을 못 읽었거나 값이 없음. 0%로 취급 금지.
    #[default]
    Unknown,
    /// 캐시가 오래됨(Orca `stale`) — 값은 참고용이다.
    Stale,
    /// 로그인 필요(Orca `error`가 로그인 계열 문구).
    NeedsLogin,
    /// 그 밖의 조회 오류.
    Error,
    /// 정상.
    Ok,
    /// 사용량 한도 도달(StopFailure `rate_limit` 감지, OWE-710·OWE-703 2단계 지시).
    /// variant는 끝에만 붙인다. 감지 시각은 꼬리 `updated_at`에 든다.
    RateLimited,
}

impl AccountStatus {
    /// 상태 배지 짧은 한글 라벨(2단계 UI용 공개 API).
    pub const fn label_ko(self) -> &'static str {
        match self {
            Self::Unknown => "모름",
            Self::Stale => "오래됨",
            Self::NeedsLogin => "로그인 필요",
            Self::Error => "오류",
            Self::Ok => "정상",
            Self::RateLimited => "한도 도달",
        }
    }

    /// 접근성·툴팁용 한 줄 설명(2단계 UI용 공개 API).
    pub const fn describe_ko(self) -> &'static str {
        match self {
            Self::Unknown => "사용량을 알 수 없음",
            Self::Stale => "오래된 사용량 값",
            Self::NeedsLogin => "다시 로그인 필요",
            Self::Error => "사용량 조회 오류",
            Self::Ok => "정상",
            Self::RateLimited => "사용량 한도에 도달함",
        }
    }
}

/// 계정별 상태 꼬리 한 행. 와이어에서는 rules 꼬리 뒤에 붙고,
/// [`FEAT_USAGE_STATUS`] 없는 피어에게는 비워 보낸다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatusInfo {
    pub account_id: String,
    #[serde(default)]
    pub status: AccountStatus,
    /// 마지막 갱신 시각(epoch 초). 모르면 `None`.
    #[serde(default)]
    pub updated_at: Option<u64>,
    /// 오류·로그인 안내 원문(표시용). 없으면 `None`.
    #[serde(default)]
    pub detail: Option<String>,
}
/// 갱신 시각이 이보다 오래되면 흐리게 표시한다(2단계 UI 기준, 30분).
pub const STATUS_UPDATED_STALE_SECS: u64 = 30 * 60;

/// 갱신 시각 한글 상대 표기(2단계 UI용 공개 API). 모르면 "갱신 없음".
#[must_use]
pub fn format_updated_ago_ko(updated_at_secs: Option<u64>, now_secs: u64) -> String {
    let Some(updated) = updated_at_secs else {
        return "갱신 없음".to_owned();
    };
    let ago = now_secs.saturating_sub(updated);
    if ago < 10 {
        "방금 전".to_owned()
    } else if ago < 60 {
        format!("{ago}초 전")
    } else if ago < 3_600 {
        format!("{}분 전", ago / 60)
    } else if ago < 86_400 {
        format!("{}시간 전", ago / 3_600)
    } else {
        format!("{}일 전", ago / 86_400)
    }
}

/// 갱신 시각이 오래됐는지(모름 포함). 2단계 UI 흐리게 표시 기준.
#[must_use]
pub fn is_updated_stale(updated_at_secs: Option<u64>, now_secs: u64) -> bool {
    match updated_at_secs {
        None => true,
        Some(updated) => now_secs.saturating_sub(updated) > STATUS_UPDATED_STALE_SECS,
    }
}

/// 창 사용률 한글 표기(2단계 UI용 공개 API): 모름 "—", 리셋 지남 "리셋됨", 그 밖 "N%".
#[must_use]
pub fn format_window_percent_ko(window: Option<&Window>, now_secs: i64) -> String {
    let Some(w) = window else {
        return "—".to_owned();
    };
    let expired = w
        .resets_at
        .as_deref()
        .and_then(crate::time::parse_reset_epoch)
        .is_some_and(|epoch| now_secs >= epoch);
    if expired {
        "리셋됨".to_owned()
    } else {
        format!("{}%", w.percent.min(100))
    }
}

/// 계정별 자동전환 기준 창. 누락된 창의 사용률은 0이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutoSwitchMetric {
    #[default]
    Max,
    FiveHour,
    SevenDay,
}

impl AutoSwitchMetric {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Max => "Max(5h/7d)",
            Self::FiveHour => "5h",
            Self::SevenDay => "7d",
        }
    }

    pub const fn short_code(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::FiveHour => "5h",
            Self::SevenDay => "7d",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "max" | "Max" => Some(Self::Max),
            "5h" | "five_hour" => Some(Self::FiveHour),
            "7d" | "seven_day" => Some(Self::SevenDay),
            _ => None,
        }
    }

    pub fn evaluate(self, u: &Snapshot) -> u8 {
        match self {
            Self::Max => u.max_percent(),
            Self::FiveHour => u.five_hour.as_ref().map_or(0, |w| w.percent),
            Self::SevenDay => u.seven_day.as_ref().map_or(0, |w| w.percent),
        }
    }

    /// 유효 사용률. 모름·리셋 지남은 `None`이다(0% 취급 금지, OWE-703).
    /// 데몬 자동전환 판정과 2단계 UI가 같은 값을 쓴다.
    /// [`Self::evaluate`]는 구경로 호환용으로 모름을 0으로 접는다.
    #[must_use]
    pub fn effective_percent(self, usage: &Snapshot, now_secs: i64) -> Option<u8> {
        let value = |window: Option<&Window>| {
            let w = window?;
            let expired = w
                .resets_at
                .as_deref()
                .and_then(crate::time::parse_reset_epoch)
                .is_some_and(|epoch| now_secs >= epoch);
            (!expired).then_some(w.percent)
        };
        match self {
            Self::Max => match (
                value(usage.five_hour.as_ref()),
                value(usage.seven_day.as_ref()),
            ) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            },
            Self::FiveHour => value(usage.five_hour.as_ref()),
            Self::SevenDay => value(usage.seven_day.as_ref()),
        }
    }
}

/// 특정 계정에서 다른 계정으로 전환하는 정책. 비활성 규칙은 전역 정책보다 우선한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoSwitchRule {
    pub provider: ProviderKind,
    pub from_account_id: String,
    pub enabled: bool,
    pub threshold_percent: u8,
    #[serde(default)]
    pub metric: AutoSwitchMetric,
    #[serde(default)]
    pub target_account_id: Option<String>,
}

/// 사용률 임계치·레이트리밋 기반 자동전환 정책(MK-108).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoSwitchPolicy {
    /// 기본 꺼짐. 사용자가 명시적으로 켜야 전역 자동전환을 시작한다.
    pub enabled: bool,
    /// 5h 또는 7d 사용률이 이 값(1~100) 이상이면 가용 최저 사용률 계정으로 전환.
    pub threshold_percent: u8,
    /// 레이트리밋(100%) 도달 시 즉시 전환.
    pub switch_on_rate_limit: bool,
    #[serde(default)]
    pub rules: Vec<AutoSwitchRule>,
}

impl Default for AutoSwitchPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_percent: DEFAULT_AUTO_SWITCH_PERCENT,
            switch_on_rate_limit: true,
            rules: Vec::new(),
        }
    }
}

impl AutoSwitchPolicy {
    pub fn rule_for(
        &self,
        provider: ProviderKind,
        from_account_id: &str,
    ) -> Option<&AutoSwitchRule> {
        self.rules
            .iter()
            .find(|r| r.provider == provider && r.from_account_id == from_account_id)
    }

    pub fn upsert_rule(&mut self, mut rule: AutoSwitchRule) {
        // K-27: 문서(1~100)와 같은 범위로 자른다. 50~99 고정은 사용자 설정(30% 등)을 말없이 바꿔버린다.
        rule.threshold_percent = rule
            .threshold_percent
            .clamp(AUTO_SWITCH_THRESHOLD_MIN, AUTO_SWITCH_THRESHOLD_MAX);
        if let Some(existing) = self
            .rules
            .iter_mut()
            .find(|r| r.provider == rule.provider && r.from_account_id == rule.from_account_id)
        {
            *existing = rule;
        } else {
            self.rules.push(rule);
        }
    }

    pub fn remove_rule(&mut self, provider: ProviderKind, from_account_id: &str) -> bool {
        let before = self.rules.len();
        self.rules
            .retain(|r| r.provider != provider || r.from_account_id != from_account_id);
        self.rules.len() != before
    }
}

/// 자동전환이 일어났을 때의 기록(MK-108).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoSwitchEvent {
    pub provider: ProviderKind,
    pub from_id: String,
    pub from_label: String,
    pub to_id: String,
    pub to_label: String,
    pub reason_percent: u8,
}

/// 다계정·자동전환 전체 스냅샷(MK-107·MK-108).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsSnapshot {
    pub accounts: Vec<AccountInfo>,
    pub auto_switch: AutoSwitchPolicy,
    pub switched: Vec<AutoSwitchEvent>,
    /// 계정별 상태·갱신 시각 꼬리(OWE-703). 와이어에서는 rules 꼬리 뒤에 붙고
    /// [`FEAT_USAGE_STATUS`] 없는 피어에게는 비워 보낸다.
    #[serde(default)]
    pub statuses: Vec<AccountStatusInfo>,
    /// OWE-712: 기능 비트 61로 협상한 메시지 마지막 꼬리.
    #[serde(default)]
    pub advanced: AccountPolicyState,
    #[serde(default)]
    pub connections: Box<ConnectionsSnapshot>,
    #[serde(default)]
    pub details: Box<ProviderDetails>,
}

impl AccountsSnapshot {
    pub fn meters_for(&self, account_id: &str) -> Option<&AccountMeters> {
        self.details
            .accounts
            .iter()
            .find(|a| a.account_id == account_id)
    }

    pub fn connection_for(&self, account_id: &str) -> ConnectionStatus {
        self.connections
            .accounts
            .iter()
            .find(|a| a.account_id == account_id)
            .map_or(ConnectionStatus::Checking, |a| a.status)
    }

    /// 꼬리 상태 조회. 없으면 `None`이다(호출자는 모름으로 표시).
    #[must_use]
    pub fn status_for(&self, account_id: &str) -> Option<&AccountStatusInfo> {
        self.statuses.iter().find(|s| s.account_id == account_id)
    }
}

/// [`kind::USAGE_REQ`]. variant는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsageRequest {
    Get,
    ListAccounts,
    SwitchAccount {
        provider: ProviderKind,
        account_id: String,
    },
    AddAccount {
        provider: ProviderKind,
        label: String,
        email: Option<String>,
        profile_dir: String,
    },
    RemoveAccount {
        provider: ProviderKind,
        account_id: String,
    },
    SetAutoSwitch(AutoSwitchPolicy),
    UpsertAutoSwitchRule(AutoSwitchRule),
    RemoveAutoSwitchRule {
        provider: ProviderKind,
        from_account_id: String,
    },
    /// 라벨(과 선택적으로 이메일)만 바꾼다. id는 그대로다(OWE-705). 이메일이 확인된 Claude 임시 이름
    /// 폴더(`claude-code-profile-n`)는 세션이 쓰지 않으면 krilld가 이메일 이름으로 옮긴다.
    /// `email`이 `None`이면 기존 값을 유지한다. [`FEAT_USAGE_MANAGE`].
    RenameAccount {
        provider: ProviderKind,
        account_id: String,
        label: String,
        email: Option<String>,
    },
    /// 등록을 지우고 Krill이 만든 프로필 폴더를 휴지통으로 옮긴다(OWE-704). 그 폴더로 실행된
    /// 세션이 남아 있으면 `Os(EBUSY)`로 거부한다. 관리 루트 밖 폴더는 건드리지 않는다. [`FEAT_USAGE_MANAGE`].
    DeleteAccount {
        provider: ProviderKind,
        account_id: String,
    },
    /// 등록되지 않은 Krill 프로필 폴더 목록 → [`UsageReply::Orphans`]. [`FEAT_USAGE_MANAGE`].
    ListOrphanProfiles,
    /// 고아 프로필 폴더(세션이 쓰는 것 제외)를 휴지통으로 옮기고 남은 목록을 돌려준다. [`FEAT_USAGE_MANAGE`].
    PurgeOrphanProfiles,
    /// OWE-712: 고급 정책만 변경한다. 기존 정책·규칙은 그대로 둔다.
    SetAdvancedPolicy(AdvancedPolicy),
    /// 읽기 전용 OAuth 사용량 조회 on/off.
    SetDirectUsage(bool),
    /// OWE-722: 프로필 공유 opt-in 설정. 미출시 관리 기능에 포함해 새 비트를 쓰지 않는다.
    /// [`FEAT_USAGE_MANAGE`]를 협상한 피어에만 보낸다.
    SetProvisioning(bool),
    /// 프로비저닝 설정 조회 → [`UsageReply::Provisioning`].
    GetProvisioning,
    /// Read Orca's native Antigravity account vault on this execution host.
    /// Credentials are materialized by the daemon and never returned over IPC.
    ImportOrcaAntigravity {
        source_dir: String,
        dry_run: bool,
    },
    /// Codex banked reset 크레딧 1개를 소진해 해당 계정의 사용량 창을 초기화한다 →
    /// [`UsageReply::ResetConsumed`]. [`FEAT_USAGE_RESET`]을 협상한 피어에만 보낸다.
    ConsumeResetCredit {
        provider: ProviderKind,
        account_id: String,
    },
    /// 사용자가 누른 사용량 새로고침 → [`UsageReply::Accounts`]. `ListAccounts`와 달리 조회 주기·
    /// Retry-After를 무시하고 바로 다시 조회한다. [`FEAT_USAGE_REFRESH`]를 협상한 피어에만 보낸다.
    RefreshUsage,
}

impl UsageRequest {
    /// Old peers may still send the retired Gemini provider discriminant; ignore those mutations.
    #[must_use]
    pub fn contains_removed_provider(&self) -> bool {
        let retired = |provider: ProviderKind| provider == ProviderKind::Gemini;
        match self {
            Self::SwitchAccount { provider, .. }
            | Self::AddAccount { provider, .. }
            | Self::RemoveAccount { provider, .. }
            | Self::RemoveAutoSwitchRule { provider, .. }
            | Self::RenameAccount { provider, .. }
            | Self::DeleteAccount { provider, .. }
            | Self::ConsumeResetCredit { provider, .. } => retired(*provider),
            Self::SetAutoSwitch(policy) => policy.rules.iter().any(|rule| retired(rule.provider)),
            Self::UpsertAutoSwitchRule(rule) => retired(rule.provider),
            _ => false,
        }
    }

    /// 로컬 폴백이 허용되는 읽기 전용 요청인가. 변이 요청은 데몬 거부가
    /// 확정적이므로 로컬 쓰기로 덮어쓰지 않는다(AS-03).
    #[must_use]
    pub fn is_read(&self) -> bool {
        matches!(
            self,
            Self::Get
                | Self::ListAccounts
                | Self::RefreshUsage
                | Self::ListOrphanProfiles
                | Self::GetProvisioning
                | Self::ImportOrcaAntigravity { dry_run: true, .. }
        )
    }

    pub fn required_features(&self) -> u64 {
        let mut features = FEAT_USAGE;
        let provider = match self {
            Self::ImportOrcaAntigravity { .. } => {
                features |= FEAT_PROVIDER_DETAILS;
                Some(ProviderKind::Antigravity)
            }
            Self::SetDirectUsage(_) => {
                features |= FEAT_ACCOUNT_CONNECTION;
                None
            }
            Self::SetAdvancedPolicy(_) => {
                features |= FEAT_ACCOUNT_POLICY;
                None
            }
            Self::SwitchAccount { provider, .. }
            | Self::AddAccount { provider, .. }
            | Self::RemoveAccount { provider, .. } => Some(*provider),
            Self::ConsumeResetCredit { provider, .. } => {
                features |= FEAT_USAGE_RESET;
                Some(*provider)
            }
            Self::RefreshUsage => {
                features |= FEAT_USAGE_REFRESH;
                None
            }
            Self::RenameAccount { provider, .. } | Self::DeleteAccount { provider, .. } => {
                features |= FEAT_USAGE_MANAGE;
                Some(*provider)
            }
            Self::ListOrphanProfiles
            | Self::PurgeOrphanProfiles
            | Self::SetProvisioning(_)
            | Self::GetProvisioning => {
                features |= FEAT_USAGE_MANAGE;
                None
            }
            Self::UpsertAutoSwitchRule(rule) => {
                features |= FEAT_USAGE_RULES;
                Some(rule.provider)
            }
            Self::RemoveAutoSwitchRule { provider, .. } => {
                features |= FEAT_USAGE_RULES;
                Some(*provider)
            }
            Self::SetAutoSwitch(policy) if !policy.rules.is_empty() => {
                features |= FEAT_USAGE_RULES;
                policy
                    .rules
                    .iter()
                    .any(|rule| rule.provider == ProviderKind::Antigravity)
                    .then_some(ProviderKind::Antigravity)
            }
            _ => None,
        };
        if provider == Some(ProviderKind::Antigravity) {
            features |= FEAT_USAGE_AGY;
        }
        features
    }
}

/// Codex banked rate-limit reset 소진 결과. `wham/rate-limit-reset-credits/consume` 응답의
/// `code` 그대로다(`reset`·`nothing_to_reset`·`no_credit`·`already_redeemed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResetConsumeOutcome {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
}

/// [`kind::USAGE_REPLY`]. variant는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsageReply {
    Current(Snapshot),
    Failed(crate::control::Failure),
    Accounts(AccountsSnapshot),
    /// [`UsageRequest::ListOrphanProfiles`]·[`UsageRequest::PurgeOrphanProfiles`] 응답. 요청한 피어만 받는다.
    Orphans(Vec<OrphanProfile>),
    /// [`UsageRequest::GetProvisioning`]·[`UsageRequest::SetProvisioning`] 응답.
    Provisioning(bool),
    /// [`UsageRequest::ConsumeResetCredit`] 응답. 소진 결과와 소진 직후 스냅샷을 함께 돌려준다.
    ResetConsumed {
        snapshot: AccountsSnapshot,
        outcome: ResetConsumeOutcome,
        windows_reset: u64,
    },
}

/// 등록되지 않은 Krill 관리 프로필 폴더(OWE-704). 이전 버전의 "제거"가 남긴 폴더 등.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanProfile {
    pub provider: ProviderKind,
    pub dir: String,
}

impl UsageReply {
    /// Hide extensions that the peer cannot decode; retain the N-1 response prefix.
    #[must_use]
    pub fn for_features(mut self, features: u64) -> Self {
        if features & FEAT_USAGE_RESET == 0
            && let Self::ResetConsumed { snapshot, .. } = self
        {
            // 소진 결과를 모르는 피어에는 스냅샷만 남긴다(요청 자체가 새 비트를 요구한다).
            self = Self::Accounts(snapshot);
        }
        if let Self::Accounts(snapshot) = &mut self {
            if features & FEAT_PROVIDER_DETAILS == 0 {
                *snapshot.details = ProviderDetails::default();
            }
            snapshot
                .accounts
                .retain(|account| account.provider != ProviderKind::Gemini);
            snapshot
                .switched
                .retain(|event| event.provider != ProviderKind::Gemini);
            snapshot
                .auto_switch
                .rules
                .retain(|rule| rule.provider != ProviderKind::Gemini);
            if features & FEAT_ACCOUNT_CONNECTION == 0 {
                *snapshot.connections = ConnectionsSnapshot::default();
            }
            if features & FEAT_ACCOUNT_POLICY == 0 {
                snapshot.advanced = AccountPolicyState::default();
            }
            if features & FEAT_USAGE_AGY == 0 {
                snapshot
                    .accounts
                    .retain(|account| account.provider != ProviderKind::Antigravity);
                snapshot
                    .switched
                    .retain(|event| event.provider != ProviderKind::Antigravity);
                snapshot
                    .auto_switch
                    .rules
                    .retain(|rule| rule.provider != ProviderKind::Antigravity);
            }
            if features & FEAT_USAGE_RULES == 0 {
                snapshot.auto_switch.rules.clear();
            }
            if features & FEAT_USAGE_STATUS == 0 {
                snapshot.statuses.clear();
            } else if features & FEAT_USAGE_RATELIMIT == 0 {
                // 한도 구분을 모르는 피어(1단계)에는 기존 표현으로 폴백한다.
                for s in &mut snapshot.statuses {
                    if s.status == AccountStatus::RateLimited {
                        s.status = AccountStatus::Error;
                        if s.detail.is_none() {
                            s.detail = Some("rate_limit".to_owned());
                        }
                    }
                }
            }
        }
        self
    }
}

impl Msg for UsageRequest {
    const KIND: u8 = kind::USAGE_REQ;

    fn encode(&self) -> Result<Vec<u8>, Error> {
        if let Self::SetAutoSwitch(policy) = self {
            let mut body = postcard::to_stdvec(&(
                5_u8,
                policy.enabled,
                policy.threshold_percent,
                policy.switch_on_rate_limit,
            ))
            .map_err(Error::Codec)?;
            append_rules(&mut body, &policy.rules)?;
            ipc::encode_frame(Self::KIND, &body)
        } else {
            ipc::encode(Self::KIND, self)
        }
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, Error> {
        if kind == Self::KIND && body.first() == Some(&5) {
            let ((_, enabled, threshold_percent, switch_on_rate_limit), tail) =
                postcard::take_from_bytes::<(u8, bool, u8, bool)>(body).map_err(Error::Codec)?;
            Ok(Self::SetAutoSwitch(AutoSwitchPolicy {
                enabled,
                threshold_percent,
                switch_on_rate_limit,
                rules: decode_rules(tail)?,
            }))
        } else {
            ipc::decode(Self::KIND, kind, body)
        }
    }
}
impl Msg for UsageReply {
    const KIND: u8 = kind::USAGE_REPLY;

    fn encode(&self) -> Result<Vec<u8>, Error> {
        if let Self::Accounts(snapshot) = self {
            let policy = &snapshot.auto_switch;
            // Keep the N-1 policy's three fields followed by `switched` unchanged.
            // Rules and statuses share one optional message tail (a tuple of two
            // length-prefixed lists) so older decoders can ignore them:
            // rules-aware decoders read the first list and skip the rest,
            // older ones skip the whole tail. Empty tail keeps legacy bytes.
            // Statuses follow the rules tail under FEAT_USAGE_STATUS (OWE-703).
            let mut body = postcard::to_stdvec(&(
                2_u8,
                &snapshot.accounts,
                (
                    policy.enabled,
                    policy.threshold_percent,
                    policy.switch_on_rate_limit,
                ),
                &snapshot.switched,
            ))
            .map_err(Error::Codec)?;
            append_tails(&mut body, &policy.rules, &snapshot.statuses)?;
            if snapshot.advanced != AccountPolicyState::default()
                || *snapshot.connections != ConnectionsSnapshot::default()
                || *snapshot.details != ProviderDetails::default()
            {
                if policy.rules.is_empty() && snapshot.statuses.is_empty() {
                    body.extend([0, 0]); // 빈 rules·statuses 목록 두 개
                }
                body.extend(postcard::to_stdvec(&snapshot.advanced).map_err(Error::Codec)?);
                if *snapshot.connections != ConnectionsSnapshot::default()
                    || *snapshot.details != ProviderDetails::default()
                {
                    body.extend(postcard::to_stdvec(&snapshot.connections).map_err(Error::Codec)?);
                }
                if *snapshot.details != ProviderDetails::default() {
                    body.extend(postcard::to_stdvec(&snapshot.details).map_err(Error::Codec)?);
                }
            }
            ipc::encode_frame(Self::KIND, &body)
        } else {
            ipc::encode(Self::KIND, self)
        }
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, Error> {
        if kind == Self::KIND && body.first() == Some(&2) {
            type LegacyAccounts = (u8, Vec<AccountInfo>, (bool, u8, bool), Vec<AutoSwitchEvent>);
            let ((_, accounts, (enabled, threshold_percent, switch_on_rate_limit), switched), tail) =
                postcard::take_from_bytes::<LegacyAccounts>(body).map_err(Error::Codec)?;
            let (rules, rest) = take_rules(tail)?;
            let (statuses, rest) = if rest.is_empty() {
                (Vec::new(), rest)
            } else {
                postcard::take_from_bytes::<Vec<AccountStatusInfo>>(rest).map_err(Error::Codec)?
            };
            let (advanced, rest) = if rest.is_empty() {
                (AccountPolicyState::default(), rest)
            } else {
                postcard::take_from_bytes::<AccountPolicyState>(rest).map_err(Error::Codec)?
            };
            let (connections, rest) = if rest.is_empty() {
                (ConnectionsSnapshot::default(), rest)
            } else {
                postcard::take_from_bytes::<ConnectionsSnapshot>(rest).map_err(Error::Codec)?
            };
            let details = if rest.is_empty() {
                ProviderDetails::default()
            } else {
                postcard::take_from_bytes::<ProviderDetails>(rest)
                    .map_err(Error::Codec)?
                    .0
            };
            Ok(Self::Accounts(AccountsSnapshot {
                accounts,
                auto_switch: AutoSwitchPolicy {
                    enabled,
                    threshold_percent,
                    switch_on_rate_limit,
                    rules,
                },
                switched,
                statuses,
                advanced,
                connections: Box::new(connections),
                details: Box::new(details),
            }))
        } else {
            ipc::decode(Self::KIND, kind, body)
        }
    }
}

fn append_rules(body: &mut Vec<u8>, rules: &[AutoSwitchRule]) -> Result<(), Error> {
    if !rules.is_empty() {
        body.extend(postcard::to_stdvec(rules).map_err(Error::Codec)?);
    }
    Ok(())
}

/// 응답 꼬리(rules + 상태)를 한 튜플로 붙인다. 둘 다 비어 있으면 바이트를
/// 쓰지 않아 구버전 바이트와 같아진다. 튜플이라 rules 인식 디코더는 첫 목록만
/// 읽고 뒤를 무시한다.
fn append_tails(
    body: &mut Vec<u8>,
    rules: &[AutoSwitchRule],
    statuses: &[AccountStatusInfo],
) -> Result<(), Error> {
    if rules.is_empty() && statuses.is_empty() {
        return Ok(());
    }
    body.extend(postcard::to_stdvec(&(rules, statuses)).map_err(Error::Codec)?);
    Ok(())
}

fn decode_rules(tail: &[u8]) -> Result<Vec<AutoSwitchRule>, Error> {
    take_rules(tail).map(|(rules, _)| rules)
}

/// rules 꼬리를 읽고 남은 바이트를 돌려준다. 비어 있으면 빈 규칙이다.
fn take_rules(tail: &[u8]) -> Result<(Vec<AutoSwitchRule>, &[u8]), Error> {
    if tail.is_empty() {
        Ok((Vec::new(), tail))
    } else {
        postcard::take_from_bytes(tail).map_err(Error::Codec)
    }
}

pub mod policy;
pub mod profiles;
pub use policy::*;

#[cfg(test)]
mod tests;
