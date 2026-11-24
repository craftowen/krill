//! 음성 설정·자동 학습 단어장 영구 저장소(`voice.json`) 및 프롬프트 토큰 추출기.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::os::fs::OpenOptionsExt as _;
use krill_proto::voice::{DEFAULT_DESKTOP_SHORTCUT, VocabEntry, VoiceConfig, VoiceConfigPatch};

const VOICE_CONFIG_FILE: &str = "voice.json";
/// 디스크에 영구 저장하는 자동 학습 단어 상한.
pub const MAX_STORED_AUTO_VOCAB: usize = 500;
/// Pipito `/local/transcribe`로 한 번에 보내는 병합 단어장 상한.
pub const MAX_SENT_VOCAB: usize = 200;
/// 자동 학습 단어장 승격 최소 등장 횟수(세션/고정 단어 외).
const CORRECTION_BOOST: u32 = 3;

#[must_use]
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[derive(Debug)]
pub struct VocabStore {
    path: Option<PathBuf>,
    config: VoiceConfig,
    dirty: bool,
    last_flush_ms: u64,
}

impl VocabStore {
    #[must_use]
    pub fn load(state_dir: Option<&Path>) -> Self {
        let path = state_dir.map(|dir| dir.join(VOICE_CONFIG_FILE));
        let config = path
            .as_deref()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|raw| serde_json::from_str::<VoiceConfig>(&raw).ok())
            .map(sanitize_config)
            .unwrap_or_default();
        Self {
            path,
            config,
            dirty: false,
            last_flush_ms: 0,
        }
    }

    #[must_use]
    pub fn config(&self) -> VoiceConfig {
        self.config.clone()
    }

    #[must_use]
    pub fn refine_enabled(&self) -> bool {
        self.config.refine_enabled
    }

    #[must_use]
    pub fn vocab_count(&self) -> u32 {
        let total = self
            .config
            .pinned_vocab
            .len()
            .saturating_add(self.config.auto_vocab.len());
        u32::try_from(total).unwrap_or(u32::MAX)
    }

    pub fn apply_patch(&mut self, patch: VoiceConfigPatch) -> io::Result<VoiceConfig> {
        if let Some(refine) = patch.refine_enabled {
            self.config.refine_enabled = refine;
            self.dirty = true;
        }
        if let Some(shortcut) = patch.desktop_shortcut {
            let trimmed = shortcut.trim();
            if !trimmed.is_empty() && trimmed.len() <= 64 {
                trimmed.clone_into(&mut self.config.desktop_shortcut);
                self.dirty = true;
            }
        }
        if let Some(word) = patch.add_pinned
            && let Some(clean) = normalize_word(&word)
        {
            let lower = clean.to_lowercase();
            self.config
                .ignored_vocab
                .retain(|w| w.to_lowercase() != lower);
            if !self
                .config
                .pinned_vocab
                .iter()
                .any(|w| w.to_lowercase() == lower)
            {
                self.config.pinned_vocab.push(clean);
                if self.config.pinned_vocab.len() > MAX_STORED_AUTO_VOCAB {
                    self.config.pinned_vocab.truncate(MAX_STORED_AUTO_VOCAB);
                }
            }
            self.dirty = true;
        }
        if let Some(word) = patch.remove_pinned
            && let Some(clean) = normalize_word(&word)
        {
            let lower = clean.to_lowercase();
            self.config
                .pinned_vocab
                .retain(|w| w.to_lowercase() != lower);
            self.dirty = true;
        }
        if let Some(word) = patch.ignore_word
            && let Some(clean) = normalize_word(&word)
        {
            let lower = clean.to_lowercase();
            self.config
                .pinned_vocab
                .retain(|w| w.to_lowercase() != lower);
            self.config
                .auto_vocab
                .retain(|e| e.word.to_lowercase() != lower);
            if !self
                .config
                .ignored_vocab
                .iter()
                .any(|w| w.to_lowercase() == lower)
            {
                self.config.ignored_vocab.push(clean);
                if self.config.ignored_vocab.len() > MAX_STORED_AUTO_VOCAB {
                    self.config.ignored_vocab.truncate(MAX_STORED_AUTO_VOCAB);
                }
            }
            self.dirty = true;
        }
        if let Some(word) = patch.unignore_word
            && let Some(clean) = normalize_word(&word)
        {
            let lower = clean.to_lowercase();
            self.config
                .ignored_vocab
                .retain(|w| w.to_lowercase() != lower);
            self.dirty = true;
        }
        self.flush_now()?;
        Ok(self.config.clone())
    }

    /// 프롬프트나 저장소·봇 이름에서 고신호 토큰을 추출해 자동 단어장을 갱신한다.
    pub fn learn_text(&mut self, text: &str, weight: u32, now_ms: u64) -> usize {
        let tokens = extract_vocab_tokens(text);
        self.record_tokens(&tokens, weight, now_ms)
    }

    /// 이미 추출된 단어/이름 목록(저장소·워크트리·봇 이름 등)을 직접 학습한다.
    pub fn record_tokens(&mut self, tokens: &[String], weight: u32, now_ms: u64) -> usize {
        if tokens.is_empty() || weight == 0 {
            return 0;
        }
        let ignored: HashSet<String> = self
            .config
            .ignored_vocab
            .iter()
            .map(|w| w.to_lowercase())
            .collect();
        let mut index: HashMap<String, usize> = self
            .config
            .auto_vocab
            .iter()
            .enumerate()
            .map(|(i, e)| (e.word.to_lowercase(), i))
            .collect();

        let mut changed = 0usize;
        let mut seen_in_call = HashSet::new();
        for raw in tokens {
            let Some(clean) = normalize_word(raw) else {
                continue;
            };
            let lower = clean.to_lowercase();
            if ignored.contains(&lower) || !seen_in_call.insert(lower.clone()) {
                continue;
            }
            if let Some(&idx) = index.get(&lower) {
                if let Some(entry) = self.config.auto_vocab.get_mut(idx) {
                    entry.count = entry.count.saturating_add(weight);
                    entry.last_seen_ms = entry.last_seen_ms.max(now_ms);
                    // 대문자·카멜케이스가 들어오면 더 정보가 많은 표기를 우선한다.
                    if has_richer_casing(&clean, &entry.word) {
                        entry.word = clean;
                    }
                    changed = changed.saturating_add(1);
                }
            } else {
                let idx = self.config.auto_vocab.len();
                self.config.auto_vocab.push(VocabEntry {
                    word: clean,
                    count: weight,
                    last_seen_ms: now_ms,
                });
                index.insert(lower, idx);
                changed = changed.saturating_add(1);
            }
        }
        if changed > 0 {
            self.prune_and_sort(now_ms);
            self.dirty = true;
        }
        changed
    }

    /// 전송 전 사용자 수정(`before` → `after`)에서 교정된 단어를 즉시 부스트한다.
    pub fn boost_corrections(&mut self, before: &str, after: &str, now_ms: u64) -> Vec<String> {
        let corrected = extract_corrections(before, after);
        if corrected.is_empty() {
            return Vec::new();
        }
        self.record_tokens(&corrected, CORRECTION_BOOST, now_ms);
        self.flush_now().ok();
        corrected
    }

    /// `[session_vocab + pinned_vocab + top auto_vocab]`을 병합하고 `ignored_vocab`을 제외해 최대 200단어를 반환한다.
    #[must_use]
    pub fn merged_vocab(&self, session_vocab: &[String], now_ms: u64) -> Vec<String> {
        let ignored: HashSet<String> = self
            .config
            .ignored_vocab
            .iter()
            .map(|w| w.to_lowercase())
            .collect();
        let mut seen = HashSet::new();
        let mut out = Vec::with_capacity(MAX_SENT_VOCAB.min(64));

        let mut push_word = |word: &str| {
            if out.len() >= MAX_SENT_VOCAB {
                return;
            }
            let trimmed = word.trim();
            if trimmed.is_empty() {
                return;
            }
            let lower = trimmed.to_lowercase();
            if ignored.contains(&lower) || !seen.insert(lower) {
                return;
            }
            out.push(trimmed.to_owned());
        };

        for w in session_vocab {
            push_word(w);
        }
        for w in &self.config.pinned_vocab {
            push_word(w);
        }

        let mut ranked: Vec<(&VocabEntry, f64)> = self
            .config
            .auto_vocab
            .iter()
            .map(|entry| (entry, score_entry(entry, now_ms)))
            .collect();
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.0.count.cmp(&a.0.count))
                .then_with(|| a.0.word.cmp(&b.0.word))
        });

        for (entry, _) in ranked {
            push_word(&entry.word);
        }
        out
    }

    pub fn flush_if_due(&mut self, now_ms: u64, min_interval_ms: u64) {
        if !self.dirty {
            return;
        }
        if now_ms.saturating_sub(self.last_flush_ms) < min_interval_ms {
            return;
        }
        if self.flush_now().is_ok() {
            self.last_flush_ms = now_ms;
        }
    }

    pub fn flush_now(&mut self) -> io::Result<()> {
        let Some(path) = &self.path else {
            self.dirty = false;
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_vec_pretty(&self.config)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "voice config encode"))?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&payload)?;
            file.sync_all().ok();
        }
        fs::rename(&tmp, path)?;
        self.dirty = false;
        self.last_flush_ms = now_unix_ms();
        Ok(())
    }

    fn prune_and_sort(&mut self, now_ms: u64) {
        self.config.auto_vocab.sort_by(|a, b| {
            let sa = score_entry(a, now_ms);
            let sb = score_entry(b, now_ms);
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.count.cmp(&a.count))
                .then_with(|| a.word.cmp(&b.word))
        });
        if self.config.auto_vocab.len() > MAX_STORED_AUTO_VOCAB {
            self.config.auto_vocab.truncate(MAX_STORED_AUTO_VOCAB);
        }
    }
}

