//! 0ms 포커스 세션 경량 문맥 캐시.
//!
//! 받아쓰기 종료 핫 패스에서는 절대 디스크 스캔이나 `git` 서브프로세스를 동기 실행하지 않는다.
//! 세션·봇·그룹·항목이 포커스되거나 폴링/메시지 전송될 때 비동기적으로 캐시를 갱신하고,
//! `transcribe` 시점에는 메모리 스냅샷만 <1ms로 읽는다.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::voice::VoiceTargetKind;

use super::vocab::extract_vocab_tokens;

/// 캐시에 유지하는 최대 대상 수(메모리 상한).
const MAX_CACHED_TARGETS: usize = 64;
/// 대상당 보관하는 최근 대화 턴 수.
const MAX_STORED_TURNS: usize = 6;
/// `surrounding` 최대 UTF-16 코드 유닛 길이(Pipito 로컬 브리지 계약).
pub const MAX_SURROUNDING_UTF16: usize = 550;
/// `session_vocab` 최대 단어 수.
pub const MAX_SESSION_VOCAB: usize = 40;
/// 워크스페이스 Git 브랜치·변경 파일 비동기 갱신 최소 주기(TTL >= 20초).
pub const GIT_REFRESH_TTL: Duration = Duration::from_secs(20);
/// Git 변경 파일명 최대 보관 수.
const MAX_GIT_FILES: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContextKey {
    pub kind: VoiceTargetKind,
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct ContextSnapshot {
    pub surrounding: String,
    pub session_vocab: Vec<String>,
    pub app_category: &'static str,
}

#[derive(Debug, Clone)]
struct TargetEntry {
    turns: VecDeque<(String, String)>,
    surrounding: String,
    turn_vocab: Vec<String>,
    cwd: Option<PathBuf>,
    git_vocab: Vec<String>,
    git_refreshed_at: Option<Instant>,
    git_refreshing: bool,
    last_touched: Instant,
}

impl TargetEntry {
    fn new(now: Instant) -> Self {
        Self {
            turns: VecDeque::new(),
            surrounding: String::new(),
            turn_vocab: Vec::new(),
            cwd: None,
            git_vocab: Vec::new(),
            git_refreshed_at: None,
            git_refreshing: false,
            last_touched: now,
        }
    }

    fn rebuild_derived(&mut self) {
        self.surrounding = format_surrounding(&self.turns);
        let mut vocab = Vec::new();
        let mut seen = HashSet::new();
        // 최신 턴부터 토큰을 우선 추출한다.
        for (_, text) in self.turns.iter().rev() {
            for tok in extract_vocab_tokens(text) {
                if vocab.len() >= MAX_SESSION_VOCAB {
                    break;
                }
                if seen.insert(tok.to_lowercase()) {
                    vocab.push(tok);
                }
            }
            if vocab.len() >= MAX_SESSION_VOCAB {
                break;
            }
        }
        self.turn_vocab = vocab;
    }

    fn merged_session_vocab(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(MAX_SESSION_VOCAB);
        let mut seen = HashSet::new();
        for tok in self.git_vocab.iter().chain(self.turn_vocab.iter()) {
            if out.len() >= MAX_SESSION_VOCAB {
                break;
            }
            if seen.insert(tok.to_lowercase()) {
                out.push(tok.clone());
            }
        }
        out
    }
}

#[derive(Debug, Default)]
pub struct ContextCache {
    entries: RwLock<HashMap<ContextKey, TargetEntry>>,
}

impl ContextCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 0ms 핫 패스 조회: 디스크·서브프로세스 없이 메모리 스냅샷만 반환한다.
    #[must_use]
    pub fn snapshot(&self, kind: Option<VoiceTargetKind>, id: Option<&str>) -> ContextSnapshot {
        let resolved_kind = kind.unwrap_or(VoiceTargetKind::Session);
        let app_category = resolved_kind.app_category();
        let Some(target_id) = id.map(str::trim).filter(|s| !s.is_empty()) else {
            return ContextSnapshot {
                surrounding: String::new(),
                session_vocab: Vec::new(),
                app_category,
            };
        };
        let key = ContextKey {
            kind: resolved_kind,
            id: target_id.to_owned(),
        };
        let guard = self
            .entries
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.get(&key) {
            Some(entry) => ContextSnapshot {
                surrounding: entry.surrounding.clone(),
                session_vocab: entry.merged_session_vocab(),
                app_category,
            },
            None => ContextSnapshot {
                surrounding: String::new(),
                session_vocab: Vec::new(),
                app_category,
            },
        }
    }

    /// 대화 턴 하나를 기록하고 `surrounding`과 `turn_vocab`을 즉시 갱신한다.
    pub fn push_turn(
        &self,
        kind: VoiceTargetKind,
        id: &str,
        role: &str,
        raw_text: &str,
        cwd: Option<PathBuf>,
    ) {
        let cleaned = clean_turn_text(raw_text);
        if cleaned.is_empty() && cwd.is_none() {
            return;
        }
        let key = ContextKey {
            kind,
            id: id.trim().to_owned(),
        };
        if key.id.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut guard = self
            .entries
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        evict_if_full(&mut guard, &key);
        let entry = guard.entry(key).or_insert_with(|| TargetEntry::new(now));
        entry.last_touched = now;
        if let Some(dir) = cwd {
            entry.cwd = Some(dir);
        }
        if !cleaned.is_empty() {
            let duplicate = entry
                .turns
                .back()
                .is_some_and(|(r, t)| r == role && t == &cleaned);
            if !duplicate {
                entry.turns.push_back((role.to_owned(), cleaned));
                while entry.turns.len() > MAX_STORED_TURNS {
                    entry.turns.pop_front();
                }
                entry.rebuild_derived();
            }
        }
    }

    /// 최근 턴 목록 전체를 교체한다(예: `chat_poll` 직후 백그라운드 갱신).
    pub fn replace_turns(
        &self,
        kind: VoiceTargetKind,
        id: &str,
        turns: &[(String, String)],
        cwd: Option<PathBuf>,
    ) {
        let key = ContextKey {
            kind,
            id: id.trim().to_owned(),
        };
        if key.id.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut guard = self
            .entries
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        evict_if_full(&mut guard, &key);
        let entry = guard.entry(key).or_insert_with(|| TargetEntry::new(now));
        entry.last_touched = now;
        if let Some(dir) = cwd {
            entry.cwd = Some(dir);
        }
        entry.turns.clear();
        let start = turns.len().saturating_sub(MAX_STORED_TURNS);
        for (role, text) in turns.iter().skip(start) {
            let cleaned = clean_turn_text(text);
            if !cleaned.is_empty() {
                entry.turns.push_back((role.clone(), cleaned));
            }
        }
        entry.rebuild_derived();
    }

    /// 대상의 `cwd`를 갱신하고, Git 캐시 TTL(>= 20초)이 지났으면 백그라운드 스레드에서 브랜치·변경 파일을 읽는다.
    pub fn schedule_git_refresh(
        self: &Arc<Self>,
        kind: VoiceTargetKind,
        id: &str,
        cwd: Option<PathBuf>,
    ) {
        let key = ContextKey {
            kind,
            id: id.trim().to_owned(),
        };
        if key.id.is_empty() {
            return;
        }
        let now = Instant::now();
        let dir_to_scan = {
            let mut guard = self
                .entries
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            evict_if_full(&mut guard, &key);
            let entry = guard
                .entry(key.clone())
                .or_insert_with(|| TargetEntry::new(now));
            entry.last_touched = now;
            if let Some(dir) = cwd
                && entry.cwd.as_ref() != Some(&dir)
            {
                entry.cwd = Some(dir);
                entry.git_refreshed_at = None;
            }
            let stale = entry
                .git_refreshed_at
                .is_none_or(|t| now.duration_since(t) >= GIT_REFRESH_TTL);
            if stale && !entry.git_refreshing {
                if let Some(dir) = entry.cwd.clone() {
                    entry.git_refreshing = true;
                    Some(dir)
                } else {
                    None
                }
            } else {
                None
            }
        };

        let Some(dir) = dir_to_scan else {
            return;
        };
        let cache = Arc::clone(self);
        thread::Builder::new()
            .name("krill-voice-git".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || {
                let vocab = collect_git_vocab(&dir);
                let mut guard = cache
                    .entries
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(entry) = guard.get_mut(&key) {
                    entry.git_vocab = vocab;
                    entry.git_refreshed_at = Some(Instant::now());
                    entry.git_refreshing = false;
                }
            })
            .ok();
    }
}

