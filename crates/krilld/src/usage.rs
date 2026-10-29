//! 사용량·레이트리밋 읽기(MK-084, T1#32). Claude·Codex 로컬 파일 읽기 전용 — 쓰지 않는다.
//!
//! - Claude: `~/.claude/statusline_cache.json`(cc-alchemy: `five_hour`/`seven_day`에
//!   `utilization`·`resets_at`) 또는 statusline stdin `rate_limits`
//!   (`five_hour`/`5_hour`·`seven_day`/`7_day`에 `used_percentage`·`resets_at`).
//!   멀티계정(`orca-usage-cache.json`)은 host 계정을 대표값으로 쓴다.
//! - Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`의 `rate_limits`
//!   (`primary` 300분 창 = 5시간, `secondary` 10080분 창 = 주간, `used_percent`).
//! - 80% 이상이면 [`Usage::warn`]이 참이다. 상태바 표시는 후속 이슈에서 붙는다.
//!
//! 로컬 파일 수집을 보완하는 계정별 자동 조회는 [`direct`]가 맡는다. 조회 전까지는 모름으로 둔다.
//! 자격증명·토큰은 읽기만 하며 토큰 갱신은 하지 않는다.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use krill_proto::time::parse_reset_epoch;

pub(crate) mod meters;

/// 경고 기준 사용률(상태바 80% 경고, MK-084 완료 조건).
pub const WARN_PERCENT: u8 = 80;
/// Claude 캐시 파일(`~/.claude` 안, 읽기 전용).
pub const CLAUDE_CACHE: &str = "statusline_cache.json";
/// Claude 멀티계정 캐시 파일(`~/.claude` 안, 읽기 전용).
pub const ORCA_CACHE: &str = "orca-usage-cache.json";
/// Codex rollout 꼬리에서 읽는 최대 바이트. 최신 `rate_limits` 줄은 파일 끝에 있다.
const TAIL_MAX: u64 = 256 * 1024;
/// Codex 5시간 창의 분(rollout `rate_limits.primary.window_minutes` 실측값).
const FIVE_HOUR_MINUTES: u64 = 300;
/// Codex 주간 창의 분(`rate_limits.secondary.window_minutes` 실측값).
const SEVEN_DAY_MINUTES: u64 = 10_080;

/// 한 창의 사용률. `None`이면 모름(파일 없음·깨짐·창 없음).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    /// 0~100. 범위를 벗어난 값은 잘라 낸다.
    pub percent: u8,
    /// 리셋 시각(원문 그대로: ISO 문자열 또는 epoch 초). 표시용이다.
    pub resets_at: Option<String>,
}

/// 5시간·주간 사용률.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

impl Usage {
    /// 둘 다 모르면 참.
    pub fn is_empty(&self) -> bool {
        self.five_hour.is_none() && self.seven_day.is_none()
    }

    /// 창 하나라도 [`WARN_PERCENT`] 이상이면 참(상태바 경고).
    pub fn warn(&self) -> bool {
        [self.five_hour.as_ref(), self.seven_day.as_ref()]
            .into_iter()
            .flatten()
            .any(|w| w.percent >= WARN_PERCENT)
    }
}

/// 계정 하나의 사용률(멀티계정 캐시용).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountUsage {
    /// Orca 캐시 행 `id`(uuid). 없으면 빈 문자열(구버전 캐시).
    pub id: String,
    pub label: String,
    /// Orca 캐시 행 `email`. `label`과 별도로 적혔을 때 병합·숨김 대조에 쓴다.
    pub email: Option<String>,
    pub host: bool,
    pub stale: bool,
    pub usage: Usage,
    /// Orca 행 `error` 원문(예: "재로그인"). 없으면 `None`.
    pub error: Option<String>,
    /// Orca 행 `detail` 원문(예: "token 400"). 표시용이다.
    pub detail: Option<String>,
    /// 캐시 파일 최상위 `updated_at`을 epoch 초로 푼 값. 없으면 `None`(호출자가 mtime으로 채운다).
    pub updated_at: Option<u64>,
}

/// 프로필 폴더에서 읽은 사용량 + 상태 근거(OWE-703 1단계).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileUsage {
    pub usage: Usage,
    pub stale: bool,
    /// 값을 읽은 파일의 수정 시각(epoch 초). 모르면 `None`.
    pub updated_at: Option<u64>,
    /// Orca 행 `error` 원문(프로필 파일이 없고 캐시 행에서 받았을 때만).
    pub error: Option<String>,
    /// Orca 행 `detail` 원문(표시용).
    pub detail: Option<String>,
}

