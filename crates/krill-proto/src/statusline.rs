//! Claude statusLine → 프로필별 `usage_cache.json` 기록(OWE-701).
//!
//! stdin JSON의 `rate_limits`를 `$CLAUDE_CONFIG_DIR/usage_cache.json`에 원자·0600으로 쓰고,
//! 사용자 statusLine 명령은 체인으로 실행해 그 출력을 그대로 돌려준다.
//! 사용자 statusLine 대체 정책은 오웬 결정에 따라 '체인(보존)'만 지원한다(덮어쓰기·무시 없음).
//!
//! 설계 참조(코드 복사 아님): `.ref/orca`의 `statusline-script.ts`(체인 없음·15초 간격),
//! `claude-statusline-rate-limits.ts`(15초·`>=2.1.80`에서 `rate_limits` 전달),
//! `claude-hook-event-versions.ts`(`statusLine` 키는 1.0.64부터, 미확인 버전은 하한으로 취급),
//! `hook-settings.ts`(단일 슬롯이라 사용자 소유면 건드리지 않음 — Krill은 체인으로 보존한다).
//!
//! 와이어 형식이 아니라 N-1 호환과 무관하다. 디스크 형식(`usage_cache.json`)은
//! 데몬 읽기 경로(`krilld::usage::parse_claude`)가 이미 아는 모양을 쓴다:
//! 최상위 `rate_limits`(stdin 원문 그대로)와 `cached_at`(epoch 초 문자열)만 두고,
//! 그 밖의 키는 건드리지 않는다.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::agent::hooks::shell_quote;
use crate::config_document::edit_json;
use crate::time::parse_reset_epoch;

pub mod context;

/// statusline이 쓰는 프로필별 사용량 캐시 파일 이름. 데몬 읽기 경로가 이미 아는 이름이다.
pub const CACHE_FILE: &str = "usage_cache.json";
/// 같은 값을 다시 쓰지 않는 최소 간격(초). Orca 클라이언트 하한(15초) 참조.
pub const DEDUP_SECS: i64 = 15;
/// `statusLine` 키를 아는 첫 Claude 버전. Orca 참조(`CLAUDE_STATUS_LINE_FIRST_VERSION`).
/// 이전 버전은 모르는 최상위 키에 `--settings` 파일 전체를 버릴 수 있어 주입하지 않는다.
pub const STATUS_LINE_MIN: (u32, u32, u32) = (1, 0, 64);
/// 체인 깊이 가드 환경 변수. 사용자 명령이 다시 `krill statusline`을 부르면 체인을 건너뛴다.
pub const DEPTH_ENV: &str = "KRILL_STATUSLINE_DEPTH";
/// statusline 실행 서브커맨드 이름(설치 명령 판별용).
pub const SUBCOMMAND: &str = "statusline";

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// `claude --version` 첫 줄에서 `X.Y.Z`를 읽는다. 접두사·접미사(`2.1.301 (Claude Code)`,
/// `2.1.80-beta`)를 허용하고, 없으면 `None`이다.
#[must_use]
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    text.split_whitespace().find_map(|token| {
        let mut parts = token.split('.');
        let major = leading_digits(parts.next()?)?;
        let minor = leading_digits(parts.next()?)?;
        let patch = leading_digits(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Some((major, minor, patch))
    })
}

/// 앞자리 숫자만 읽는다. 바이트 슬라이싱 없이 ASCII 숫자만 모아 파싱한다.
fn leading_digits(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// 이 버전에 statusLine을 주입해도 안전한가. `None`(프로브 실패)이나 파싱 실패는
/// 구버전의 증거가 아니라 주입한다(Orca `UNRESOLVED_CLAUDE_VERSION` 정책).
/// 파싱된 버전이 [`STATUS_LINE_MIN`] 미만일 때만 거짓이다.
#[must_use]
pub fn supports_status_line(version: Option<&str>) -> bool {
    version
        .and_then(parse_version)
        .is_none_or(|v| v >= STATUS_LINE_MIN)
}

/// statusline stdin JSON에서 `rate_limits` 객체를 꺼낸다. 없거나 객체가 아니면 `None`.
#[must_use]
pub fn extract_rate_limits(payload: &serde_json::Value) -> Option<&serde_json::Value> {
    payload.get("rate_limits").filter(|v| v.is_object())
}

/// `$CLAUDE_CONFIG_DIR/usage_cache.json` 경로.
#[must_use]
pub fn cache_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CACHE_FILE)
}

/// 지금 기록해야 하는가. 파일이 없거나·깨졌거나·값이 바뀌었거나·`cached_at`이
/// [`DEDUP_SECS`]보다 오래됐으면(미래 시각 포함) 참이다.
#[must_use]
pub fn should_record(
    existing: Option<&str>,
    rate_limits: &serde_json::Value,
    now_epoch: i64,
) -> bool {
    let Some(text) = existing else {
        return true;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return true;
    };
    if value.get("rate_limits") != Some(rate_limits) {
        return true;
    }
    let fresh = value
        .get("cached_at")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_reset_epoch)
        .and_then(|cached| now_epoch.checked_sub(cached))
        .is_some_and(|age| (0..DEDUP_SECS).contains(&age));
    !fresh
}