fn evict_if_full(map: &mut HashMap<ContextKey, TargetEntry>, incoming: &ContextKey) {
    if map.len() < MAX_CACHED_TARGETS || map.contains_key(incoming) {
        return;
    }
    if let Some(oldest_key) = map
        .iter()
        .min_by_key(|(_, entry)| entry.last_touched)
        .map(|(k, _)| k.clone())
    {
        map.remove(&oldest_key);
    }
}

fn collect_git_vocab(dir: &PathBuf) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    if let Ok(branch_out) = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(dir)
        .output()
        && branch_out.status.success()
    {
        let branch = String::from_utf8_lossy(&branch_out.stdout)
            .trim()
            .to_owned();
        if !branch.is_empty() && branch != "HEAD" {
            if seen.insert(branch.to_lowercase()) {
                out.push(branch.clone());
            }
            for seg in branch.split('/') {
                if seg.len() >= 2 && seen.insert(seg.to_lowercase()) {
                    out.push(seg.to_owned());
                }
            }
        }
    }

    if let Ok(status_out) = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        && status_out.status.success()
    {
        let text = String::from_utf8_lossy(&status_out.stdout);
        for line in text.lines().take(MAX_GIT_FILES) {
            let path_part = line.get(3..).unwrap_or("").trim();
            let actual = path_part
                .rsplit_once(" -> ")
                .map_or(path_part, |(_, rhs)| rhs)
                .trim_matches('"');
            if let Some(file_name) = std::path::Path::new(actual)
                .file_name()
                .and_then(|s| s.to_str())
                && !file_name.is_empty()
                && seen.insert(file_name.to_lowercase())
            {
                out.push(file_name.to_owned());
            }
        }
    }
    out.truncate(MAX_SESSION_VOCAB / 2);
    out
}

