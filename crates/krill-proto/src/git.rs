//! MK-053 변경 목록. 경로는 git의 NUL 구분 출력 그대로(비 UTF-8·탭·개행 보존).

use serde::{Deserialize, Serialize};

/// 목록 상한. 도달하면 git 자식을 종료·회수하고 `limited`를 표시한다.
pub const MAX_FILES: usize = 1_000;
/// 경로 합계 상한. 1MiB 프레임 안에 두 경로와 numstat을 담을 공간을 남긴다.
pub const MAX_PATH_BYTES: usize = 512 * 1024;

/// MK-088 / OWE-738(MK-184): 파일 단위 index 변경, 커밋, 원격 동기화(Push / Pull / Sync / Commit & Push).
/// variant는 끝에만 추가한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edit {
    Stage {
        path: Vec<u8>,
        old_path: Option<Vec<u8>>,
    },
    Unstage {
        path: Vec<u8>,
        old_path: Option<Vec<u8>>,
    },
    Commit {
        message: String,
    },
    Push,
    Pull,
    Sync,
    CommitAndPush {
        message: String,
    },
}

impl Edit {
    /// 원격 동기화(`FEAT_GIT_SYNC`) 협상이 필요한 변형인지 반환한다.
    #[must_use]
    pub const fn requires_sync(&self) -> bool {
        matches!(
            self,
            Self::Push | Self::Pull | Self::Sync | Self::CommitAndPush { .. }
        )
    }

    /// 이 편집 요청을 보내거나 수락할 때 요구되는 IPC 기능 비트 마스크.
    #[must_use]
    pub const fn required_features(&self) -> u64 {
        let base = crate::control::FEAT_CONTROL | crate::control::FEAT_GIT_EDIT;
        if self.requires_sync() {
            base | crate::ipc::FEAT_GIT_SYNC
        } else {
            base
        }
    }
}

/// 커밋 메시지 UTF-8 바이트 상한.
pub const MAX_MESSAGE: usize = 16 * 1024;

/// 텍스트는 줄 수, 바이너리는 둘 다 `None`(`git --numstat`의 `-`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Numstat {
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: Vec<u8>,
    /// rename/copy 원래 경로.
    pub old_path: Option<Vec<u8>>,
    /// porcelain v2 XY. 미추적은 `??`, 충돌은 git의 XY 그대로.
    pub status: [u8; 2],
    pub staged: Option<Numstat>,
    pub unstaged: Option<Numstat>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    #[serde(deserialize_with = "files")]
    pub files: Vec<ChangedFile>,
    /// 개수·경로 바이트 상한에 도달했다. 전체 목록이라는 뜻이 아니다.
    pub limited: bool,
}

// Enforce collection/path bounds on incoming replies as well as outgoing git output.
fn files<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ChangedFile>, D::Error> {
    struct Files;
    impl<'de> serde::de::Visitor<'de> for Files {
        type Value = Vec<ChangedFile>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a bounded git change list")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error as _;
            if seq.size_hint().is_some_and(|n| n > MAX_FILES) {
                return Err(A::Error::custom("too many changed files"));
            }
            let mut files = Vec::new();
            let mut bytes = 0usize;
            while let Some(file) = seq.next_element::<ChangedFile>()? {
                if files.len() >= MAX_FILES {
                    return Err(A::Error::custom("too many changed files"));
                }
                bytes = bytes
                    .checked_add(file.path.len())
                    .and_then(|n| n.checked_add(file.old_path.as_ref().map_or(0, Vec::len)))
                    .filter(|n| *n <= MAX_PATH_BYTES)
                    .ok_or_else(|| A::Error::custom("git paths exceed byte limit"))?;
                files.push(file);
            }
            Ok(files)
        }
    }
    d.deserialize_seq(Files)
}

/// OWE-585(MK-053): `FSEvents` 변경 감시 요청.
/// `visible == false`면 해당 저장소 감시를 숨김으로 전환해 진행 중인 git 자식을 즉시 취소·회수한다.
/// `visible == true`면 감시를 가시 상태로 두고 `poll()`을 한 번 진행해 `since` 이후 새 세대가 있으면 반환한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitWatchRequest {
    pub repo: Vec<u8>,
    pub visible: bool,
    pub since: u64,
    pub force: bool,
}

/// OWE-585(MK-053): [`GitWatchRequest`]의 응답.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitWatchReply {
    /// 감시가 숨김 상태다(진행 중이던 git 자식 취소·회수 완료).
    Hidden,
    /// `since` 세대 이후 확정된 새 스냅샷이 아직 없다(`pending == true`면 디바운스·쿨다운·자식 대기 중).
    Unchanged { pending: bool },
    /// 새 스냅샷이 확정됐다(`generation > since` 또는 `since == 0`).
    Changed { generation: u64, changes: Changes },
    /// git 실행·파싱 실패(`generation > since` 또는 `since == 0`).
    Failed { generation: u64, error: String },
}

/// 첫 프롬프트 기반 브랜치·세션 이름 자동 변경 비활성화 플래그 파일 이름(OWE-737).
/// 상태 디렉터리에 이 파일이 없을 때(기본값) 자동 변경이 켜진다.
pub const AUTO_RENAME_DISABLED_FLAG: &str = "auto-rename-branch.disabled";

/// 워크스페이스 신뢰 프리셋 활성화 플래그 파일 이름(OWE-737).
/// 외부 에이전트 전역 설정(`~/.claude.json`, `~/.codex/config.toml`, Cursor/Antigravity)을 수정하므로
/// 상태 디렉터리에 이 파일이 있을 때만(사용자 명시적 동의 후) 켜지며 기본값은 꺼짐(`false`)이다.
pub const WORKSPACE_TRUST_PRESET_ENABLED_FLAG: &str = "workspace-trust-preset.enabled";

/// 첫 프롬프트에서 파생된 세션별 자동 제목 저장 파일 이름(OWE-737).
pub const SESSION_TITLES_FILE: &str = "session-titles.json";

/// 세션별 최근 프롬프트 턴 히스토리 저장 파일 이름.
pub const SESSION_PROMPTS_FILE: &str = "session-prompts.json";