/// 캐시 파일 본문. 기존 객체의 모르는 키는 그대로 두고 `rate_limits`·`cached_at`만 둔다.
#[must_use]
pub fn render_cache(
    existing: Option<&str>,
    rate_limits: &serde_json::Value,
    now_epoch: i64,
) -> String {
    let mut document = existing
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(object) = document.as_object_mut() {
        object.insert("rate_limits".to_owned(), rate_limits.clone());
        object.insert(
            "cached_at".to_owned(),
            serde_json::Value::String(now_epoch.to_string()),
        );
    }
    let mut text = serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_owned());
    text.push('\n');
    text
}

/// 캐시 파일을 원자·0600으로 쓴다. 디렉터리가 없으면 만들지 않고 `Ok(false)`(기록 건너뜀).
/// 같은 디렉터리의 숨김 임시 파일에 0600으로 쓰고 fsync 뒤 rename한다.
pub fn write_cache_file(dir: &Path, text: &str) -> io::Result<bool> {
    write_named_cache_file(dir, CACHE_FILE, text)
}

fn write_named_cache_file(dir: &Path, name: &str, text: &str) -> io::Result<bool> {
    use crate::os::fs::OpenOptionsExt as _;
    use std::fs::OpenOptions;
    use std::io::Write as _;
    if !dir.is_dir() {
        return Ok(false);
    }
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(".{name}.tmp-{}-{seq}", std::process::id()));
    let target = dir.join(name);
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &target)?;
        Ok::<(), io::Error>(())
    })();
    if written.is_err() {
        std::fs::remove_file(&temp).ok();
        written?;
    }
    // Windows는 권한 비트가 없고 디렉터리를 `File::open`으로 열 수 없다.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))?;
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(true)
}

/// `settings.json`의 `statusLine` 슬롯 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// 슬롯이 비었다(키 없음·객체 아님·명령 문자열 없음). 설치하면 새로 둔다.
    Empty,
    /// Krill이 둔 명령이다. 설치는 그대로 두고, 제거는 receipt 원문으로 복원한다.
    Managed,
    /// 사용자 명령이다. 설치는 체인으로 감싸 보존한다.
    User,
}

/// Krill이 설치한 명령 모양. 우리 형식이 아니면 `Foreign`(사용자 소유)이다.
/// 깊이 가드(`DEPTH_ENV`)가 체인 재귀를 막으므로 의심 형식은 따로 두지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnCommand {
    /// `<exe> statusline`. 체인 없음.
    Bare,
    /// `<exe> statusline --chain <원본>`. `String`은 꺼낸 사용자 원본 명령이다.
    Chained(String),
    /// 사용자 소유. 체인 대상이다.
    Foreign,
}

/// 설치 명령(체인 없음).
#[must_use]
pub fn install_command(exe: &str) -> String {
    format!("{} {SUBCOMMAND}", shell_quote(exe))
}

/// 설치 명령(사용자 원본을 체인으로 보존).
#[must_use]
pub fn chain_command(exe: &str, user_command: &str) -> String {
    format!(
        "{} {SUBCOMMAND} --chain {}",
        shell_quote(exe),
        shell_quote(user_command)
    )
}

/// Krill 설치 명령을 구조로 판별한다. 엄격한 형식 일치만 우리 것으로 본다.
#[must_use]
pub fn classify_command(cmd: &str, exe: &str) -> OwnCommand {
    let quoted = shell_quote(exe);
    if cmd.trim() == format!("{quoted} {SUBCOMMAND}") {
        return OwnCommand::Bare;
    }
    let chained_prefix = format!("{quoted} {SUBCOMMAND} --chain ");
    if let Some(rest) = cmd.trim().strip_prefix(chained_prefix.as_str())
        && let Some(original) = decode_shell_arg(rest)
    {
        return OwnCommand::Chained(original);
    }
    OwnCommand::Foreign
}

/// `shell_quote`의 역함수. 단일 인자(`'...'`·`'\''` 이스케이프)만 받고,
/// 다시 감싸면 같아지는 엄격한 왕복 검사로 우리 형식을 확인한다.
fn decode_shell_arg(quoted: &str) -> Option<String> {
    let inner = quoted.strip_prefix('\'')?.strip_suffix('\'')?;
    if inner.contains('\'') && !inner.contains("'\"'\"'") {
        return None;
    }
    let decoded = inner.replace("'\"'\"'", "'");
    if shell_quote(&decoded) == quoted {
        Some(decoded)
    } else {
        None
    }
}

/// `settings.json` 전체에서 `statusLine` 값만 읽는다. 파일이 없으면 `None`,
/// 깨졌거나 객체가 아니면 오류(호출자는 쓰지 않는다).
pub fn read_slot(text: Option<&str>) -> io::Result<Option<serde_json::Value>> {
    let Some(text) = text else {
        return Ok(None);
    };
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| invalid(format!("settings.json 파싱 실패: {e}")))?;
    if !value.is_object() {
        return Err(invalid("settings.json은 객체여야 합니다"));
    }
    Ok(value.get("statusLine").cloned())
}