/// ANSI 시퀀스, 대형 코드 펜스, 도구 원시 JSON을 정리해 컴팩트 텍스트로 만든다.
#[must_use]
pub fn clean_turn_text(raw: &str) -> String {
    let no_ansi = strip_ansi(raw);
    let mut out = String::with_capacity(no_ansi.len().min(512));
    let mut in_fence = false;
    let mut fence_added = false;

    for line in no_ansi.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_fence {
                in_fence = false;
            } else {
                in_fence = true;
                fence_added = false;
            }
            continue;
        }
        if in_fence {
            if !fence_added {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str("[code]");
                fence_added = true;
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        // 도구 원시 JSON 줄 제거
        if (trimmed.starts_with("{\"") && trimmed.ends_with('}'))
            || (trimmed.starts_with("[{\"") && trimmed.ends_with("}]"))
        {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(trimmed);
        if out.len() > 1024 {
            break;
        }
    }
    collapse_whitespace(&out)
}

fn format_surrounding(turns: &VecDeque<(String, String)>) -> String {
    if turns.is_empty() {
        return String::new();
    }
    // 최근 2~3턴을 선택한다.
    let take_count = turns.len().min(3);
    let start = turns.len().saturating_sub(take_count);
    let mut lines = Vec::with_capacity(take_count);
    for (role, text) in turns.iter().skip(start) {
        let label = match role.to_ascii_lowercase().as_str() {
            "user" | "human" => "U",
            "assistant" | "bot" | "agent" => "A",
            _ => role.as_str(),
        };
        let snippet = truncate_utf16(text, 200);
        lines.push(format!("{label}: {snippet}"));
    }
    let joined = lines.join("\n");
    truncate_utf16(&joined, MAX_SURROUNDING_UTF16)
}

#[must_use]
pub fn truncate_utf16(s: &str, max_units: usize) -> String {
    let mut out = String::new();
    let mut units = 0usize;
    for ch in s.chars() {
        let len = ch.len_utf16();
        if units.saturating_add(len) > max_units {
            break;
        }
        out.push(ch);
        units = units.saturating_add(len);
    }
    out
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    let _ = chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    let _ = chars.next();
                    let mut prev_esc = false;
                    for c in chars.by_ref() {
                        if c == '\x07' || (prev_esc && c == '\\') {
                            break;
                        }
                        prev_esc = c == '\x1b';
                    }
                }
                Some(_) => {
                    let _ = chars.next();
                }
                None => {}
            }
            continue;
        }
        if !ch.is_control() || matches!(ch, '\n' | '\t') {
            out.push(ch);
        }
    }
    out
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}