/// 세션 프롬프트 요약 표시 최대 글자 수.
pub const MAX_PROMPT_DISPLAY_CHARS: usize = 240;

/// 세션당 보존할 최근 프롬프트 턴 최대 개수.
pub const MAX_PROMPT_HISTORY_TURNS: usize = 5;

/// 브랜치 슬러그 최대 글자 수(OWE-737).
pub const MAX_BRANCH_SLUG_CHARS: usize = 50;

/// 첫 프롬프트에서 추출할 최대 단어 수.
const MAX_PROMPT_WORDS: usize = 5;

const FILLER_WORDS: &[&str] = &[
    "please", "can", "could", "would", "you", "help", "me", "i", "want", "need", "to", "lets",
    "let", "us", "the", "a", "an", "in", "on", "for", "with", "and", "or", "of", "that", "this",
    "is", "are",
];

const CREATURE_NAMES: &[&str] = &[
    "ant", "badger", "bear", "beaver", "bison", "bobcat", "camel", "cobra", "condor", "cougar",
    "coyote", "crab", "crane", "crow", "deer", "dingo", "dolphin", "dove", "dragon", "duck",
    "eagle", "eel", "elk", "emu", "falcon", "ferret", "finch", "fox", "frog", "gecko", "goat",
    "goose", "gopher", "hawk", "heron", "horse", "husky", "hyena", "ibis", "iguana", "impala",
    "jackal", "jaguar", "jay", "kiwi", "koala", "koi", "krill", "lemur", "lion", "lizard", "llama",
    "lobster", "lynx", "macaw", "magpie", "mallard", "mamba", "manta", "marten", "meerkat", "mink",
    "mole", "monkey", "moose", "moth", "mouse", "mule", "newt", "ocelot", "orca", "oriole",
    "osprey", "otter", "owl", "ox", "panda", "panther", "parrot", "pelican", "penguin", "perch",
    "pigeon", "pike", "pony", "puffin", "puma", "python", "quail", "rabbit", "raccoon", "ram",
    "raven", "ray", "robin", "salmon", "seal", "shark", "sheep", "shrew", "skunk", "sloth",
    "snail", "snake", "snipe", "sparrow", "spider", "squid", "stork", "swan", "swift", "tapir",
    "tern", "tiger", "toad", "toucan", "trout", "tuna", "turkey", "turtle", "viper", "vole",
    "walrus", "wasp", "weasel", "whale", "wolf", "wombat", "wren", "yak", "zebra",
];

/// 첫 프롬프트 기반 자동 이름 변경이 켜져 있는지 판정한다(기본값 `true`).
#[must_use]
pub fn is_auto_rename_enabled(state_dir: Option<&std::path::Path>) -> bool {
    state_dir.is_none_or(|d| !d.join(AUTO_RENAME_DISABLED_FLAG).exists())
}

/// 워크스페이스 신뢰 프리셋이 켜져 있는지 판정한다(기본값 `false`, 명시적 활성화 시에만 `true`).
#[must_use]
pub fn is_workspace_trust_preset_enabled(state_dir: Option<&std::path::Path>) -> bool {
    state_dir.is_some_and(|d| d.join(WORKSPACE_TRUST_PRESET_ENABLED_FLAG).is_file())
}

/// 보호 대상 기본 브랜치(`main`, `master` 등)인지 판정한다.
#[must_use]
pub fn is_protected_branch_name(name: &str) -> bool {
    let trimmed = name.trim();
    let leaf = trimmed.rsplit('/').next().unwrap_or(trimmed);
    matches!(
        leaf.to_ascii_lowercase().as_str(),
        "main" | "master" | "develop" | "dev" | "staging" | "production" | "trunk" | "release"
    )
}