/// 슬롯 값의 상태. 객체 안 문자열 `command`만 본다.
#[must_use]
pub fn slot_state(slot: Option<&serde_json::Value>, exe: &str) -> SlotState {
    let command = slot
        .and_then(|slot| slot.get("command"))
        .and_then(serde_json::Value::as_str);
    match command {
        None => SlotState::Empty,
        Some(cmd) => match classify_command(cmd, exe) {
            OwnCommand::Foreign => SlotState::User,
            OwnCommand::Bare | OwnCommand::Chained(_) => SlotState::Managed,
        },
    }
}

/// 원문 보존 편집용 최소 스키마. 모르는 키는 투명하게 유지된다(`config_document`).
/// `None`은 직렬화에서 빠져 제거로 해석된다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct ClaudeSettings {
    #[serde(
        default,
        rename = "statusLine",
        skip_serializing_if = "Option::is_none"
    )]
    status_line: Option<serde_json::Value>,
}

fn empty_command_slot(command: &str) -> serde_json::Value {
    serde_json::json!({"type": "command", "command": command})
}

/// 설치 후 슬롯 값과 상태를 계산한다. 기존 객체의 모르는 키(예: `padding`)는 유지하고
/// `type`·`command`만 둔다. `Managed`이면 원문을 그대로 둔다(체인 소실 방지).
pub fn install_slot(slot: Option<&serde_json::Value>, exe: &str) -> (SlotState, serde_json::Value) {
    let state = slot_state(slot, exe);
    match state {
        SlotState::Managed => (state, slot.cloned().unwrap_or(serde_json::Value::Null)),
        SlotState::Empty | SlotState::User => {
            let command = match state {
                SlotState::User => {
                    let user_command = slot
                        .and_then(|slot| slot.get("command"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    chain_command(exe, user_command)
                }
                _ => install_command(exe),
            };
            let mut next = slot
                .filter(|slot| slot.is_object())
                .cloned()
                .unwrap_or_else(|| empty_command_slot(&command));
            if let Some(object) = next.as_object_mut() {
                object.insert(
                    "type".to_owned(),
                    serde_json::Value::String("command".to_owned()),
                );
                object.insert("command".to_owned(), serde_json::Value::String(command));
            }
            (state, next)
        }
    }
}

/// 설치 후 `settings.json` 전문. `Managed`이면 바이트 단위로 같은 원문이 돌아온다.
pub fn apply_install(before: Option<&str>, exe: &str) -> io::Result<(String, SlotState)> {
    let original = before.unwrap_or("{}");
    let slot = read_slot(Some(original))?;
    let (state, next_slot) = install_slot(slot.as_ref(), exe);
    if state == SlotState::Managed {
        return Ok((original.to_owned(), state));
    }
    let updated = ClaudeSettings {
        status_line: Some(next_slot),
    };
    edit_json(original, &updated).map(|text| (text, state))
}

/// 제거 후 `settings.json` 전문. 현재 슬롯이 설치 당시(`after`)와 같거나
/// 여전히 우리 형식이면 `before` 슬롯으로 복원하고, 사용자 소유면 오류로 보존한다.
/// 쓸 게 없으면 `None`이다.
pub fn apply_uninstall(
    current: Option<&str>,
    before: Option<&str>,
    after: &str,
    exe: &str,
) -> io::Result<Option<String>> {
    let Some(current_text) = current else {
        return Ok(None);
    };
    // 설치 전 파일이 없고 설치 후 그대로면 우리가 만든 파일이라 통째로 지운다(훅과 같은 기준).
    if before.is_none() && current_text == after {
        return Ok(None);
    }
    let current_slot = read_slot(Some(current_text))?;
    // 사용자가 슬롯을 통째로 지웠으면 opt-out으로 보고 건드리지 않는다.
    if current_slot.is_none() {
        return Ok(None);
    }
    let after_slot = read_slot(Some(after))?;
    // 설치 당시와 같거나, 여전히 정확히 우리 형식이면 우리 소유로 보고 복원한다.
    // 그 밖(사용자 소유·의심)은 보존하고 오류로 알린다.
    let command = current_slot
        .as_ref()
        .and_then(|slot| slot.get("command"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let owned = current_slot == after_slot
        || matches!(
            classify_command(command, exe),
            OwnCommand::Bare | OwnCommand::Chained(_)
        );
    if !owned {
        return Err(invalid(
            "statusLine이 Krill 설치 이후 바뀌었습니다; 덮어쓰지 않습니다",
        ));
    }
    let before_slot = match before {
        None => None,
        Some(text) => read_slot(Some(text))?,
    };
    let updated = ClaudeSettings {
        status_line: before_slot,
    };
    let next = edit_json(current_text, &updated)?;
    if next == current_text {
        return Ok(None);
    }
    Ok(Some(next))
}

#[cfg(test)]
mod tests;
