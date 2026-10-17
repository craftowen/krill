//! 서명된 에이전트 프로필·화면 규칙(MK-091, T4-37).
//!
//! 앱 업데이트 없이 규칙을 교체하는 배포 채널의 검증·적용 부분이다. 이번 이슈는 로컬
//! 서명 규칙 파일의 검증·적용까지이며 외부 서버 fetch는 없다. 서명이 틀리거나 파일이
//! 손상됐으면 거부하고 호출자는 내장 규칙을 유지한다.
//!
//! # 파일 형식
//!
//! ```text
//! [u32 LE payload_len][payload TOML 바이트][64바이트 Ed25519 서명]
//! ```
//!
//! 서명 대상은 payload 바이트 전체다. payload는 `agents.toml`과 같은 `[agent.<id>]`
//! 표에 `[screen]` 표를 더한 TOML이다. 모르는 키(전역 `permission` 포함)는 무시한다.
//! 전역 권한은 서명 파일로 바꾸지 않는다. 로컬 토글이 우선이다.
//!
//! 새 IPC 기능 비트는 쓰지 않는다. 서명 프로필은 기존 `AgentSettings` 조회에 그대로
//! 담기고, 서명 화면 규칙은 기존 보조 감지 경로가 그대로 쓴다.

use std::collections::BTreeMap;
use std::fmt;

use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

use super::{Agents, Profile};

/// 서명 규칙 파일 이름(상태 디렉터리 아래).
pub const RULES_FILE_NAME: &str = "agent-rules.signed";
/// 신뢰 앵커(공개키 hex 64자) 파일 이름. 없으면 서명 규칙을 쓰지 않고 내장 규칙만 쓴다.
pub const PUBLIC_KEY_FILE_NAME: &str = "agent-rules.pub";
/// 서명 파일 전체 상한.
pub const MAX_SIGNED_BYTES: usize = 64 * 1024;
/// 페이로드(TOML) 상한.
pub const MAX_PAYLOAD_BYTES: usize = 56 * 1024;
/// 서명 규칙이 더하는 프로필 수 상한.
pub const MAX_SIGNED_PROFILES: usize = crate::control::MAX_AGENT_PROFILES;
/// 규칙 문자열(문구·접두·id·이름) 바이트 상한.
pub const MAX_RULE_TEXT: usize = crate::control::MAX_AGENT_TEXT;
/// 규칙 목록(문구·접두) 항목 수 상한.
pub const MAX_RULE_ENTRIES: usize = 64;
/// 글리프 목록 항목 수 상한.
pub const MAX_GLYPH_ENTRIES: usize = 32;
/// 지원하는 페이로드 버전. 다른 버전은 거부한다.
pub const SUPPORTED_VERSION: u32 = 1;

/// Ed25519 공개키(32바이트).
pub type PublicKey = [u8; 32];
/// Ed25519 서명(64바이트).
pub type Signature = [u8; 64];

/// 서명 규칙을 읽지 못한 이유. 파일 내용은 담지 않는다(로그 리댁션).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesError {
    /// [`MAX_SIGNED_BYTES`]·[`MAX_PAYLOAD_BYTES`]를 넘는다.
    TooLarge,
    /// 길이·서명·공개키가 짧거나 남는 바이트가 있다.
    Truncated,
    /// 서명이 공개키·페이로드와 맞지 않는다.
    BadSignature,
    /// TOML이 깨졌거나 UTF-8이 아니다.
    BadPayload,
    /// [`SUPPORTED_VERSION`]이 아니다.
    UnsupportedVersion,
    /// 프로필·문구 수나 길이가 상한을 넘는다.
    OverLimit,
}

impl fmt::Display for RulesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("signed rules exceed size limit"),
            Self::Truncated => f.write_str("signed rules truncated"),
            Self::BadSignature => f.write_str("signed rules signature mismatch"),
            Self::BadPayload => f.write_str("signed rules payload unreadable"),
            Self::UnsupportedVersion => f.write_str("signed rules version unsupported"),
            Self::OverLimit => f.write_str("signed rules exceed entry limit"),
        }
    }
}

impl std::error::Error for RulesError {}

/// 화면 메뉴 판정. `krilld`의 `AgentState`로 옮기기 전 단계다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuHit {
    Blocked,
    Waiting,
}