/// 자동 생성된 기본 브랜치·워크트리·세션 이름(`worktree-1`, `wt-2`, `claude-1`, `ant` 등)인지 판정한다(OWE-737).
///
/// 참고: `.ref/orca/src/shared/branch-name-from-work.ts`, `.ref/orca/src/shared/creature-names.ts`
/// (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).
#[must_use]
pub fn is_default_branch_or_worktree_name(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() || is_protected_branch_name(trimmed) {
        return false;
    }
    let leaf = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let normalized: String = leaf
        .chars()
        .map(|c| {
            if c == ' ' || c == '_' {
                '-'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let (base, had_num_suffix) = match normalized.rsplit_once('-') {
        Some((prefix, suffix))
            if !prefix.is_empty()
                && !suffix.is_empty()
                && suffix.chars().all(|c| c.is_ascii_digit()) =>
        {
            (prefix, true)
        }
        _ => (normalized.as_str(), false),
    };
    if matches!(
        base,
        "worktree"
            | "wt"
            | "session"
            | "세션"
            | "claude"
            | "codex"
            | "agy"
            | "antigravity"
            | "gemini"
            | "cursor"
            | "cursor-agent"
            | "opencode"
    ) {
        return true;
    }
    if had_num_suffix && matches!(base, "feat" | "feature" | "branch" | "task") {
        return true;
    }
    CREATURE_NAMES.binary_search(&base).is_ok()
}

/// 임의 문자열을 git 브랜치에 쓸 수 있는 슬러그(최대 50자)로 정규화한다(OWE-737).
#[must_use]
pub fn sanitize_branch_slug(raw: &str) -> Option<String> {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch == '\'' || ch == '\u{2019}' || ch == '`' {
            continue;
        }
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                slug.push(lower);
            }
            prev_dash = false;
        } else if !slug.is_empty() && !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let trimmed = slug.trim_matches(['-', '.']);
    let capped: String = trimmed.chars().take(MAX_BRANCH_SLUG_CHARS).collect();
    let final_slug = capped.trim_matches(['-', '.']).to_owned();
    if final_slug.is_empty()
        || final_slug == "."
        || final_slug == ".."
        || std::path::Path::new(&final_slug)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"))
    {
        return None;
    }
    Some(final_slug)
}

/// 프롬프트 원문에서 `<USER_REQUEST>` 래퍼, 오케스트레이션·핸드오프 프리앰블, `[Image #N]`·`[Pasted ...]` 마커, 선행 붙여넣기 파일 경로를 걷어낸다.
#[must_use]
pub fn clean_user_prompt(raw: &str) -> String {
    let mut s = raw.trim();
    if let Some(start) = s.find("<USER_REQUEST>") {
        let after = start.saturating_add("<USER_REQUEST>".len());
        if let Some(tail) = s.get(after..) {
            s = if let Some(end) = tail.find("</USER_REQUEST>") {
                tail.get(..end).unwrap_or(tail).trim()
            } else {
                tail.trim()
            };
        }
    }
    if let Some(stripped) = strip_orchestration_or_handoff_wrapper(s) {
        s = stripped;
    }
    loop {
        s = s.trim_start();
        if let Some(rest) = strip_leading_bracket_marker(s) {
            s = rest;
            continue;
        }
        if let Some(rest) = strip_leading_paste_path(s) {
            s = rest;
            continue;
        }
        break;
    }
    s.trim().to_owned()
}

fn strip_orchestration_or_handoff_wrapper(s: &str) -> Option<&str> {
    if let Some(rest) = s
        .strip_prefix("[KRILL ORCHESTRATION PREAMBLE]")
        .or_else(|| s.strip_prefix("[MK ORCHESTRATION PREAMBLE]"))
    {
        if let Some((_, after_label)) = rest.split_once("Task Prompt:") {
            let body = after_label
                .split_once("\nFirst read the Krill worker contract:")
                .map_or(after_label, |(prompt, _)| prompt);
            return Some(body.trim());
        }
        let rest = rest.trim_start();
        if let Some(after_run) = rest.strip_prefix("Run:") {
            return Some(
                after_run
                    .split_once('\n')
                    .map_or("", |(_, tail)| tail.trim()),
            );
        }
        return Some(rest);
    }
    if let Some(rest) = s.strip_prefix("[Krill Worker]") {
        let body = rest
            .split_once("\nRead `krill skills get orchestration")
            .map_or(rest, |(head, _)| head);
        let first_line_end = body.find('\n').unwrap_or(body.len());
        let first_line = body.get(..first_line_end).unwrap_or(body);
        if let Some((_, after_colon)) = first_line.split_once(": ") {
            let offset = first_line.len().checked_sub(after_colon.len())?;
            return body.get(offset..).map(str::trim);
        }
        return Some(body.trim());
    }
    if let Some(rest) = s
        .strip_prefix("[KRILL SESSION HANDOFF]")
        .or_else(|| s.strip_prefix("[MK SESSION HANDOFF]"))
        && let Some((_, after_inst)) = rest.split_once("Instructions:")
    {
        let body = after_inst
            .split_once("\n--- Recent terminal context ---")
            .map_or(after_inst, |(inst, _)| inst);
        return Some(body.trim());
    }
    if (s.starts_with("[KRILL HANDOFF from session") || s.starts_with("[MK HANDOFF from session"))
        && let Some((_, after_prompt)) = s.split_once("\nPrompt:\n")
    {
        let body = after_prompt
            .split_once("\nRecent terminal context:\n")
            .map_or(after_prompt, |(prompt, _)| prompt);
        return Some(body.trim());
    }
    None
}

fn strip_leading_bracket_marker(s: &str) -> Option<&str> {
    let (inner, rest) = s.strip_prefix('[')?.split_once(']')?;
    let lower = inner.trim().to_ascii_lowercase();
    if lower.starts_with("image #")
        || lower.starts_with("pasted content")
        || lower.starts_with("pasted text")
    {
        Some(rest)
    } else {
        None
    }
}

fn is_paste_or_media_path(path: &str) -> bool {
    if !path.starts_with('/') || path.len() < 4 {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    lower.starts_with("/var/folders/")
        || lower.starts_with("/private/var/folders/")
        || lower.starts_with("/tmp/")
        || lower.starts_with("/private/tmp/")
        || lower.contains("/krill-paste/")
        || [
            ".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic", ".svg", ".pdf",
        ]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn strip_leading_paste_path(s: &str) -> Option<&str> {
    for quote in ['\'', '"'] {
        if let Some(after_open) = s.strip_prefix(quote)
            && let Some((candidate, rest)) = after_open.split_once(quote)
            && is_paste_or_media_path(candidate)
        {
            return Some(rest);
        }
    }
    if s.starts_with('/') {
        let end = s.find(char::is_whitespace).unwrap_or(s.len());
        let candidate = s.get(..end)?;
        if is_paste_or_media_path(candidate) {
            return s.get(end..);
        }
    }
    None
}

/// 첫 프롬프트 원문에서 의미 있는 첫 줄을 골라 브랜치·세션 슬러그를 만든다(OWE-737).
///
/// 참고: `.ref/orca/src/shared/branch-name-from-work.ts`
/// (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).
#[must_use]
pub fn prompt_branch_slug(prompt: &str) -> Option<String> {
    let cleaned_prompt = clean_user_prompt(prompt);
    let prompt = cleaned_prompt.as_str();
    if prompt.is_empty() {
        return None;
    }
    let mut in_fence = false;
    let mut candidate_line = None;
    for line in prompt.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed.is_empty() {
            continue;
        }
        let stripped = trimmed.trim_start_matches(['#', '>', '-', '*', ' ']).trim();
        if !stripped.is_empty() {
            candidate_line = Some(stripped);
            break;
        }
    }
    let line = candidate_line.or_else(|| prompt.lines().map(str::trim).find(|l| !l.is_empty()))?;
    let cleaned = line.replace(['\'', '\u{2019}', '`'], "");
    let words: Vec<String> = cleaned
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.chars().flat_map(char::to_lowercase).collect::<String>())
        .collect();
    if words.is_empty() {
        return sanitize_branch_slug(line);
    }
    let filtered: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| !FILLER_WORDS.contains(w))
        .collect();
    let chosen: Vec<&str> = if filtered.is_empty() {
        words
            .iter()
            .map(String::as_str)
            .take(MAX_PROMPT_WORDS)
            .collect()
    } else {
        filtered.into_iter().take(MAX_PROMPT_WORDS).collect()
    };
    sanitize_branch_slug(&chosen.join("-"))
}