fn sanitize_config(mut cfg: VoiceConfig) -> VoiceConfig {
    if cfg.desktop_shortcut.trim().is_empty() {
        DEFAULT_DESKTOP_SHORTCUT.clone_into(&mut cfg.desktop_shortcut);
    }
    cfg.pinned_vocab.truncate(MAX_STORED_AUTO_VOCAB);
    cfg.ignored_vocab.truncate(MAX_STORED_AUTO_VOCAB);
    cfg.auto_vocab.truncate(MAX_STORED_AUTO_VOCAB);
    cfg
}

fn score_entry(entry: &VocabEntry, now_ms: u64) -> f64 {
    const SECS_PER_DAY: f64 = 86_400.0;
    let age_secs =
        u32::try_from(now_ms.saturating_sub(entry.last_seen_ms) / 1_000).unwrap_or(u32::MAX);
    let age_days = f64::from(age_secs) / SECS_PER_DAY;
    // 반감기 30일 감쇠 + 빈도 가중치
    let recency = 1.0 / (1.0 + age_days / 30.0);
    f64::from(entry.count) * (0.35 + 0.65 * recency)
}

fn has_richer_casing(candidate: &str, existing: &str) -> bool {
    let cand_upper = candidate.chars().filter(char::is_ascii_uppercase).count();
    let exist_upper = existing.chars().filter(char::is_ascii_uppercase).count();
    cand_upper > exist_upper
}