/// 파일 수정 시각(epoch 초). 없으면 `None`(모름).
pub fn mtime_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// 0~100 정수로. 소수(Codex `used_percent` 74.0 등)는 반올림한다.
fn percent(v: &serde_json::Value) -> Option<u8> {
    if let Some(n) = v.as_u64() {
        return Some(u8::try_from(n).unwrap_or(u8::MAX).min(100));
    }
    let f = v.as_f64()?;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "0~100으로 clamp·round한 뒤라 잘림 없음"
    )]
    #[expect(
        clippy::cast_sign_loss,
        reason = "0 이상으로 clamp한 뒤라 부호 손실 없음"
    )]
    let n = f.clamp(0.0, 100.0).round() as u64;
    Some(u8::try_from(n).unwrap_or(u8::MAX).min(100))
}

/// 리셋 시각: 문자열은 그대로, 숫자(epoch 초)는 10진 문자열로. 표시용이라 정규화하지 않는다.
fn resets(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// 창 객체 하나. `utilization`(Claude 캐시) 또는 `used_percentage`(stdin `rate_limits`)를 읽는다.
fn window(v: &serde_json::Value, keys: &[&str]) -> Option<Window> {
    let w = keys.iter().find_map(|k| v.get(*k))?;
    let percent = percent(w.get("utilization").or_else(|| w.get("used_percentage"))?)?;
    Some(Window {
        percent,
        resets_at: w.get("resets_at").and_then(resets),
    })
}

/// Claude 캐시 또는 statusline stdin JSON을 읽는다. 깨졌으면 빈 값(모름).
pub fn parse_claude(s: &str) -> Usage {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
        return Usage::default();
    };
    let body = v.get("rate_limits").unwrap_or(&v);
    Usage {
        five_hour: window(body, &["five_hour", "5_hour"]),
        seven_day: window(body, &["seven_day", "7_day"]),
    }
}

/// 멀티계정 캐시(`orca-usage-cache.json`)를 읽는다. 깨졌으면 빈 목록.
/// 행의 `error`·`detail`·파일 최상위 `updated_at`도 함께 담는다(OWE-703).
pub fn parse_orca(s: &str) -> Vec<AccountUsage> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
        return Vec::new();
    };
    let updated_at = v.get("updated_at").and_then(parse_epoch_secs);
    let Some(rows) = v.get("accounts").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .map(|a| AccountUsage {
            id: a
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            label: a
                .get("label")
                .or_else(|| a.get("email"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            email: non_empty_str(a.get("email")),
            host: a
                .get("host")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            stale: a
                .get("stale")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            usage: Usage {
                five_hour: window(a, &["five_hour"]),
                seven_day: window(a, &["seven_day"]),
            },
            error: non_empty_str(a.get("error")),
            detail: non_empty_str(a.get("detail")),
            updated_at,
        })
        .collect()
}

/// 빈 문자열·공백은 `None`으로 둔다.
fn non_empty_str(v: Option<&serde_json::Value>) -> Option<String> {
    let s = v?.as_str()?;
    (!s.trim().is_empty()).then(|| s.to_owned())
}

/// epoch 초 숫자 또는 RFC 3339 문자열을 epoch 초로 푼다.
/// 13자리 이상 숫자는 밀리초로 보고 초로 나눈다(Orca `cooldown_until` 형식).
fn parse_epoch_secs(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => {
            let raw = n.as_u64()?;
            Some(if raw >= 10_000_000_000 {
                raw / 1_000
            } else {
                raw
            })
        }
        serde_json::Value::String(s) => parse_reset_epoch(s).and_then(|e| u64::try_from(e).ok()),
        _ => None,
    }
}

/// 대표 계정: host 우선, 없으면 사용률이 있는 첫 계정.
pub fn primary(accounts: &[AccountUsage]) -> Option<&AccountUsage> {
    accounts
        .iter()
        .find(|a| a.host)
        .or_else(|| accounts.iter().find(|a| !a.usage.is_empty()))
}

/// rollout 한 줄의 `rate_limits`를 읽는다. 창 분으로 5시간·주간을 가른다.
fn codex_windows(rl: &serde_json::Value) -> Usage {
    let mut out = Usage::default();
    for w in [rl.get("primary"), rl.get("secondary")] {
        let Some(w) = w else { continue };
        let minutes = w.get("window_minutes").and_then(serde_json::Value::as_u64);
        let slot = match minutes {
            Some(FIVE_HOUR_MINUTES) => &mut out.five_hour,
            Some(SEVEN_DAY_MINUTES) => &mut out.seven_day,
            _ => continue,
        };
        let Some(p) = w.get("used_percent").and_then(percent) else {
            continue;
        };
        *slot = Some(Window {
            percent: p,
            resets_at: w.get("resets_at").and_then(resets),
        });
    }
    out
}