/// 브랜치 슬러그(`fix-login-bug` 또는 `feat/fix-login-bug`)를 사람이 읽기 좋은 제목(`fix login bug`)으로 바꾼다.
#[must_use]
pub fn humanize_branch_slug(slug: &str) -> String {
    let leaf = slug.trim().rsplit('/').next().unwrap_or(slug.trim());
    leaf.replace(['-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// hook payload(JSON)에서 사용자 첫 프롬프트 원문을 꺼낸다(`UserPromptSubmit`, `beforeSubmitPrompt`, `BeforeAgent`, `PreInvocation`).
#[must_use]
pub fn extract_prompt_from_hook_payload(payload: &[u8]) -> Option<String> {
    let val: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let obj = val.as_object()?;
    let event = obj
        .get("hook_event_name")
        .or_else(|| obj.get("hookEventName"))
        .or_else(|| obj.get("event"))
        .and_then(serde_json::Value::as_str)?;
    if !matches!(
        event,
        "UserPromptSubmit" | "beforeSubmitPrompt" | "BeforeAgent" | "PreInvocation"
    ) {
        return None;
    }
    for key in [
        "prompt",
        "user_prompt",
        "userPrompt",
        "lastUserInput",
        "last_user_input",
        "message",
        "input",
        "text",
    ] {
        if let Some(s) = obj.get(key).and_then(serde_json::Value::as_str) {
            let cleaned = clean_user_prompt(s);
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }
    None
}

/// `session-titles.json` 읽기-수정-쓰기 전체를 직렬화하는 프로세스 내 잠금(OWE-755).
/// 파일 IO만 안에서 하고 다른 잠금을 잡지 않는 리프라 잠금 순서 문제가 없다.
static SESSION_TITLES_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// 원자 쓰기 임시 파일명 중복 방지용 단조 증가 시퀀스 번호(OWE-755).
static SESSION_TITLES_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const SESSION_TITLES_LOCK_FILE: &str = ".session-titles.lock";

/// 세션의 첫 프롬프트 자동 제목을 상태 디렉터리에 기록한다(이미 기록된 세션은 덮어쓰지 않는다).
pub fn record_session_auto_title(
    state_dir: &std::path::Path,
    session: crate::SessionId,
    title: &str,
) -> std::io::Result<bool> {
    use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    use std::collections::BTreeMap;
    use std::fs::{DirBuilder, OpenOptions};
    use std::io::Write as _;

    let clean: String = title
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_BRANCH_SLUG_CHARS)
        .collect();
    if clean.is_empty() {
        return Ok(false);
    }
    let _guard = SESSION_TITLES_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state_dir)?;
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state_dir.join(SESSION_TITLES_LOCK_FILE))?;
    lock_file.lock()?;

    let path = state_dir.join(SESSION_TITLES_FILE);
    let mut map: BTreeMap<u64, String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if map.contains_key(&session.0) {
        return Ok(false);
    }
    while map.len() >= 256 {
        let Some(&first) = map.keys().next() else {
            break;
        };
        map.remove(&first);
    }
    map.insert(session.0, clean);
    let mut bytes = serde_json::to_vec(&map).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    let seq = SESSION_TITLES_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{seq}", std::process::id()));
    let write_res = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        drop(file);
        std::fs::rename(&tmp, &path)
    })();
    if write_res.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    write_res?;
    Ok(true)
}

/// 상태 디렉터리에 기록된 세션별 자동 제목 목록을 읽는다.
#[must_use]
pub fn load_session_auto_titles(
    state_dir: &std::path::Path,
) -> std::collections::BTreeMap<crate::SessionId, String> {
    let path = state_dir.join(SESSION_TITLES_FILE);
    let Ok(text) = std::fs::read_to_string(path) else {
        return std::collections::BTreeMap::new();
    };
    let Ok(map) = serde_json::from_str::<std::collections::BTreeMap<u64, String>>(&text) else {
        return std::collections::BTreeMap::new();
    };
    map.into_iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .map(|(k, v)| (crate::SessionId(k), v))
        .collect()
}

/// 세션에서 사용자가 제출한 개별 프롬프트 턴 기록.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPromptTurn {
    /// 정제된 프롬프트 원문 요약(최대 [`MAX_PROMPT_DISPLAY_CHARS`]자).
    pub text: String,
    /// 제출 시각(Unix epoch nanoseconds).
    #[serde(default)]
    pub unix_ns: u64,
    /// 1부터 시작하는 세션 누적 턴 번호.
    #[serde(default)]
    pub turn: u32,
}

/// 세션별 최근 프롬프트 히스토리(최신 턴이 `turns`의 0번 원소).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPromptHistory {
    /// 이 세션에서 기록된 누적 프롬프트 턴 수.
    #[serde(default)]
    pub total_turns: u32,
    /// 최신순 프롬프트 턴 링버퍼(최대 [`MAX_PROMPT_HISTORY_TURNS`]개).
    #[serde(default)]
    pub turns: Vec<SessionPromptTurn>,
}

/// 프롬프트 원문을 UI 표시에 적합하게 정제하고 최대 `max_chars` 글자로 자른다.
#[must_use]
pub fn clean_prompt_display(raw: &str, max_chars: usize) -> Option<String> {
    let cleaned = clean_user_prompt(raw);
    let mut normalized = String::new();
    let mut prev_newline = false;
    for ch in cleaned.chars() {
        if ch == '\n' {
            if !normalized.is_empty() && !prev_newline {
                normalized.push('\n');
                prev_newline = true;
            }
        } else if !ch.is_control() {
            normalized.push(ch);
            prev_newline = false;
        }
    }
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return None;
    }
    let char_count = trimmed.chars().count();
    if char_count <= max_chars {
        return Some(trimmed.to_owned());
    }
    let capped: String = trimmed.chars().take(max_chars).collect();
    Some(format!("{}…", capped.trim_end()))
}