fn normalize_word(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches(|c: char| {
        matches!(
            c,
            '`' | '"'
                | '\''
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | ','
                | ';'
                | ':'
                | '!'
                | '?'
                | '<'
                | '>'
        )
    });
    if trimmed.is_empty() || trimmed.len() > 64 || trimmed.chars().any(char::is_control) {
        return None;
    }
    Some(trimmed.to_owned())
}

/// 프롬프트/대화 텍스트에서 도메인 고유어·기술 식별자·파일명·약어를 추출한다.
#[must_use]
pub fn extract_vocab_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for raw_piece in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
                    | '"'
                    | '\''
                    | '`'
                    | '|'
                    | '!'
                    | '?'
                    | '~'
                    | '='
                    | '+'
                    | '*'
                    | '&'
                    | '^'
                    | '%'
                    | '$'
                    | '#'
                    | '@'
                    | '\\'
            )
    }) {
        let piece = raw_piece.trim_matches(|c: char| matches!(c, '.' | ':' | '-' | '_' | '/'));
        if piece.is_empty() {
            continue;
        }
        // URL 및 프로토콜 제외
        let lower = piece.to_ascii_lowercase();
        if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("ws://")
            || lower.starts_with("wss://")
            || lower.starts_with("file://")
            || lower.contains("://")
        {
            continue;
        }

        // 경로 형태(`crates/krilld/src/voice/bridge.rs`)면 마지막 파일명과 의미 있는 세그먼트를 추출
        if piece.contains('/') {
            for seg in piece.split('/') {
                if let Some(tok) = classify_token(seg)
                    && seen.insert(tok.to_lowercase())
                {
                    out.push(tok);
                }
            }
            continue;
        }

        if let Some(tok) = classify_token(piece)
            && seen.insert(tok.to_lowercase())
        {
            out.push(tok);
        }
    }
    out
}