/// rollout 한 줄을 읽는다. `rate_limits`가 없으면 `None`.
pub fn parse_codex_line(line: &str) -> Option<Usage> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let rl = v
        .get("rate_limits")
        .or_else(|| v.get("payload").and_then(|p| p.get("rate_limits")))?;
    let usage = codex_windows(rl);
    (!usage.is_empty()).then_some(usage)
}

/// rollout 파일 전체: 마지막 `rate_limits` 줄이 현재값이다.
pub fn parse_codex_rollout(text: &str) -> Option<Usage> {
    text.lines().rev().find_map(parse_codex_line)
}

/// `~/.claude`에서 읽는다. `statusline_cache.json` 우선, 없거나 비었으면 멀티계정 대표값.
pub fn read_claude(home: &Path) -> Option<Usage> {
    let dir = home.join(".claude");
    if let Ok(s) = std::fs::read_to_string(dir.join(CLAUDE_CACHE)) {
        let usage = parse_claude(&s);
        if !usage.is_empty() {
            return Some(usage);
        }
    }
    let s = std::fs::read_to_string(dir.join(ORCA_CACHE)).ok()?;
    primary(&parse_orca(&s)).map(|a| a.usage.clone())
}

/// 가장 새 rollout 파일을 찾는다(재귀, `rollout-*.jsonl`).
pub(crate) fn latest_rollout(sessions: &Path) -> Option<PathBuf> {
    let mut stack = vec![sessions.to_path_buf()];
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Some(dir) = stack.pop() {
        let Ok(rows) = std::fs::read_dir(&dir) else {
            continue;
        };
        for row in rows.flatten() {
            let Ok(ft) = row.file_type() else {
                continue;
            };
            let path = row.path();
            if ft.is_dir() {
                if row
                    .file_name()
                    .to_str()
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                {
                    stack.push(path);
                }
            } else if ft.is_file()
                && path
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|n| {
                        n.starts_with("rollout-")
                            && std::path::Path::new(n)
                                .extension()
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
                    })
            {
                let modified = row.metadata().and_then(|m| m.modified()).ok();
                let Some(modified) = modified else {
                    continue;
                };
                if best.as_ref().is_none_or(|(t, _)| modified >= *t) {
                    best = Some((modified, path));
                }
            }
        }
    }
    best.map(|(_, path)| path)
}

/// 파일 꼬리만 읽는다. 앞부분이 잘렸으면 첫 줄(불완전 가능)을 버린다.
fn read_tail(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut file = file;
    let start = len.saturating_sub(TAIL_MAX);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let slice = if start > 0 {
        bytes
            .iter()
            .position(|&b| b == b'\n')
            .and_then(|i| bytes.get(i + 1..))
            .unwrap_or_default()
    } else {
        bytes.as_slice()
    };
    Some(String::from_utf8_lossy(slice).into_owned())
}

/// 이 계정만 쓴 최신 rollout. 심링크로 공유한 `sessions`(가져오기·프로비저닝)나 계정 인계로
/// 하드링크된 rollout은 마지막에 쓴 계정을 알 수 없어 다른 계정의 사용량을 옮겨 오므로 쓰지 않는다.
/// ponytail: 다른 프로필이 심링크로 들어와 쓰는 실제 폴더(호스트 `~/.codex/sessions`)는 가려내지 못한다.
/// 그 폴더 값도 직접 조회 값이 없을 때(재시작 직후·조회 실패·직접 조회 끔)만 쓰인다.
pub(crate) fn own_rollout(sessions: &Path) -> Option<PathBuf> {
    if sessions.is_symlink() {
        return None;
    }
    let path = latest_rollout(sessions)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if std::fs::metadata(&path).ok()?.nlink() > 1 {
            return None;
        }
    }
    Some(path)
}

/// `~/.codex/sessions`에서 읽는다. 없거나 비었으면 `None`(모름). 읽기만 한다.
pub fn read_codex(sessions: &Path) -> Option<Usage> {
    let tail = read_tail(&own_rollout(sessions)?)?;
    parse_codex_rollout(&tail)
}

pub mod accounts;
pub mod direct;
pub mod provision;

#[cfg(test)]
#[cfg(unix)]
mod tests;