/// 단답형 승인·진행 지시(`"ㅇㅇ"`, `"진행해"`, `"yes"`, `"ok"` 등)인지 판정한다.
///
/// 마지막 프롬프트가 단답형이면 직전 실질 작업 지시 프롬프트를 함께 보존해
/// `"기존 지시문 (→ ㅇㅇ 진행해)"` 형태로 합성할 때 사용한다.
#[must_use]
pub fn is_ack_or_trivial_prompt(text: &str) -> bool {
    let trimmed: String = text
        .trim()
        .chars()
        .filter(|c| !c.is_ascii_punctuation() && !matches!(c, '!' | '?' | '.' | ',' | '~' | '…'))
        .collect();
    let norm = trimmed.trim().to_lowercase();
    if norm.is_empty() || norm.chars().count() <= 4 {
        return true;
    }
    let words: Vec<&str> = norm.split_whitespace().collect();
    if words.is_empty() || words.len() > 3 {
        return false;
    }
    words.iter().all(|w| {
        matches!(
            *w,
            "ㅇㅇ"
                | "네"
                | "응"
                | "예"
                | "좋아"
                | "좋아요"
                | "진행"
                | "진행해"
                | "진행해줘"
                | "진행해주세요"
                | "진행해요"
                | "계속"
                | "계속해"
                | "계속해줘"
                | "계속해주세요"
                | "고"
                | "고고"
                | "확인"
                | "승인"
                | "그래"
                | "그렇게"
                | "그대로"
                | "해"
                | "해줘"
                | "해주세요"
                | "맞아"
                | "맞아요"
                | "오케이"
                | "오키"
                | "ㄱㄱ"
                | "가자"
                | "yes"
                | "y"
                | "yep"
                | "yeah"
                | "ok"
                | "okay"
                | "sure"
                | "continue"
                | "proceed"
                | "go"
                | "ahead"
                | "lgtm"
                | "ship"
                | "it"
                | "do"
                | "please"
        )
    })
}

impl SessionPromptHistory {
    /// 새 프롬프트 턴을 기록한다.
    /// 직전 턴과 동일한 텍스트가 3초 이내에 중복 도착하면(훅 + `SendInput` 동시 호출 등) 무시하고 `false`를 돌려준다.
    pub fn record_turn(&mut self, raw: &str, unix_ns: u64) -> bool {
        let Some(clean) = clean_prompt_display(raw, MAX_PROMPT_DISPLAY_CHARS) else {
            return false;
        };
        if let Some(latest) = self.turns.first()
            && latest.text == clean
            && unix_ns.abs_diff(latest.unix_ns) < 3_000_000_000
        {
            return false;
        }
        let next_turn = self.total_turns.saturating_add(1);
        self.total_turns = next_turn;
        self.turns.insert(
            0,
            SessionPromptTurn {
                text: clean,
                unix_ns,
                turn: next_turn,
            },
        );
        self.turns.truncate(MAX_PROMPT_HISTORY_TURNS);
        true
    }

    /// 실행 단계마다 반복되는 마지막 사용자 입력은 새 제출이 아니다.
    fn record_context(&mut self, raw: &str, unix_ns: u64) -> bool {
        let Some(clean) = clean_prompt_display(raw, MAX_PROMPT_DISPLAY_CHARS) else {
            return false;
        };
        if self.turns.first().is_some_and(|turn| turn.text == clean) {
            return false;
        }
        self.record_turn(raw, unix_ns)
    }

    /// 출처 정보가 없는 이전 버전 기록에만 적용한다. 명시적 재제출은 합치지 않는다.
    fn repair_context_duplicates(&mut self, raw: &str) -> bool {
        let Some(clean) = clean_prompt_display(raw, MAX_PROMPT_DISPLAY_CHARS) else {
            return false;
        };
        let repeated = self
            .turns
            .iter()
            .take_while(|turn| turn.text == clean)
            .count();
        if repeated <= 1 {
            return false;
        }
        self.turns.drain(..repeated.saturating_sub(1));
        let removed = u32::try_from(repeated.saturating_sub(1)).unwrap_or(u32::MAX);
        self.total_turns = self.total_turns.saturating_sub(removed);
        true
    }

    /// 최신 턴의 타임스탬프를 돌려준다.
    #[must_use]
    pub fn latest_unix_ns(&self) -> Option<u64> {
        self.turns.first().map(|t| t.unix_ns)
    }

    /// 단답형 후속 응답(`"ㅇㅇ 진행해"` 등)일 때 직전 실질 프롬프트와 병합한 표시 문자열을 돌려준다.
    #[must_use]
    pub fn display_last_prompt(&self) -> Option<String> {
        let latest = self.turns.first()?;
        if is_ack_or_trivial_prompt(&latest.text)
            && let Some(substantive) = self
                .turns
                .iter()
                .skip(1)
                .find(|t| !is_ack_or_trivial_prompt(&t.text))
        {
            return Some(format!("{} (→ {})", substantive.text, latest.text));
        }
        Some(latest.text.clone())
    }

    /// 상단 고정 바(Sticky Prompt Bar) 등 한 줄 요약 표시에 쓸 공백 정규화 문자열을 돌려준다.
    #[must_use]
    pub fn single_line_summary(&self) -> Option<String> {
        let display = self.display_last_prompt()?;
        let collapsed: Vec<&str> = display.split_whitespace().collect();
        if collapsed.is_empty() {
            None
        } else {
            Some(collapsed.join(" "))
        }
    }

    /// 호버 카드 하단에 `"이전(Turn N-1)"` 맥락으로 보여줄 직전 턴을 돌려준다.
    #[must_use]
    pub fn previous_turn(&self) -> Option<&SessionPromptTurn> {
        let latest = self.turns.first()?;
        if is_ack_or_trivial_prompt(&latest.text)
            && let Some(idx) = self
                .turns
                .iter()
                .skip(1)
                .position(|t| !is_ack_or_trivial_prompt(&t.text))
        {
            return self.turns.get(idx.saturating_add(2));
        }
        self.turns.get(1)
    }
}

static SESSION_PROMPTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static SESSION_PROMPTS_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const SESSION_PROMPTS_LOCK_FILE: &str = ".session-prompts.lock";

/// 세션의 프롬프트 턴을 상태 디렉터리(`session-prompts.json`)에 원자적으로 추가 기록한다.
pub fn record_session_prompt(
    state_dir: &std::path::Path,
    session: crate::SessionId,
    prompt: &str,
    unix_ns: u64,
) -> std::io::Result<bool> {
    record_prompt(
        state_dir,
        session,
        prompt,
        unix_ns,
        PromptRecord::Submission,
        None,
    )
}