/// 화면 규칙. 기본값은 MK-049의 하드코딩과 같은 내장 규칙이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenRules {
    /// 선택 항목과 함께 있으면 멈춤(승인 메뉴·trust)으로 보는 문구(소문자 비교).
    #[serde(default = "default_blocked_phrases")]
    pub blocked_phrases: Vec<String>,
    /// 선택 항목과 함께 있으면 대기(진행 확인)로 보는 문구(소문자 비교).
    #[serde(default = "default_waiting_phrases")]
    pub waiting_phrases: Vec<String>,
    /// 모두 있어야 대기 조건이 되는 문구 쌍(소문자 비교). 비어 있으면 쓰지 않는다.
    #[serde(default = "default_waiting_combo")]
    pub waiting_combo: Vec<String>,
    /// 메뉴 선택 줄 접두(앞 공백·`│┃` 제거 뒤, 대소문자 그대로).
    #[serde(default = "default_select_prefixes")]
    pub select_prefixes: Vec<String>,
    /// 대화상자가 사라지고 남은 프롬프트 줄(양끝 공백 제거 뒤, 정확히 일치).
    #[serde(default = "default_prompt_lines")]
    pub prompt_lines: Vec<String>,
    /// 완료 글리프(구분 공백 뒤).
    #[serde(default = "default_done_glyph")]
    pub done_glyph: char,
    /// 작업 중 글리프.
    #[serde(default = "default_working_glyphs")]
    pub working_glyphs: Vec<char>,
    /// 스피너 글리프. 본문을 기억해 스피너가 사라지면 완료로 본다.
    #[serde(default = "default_spinner_glyphs")]
    pub spinner_glyphs: Vec<char>,
    /// 스피너 본문 앞 구분자(Codex `⠋ | …`의 `|`).
    #[serde(default = "default_spinner_separators")]
    pub spinner_separators: Vec<char>,
}