/// 전송 전 수정(`before` → `after`)에서 사용자가 교정한 토큰을 추출한다.
#[must_use]
pub fn extract_corrections(before: &str, after: &str) -> Vec<String> {
    let before_tokens: HashSet<String> = extract_vocab_tokens(before)
        .into_iter()
        .map(|t| t.to_lowercase())
        .collect();
    let before_raw: HashSet<String> = before
        .split_whitespace()
        .map(|s| {
            s.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
                .to_lowercase()
        })
        .filter(|s| !s.is_empty())
        .collect();

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for tok in extract_vocab_tokens(after) {
        let lower = tok.to_lowercase();
        if !before_tokens.contains(&lower) && !before_raw.contains(&lower) && seen.insert(lower) {
            out.push(tok);
        }
    }
    out
}

fn classify_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(|c: char| matches!(c, '.' | ':' | '-' | '_' | '/'));
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    // 순수 숫자·버전·긴 hex 해시 필터
    if trimmed.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    if is_long_hex_hash(trimmed) {
        return None;
    }

    let has_hangul = trimmed.chars().any(is_hangul);
    let has_ascii_alpha = trimmed.chars().any(|c| c.is_ascii_alphabetic());

    if has_hangul {
        let stripped = strip_korean_particle(trimmed);
        let char_count = stripped.chars().count();
        if !(2..=24).contains(&char_count) {
            return None;
        }
        if is_korean_stopword(stripped) {
            return None;
        }
        // 한영 혼합 도메인어(예: "크릴앱", "E2EE프레임", "PR리뷰") 또는 2자 이상 한글 도메인 명사
        return Some(stripped.to_owned());
    }

    if !has_ascii_alpha {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    if is_english_stopword(&lower) {
        return None;
    }

    // 파일명(확장자 포함: pwa_rpc.rs, Cargo.toml, bridge.json)
    if let Some((stem, ext)) = trimmed.rsplit_once('.')
        && !stem.is_empty()
        && (1..=6).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && is_code_extension(&ext.to_ascii_lowercase())
    {
        return Some(trimmed.to_owned());
    }

    // 기술 식별자: snake_case, kebab-case, camelCase, PascalCase, 영숫자 혼합(E2EE, v1, krilld)
    let is_snake_or_kebab = (trimmed.contains('_') || trimmed.contains('-')) && trimmed.len() >= 3;
    let is_camel_or_pascal = has_internal_case_transition(trimmed);
    let is_acronym = (2..=8).contains(&trimmed.len())
        && trimmed
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && trimmed.chars().any(|c| c.is_ascii_uppercase());
    let has_digit_and_alpha = trimmed.len() >= 2 && trimmed.chars().any(|c| c.is_ascii_digit());

    if is_snake_or_kebab || is_camel_or_pascal || is_acronym || has_digit_and_alpha {
        return Some(trimmed.to_owned());
    }

    // 고유/기술 단어(3자 이상, 불용어 제외)
    if trimmed.len() >= 3 && trimmed.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some(trimmed.to_owned());
    }

    None
}

fn is_long_hex_hash(s: &str) -> bool {
    let hex_part = s.strip_prefix("0x").unwrap_or(s);
    let hex_only: String = hex_part.chars().filter(|&c| c != '-').collect();
    hex_only.len() >= 8
        && hex_only.chars().all(|c| c.is_ascii_hexdigit())
        && hex_only.chars().any(|c| c.is_ascii_digit())
}

fn has_internal_case_transition(s: &str) -> bool {
    let mut prev_lower = false;
    let mut prev_upper = false;
    for (idx, ch) in s.chars().enumerate() {
        if idx > 0 && ch.is_ascii_uppercase() && prev_lower {
            return true;
        }
        if idx > 1 && ch.is_ascii_lowercase() && prev_upper {
            return true;
        }
        prev_lower = ch.is_ascii_lowercase();
        prev_upper = ch.is_ascii_uppercase();
    }
    false
}