/// `PreInvocation`의 `lastUserInput`은 턴 식별자가 아닌 반복 문맥이다.
/// 직접 제출 훅은 같은 문장을 다시 제출해도 기존 시간 기반 병합 규칙만 적용한다.
pub fn record_session_prompt_from_hook(
    state_dir: &std::path::Path,
    session: crate::SessionId,
    payload: &[u8],
    event_id: crate::hook::EventId,
) -> std::io::Result<bool> {
    let Some(prompt) = extract_prompt_from_hook_payload(payload) else {
        return Ok(false);
    };
    let value: serde_json::Value =
        serde_json::from_slice(payload).map_err(std::io::Error::other)?;
    let event = value
        .get("hook_event_name")
        .or_else(|| value.get("hookEventName"))
        .or_else(|| value.get("event"))
        .and_then(serde_json::Value::as_str);
    let source = if event == Some("PreInvocation") {
        PromptRecord::Context
    } else {
        PromptRecord::Submission
    };
    record_prompt(
        state_dir,
        session,
        &prompt,
        event_id.unix_ns,
        source,
        Some(event_id),
    )
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PromptRecord {
    Submission,
    Context,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredPromptHistory {
    #[serde(flatten)]
    history: SessionPromptHistory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<PromptRecord>,
    /// 소켓 ACK 유실 뒤 spool로 재전송돼도 동일 제출로 취급한다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hook_ids: Vec<crate::hook::EventId>,
}

fn record_prompt(
    state_dir: &std::path::Path,
    session: crate::SessionId,
    prompt: &str,
    unix_ns: u64,
    source: PromptRecord,
    event_id: Option<crate::hook::EventId>,
) -> std::io::Result<bool> {
    use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    use std::collections::BTreeMap;
    use std::fs::{DirBuilder, OpenOptions};
    use std::io::Write as _;

    if clean_prompt_display(prompt, MAX_PROMPT_DISPLAY_CHARS).is_none() {
        return Ok(false);
    }
    let _guard = SESSION_PROMPTS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state_dir)?;
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state_dir.join(SESSION_PROMPTS_LOCK_FILE))?;
    lock_file.lock()?;

    let path = state_dir.join(SESSION_PROMPTS_FILE);
    let mut map: BTreeMap<u64, StoredPromptHistory> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if !map.contains_key(&session.0) {
        while map.len() >= 256 {
            let Some(&first) = map.keys().next() else {
                break;
            };
            map.remove(&first);
        }
    }
    let entry = map.entry(session.0).or_default();
    if event_id.is_some_and(|id| entry.hook_ids.contains(&id)) {
        return Ok(false);
    }
    let changed = match source {
        PromptRecord::Submission => entry.history.record_turn(prompt, unix_ns),
        PromptRecord::Context => {
            let repaired =
                entry.source.is_none() && entry.history.repair_context_duplicates(prompt);
            entry.history.record_context(prompt, unix_ns) || repaired
        }
    };
    if !changed && event_id.is_none() {
        return Ok(false);
    }
    if let Some(id) = event_id {
        entry.hook_ids.insert(0, id);
        entry.hook_ids.truncate(32);
    }
    entry.source = Some(source);
    let mut bytes = serde_json::to_vec(&map).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    let seq = SESSION_PROMPTS_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{seq}", std::process::id()));
    let write_res = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        drop(file);
        std::fs::rename(&tmp, &path)
    })();
    if write_res.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    write_res?;
    Ok(changed)
}

/// 상태 디렉터리에 기록된 세션별 프롬프트 히스토리 목록을 읽는다.
#[must_use]
pub fn load_session_prompt_histories(
    state_dir: &std::path::Path,
) -> std::collections::BTreeMap<crate::SessionId, SessionPromptHistory> {
    let path = state_dir.join(SESSION_PROMPTS_FILE);
    let Ok(text) = std::fs::read_to_string(path) else {
        return std::collections::BTreeMap::new();
    };
    let Ok(map) =
        serde_json::from_str::<std::collections::BTreeMap<u64, SessionPromptHistory>>(&text)
    else {
        return std::collections::BTreeMap::new();
    };
    map.into_iter()
        .filter(|(_, v)| !v.turns.is_empty())
        .map(|(k, v)| (crate::SessionId(k), v))
        .collect()
}

#[cfg(test)]
mod slug_tests {
    use super::*;

    #[test]
    fn prompt_slug_handles_english_korean_special_chars_and_length() {
        assert_eq!(
            prompt_branch_slug("Please help me fix the login timeout bug!").as_deref(),
            Some("fix-login-timeout-bug")
        );
        assert_eq!(
            prompt_branch_slug("로그인 세션 만료 버그 수정해줘").as_deref(),
            Some("로그인-세션-만료-버그-수정해줘")
        );
        assert_eq!(
            prompt_branch_slug("```sh\ncargo test\n```\n# Refactor sidebar cache timer UI")
                .as_deref(),
            Some("refactor-sidebar-cache-timer-ui")
        );
        let long =
            "alpha1234567890 beta1234567890 gamma1234567890 delta1234567890 epsilon1234567890";
        let slug = prompt_branch_slug(long).unwrap();
        assert!(slug.chars().count() <= MAX_BRANCH_SLUG_CHARS);
        assert!(!slug.ends_with('-'));
        assert_eq!(prompt_branch_slug("   ... ---   "), None);
    }

    #[test]
    fn default_branch_or_worktree_names_are_detected() {
        assert!(is_default_branch_or_worktree_name("worktree-1"));
        assert!(is_default_branch_or_worktree_name("wt-42"));
        assert!(is_default_branch_or_worktree_name("claude-2"));
        assert!(is_default_branch_or_worktree_name("codex"));
        assert!(is_default_branch_or_worktree_name("session-3"));
        assert!(is_default_branch_or_worktree_name("세션 3"));
        assert!(is_default_branch_or_worktree_name("orca"));
        assert!(is_default_branch_or_worktree_name("krill-2"));
        assert!(!is_default_branch_or_worktree_name("main"));
        assert!(!is_default_branch_or_worktree_name("master"));
        assert!(!is_default_branch_or_worktree_name("fix-login-bug"));
    }