fn default_blocked_phrases() -> Vec<String> {
    [
        "trust this folder",
        "trust the files",
        "trust this workspace",
        "do you trust the contents of this project",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn default_waiting_phrases() -> Vec<String> {
    [
        "do you want to proceed",
        "would you like to run",
        "allow this",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn default_waiting_combo() -> Vec<String> {
    ["enter to select", "esc to cancel"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn default_select_prefixes() -> Vec<String> {
    [
        "❯ 1.", "› 1.", "> 1.", "1. Yes", "1. Allow", "❯ Yes", "› Yes", "> Yes", "❯ No", "› No",
        "> No",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn default_prompt_lines() -> Vec<String> {
    ["❯", "›", ">"].into_iter().map(str::to_owned).collect()
}

fn default_done_glyph() -> char {
    '✳'
}

fn default_working_glyphs() -> Vec<char> {
    ['◐', '◑', '◒', '◓'].to_vec()
}

fn default_spinner_glyphs() -> Vec<char> {
    ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'].to_vec()
}

fn default_spinner_separators() -> Vec<char> {
    ['|'].to_vec()
}

impl Default for ScreenRules {
    fn default() -> Self {
        Self {
            blocked_phrases: default_blocked_phrases(),
            waiting_phrases: default_waiting_phrases(),
            waiting_combo: default_waiting_combo(),
            select_prefixes: default_select_prefixes(),
            prompt_lines: default_prompt_lines(),
            done_glyph: default_done_glyph(),
            working_glyphs: default_working_glyphs(),
            spinner_glyphs: default_spinner_glyphs(),
            spinner_separators: default_spinner_separators(),
        }
    }
}

impl ScreenRules {
    /// 메뉴 선택 항목이 있고 멈춤·대기 문구가 있으면 판정한다. 설명 문장 한 줄만으로는
    /// 상태를 바꾸지 않는다(MK-049와 같은 규칙).
    #[must_use]
    pub fn menu_hit(&self, screen: &str) -> Option<MenuHit> {
        let selected = screen.lines().any(|line| {
            let line = line.trim_start_matches(|c: char| c.is_whitespace() || c == '│' || c == '┃');
            self.select_prefixes
                .iter()
                .any(|prefix| line.starts_with(prefix.as_str()))
        });
        if !selected {
            return None;
        }
        let lower = screen.to_lowercase();
        let contains = |phrase: &String| lower.contains(&phrase.to_lowercase());
        if self.blocked_phrases.iter().any(contains) {
            Some(MenuHit::Blocked)
        } else if self.waiting_phrases.iter().any(contains)
            || (!self.waiting_combo.is_empty() && self.waiting_combo.iter().all(contains))
        {
            Some(MenuHit::Waiting)
        } else {
            None
        }
    }

    /// 대화상자가 사라지고 프롬프트만 남았는가.
    #[must_use]
    pub fn is_prompt(&self, screen: &str) -> bool {
        screen.lines().any(|line| {
            let trimmed = line.trim();
            self.prompt_lines.iter().any(|prompt| trimmed == prompt)
                || trimmed.starts_with("> Accept-edits mode:")
        })
    }
}

/// 서명 페이로드. `[agent.<id>]`는 [`Agents`]와 같은 모양이다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RulesPayload {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub screen: ScreenRules,
    #[serde(default, rename = "agent")]
    pub profiles: BTreeMap<String, Profile>,
}

impl RulesPayload {
    /// 내장 프로필에 서명 규칙을 겹친다. 같은 id는 서명 규칙이 통째로 이긴다(사용자
    /// 파일은 그 위에 겹친다). 전역 `permission`은 서명 파일에 있어도 바꾸지 않는다.
    pub fn apply(&self, agents: &mut Agents) {
        agents.profiles.extend(self.profiles.clone());
    }
}

/// `public` 공개키로 `payload`의 Ed25519 `signature`를 검증한다.
#[must_use]
pub fn verify(public: &PublicKey, payload: &[u8], signature: &Signature) -> bool {
    UnparsedPublicKey::new(&ED25519, public)
        .verify(payload, signature)
        .is_ok()
}

/// 서명 파일 바이트를 만든다. 배포 도구·테스트용이다.
pub fn pack(payload: &[u8], signature: &Signature) -> Result<Vec<u8>, RulesError> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(RulesError::TooLarge);
    }
    let len = u32::try_from(payload.len()).map_err(|_| RulesError::TooLarge)?;
    let mut out = Vec::with_capacity(payload.len().saturating_add(68));
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(signature);
    Ok(out)
}

/// 서명 파일 바이트를 검증하고 페이로드를 읽는다. 실패하면 호출자는 내장 규칙을 유지한다.
pub fn load_signed(public: &PublicKey, bytes: &[u8]) -> Result<RulesPayload, RulesError> {
    if bytes.len() > MAX_SIGNED_BYTES {
        return Err(RulesError::TooLarge);
    }
    let (len_bytes, rest) = bytes
        .split_first_chunk::<4>()
        .ok_or(RulesError::Truncated)?;
    let len = usize::try_from(u32::from_le_bytes(*len_bytes)).map_err(|_| RulesError::TooLarge)?;
    if len > MAX_PAYLOAD_BYTES {
        return Err(RulesError::TooLarge);
    }
    let payload = rest.get(..len).ok_or(RulesError::Truncated)?;
    let tail = rest.get(len..).ok_or(RulesError::Truncated)?;
    if tail.len() != 64 {
        return Err(RulesError::Truncated);
    }
    let signature: &Signature = tail.first_chunk().ok_or(RulesError::Truncated)?;
    if !verify(public, payload, signature) {
        return Err(RulesError::BadSignature);
    }
    let text = str::from_utf8(payload).map_err(|_| RulesError::BadPayload)?;
    let parsed: RulesPayload = toml::from_str(text).map_err(|_| RulesError::BadPayload)?;
    check_limits(&parsed)?;
    if parsed.version != SUPPORTED_VERSION {
        return Err(RulesError::UnsupportedVersion);
    }
    Ok(parsed)
}

fn check_limits(payload: &RulesPayload) -> Result<(), RulesError> {
    if payload.profiles.len() > MAX_SIGNED_PROFILES {
        return Err(RulesError::OverLimit);
    }
    let text_ok = |s: &String| s.len() <= MAX_RULE_TEXT;
    if payload
        .profiles
        .iter()
        .any(|(id, profile)| !text_ok(id) || !text_ok(&profile.name))
    {
        return Err(RulesError::OverLimit);
    }
    let screen = &payload.screen;
    let lists = [
        &screen.blocked_phrases,
        &screen.waiting_phrases,
        &screen.waiting_combo,
        &screen.select_prefixes,
        &screen.prompt_lines,
    ];
    if lists
        .iter()
        .any(|list| list.len() > MAX_RULE_ENTRIES || list.iter().any(|s| !text_ok(s)))
    {
        return Err(RulesError::OverLimit);
    }
    if screen.working_glyphs.len() > MAX_GLYPH_ENTRIES
        || screen.spinner_glyphs.len() > MAX_GLYPH_ENTRIES
        || screen.spinner_separators.len() > MAX_GLYPH_ENTRIES
    {
        return Err(RulesError::OverLimit);
    }
    Ok(())
}

/// 신뢰 앵커 파일 내용(hex 64자, 앞뒤 공백 허용)을 공개키로 읽는다.
#[must_use]
pub fn decode_public_key(text: &str) -> Option<PublicKey> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let raw = text.as_bytes();
    let (chunks, _) = raw.as_chunks::<2>();
    for (slot, pair) in out.iter_mut().zip(chunks) {
        let [hi, lo] = *pair;
        *slot = (nibble(hi)? << 4) | nibble(lo)?;
    }
    Some(out)
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.saturating_sub(b'0')),
        b'a'..=b'f' => Some(byte.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(byte.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
