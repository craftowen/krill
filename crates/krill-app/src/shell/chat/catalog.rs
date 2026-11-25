//! OWE-1350 (REQ-2): 데스크톱 컴포저 `@` 워크스페이스 파일 및 `/` 슬래시 명령/스킬 자동완성 카탈로그.

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub(super) const MAX_AUTOCOMPLETE_ITEMS: usize = 80;
const MAX_WALK_VISITS: usize = 6_000;
const MAX_ANCESTOR_DEPTH: usize = 8;
const MAX_MD_HEAD_BYTES: usize = 4_096;
const WORKSPACE_CACHE_TTL: Duration = Duration::from_secs(10);
const WORKSPACE_CACHE_CAP: usize = 8;

#[derive(Clone)]
struct CachedWorkspaceFiles {
    cwd: PathBuf,
    cached_at: Instant,
    files: Vec<String>,
}

fn workspace_file_cache() -> &'static Mutex<Vec<CachedWorkspaceFiles>> {
    static CACHE: OnceLock<Mutex<Vec<CachedWorkspaceFiles>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

pub(super) fn has_fresh_workspace_files_cache(cwd: &Path) -> bool {
    let Ok(guard) = workspace_file_cache().lock() else {
        return false;
    };
    let now = Instant::now();
    guard
        .iter()
        .any(|entry| entry.cwd == cwd && now.duration_since(entry.cached_at) < WORKSPACE_CACHE_TTL)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutocompleteKind {
    File,
    Slash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutocompleteTrigger {
    pub kind: AutocompleteKind,
    pub query: String,
    pub replace_range: Range<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutocompleteSource {
    Builtin,
    Command,
    Skill,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AutocompleteCandidate {
    pub label: String,
    pub detail: String,
    pub insert_text: String,
    pub source: AutocompleteSource,
}

#[derive(Default)]
pub(crate) struct AutocompleteState {
    pub trigger: Option<AutocompleteTrigger>,
    pub dismissed: Option<(AutocompleteKind, usize, String)>,
    pub items: Vec<AutocompleteCandidate>,
    pub selected: usize,
    pub loading: bool,
    pub generation: u64,
    pub task: Option<gpui::Task<()>>,
    pub scroll: gpui::ScrollHandle,
}

impl AutocompleteState {
    pub(super) fn is_open(&self) -> bool {
        self.trigger.is_some() && !self.items.is_empty()
    }

    pub(super) fn should_render_popover(&self) -> bool {
        self.trigger.is_some()
    }

    pub(super) fn close(&mut self) {
        self.trigger = None;
        self.items.clear();
        self.selected = 0;
        self.loading = false;
        self.task = None;
    }

    pub(super) fn dismiss_current(&mut self) {
        if let Some(trigger) = &self.trigger {
            self.dismissed = Some((
                trigger.kind,
                trigger.replace_range.start,
                trigger.query.clone(),
            ));
        }
        self.close();
    }

    pub(super) fn step_selection(&mut self, delta: isize) {
        let len = self.items.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let cur = self.selected.min(len - 1);
        self.selected = if delta >= 0 {
            (cur + 1) % len
        } else if cur == 0 {
            len - 1
        } else {
            cur - 1
        };
        self.scroll.scroll_to_item(self.selected);
    }
}

/// UTF-8 바이트 오프셋을 안전하게 문자 경계로 내림 보정한다.
pub(super) fn clamp_char_boundary(text: &str, byte_offset: usize) -> usize {
    let mut idx = byte_offset.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// UTF-8 바이트 오프셋을 GPUI `EntityInputHandler`가 요구하는 UTF-16 코드 유닛 오프셋으로 변환한다.
pub(super) fn utf8_to_utf16_offset(text: &str, byte_offset: usize) -> usize {
    let clamped = clamp_char_boundary(text, byte_offset);
    text.get(..clamped).map_or(0, |s| s.encode_utf16().count())
}

/// 첨부 칩에 표시할 사람이 읽기 쉬운 바이트 용량 문자열을 반환한다.
pub(super) fn format_byte_len(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        let tenths = (bytes * 10 + GB / 2) / GB;
        format!("{}.{} GB", tenths / 10, tenths % 10)
    } else if bytes >= MB {
        let tenths = (bytes * 10 + MB / 2) / MB;
        format!("{}.{} MB", tenths / 10, tenths % 10)
    } else if bytes >= KB {
        let tenths = (bytes * 10 + KB / 2) / KB;
        format!("{}.{} KB", tenths / 10, tenths % 10)
    } else {
        format!("{bytes} B")
    }
}

/// 커서 위치 기준으로 줄 시작 `/<검색어>` 또는 공백/줄시작 뒤 `@<검색어>` 트리거를 감지한다.
pub(super) fn detect_autocomplete_trigger(
    text: &str,
    cursor_offset: usize,
) -> Option<AutocompleteTrigger> {
    let cursor = clamp_char_boundary(text, cursor_offset);
    let before = text.get(..cursor)?;
    let line_start = before.rfind('\n').map_or(0, |idx| idx + 1);
    let line_before = before.get(line_start..)?;

    if let Some(rest) = line_before.strip_prefix('/')
        && !rest.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        && !rest.contains('/')
    {
        return Some(AutocompleteTrigger {
            kind: AutocompleteKind::Slash,
            query: rest.to_owned(),
            replace_range: line_start..cursor,
        });
    }

    let rel_at = line_before.rfind('@')?;
    let at_line_or_ws = rel_at == 0
        || line_before
            .get(..rel_at)
            .and_then(|prefix| prefix.chars().next_back())
            .is_some_and(char::is_whitespace);
    if !at_line_or_ws {
        return None;
    }
    let query = line_before.get(rel_at + 1..)?;
    if query
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
        || query.contains("..")
    {
        return None;
    }
    Some(AutocompleteTrigger {
        kind: AutocompleteKind::File,
        query: query.to_owned(),
        replace_range: (line_start + rel_at)..cursor,
    })
}

fn strip_control_chars(input: &str) -> String {
    input
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}

fn is_excluded_component(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return true;
    }
    if name.chars().any(char::is_control) {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        ".git" | "target" | "node_modules" | ".ds_store"
    ) || lower.starts_with(".env")
}

/// 상대 경로가 안전하고 제외 규칙(`.git`, `target`, `node_modules`, `.env*`, `..`, 제어문자)을 통과하는지 검사한다.
pub(super) fn is_allowed_workspace_rel_path(rel: &Path) -> bool {
    if rel.as_os_str().is_empty() || rel.is_absolute() {
        return false;
    }
    let mut has_component = false;
    for comp in rel.components() {
        match comp {
            Component::Normal(seg) => {
                let Some(text) = seg.to_str() else {
                    return false;
                };
                if is_excluded_component(text) {
                    return false;
                }
                has_component = true;
            }
            Component::ParentDir
            | Component::RootDir
            | Component::CurDir
            | Component::Prefix(_) => {
                return false;
            }
        }
    }
    has_component
}

fn normalize_rel_display(rel: &Path) -> Option<String> {
    if !is_allowed_workspace_rel_path(rel) {
        return None;
    }
    let mut parts = Vec::new();
    for comp in rel.components() {
        let Component::Normal(seg) = comp else {
            return None;
        };
        parts.push(seg.to_str()?);
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn format_file_insert_text(rel_display: &str) -> String {
    if rel_display.chars().any(char::is_whitespace) {
        let escaped = rel_display.replace('"', "\\\"");
        format!("@\"{escaped}\" ")
    } else {
        format!("@{rel_display} ")
    }
}

fn collect_git_workspace_files(cwd: &Path) -> Option<Vec<String>> {
    let output = krill_proto::os::background_command("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .arg("-C")
        .arg(cwd)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in output.stdout.split(|&b| b == 0) {
        if raw.is_empty() {
            continue;
        }
        let Ok(text) = std::str::from_utf8(raw) else {
            continue;
        };
        let rel = Path::new(text);
        let Some(display) = normalize_rel_display(rel) else {
            continue;
        };
        let full = cwd.join(rel);
        let Ok(meta) = fs::symlink_metadata(&full) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        if seen.insert(display.clone()) {
            files.push(display);
        }
    }
    Some(files)
}

fn collect_walk_workspace_files(cwd: &Path) -> Vec<String> {
    let Ok(root_meta) = fs::symlink_metadata(cwd) else {
        return Vec::new();
    };
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(PathBuf::new());
    let mut visits = 0usize;

    while let Some(rel_dir) = queue.pop_front() {
        if visits >= MAX_WALK_VISITS {
            break;
        }
        let dir_path = if rel_dir.as_os_str().is_empty() {
            cwd.to_path_buf()
        } else {
            cwd.join(&rel_dir)
        };
        let Ok(entries) = fs::read_dir(&dir_path) else {
            continue;
        };
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            if visits >= MAX_WALK_VISITS {
                break;
            }
            visits += 1;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if is_excluded_component(name) {
                continue;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let child_rel = if rel_dir.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                rel_dir.join(name)
            };
            if ft.is_dir() {
                subdirs.push(child_rel);
            } else if ft.is_file()
                && let Some(display) = normalize_rel_display(&child_rel)
                && seen.insert(display.clone())
            {
                files.push(display);
            }
        }
        subdirs.sort();
        for sub in subdirs {
            queue.push_back(sub);
        }
    }
    files
}

fn file_match_rank(path: &str, query_lower: &str) -> Option<u8> {
    if query_lower.is_empty() {
        return Some(0);
    }
    let lower = path.to_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(lower.as_str());
    if base == query_lower {
        Some(0)
    } else if base.starts_with(query_lower) {
        Some(1)
    } else if lower.starts_with(query_lower) {
        Some(2)
    } else if base.contains(query_lower) {
        Some(3)
    } else if lower.contains(query_lower) {
        Some(4)
    } else {
        None
    }
}

fn load_or_collect_workspace_files(cwd: &Path) -> Vec<String> {
    let now = Instant::now();
    if let Ok(mut guard) = workspace_file_cache().lock() {
        guard.retain(|entry| now.duration_since(entry.cached_at) < WORKSPACE_CACHE_TTL);
        if let Some(entry) = guard.iter().find(|entry| entry.cwd == cwd) {
            return entry.files.clone();
        }
    }
    let collected =
        collect_git_workspace_files(cwd).unwrap_or_else(|| collect_walk_workspace_files(cwd));
    if let Ok(mut guard) = workspace_file_cache().lock() {
        guard.retain(|entry| {
            entry.cwd != cwd && now.duration_since(entry.cached_at) < WORKSPACE_CACHE_TTL
        });
        guard.push(CachedWorkspaceFiles {
            cwd: cwd.to_path_buf(),
            cached_at: now,
            files: collected.clone(),
        });
        while guard.len() > WORKSPACE_CACHE_CAP {
            guard.remove(0);
        }
    }
    collected
}

/// 로컬 워크스페이스 `cwd` 기준 `@<검색어>` 상대 경로 파일 자동완성 후보를 최대 80개까지 반환한다.
pub(super) fn scan_workspace_files(cwd: &Path, query: &str) -> Vec<AutocompleteCandidate> {
    let query_clean = strip_control_chars(query);
    if query_clean.contains("..") {
        return Vec::new();
    }
    let raw_files = load_or_collect_workspace_files(cwd);
    let query_lower = query_clean.to_lowercase();
    let mut ranked: Vec<(u8, usize, usize, String)> = raw_files
        .into_iter()
        .filter_map(|path| {
            let rank = file_match_rank(&path, &query_lower)?;
            let depth = path.bytes().filter(|&b| b == b'/').count();
            let len = path.len();
            Some((rank, depth, len, path))
        })
        .collect();
    ranked.sort();
    ranked
        .into_iter()
        .take(MAX_AUTOCOMPLETE_ITEMS)
        .map(|(_, _, _, path)| AutocompleteCandidate {
            insert_text: format_file_insert_text(&path),
            label: path,
            detail: String::new(),
            source: AutocompleteSource::File,
        })
        .collect()
}

fn normalize_agent_family(agent: &str) -> &'static str {
    let lower = agent.trim().to_ascii_lowercase();
    if lower.contains("codex") {
        "codex"
    } else if lower.contains("omp") {
        "omp"
    } else if lower.contains("agy") || lower.contains("antigravity") || lower.contains("gemini") {
        "agy"
    } else if lower.contains("opencode") {
        "opencode"
    } else {
        "claude"
    }
}

/// Returns true when a bare slash command executes locally inside the CLI
/// without echoing a `ChatItemKind::User` turn in the saved transcript.
pub(super) fn slash_command_is_local_non_echoed(_agent: &str, payload: &str) -> bool {
    let trimmed = payload.trim();
    if !trimmed.starts_with('/') || trimmed.contains(char::is_whitespace) {
        return false;
    }
    let cmd = trimmed.trim_start_matches('/').to_ascii_lowercase();
    matches!(
        cmd.as_str(),
        "clear"
            | "compact"
            | "cost"
            | "doctor"
            | "status"
            | "help"
            | "model"
            | "models"
            | "theme"
            | "themes"
            | "permissions"
            | "mcp"
            | "mcps"
            | "memory"
            | "memories"
            | "undo"
            | "redo"
            | "share"
            | "diff"
            | "plugins"
            | "vim"
            | "hooks"
            | "config"
            | "hotkeys"
            | "agents"
            | "rename"
            | "fork"
            | "branch"
            | "tree"
            | "export"
            | "connect"
    )
}

/// Returns true when a bare slash command opens a TUI dialog that Chat cannot render inline,
/// so the pane should switch to Terminal only after prompt delivery settles.
pub(super) fn slash_command_opens_in_terminal(
    agent: &str,
    payload: &str,
    model_picker_supported: bool,
) -> bool {
    let trimmed = payload.trim();
    if !trimmed.starts_with('/') || trimmed.contains(char::is_whitespace) {
        return false;
    }
    let cmd = trimmed.trim_start_matches('/').to_ascii_lowercase();
    if cmd == "model" && !model_picker_supported {
        return true;
    }
    match normalize_agent_family(agent) {
        "codex" => matches!(
            cmd.as_str(),
            "permissions"
                | "keymap"
                | "experimental"
                | "memories"
                | "skills"
                | "review"
                | "rename"
                | "resume"
                | "title"
                | "statusline"
                | "theme"
                | "pets"
                | "plugins"
                | "feedback"
                | "subagents"
        ),
        "opencode" => matches!(
            cmd.as_str(),
            "models"
                | "agents"
                | "sessions"
                | "skills"
                | "mcps"
                | "mcp"
                | "connect"
                | "status"
                | "themes"
                | "theme"
                | "diff"
                | "editor"
                | "debug"
                | "help"
                | "fork"
                | "rename"
                | "timeline"
                | "export"
        ),
        "omp" => matches!(
            cmd.as_str(),
            "model"
                | "switch"
                | "resume"
                | "fork"
                | "branch"
                | "tree"
                | "git"
                | "settings"
                | "extensions"
                | "hotkeys"
        ),
        _ => matches!(
            cmd.as_str(),
            "resume"
                | "permissions"
                | "login"
                | "logout"
                | "mcp"
                | "hooks"
                | "agents"
                | "config"
                | "theme"
                | "vim"
                | "plugins"
                | "ide"
                | "memory"
                | "status"
                | "doctor"
                | "cost"
        ),
    }
}

fn builtin_slash_commands(family: &str) -> Vec<(&'static str, &'static str)> {
    use krill_app::i18n::catalog::chat as ctext;
    let mut base = vec![
        ("model", ctext::SLASH_MODEL_DESC.get()),
        ("btw", ctext::SLASH_BTW_DESC.get()),
        ("side", ctext::SLASH_SIDE_DESC.get()),
        ("compact", ctext::SLASH_COMPACT_DESC.get()),
        ("clear", ctext::SLASH_CLEAR_DESC.get()),
        ("review", ctext::SLASH_REVIEW_DESC.get()),
    ];
    match family {
        "codex" => {
            base.extend([
                ("init", ctext::SLASH_INIT_AGENTS_DESC.get()),
                ("status", ctext::SLASH_STATUS_DESC.get()),
                ("permissions", ctext::SLASH_PERMISSIONS_DESC.get()),
                ("diff", ctext::SLASH_DIFF_DESC.get()),
            ]);
        }
        "agy" => {
            base.extend([
                ("plan", ctext::SLASH_PLAN_DESC.get()),
                ("boost", ctext::SLASH_BOOST_DESC.get()),
                ("goal", ctext::SLASH_GOAL_DESC.get()),
            ]);
        }
        "opencode" => {
            base.extend([
                ("init", ctext::SLASH_INIT_AGENTS_DESC.get()),
                ("undo", ctext::SLASH_UNDO_DESC.get()),
                ("redo", ctext::SLASH_REDO_DESC.get()),
                ("share", ctext::SLASH_SHARE_DESC.get()),
            ]);
        }
        _ => {
            base.extend([
                ("init", ctext::SLASH_INIT_CLAUDE_DESC.get()),
                ("cost", ctext::SLASH_COST_DESC.get()),
                ("doctor", ctext::SLASH_DOCTOR_DESC.get()),
            ]);
        }
    }
    base
}

fn read_markdown_description(path: &Path) -> String {
    use std::io::Read as _;
    let Ok(meta) = fs::symlink_metadata(path) else {
        return String::new();
    };
    if !meta.is_file() {
        return String::new();
    }
    let Ok(file) = fs::File::open(path) else {
        return String::new();
    };
    let mut buf = Vec::new();
    if file
        .take(MAX_MD_HEAD_BYTES as u64)
        .read_to_end(&mut buf)
        .is_err()
    {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    if let Some(first) = lines.next()
        && first.trim() == "---"
    {
        for line in lines.by_ref() {
            let trimmed = line.trim();
            if trimmed == "---" {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("description:") {
                let unquoted = rest.trim().trim_matches('"').trim_matches('\'');
                let cleaned = strip_control_chars(unquoted);
                if !cleaned.is_empty() {
                    return cleaned.chars().take(100).collect();
                }
            }
        }
    }
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "---" || trimmed.starts_with('#') {
            continue;
        }
        let cleaned = strip_control_chars(trimmed);
        if !cleaned.is_empty() {
            return cleaned.chars().take(100).collect();
        }
    }
    String::new()
}

fn valid_slash_slug(slug: &str) -> Option<String> {
    let cleaned = strip_control_chars(slug);
    if cleaned.is_empty()
        || cleaned.starts_with('.')
        || cleaned.chars().any(|ch| ch.is_whitespace() || ch == '/')
    {
        return None;
    }
    Some(cleaned)
}

fn collect_command_md_dir(dir: &Path, out: &mut Vec<(String, String, AutocompleteSource)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(slug) = valid_slash_slug(stem) else {
            continue;
        };
        let desc = read_markdown_description(&path);
        found.push((slug, desc, AutocompleteSource::Command));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    out.extend(found);
}

fn collect_skill_dir(dir: &Path, out: &mut Vec<(String, String, AutocompleteSource)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(slug) = valid_slash_slug(name) else {
            continue;
        };
        let desc = read_markdown_description(&skill_md);
        found.push((slug, desc, AutocompleteSource::Skill));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    out.extend(found);
}

fn workspace_ancestors(cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (depth, anc) in cwd.ancestors().enumerate() {
        if depth >= MAX_ANCESTOR_DEPTH {
            break;
        }
        if home.is_some_and(|h| anc == h) {
            break;
        }
        out.push(anc.to_path_buf());
        if anc.join(".git").exists() {
            break;
        }
    }
    out
}

/// 에이전트 프로필(`claude`, `codex`, `agy`, `opencode`)별 슬래시 명령/스킬 카탈로그를 최대 80개까지 반환한다.
/// `local == false`(원격 세션 등)일 때는 로컬 파일시스템에 접근하지 않고 기본 명령만 반환한다.
pub(super) fn scan_slash_catalog(
    home: Option<&Path>,
    cwd: Option<&Path>,
    agent: &str,
    local: bool,
    query: &str,
) -> Vec<AutocompleteCandidate> {
    let family = normalize_agent_family(agent);
    let mut raw: Vec<(String, String, AutocompleteSource)> = builtin_slash_commands(family)
        .into_iter()
        .map(|(slug, desc)| {
            (
                slug.to_owned(),
                desc.to_owned(),
                AutocompleteSource::Builtin,
            )
        })
        .collect();

    if local {
        if let Some(cwd) = cwd {
            for anc in workspace_ancestors(cwd, home) {
                match family {
                    "codex" => {
                        collect_skill_dir(&anc.join(".codex/skills"), &mut raw);
                        collect_skill_dir(&anc.join(".agents/skills"), &mut raw);
                    }
                    "agy" => {
                        collect_command_md_dir(&anc.join(".agent/workflows"), &mut raw);
                        collect_command_md_dir(&anc.join(".agents/workflows"), &mut raw);
                        collect_skill_dir(&anc.join(".agent/skills"), &mut raw);
                        collect_skill_dir(&anc.join(".agents/skills"), &mut raw);
                    }
                    "opencode" => {
                        collect_command_md_dir(&anc.join(".opencode/commands"), &mut raw);
                        collect_command_md_dir(&anc.join(".opencode/command"), &mut raw);
                        collect_skill_dir(&anc.join(".opencode/skills"), &mut raw);
                        collect_skill_dir(&anc.join(".agents/skills"), &mut raw);
                    }
                    _ => {
                        collect_command_md_dir(&anc.join(".claude/commands"), &mut raw);
                        collect_skill_dir(&anc.join(".claude/skills"), &mut raw);
                        collect_skill_dir(&anc.join(".agents/skills"), &mut raw);
                    }
                }
            }
        }
        if let Some(home) = home {
            match family {
                "codex" => {
                    collect_command_md_dir(&home.join(".codex/prompts"), &mut raw);
                    collect_skill_dir(&home.join(".codex/skills"), &mut raw);
                    collect_skill_dir(&home.join(".agents/skills"), &mut raw);
                }
                "agy" => {
                    collect_command_md_dir(
                        &home.join(".gemini/antigravity/global_workflows"),
                        &mut raw,
                    );
                    collect_skill_dir(&home.join(".gemini/antigravity-cli/skills"), &mut raw);
                    collect_skill_dir(&home.join(".gemini/skills"), &mut raw);
                    collect_skill_dir(&home.join(".agents/skills"), &mut raw);
                }
                "opencode" => {
                    collect_command_md_dir(&home.join(".config/opencode/commands"), &mut raw);
                    collect_command_md_dir(&home.join(".config/opencode/command"), &mut raw);
                    collect_skill_dir(&home.join(".config/opencode/skills"), &mut raw);
                    collect_skill_dir(&home.join(".agents/skills"), &mut raw);
                }
                _ => {
                    collect_command_md_dir(&home.join(".claude/commands"), &mut raw);
                    collect_skill_dir(&home.join(".claude/skills"), &mut raw);
                    collect_skill_dir(&home.join(".agents/skills"), &mut raw);
                }
            }
        }
    }

    let query_lower = strip_control_chars(query).to_lowercase();
    let mut seen = BTreeSet::new();
    let mut ranked: Vec<(u8, usize, String, String, AutocompleteSource)> = Vec::new();
    for (idx, (slug, desc, source)) in raw.into_iter().enumerate() {
        let key_lower = slug.to_lowercase();
        if !seen.insert(key_lower.clone()) {
            continue;
        }
        let desc_clean = strip_control_chars(&desc);
        let rank = if query_lower.is_empty() {
            match source {
                AutocompleteSource::Builtin => 0,
                AutocompleteSource::Command => 1,
                AutocompleteSource::Skill => 2,
                AutocompleteSource::File => 3,
            }
        } else if key_lower == query_lower {
            0
        } else if key_lower.starts_with(&query_lower) {
            1
        } else if key_lower.contains(&query_lower) {
            2
        } else if desc_clean.to_lowercase().contains(&query_lower) {
            3
        } else {
            continue;
        };
        ranked.push((rank, idx, slug, desc_clean, source));
    }

    ranked.sort_by_key(|a| (a.0, a.1));
    ranked
        .into_iter()
        .take(MAX_AUTOCOMPLETE_ITEMS)
        .map(|(_, _, slug, detail, source)| {
            let cmd = format!("/{slug}");
            AutocompleteCandidate {
                insert_text: format!("{cmd} "),
                label: cmd,
                detail,
                source,
            }
        })
        .collect()
}

pub(super) fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Parses a daemon `command_catalog` JSON workflow response into filtered `AutocompleteCandidate` items.
pub(super) fn parse_remote_slash_items(
    raw: &str,
    query: &str,
) -> Option<Vec<AutocompleteCandidate>> {
    let root: serde_json::Value = serde_json::from_str(raw).ok()?;
    if root.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let items = root.pointer("/data/items")?.as_array()?;
    let query_lower = strip_control_chars(query).to_lowercase();
    let mut candidates = Vec::new();
    for entry in items {
        let raw_name = entry
            .get("text")
            .or_else(|| entry.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let clean = strip_control_chars(raw_name).trim().to_owned();
        if clean.is_empty() {
            continue;
        }
        let label = if clean.starts_with('/') || clean.starts_with('$') {
            clean
        } else {
            format!("/{clean}")
        };
        let desc = strip_control_chars(
            entry
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
        );
        if !query_lower.is_empty()
            && !label.to_lowercase().contains(&query_lower)
            && !desc.to_lowercase().contains(&query_lower)
        {
            continue;
        }
        let source = match entry
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "skill" => AutocompleteSource::Skill,
            "command"
                if entry.get("scope").and_then(serde_json::Value::as_str) == Some("Built-in") =>
            {
                AutocompleteSource::Builtin
            }
            _ => AutocompleteSource::Command,
        };
        candidates.push(AutocompleteCandidate {
            insert_text: format!("{label} "),
            label,
            detail: desc,
            source,
        });
        if candidates.len() >= MAX_AUTOCOMPLETE_ITEMS {
            break;
        }
    }
    Some(candidates)
}

/// Parses a daemon `mention_files` JSON workflow response into safe relative `AutocompleteCandidate` items.
pub(super) fn parse_remote_mention_items(
    raw: &str,
    query: &str,
) -> Option<Vec<AutocompleteCandidate>> {
    let root: serde_json::Value = serde_json::from_str(raw).ok()?;
    if root.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let items = root.pointer("/data/items")?.as_array()?;
    let query_lower = strip_control_chars(query).to_lowercase();
    let mut candidates = Vec::new();
    for entry in items {
        let raw_path = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let clean = strip_control_chars(raw_path).trim().to_owned();
        if clean.is_empty()
            || clean.starts_with('/')
            || clean.contains('\\')
            || Path::new(&clean)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            continue;
        }
        if !query_lower.is_empty() && !clean.to_lowercase().contains(&query_lower) {
            continue;
        }
        let insert_text = if clean.contains(char::is_whitespace) {
            format!("@\"{clean}\" ")
        } else {
            format!("@{clean} ")
        };
        candidates.push(AutocompleteCandidate {
            label: clean,
            detail: String::new(),
            insert_text,
            source: AutocompleteSource::File,
        });
        if candidates.len() >= MAX_AUTOCOMPLETE_ITEMS {
            break;
        }
    }
    Some(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let unique = format!(
            "krill-chat-catalog-{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let dir = std::env::temp_dir().join(unique);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn trigger_detection_handles_slash_at_line_start_and_file_mentions_with_utf8() {
        assert_eq!(
            detect_autocomplete_trigger("/comp", 5),
            Some(AutocompleteTrigger {
                kind: AutocompleteKind::Slash,
                query: "comp".into(),
                replace_range: 0..5,
            })
        );
        // Slash on a second line after newline triggers at that line's start.
        let multiline = "첫 줄 안내\n/rev";
        assert_eq!(
            detect_autocomplete_trigger(multiline, multiline.len()),
            Some(AutocompleteTrigger {
                kind: AutocompleteKind::Slash,
                query: "rev".into(),
                replace_range: "첫 줄 안내\n".len()..multiline.len(),
            })
        );
        // Mid-line slash or path does not trigger slash autocomplete.
        assert_eq!(
            detect_autocomplete_trigger("확인해 /usr/bin", "확인해 /usr".len()),
            None
        );
        // Slash followed by space closes slash trigger, allowing subsequent @file trigger.
        let cmd_and_file = "/review @src/ma";
        assert_eq!(
            detect_autocomplete_trigger(cmd_and_file, cmd_and_file.len()),
            Some(AutocompleteTrigger {
                kind: AutocompleteKind::File,
                query: "src/ma".into(),
                replace_range: 8..cmd_and_file.len(),
            })
        );
        // Email address must never trigger @file autocomplete.
        let email = "contact user@example.com";
        assert_eq!(detect_autocomplete_trigger(email, email.len()), None);
        // Mid-character UTF-8 offset is safely clamped without panicking.
        let korean = "한글 @파일";
        let mid_byte = 1;
        assert_eq!(detect_autocomplete_trigger(korean, mid_byte), None);
        assert_eq!(
            detect_autocomplete_trigger(korean, korean.len()),
            Some(AutocompleteTrigger {
                kind: AutocompleteKind::File,
                query: "파일".into(),
                replace_range: "한글 ".len()..korean.len(),
            })
        );
        // Parent traversal in @query is rejected immediately.
        assert_eq!(detect_autocomplete_trigger("@../secret", 10), None);
    }

    #[test]
    fn workspace_file_scanner_excludes_dotenv_git_target_node_modules_and_caps_at_80() {
        let root = temp_test_dir("files");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn ok() {}\n").unwrap();
        fs::write(root.join(".env"), "SECRET=1\n").unwrap();
        fs::write(root.join(".env.local"), "SECRET=2\n").unwrap();
        fs::write(root.join(".envrc"), "export SECRET=3\n").unwrap();
        fs::write(root.join(".git/config"), "[core]\n").unwrap();
        fs::write(root.join("target/debug/app"), "bin").unwrap();
        fs::write(root.join("node_modules/pkg/index.js"), "module.exports={}").unwrap();

        #[cfg(unix)]
        {
            let outside = temp_test_dir("outside");
            let secret = outside.join("escaped.txt");
            fs::write(&secret, "secret").unwrap();
            std::os::unix::fs::symlink(&secret, root.join("src/escaped_link.txt")).unwrap();
            fs::remove_dir_all(outside).ok();
        }

        for idx in 0..95 {
            fs::write(root.join(format!("src/mod_{idx:03}.rs")), "// ok\n").unwrap();
        }

        let all = scan_workspace_files(&root, "");
        assert_eq!(all.len(), MAX_AUTOCOMPLETE_ITEMS);
        assert!(all.iter().all(|c| !c.label.contains(".env")));
        assert!(all.iter().all(|c| !c.label.contains(".git")));
        assert!(all.iter().all(|c| !c.label.contains("target/")));
        assert!(all.iter().all(|c| !c.label.contains("node_modules")));
        assert!(all.iter().all(|c| !c.label.contains("escaped_link")));

        let env_probe = scan_workspace_files(&root, "env");
        assert!(
            env_probe.is_empty(),
            ".env* files must never appear: {env_probe:?}"
        );

        let lib_probe = scan_workspace_files(&root, "lib.rs");
        assert_eq!(lib_probe.len(), 1);
        assert_eq!(lib_probe[0].label, "src/lib.rs");
        assert_eq!(lib_probe[0].insert_text, "@src/lib.rs ");

        assert!(!is_allowed_workspace_rel_path(Path::new("../Cargo.toml")));
        assert!(!is_allowed_workspace_rel_path(Path::new(
            "src/../../etc/passwd"
        )));
        assert!(!is_allowed_workspace_rel_path(Path::new("/etc/passwd")));
        assert!(!is_allowed_workspace_rel_path(Path::new(".env.production")));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn slash_catalog_loads_agent_skills_strips_control_chars_and_guards_remote() {
        let root = temp_test_dir("slash");
        let home = root.join("home");
        let repo = root.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join(".claude/commands")).unwrap();
        fs::create_dir_all(repo.join(".claude/skills/krig")).unwrap();
        fs::create_dir_all(home.join(".codex/skills/deploy-check")).unwrap();
        fs::create_dir_all(home.join(".gemini/antigravity-cli/skills/agy-boost")).unwrap();
        fs::create_dir_all(repo.join(".opencode/commands")).unwrap();

        fs::write(
            repo.join(".claude/commands/ship-it.md"),
            "---\ndescription: \"Ship release\x07\x1b safely\"\n---\nBody\n",
        )
        .unwrap();
        fs::write(
            repo.join(".claude/skills/krig/SKILL.md"),
            "---\nname: krig\ndescription: Engineering verification harness\n---\n",
        )
        .unwrap();
        fs::write(
            home.join(".codex/skills/deploy-check/SKILL.md"),
            "# Heading\nVerify deployment invariants\n",
        )
        .unwrap();
        fs::write(
            home.join(".gemini/antigravity-cli/skills/agy-boost/SKILL.md"),
            "---\ndescription: Deep cross-check\n---\n",
        )
        .unwrap();
        fs::write(
            repo.join(".opencode/commands/lint-fix.md"),
            "Fix lint warnings\n",
        )
        .unwrap();

        let claude_items = scan_slash_catalog(Some(&home), Some(&repo), "claude", true, "");
        assert!(claude_items.iter().any(|c| c.label == "/model"));
        assert!(claude_items.iter().any(|c| c.label == "/compact"));
        assert!(claude_items.iter().any(|c| c.label == "/clear"));
        assert!(claude_items.iter().any(|c| c.label == "/review"));
        let ship = claude_items.iter().find(|c| c.label == "/ship-it").unwrap();
        assert_eq!(ship.detail, "Ship release safely");
        assert!(!ship.detail.chars().any(char::is_control));
        assert!(claude_items.iter().any(|c| c.label == "/krig"));

        let codex_items = scan_slash_catalog(Some(&home), Some(&repo), "codex", true, "deploy");
        assert_eq!(codex_items.len(), 1);
        assert_eq!(codex_items[0].label, "/deploy-check");
        assert_eq!(codex_items[0].insert_text, "/deploy-check ");

        let agy_items = scan_slash_catalog(Some(&home), Some(&repo), "agy", true, "agy-boost");
        assert_eq!(agy_items.len(), 1);
        assert_eq!(agy_items[0].label, "/agy-boost");

        let oc_items = scan_slash_catalog(Some(&home), Some(&repo), "opencode", true, "lint");
        assert_eq!(oc_items.len(), 1);
        assert_eq!(oc_items[0].label, "/lint-fix");

        // Remote session (`local == false`) must NOT read local skills/commands.
        let remote_items = scan_slash_catalog(Some(&home), Some(&repo), "claude", false, "");
        assert!(remote_items.iter().any(|c| c.label == "/model"));
        assert!(
            remote_items
                .iter()
                .all(|c| c.label != "/ship-it" && c.label != "/krig")
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn tui_opening_slash_commands_are_recognized_without_false_positives() {
        assert!(slash_command_opens_in_terminal("claude", "/resume", true));
        assert!(slash_command_opens_in_terminal(
            "claude",
            " /permissions ",
            true
        ));
        assert!(slash_command_opens_in_terminal("claude", "/login", true));
        assert!(!slash_command_opens_in_terminal("claude", "/model", true));
        assert!(slash_command_opens_in_terminal("claude", "/model", false));
        assert!(!slash_command_opens_in_terminal(
            "claude", "/compact", false
        ));
        assert!(!slash_command_opens_in_terminal("claude", "/clear", false));
        assert!(!slash_command_opens_in_terminal("claude", "/review", false));
        assert!(!slash_command_opens_in_terminal(
            "claude",
            "/resume session-123",
            true
        ));
        assert!(slash_command_opens_in_terminal(
            "codex",
            "/permissions",
            true
        ));
        assert!(slash_command_opens_in_terminal("opencode", "/models", true));
        assert!(slash_command_opens_in_terminal("omp", "/model", true));
    }
}