    #[test]
    fn extract_prompt_and_record_session_title_once() {
        let payload =
            br#"{"hook_event_name":"UserPromptSubmit","prompt":"Fix cache timer countdown"}"#;
        let prompt = extract_prompt_from_hook_payload(payload).unwrap();
        let slug = prompt_branch_slug(&prompt).unwrap();
        assert_eq!(slug, "fix-cache-timer-countdown");
        assert_eq!(humanize_branch_slug(&slug), "fix cache timer countdown");

        let agy_payload = r#"{"hook_event_name":"PreInvocation","lastUserInput":"<USER_REQUEST>\n'/var/folders/nm/jz7t2dm539597y551gd3tzw40000gn/T/krill-paste/mobile-123.jpg' 안티그래비티 제목 요약 표시\n</USER_REQUEST>"}"#.as_bytes();
        let agy_prompt = extract_prompt_from_hook_payload(agy_payload).unwrap();
        assert_eq!(agy_prompt, "안티그래비티 제목 요약 표시");
        assert_eq!(
            prompt_branch_slug(&agy_prompt).as_deref(),
            Some("안티그래비티-제목-요약-표시")
        );
        assert_eq!(
            prompt_branch_slug("[Image #1] UI 밀리는 거 수정").as_deref(),
            Some("ui-밀리는-거-수정")
        );
        assert_eq!(
            prompt_branch_slug("'/var/folders/nm/foo/T/krill-paste/img.png'"),
            None
        );
        let orch_prompt = "[KRILL ORCHESTRATION PREAMBLE]\nRun: run-1 | Dispatch: disp-1 | Worker: w1 | Task: task-1\nTask Prompt: 상단 탭 클릭 시 사이드바 포커싱 동기화\nFirst read the Krill worker contract: krill skills get orchestration";
        assert_eq!(
            clean_user_prompt(orch_prompt),
            "상단 탭 클릭 시 사이드바 포커싱 동기화"
        );
        assert_eq!(
            prompt_branch_slug(orch_prompt).as_deref(),
            Some("상단-탭-클릭-시-사이드바")
        );
        let min_prompt = "[Krill Worker] run=run-1 task=task-1 dispatch=disp-1 worker=w1: 모바일 채팅 레이아웃 수정\nRead `krill skills get orchestration --reference references/worker-contract.md`";
        assert_eq!(clean_user_prompt(min_prompt), "모바일 채팅 레이아웃 수정");
        let handoff_prompt = "[KRILL SESSION HANDOFF]\nSource session: s42\nTarget agent: codex\nInstructions: 남은 단위 테스트 마무리\n--- Recent terminal context ---\nfoo\n--- End context ---\n";
        assert_eq!(clean_user_prompt(handoff_prompt), "남은 단위 테스트 마무리");

        let dir = std::env::temp_dir().join(format!("owe737-titles-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let sid = crate::SessionId(7);
        assert!(record_session_auto_title(&dir, sid, &slug).unwrap());
        assert!(!record_session_auto_title(&dir, sid, "second-prompt-ignored").unwrap());
        let titles = load_session_auto_titles(&dir);
        assert_eq!(
            titles.get(&sid).map(String::as_str),
            Some("fix-cache-timer-countdown")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn owe755_concurrent_record_session_auto_title_never_loses_titles_or_errors() {
        use std::sync::{Arc, Barrier};

        let dir = std::env::temp_dir().join(format!("owe755-titles-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();

        let threads = 12_u64;
        let per_thread = 10_u64;
        let shared_sid = crate::SessionId(9999);
        let barrier = Arc::new(Barrier::new(usize::try_from(threads).unwrap()));
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let d = dir.clone();
                let b = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    b.wait();
                    let mut shared_won = 0_usize;
                    for i in 0..per_thread {
                        let sid = crate::SessionId(t * 100 + i + 1);
                        let title = format!("task-{t}-{i}");
                        assert!(
                            record_session_auto_title(&d, sid, &title).unwrap(),
                            "first record for {sid:?} must return Ok(true)"
                        );
                        assert!(
                            !record_session_auto_title(&d, sid, "duplicate-ignored").unwrap(),
                            "second record for {sid:?} must return Ok(false)"
                        );
                    }
                    if record_session_auto_title(&d, shared_sid, &format!("shared-winner-{t}"))
                        .unwrap()
                    {
                        shared_won += 1;
                    }
                    shared_won
                })
            })
            .collect();

        let total_shared_winners: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(
            total_shared_winners, 1,
            "exactly one thread must win first title for shared session"
        );

        let titles = load_session_auto_titles(&dir);
        let expected_count = usize::try_from(threads * per_thread + 1).unwrap();
        assert_eq!(titles.len(), expected_count);
        for t in 0..threads {
            for i in 0..per_thread {
                let sid = crate::SessionId(t * 100 + i + 1);
                let expected = format!("task-{t}-{i}");
                assert_eq!(
                    titles.get(&sid).map(String::as_str),
                    Some(expected.as_str())
                );
            }
        }
        assert!(titles.contains_key(&shared_sid));

        for entry in std::fs::read_dir(&dir).unwrap() {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            assert!(!name.contains(".tmp."), "leftover tmp file: {name}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    fn prompt_event_id(unix_ns: u64) -> crate::hook::EventId {
        crate::hook::EventId { pid: 1, unix_ns }
    }

    #[test]
    fn invocation_context_deduplicates_delayed_hooks_but_preserves_real_resubmissions() {
        let dir = std::env::temp_dir().join(format!("prompt-context-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let sid = crate::SessionId(77);
        let context =
            br#"{"hook_event_name":"PreInvocation","lastUserInput":"Continue the original task"}"#;
        assert!(record_session_prompt_from_hook(&dir, sid, context, prompt_event_id(1)).unwrap());
        for timestamp in [4_000_000_001, 40_000_000_001, 400_000_000_001] {
            assert!(
                !record_session_prompt_from_hook(&dir, sid, context, prompt_event_id(timestamp))
                    .unwrap()
            );
        }
        let history = load_session_prompt_histories(&dir).remove(&sid).unwrap();
        assert_eq!(history.total_turns, 1);
        assert_eq!(history.turns[0].unix_ns, 1);
        // A genuine repeated submission is still a new turn after the receipt merge window.
        let submitted =
            br#"{"hook_event_name":"UserPromptSubmit","prompt":"Continue the original task"}"#;
        assert!(
            record_session_prompt_from_hook(&dir, sid, submitted, prompt_event_id(500_000_000_001))
                .unwrap()
        );
        assert_eq!(load_session_prompt_histories(&dir)[&sid].total_turns, 2);
        assert!(
            !record_session_prompt_from_hook(&dir, sid, context, prompt_event_id(600_000_000_001))
                .unwrap()
        );
        assert_eq!(load_session_prompt_histories(&dir)[&sid].total_turns, 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replayed_prompt_hook_is_ignored_even_after_a_newer_prompt() {
        let dir = std::env::temp_dir().join(format!("prompt-replay-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let sid = crate::SessionId(77);
        let first = br#"{"hook_event_name":"UserPromptSubmit","prompt":"First user request"}"#;
        let second = br#"{"hook_event_name":"UserPromptSubmit","prompt":"A newer user request"}"#;
        assert!(record_session_prompt_from_hook(&dir, sid, first, prompt_event_id(1)).unwrap());
        assert!(
            record_session_prompt_from_hook(&dir, sid, second, prompt_event_id(10_000_000_000))
                .unwrap()
        );
        assert!(!record_session_prompt_from_hook(&dir, sid, first, prompt_event_id(1)).unwrap());
        let history = load_session_prompt_histories(&dir).remove(&sid).unwrap();
        assert_eq!(history.total_turns, 2);
        assert_eq!(history.turns[0].text, "A newer user request");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invocation_context_repairs_legacy_disk_history_once() {
        let dir =
            std::env::temp_dir().join(format!("legacy-prompt-context-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let sid = crate::SessionId(77);
        let mut history = SessionPromptHistory::default();
        history.record_turn("Previous user task", 1);
        history.record_turn("Current user task", 10_000_000_000);
        history.record_turn("Current user task", 20_000_000_000);
        let original = std::collections::BTreeMap::from([(sid.0, history)]);
        std::fs::write(
            dir.join(SESSION_PROMPTS_FILE),
            serde_json::to_vec(&original).unwrap(),
        )
        .unwrap();
        let context = br#"{"event":"PreInvocation","lastUserInput":"Current user task"}"#;
        assert!(
            record_session_prompt_from_hook(&dir, sid, context, prompt_event_id(40_000_000_000))
                .unwrap()
        );
        let history = load_session_prompt_histories(&dir).remove(&sid).unwrap();
        assert_eq!(history.total_turns, 2);
        assert_eq!(history.turns.len(), 2);
        assert!(
            !record_session_prompt_from_hook(&dir, sid, context, prompt_event_id(80_000_000_000))
                .unwrap()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invocation_context_repairs_retained_duplicates_and_keeps_previous_task() {
        let mut history = SessionPromptHistory::default();
        history.record_turn("Previous user task", 1);
        history.record_turn("Current user task", 10_000_000_000);
        history.record_turn("Current user task", 20_000_000_000);
        history.record_turn("Current user task", 30_000_000_000);
        assert!(history.repair_context_duplicates("Current user task"));
        assert_eq!(history.total_turns, 2);
        assert_eq!(history.turns.len(), 2);
        assert_eq!(history.turns[0].turn, 2);
        assert_eq!(history.turns[0].unix_ns, 10_000_000_000);
        assert_eq!(history.turns[1].text, "Previous user task");
        assert!(!history.record_context("Current user task", 90_000_000_000));
        assert!(history.record_context("A different user task", 100_000_000_000));
        assert_eq!(history.total_turns, 3);
    }

    #[test]
    fn session_prompt_history_tracks_turns_merges_short_acks_and_deduplicates() {
        let dir = std::env::temp_dir().join(format!("session-prompts-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let sid = crate::SessionId(42);

        let t1 = 10_000_000_000_u64;
        assert!(record_session_prompt(&dir, sid, "로그인 세션 만료 버그 수정해줘", t1).unwrap());
        // 3초 이내 동일 프롬프트 중복 호출(훅 + SendInput)은 무시된다.
        assert!(
            !record_session_prompt(
                &dir,
                sid,
                "로그인 세션 만료 버그 수정해줘",
                t1.saturating_add(500_000_000)
            )
            .unwrap()
        );

        let t2 = t1.saturating_add(10_000_000_000);
        assert!(
            record_session_prompt(
                &dir,
                sid,
                "쿠키 만료 테스트 케이스 3개 추가하고 PR 올려줘",
                t2
            )
            .unwrap()
        );

        let t3 = t2.saturating_add(10_000_000_000);
        assert!(record_session_prompt(&dir, sid, "ㅇㅇ 진행해", t3).unwrap());

        let histories = load_session_prompt_histories(&dir);
        let hist = histories.get(&sid).unwrap();
        assert_eq!(hist.total_turns, 3);
        assert_eq!(hist.turns.len(), 3);
        assert_eq!(
            hist.display_last_prompt().as_deref(),
            Some("쿠키 만료 테스트 케이스 3개 추가하고 PR 올려줘 (→ ㅇㅇ 진행해)")
        );
        assert_eq!(
            hist.previous_turn().map(|t| (t.turn, t.text.as_str())),
            Some((1, "로그인 세션 만료 버그 수정해줘"))
        );

        // 5개 초과 시 최근 5개만 유지하고 total_turns는 누적 증가한다.
        for step in 4_u64..=7_u64 {
            let ts = t3.saturating_add(step.saturating_mul(10_000_000_000));
            assert!(
                record_session_prompt(&dir, sid, &format!(" 후속 작업 지시 {step} 단계 진행 "), ts)
                    .unwrap()
            );
        }
        let hist = load_session_prompt_histories(&dir).remove(&sid).unwrap();
        assert_eq!(hist.total_turns, 7);
        assert_eq!(hist.turns.len(), MAX_PROMPT_HISTORY_TURNS);
        assert_eq!(hist.turns.first().map(|t| t.turn), Some(7));
        assert_eq!(hist.turns.last().map(|t| t.turn), Some(3));

        std::fs::remove_dir_all(&dir).ok();
    }
}