const fn is_hangul(c: char) -> bool {
    matches!(c, '\u{AC00}'..='\u{D7A3}' | '\u{3131}'..='\u{318E}')
}

fn strip_korean_particle(word: &str) -> &str {
    const SUFFIXES: &[&str] = &[
        "에서는",
        "으로는",
        "까지는",
        "부터는",
        "처럼은",
        "에서도",
        "으로도",
        "까지는",
        "에서",
        "으로",
        "까지",
        "부터",
        "처럼",
        "보다",
        "이랑",
        "하고",
        "한테",
        "에게",
        "은",
        "는",
        "이",
        "가",
        "을",
        "를",
        "에",
        "로",
        "의",
        "와",
        "과",
        "도",
        "만",
        "랑",
    ];
    let chars = word.chars().count();
    for suffix in SUFFIXES {
        let s_chars = suffix.chars().count();
        if chars >= s_chars.saturating_add(2)
            && let Some(stem) = word.strip_suffix(suffix)
        {
            return stem;
        }
    }
    word
}

fn is_code_extension(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "js"
            | "cjs"
            | "mjs"
            | "ts"
            | "tsx"
            | "jsx"
            | "py"
            | "go"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "swift"
            | "kt"
            | "java"
            | "rb"
            | "sh"
            | "zsh"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
            | "md"
            | "html"
            | "css"
            | "sql"
            | "wav"
            | "svg"
            | "png"
            | "lock"
    )
}

fn is_english_stopword(lower: &str) -> bool {
    matches!(
        lower,
        "the"
            | "and"
            | "for"
            | "that"
            | "this"
            | "with"
            | "from"
            | "have"
            | "has"
            | "had"
            | "not"
            | "but"
            | "are"
            | "was"
            | "were"
            | "been"
            | "will"
            | "would"
            | "could"
            | "should"
            | "can"
            | "may"
            | "might"
            | "must"
            | "you"
            | "your"
            | "our"
            | "their"
            | "they"
            | "them"
            | "she"
            | "her"
            | "his"
            | "him"
            | "its"
            | "all"
            | "any"
            | "some"
            | "one"
            | "two"
            | "out"
            | "into"
            | "over"
            | "then"
            | "than"
            | "when"
            | "where"
            | "what"
            | "which"
            | "while"
            | "who"
            | "why"
            | "how"
            | "just"
            | "only"
            | "also"
            | "more"
            | "most"
            | "very"
            | "here"
            | "there"
            | "now"
            | "please"
            | "make"
            | "check"
            | "use"
            | "using"
            | "add"
            | "get"
            | "set"
            | "run"
            | "see"
            | "let"
            | "need"
            | "want"
            | "like"
            | "about"
            | "after"
            | "before"
            | "other"
            | "each"
            | "both"
            | "same"
            | "such"
            | "well"
            | "back"
            | "still"
            | "even"
            | "done"
            | "yes"
            | "true"
            | "false"
            | "null"
            | "none"
    )
}

fn is_korean_stopword(word: &str) -> bool {
    matches!(
        word,
        "이거"
            | "저거"
            | "그거"
            | "이것"
            | "저것"
            | "그것"
            | "여기"
            | "저기"
            | "거기"
            | "지금"
            | "오늘"
            | "내일"
            | "어제"
            | "그냥"
            | "일단"
            | "다시"
            | "계속"
            | "정말"
            | "진짜"
            | "너무"
            | "많이"
            | "조금"
            | "그리고"
            | "하지만"
            | "그래서"
            | "그런데"
            | "또는"
            | "혹은"
            | "때문"
            | "위해"
            | "대한"
            | "관련"
            | "부분"
            | "정도"
            | "경우"
            | "어떻게"
            | "무엇"
            | "언제"
            | "어디"
            | "누구"
            | "확인"
            | "진행"
            | "부탁"
            | "해줘"
            | "해주세요"
            | "합니다"
            | "하세요"
            | "입니다"
            | "있습니다"
            | "없습니다"
            | "됩니다"
            | "있다"
            | "없다"
            | "한다"
            | "하는"
            | "하고"
            | "하면"
            | "해서"
            | "에서"
            | "으로"
            | "까지"
            | "부터"
            | "바로"
            | "전부"
            | "모두"
            | "하나"
            | "우리"
            | "내가"
            | "제가"
    )
}
