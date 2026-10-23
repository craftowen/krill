//! MK-057 Jump 팔레트(Cmd+J) + MK-181 AI Vault 통합 검색:
//! Orca `WorktreeJumpPalette` 패리티 레이아웃(배경 스크림, 상단 검색창 + 필터 버튼/칩,
//! `최근 채팅 및 터미널` 섹션, `최근 작업 공간` 섹션, 빠른 액션/명령 섹션, 하단 단축키 푸터)으로
//! 열린 세션·워크트리·AI Vault 대화 기록·명령을 한 화면에서 탐색·실행한다.
//!
//! 검색어는 조합 중인 한글(IME preedit)까지 포함해 즉시 거른다: gpui-component `Input`은 조합 중에 `Change`를 내지
//! 않으므로 입력 상태를 직접 관찰한다.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    Context, Entity, EventEmitter, FontWeight, Hsla, KeyDownEvent, ScrollHandle, Subscription,
    Window, deferred, div, prelude::*, px,
};
use gpui_component::input::{Enter, Escape, Input, InputState, MoveDown, MoveUp};
use gpui_component::{Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::SessionId;
use krill_proto::agent_history::{VaultAgent, VaultHit, VaultScope};
use krill_proto::control::AgentState;

use crate::i18n::catalog::agent_history as vault_text;
use crate::i18n::catalog::jump as text;
use crate::layout::TabColor;

const EMPTY_QUERY_RECENT_TAB_CAP: usize = 6;
const EMPTY_QUERY_WORKTREE_CAP: usize = 5;
const VISIBLE_ROWS: usize = 8;
const ROW_HEIGHT: f32 = 28.;

/// 팔레트 한 줄(열린 세션 또는 빠른 열기 파일). `label`의 소문자 글자를 미리 펼쳐 둬 키 입력마다 할당하지 않는다.
#[expect(
    clippy::struct_excessive_bools,
    reason = "UI row model carries independent display flags (needs_you, unread, current tab/worktree, file)"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: SessionId,
    pub label: String,
    pub needs_you: bool,
    pub title: String,
    pub agent: String,
    pub state: Option<AgentState>,
    pub unread: bool,
    pub age: String,
    pub is_current_tab: bool,
    pub is_current_worktree: bool,
    pub host_label: String,
    pub repo_name: String,
    pub worktree_name: String,
    pub is_file: bool,
    chars: Vec<char>,
    filename_chars: Vec<char>,
}

impl Item {
    pub fn new(id: SessionId, label: String, needs_you: bool) -> Self {
        let agent = label
            .split_whitespace()
            .next()
            .filter(|w| {
                matches!(
                    *w,
                    "claude"
                        | "codex"
                        | "agy"
                        | "antigravity"
                        | "gemini"
                        | "cursor"
                        | "cursor-agent"
                        | "opencode"
                )
            })
            .unwrap_or_default()
            .to_owned();
        let filename_chars = lower_chars(
            Path::new(&label)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&label),
        );
        let chars = label.chars().flat_map(char::to_lowercase).collect();
        Self {
            id,
            title: label.clone(),
            label,
            needs_you,
            agent,
            state: None,
            unread: false,
            age: String::new(),
            is_current_tab: false,
            is_current_worktree: false,
            host_label: String::new(),
            repo_name: String::new(),
            worktree_name: String::new(),
            is_file: false,
            chars,
            filename_chars,
        }
    }

    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = agent.into();
        self
    }

    pub fn rebuild_search_chars(&mut self) {
        self.filename_chars = lower_chars(
            Path::new(&self.label)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&self.label),
        );
        let combined = format!(
            "{} {} {} {} {} {}",
            self.label, self.title, self.agent, self.repo_name, self.worktree_name, self.id.0
        );
        self.chars = combined.chars().flat_map(char::to_lowercase).collect();
    }
}

/// 최근 작업 공간(워크트리·폴더) 한 줄.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeItem {
    pub path: PathBuf,
    pub session_id: Option<SessionId>,
    pub name: String,
    pub branch: String,
    pub repo_name: String,
    pub host_label: String,
    pub age: String,
    pub state: Option<AgentState>,
    pub unread: bool,
    pub is_main: bool,
    pub is_current: bool,
    chars: Vec<char>,
}

impl WorktreeItem {
    #[expect(
        clippy::too_many_arguments,
        reason = "WorktreeItem constructor initializes all display fields directly"
    )]
    #[must_use]
    pub fn new(
        path: PathBuf,
        session_id: Option<SessionId>,
        name: String,
        branch: String,
        repo_name: String,
        host_label: String,
        age: String,
        state: Option<AgentState>,
        unread: bool,
        is_main: bool,
        is_current: bool,
    ) -> Self {
        let combined = format!("{name} {branch} {repo_name} {}", path.display());
        let chars = combined.chars().flat_map(char::to_lowercase).collect();
        Self {
            path,
            session_id,
            name,
            branch,
            repo_name,
            host_label,
            age,
            state,
            unread,
            is_main,
            is_current,
            chars,
        }
    }
}

/// Jump 팔레트의 워크트리 한 줄(OWE-865): 세션이 열린 워크트리(열기)와
/// 아직 세션이 없는 워크트리·폴더(새 세션 만들기)를 함께 검색한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub label: String,
    pub detail: Option<String>,
    pub session: Option<SessionId>,
    chars: Vec<char>,
}

impl WorktreeEntry {
    pub fn new(
        path: PathBuf,
        label: String,
        detail: Option<String>,
        session: Option<SessionId>,
    ) -> Self {
        let chars = lower_chars(&format!("{label} {}", path.display()));
        Self {
            path,
            label,
            detail,
            session,
            chars,
        }
    }
}

impl From<WorktreeEntry> for WorktreeItem {
    fn from(entry: WorktreeEntry) -> Self {
        Self::new(
            entry.path,
            entry.session,
            entry.label,
            String::new(),
            entry.detail.unwrap_or_default(),
            String::new(),
            String::new(),
            None,
            false,
            false,
            false,
        )
    }
}

/// 나노초 타임스탬프 차이를 Orca `formatPaletteSessionAge` 규칙(`1분 미만`, `14분`, `1시간`, `2일`)으로 포맷한다.
#[must_use]
pub fn format_age_ns(since_unix_ns: u64, now_unix_ns: u64) -> String {
    if since_unix_ns == 0 || now_unix_ns == 0 {
        return String::new();
    }
    format_elapsed_secs(now_unix_ns.saturating_sub(since_unix_ns) / 1_000_000_000)
}

/// 초 단위 수정 시각을 상대 시간 문자열로 포맷한다.
#[must_use]
pub fn format_age_secs(modified_secs: u64, now_secs: u64) -> String {
    if modified_secs == 0 || now_secs == 0 {
        return String::new();
    }
    format_elapsed_secs(now_secs.saturating_sub(modified_secs))
}

fn format_elapsed_secs(secs: u64) -> String {
    if secs < 60 {
        text::AGE_UNDER_MIN.get().to_owned()
    } else if secs < 3600 {
        text::AGE_MIN.format(&[("n", &(secs / 60).to_string())])
    } else if secs < 48 * 3600 {
        text::AGE_HOUR.format(&[("n", &(secs / 3600).to_string())])
    } else {
        text::AGE_DAY.format(&[("n", &(secs / 86_400).to_string())])
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn repo_badge_color(repo_name: &str) -> Hsla {
    const COLORS: [TabColor; 8] = [
        TabColor::Teal,
        TabColor::Blue,
        TabColor::Red,
        TabColor::Purple,
        TabColor::Green,
        TabColor::Orange,
        TabColor::Pink,
        TabColor::Yellow,
    ];
    let hash = repo_name.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(usize::from(b))
    });
    crate::ui::tab_tag_color(COLORS[hash % COLORS.len()])
}

fn cwd_repo_and_worktree(cwd: &str) -> (String, String) {
    let trimmed = cwd.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return (String::new(), String::new());
    }
    let path = Path::new(trimmed);
    let leaf = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|p| {
            !p.is_empty()
                && !matches!(
                    p.as_str(),
                    "Users" | "home" | "tmp" | "var" | "private" | "Documents" | "wProject"
                )
        });
    if let Some(repo) = parent {
        (repo, leaf)
    } else {
        (leaf, String::new())
    }
}

const HANGUL_BASE: u32 = 0xAC00;
const INITIALS: [char; 19] = [
    'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ',
    'ㅌ', 'ㅍ', 'ㅎ',
];

/// 한글 음절의 `0xAC00`부터의 오프셋(초성 × 588 + 중성 × 28 + 종성).
fn syllable(c: char) -> Option<u32> {
    u32::from(c)
        .checked_sub(HANGUL_BASE)
        .filter(|&s| s < 11_172)
}

/// 검색어 글자 `q`가 `c`에 맞는가. 조합 중 글자를 위해 받침 없는 음절은 같은 초성·중성 음절(받침 무관)에,
/// 자음(ㄱ~ㅎ)은 그 초성의 음절에 맞는다(초성 검색 겸용).
// ponytail: 받침이 다음 음절로 넘어가는 순간("한" → "하나")은 한 타 동안 놓친다. 불편하면 자모 분해 비교로.
fn char_matches(q: char, c: char) -> bool {
    if q == c {
        return true;
    }
    let Some(s) = syllable(c) else {
        return false;
    };
    if let Some(qs) = syllable(q) {
        return qs % 28 == 0 && qs / 28 == s / 28;
    }
    INITIALS
        .iter()
        .position(|&i| i == q)
        .and_then(|i| u32::try_from(i).ok())
        .is_some_and(|i| i == s / 588)
}

/// `query`가 `text`의 부분 수열이면 점수(연속 일치·단어 시작에 가점), 아니면 `None`.
// ponytail: 왼쪽부터 탐욕 일치라 뒤쪽의 더 좋은 연속 일치를 놓칠 수 있다. 순위 불만이 나오면 DP(fzf v2식)로.
fn score(query: &[char], text: &[char]) -> Option<u32> {
    let mut total = 0;
    let mut from = 0;
    let mut prev = None;
    for &q in query {
        let i = from + text.get(from..)?.iter().position(|&c| char_matches(q, c))?;
        total += 1;
        if prev.is_some_and(|p: usize| p + 1 == i) {
            total += 4;
        }
        if i.checked_sub(1)
            .and_then(|j| text.get(j))
            .is_none_or(|c| !c.is_alphanumeric())
        {
            total += 2;
        }
        prev = Some(i);
        from = i + 1;
    }
    Some(total)
}

/// 일치하는 항목의 인덱스. 점수 높은 순, 같으면 `boost`가 참인 항목 먼저, 그다음 원래 순서. 공백은 무시한다.
fn rank(all: &[&[char]], boost: &[bool], query: &str) -> Vec<usize> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return (0..all.len())
            .filter(|&i| boost.get(i).copied().unwrap_or(false))
            .chain((0..all.len()).filter(|&i| !boost.get(i).copied().unwrap_or(false)))
            .collect();
    }
    let mut hits: Vec<_> = all
        .iter()
        .enumerate()
        .filter_map(|(i, text)| {
            score(&query, text).map(|s| (Reverse(s), !boost.get(i).copied().unwrap_or(false), i))
        })
        .collect();
    hits.sort_unstable();
    hits.into_iter().map(|(_, _, i)| i).collect()
}

/// 일치하는 항목의 인덱스. 점수 높은 순, 같으면 입력 대기 세션 먼저, 그다음 원래 순서. 공백은 무시한다.
pub fn search(items: &[Item], query: &str) -> Vec<usize> {
    let all: Vec<&[char]> = items.iter().map(|item| item.chars.as_slice()).collect();
    let boost: Vec<bool> = items.iter().map(|item| item.needs_you).collect();
    if !query.trim().is_empty() && items.iter().any(|item| item.is_file) {
        let query = lower_chars(query.trim());
        let mut hits: Vec<_> = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let path_score = score(&query, &item.chars)?;
                let bonus = score(&query, &item.filename_chars).map_or(0, |s| s + 100);
                Some((Reverse(path_score + bonus), index))
            })
            .collect();
        hits.sort_unstable();
        hits.into_iter().map(|(_, index)| index).collect()
    } else {
        rank(&all, &boost, query)
    }
}

fn lower_chars(text: &str) -> Vec<char> {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// 워크트리 목록에서 `query`와 일치하는 인덱스(점수 높은 순, 원래 순서는 유지).
pub fn worktree_search(entries: &[WorktreeEntry], query: &str) -> Vec<usize> {
    let all: Vec<&[char]> = entries.iter().map(|entry| entry.chars.as_slice()).collect();
    rank(&all, &vec![false; entries.len()], query)
}

fn search_worktrees(
    items: &[WorktreeItem],
    query: &str,
    project_filter: Option<&str>,
) -> Vec<usize> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let mut hits: Vec<_> = items
        .iter()
        .enumerate()
        .filter(|(_, wt)| project_filter.is_none_or(|p| wt.repo_name == p))
        .filter_map(|(i, wt)| score(&query, &wt.chars).map(|s| (Reverse(s), !wt.is_current, i)))
        .collect();
    hits.sort_unstable();
    hits.into_iter().map(|(_, _, i)| i).collect()
}

/// 팔레트에서 바로 실행하는 빠른 액션(OWE-865). `QuickCommand`는 설정의
/// `quick_commands` 목록 인덱스를 가리킨다(팔레트가 닫히면 무효).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    NewWorktree,
    NewTab,
    SplitRight,
    SplitDown,
    OpenSettings,
    ToggleSidebar,
    ToggleExplorer,
    ToggleSearch,
    QuickCommand(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionEntry {
    pub kind: ActionKind,
    pub label: String,
    chars: Vec<char>,
}

impl ActionEntry {
    pub fn new(kind: ActionKind, label: String) -> Self {
        let chars = lower_chars(&label);
        Self { kind, label, chars }
    }
}

/// 빠른 액션 목록에서 `query`와 일치하는 인덱스(점수 높은 순, 원래 순서는 유지).
pub fn action_search(actions: &[ActionEntry], query: &str) -> Vec<usize> {
    let all: Vec<&[char]> = actions
        .iter()
        .map(|action| action.chars.as_slice())
        .collect();
    rank(&all, &vec![false; actions.len()], query)
}

/// 팔레트 진입점별 섹션 순서(OWE-865): ⌘J는 세션부터, ⇧⌘P는 액션부터 보여준다.
/// OWE-862: ⌘P 파일 열기는 세션 목록만 쓰고 파일용 빈 문구·플레이스홀더를 쓴다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Jump,
    Commands,
    Files,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Sessions,
    Vault,
    Worktrees,
    Actions,
    QuickCommands,
}

impl Mode {
    fn sections(self) -> [Section; 5] {
        match self {
            Mode::Jump | Mode::Files => [
                Section::Sessions,
                Section::Vault,
                Section::Worktrees,
                Section::Actions,
                Section::QuickCommands,
            ],
            Mode::Commands => [
                Section::Actions,
                Section::QuickCommands,
                Section::Sessions,
                Section::Vault,
                Section::Worktrees,
            ],
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Mode::Jump => text::PLACEHOLDER.get(),
            Mode::Commands => text::PLACEHOLDER_COMMANDS.get(),
            Mode::Files => text::PLACEHOLDER_FILES.get(),
        }
    }

    fn empty_text(self) -> &'static str {
        match self {
            Mode::Files => text::EMPTY_FILES.get(),
            Mode::Jump | Mode::Commands => text::EMPTY.get(),
        }
    }
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(rest) = trimmed.strip_prefix("/private/tmp")
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("/tmp{rest}");
    }
    if let Some(rest) = trimmed.strip_prefix("/private/var")
        && (rest.is_empty() || rest.starts_with('/'))
    {
        return format!("/var{rest}");
    }
    trimmed.to_owned()
}

fn filter_vault(
    hits: &[VaultHit],
    query: &str,
    scope: VaultScope,
    agent_filter: Option<VaultAgent>,
    project_filter: Option<&str>,
    workspace_path: Option<&str>,
    project_paths: &[String],
) -> Vec<usize> {
    if hits.is_empty() {
        return Vec::new();
    }
    let active_paths: Vec<String> = match scope {
        VaultScope::All => Vec::new(),
        VaultScope::Project => {
            if project_paths.is_empty() {
                workspace_path.into_iter().map(str::to_owned).collect()
            } else {
                project_paths.to_vec()
            }
        }
        VaultScope::Workspace => workspace_path.into_iter().map(str::to_owned).collect(),
    };
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|tok| {
            !tok.starts_with("agent:") && !tok.starts_with("path:") && !tok.starts_with("repo:")
        })
        .map(str::to_lowercase)
        .collect();
    hits.iter()
        .enumerate()
        .filter(|(_, hit)| agent_filter.is_none_or(|a| hit.agent == a))
        .filter(|(_, hit)| {
            project_filter.is_none_or(|proj| {
                let (repo, wt) = cwd_repo_and_worktree(&hit.cwd);
                repo == proj || wt == proj
            })
        })
        .filter(|(_, hit)| {
            if active_paths.is_empty() {
                return true;
            }
            let norm = normalize_path(&hit.cwd);
            active_paths.iter().any(|p| {
                let p_norm = normalize_path(p);
                norm == p_norm
                    || norm
                        .strip_prefix(p_norm.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
        .filter(|(_, hit)| {
            if terms.is_empty() {
                return true;
            }
            let hay = format!(
                "{} {} {} {} {}",
                hit.title.to_lowercase(),
                hit.summary.to_lowercase(),
                hit.snippet.to_lowercase(),
                hit.cwd.to_lowercase(),
                hit.id.to_lowercase()
            );
            terms.iter().all(|t| hay.contains(t))
        })
        .map(|(i, _)| i)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Confirm(SessionId),
    /// 세션 없는 `worktrees` 목록 인덱스: 새 세션을 만든다.
    /// (세션이 있는 항목은 [`Event::Confirm`]으로 나간다.)
    OpenWorktree(usize),
    /// 빠른 액션 실행(빠른 명령은 설정 목록 인덱스).
    Action(ActionKind),
    ResumeVault(usize),
    Dismiss,
}

pub struct Palette {
    language: crate::i18n::Language,
    mode: Mode,
    /// 입력칸에 지금 들어 있는 플레이스홀더의 모드(언어·모드 변경 때 다시 맞춘다).
    placeholder_mode: Mode,
    items: std::sync::Arc<Vec<Item>>,
    item_task: Option<gpui::Task<()>>,
    item_generation: u64,
    matches: Vec<usize>,
    worktrees: Vec<WorktreeItem>,
    worktree_matches: Vec<usize>,
    actions: Vec<ActionEntry>,
    action_matches: Vec<usize>,
    quick_commands: Vec<crate::commands::QuickCommand>,
    quick_matches: Vec<usize>,
    vault_hits: Vec<VaultHit>,
    vault_matches: Vec<usize>,
    projects: Vec<String>,
    scope: VaultScope,
    agent_filter: Option<VaultAgent>,
    project_filter: Option<String>,
    workspace_path: Option<String>,
    project_paths: Vec<String>,
    expanded_chats: bool,
    expanded_worktrees: bool,
    filter_open: bool,
    selected: usize,
    query: String,
    input: Entity<InputState>,
    scroll: ScrollHandle,
    quick_scroll: ScrollHandle,
    history: Option<Entity<crate::agent_history::Panel>>,
    history_sub: Option<Subscription>,
    _observe: Subscription,
}

impl EventEmitter<Event> for Palette {}

impl std::fmt::Debug for Palette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Palette")
            .field("items", &self.items.len())
            .field("matches", &self.matches.len())
            .field("vault_hits", &self.vault_hits.len())
            .field("worktrees", &self.worktrees.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Palette {
    pub fn new(items: Vec<Item>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(text::PLACEHOLDER.get()));
        // 조합 중 글자도 값에 들어 있다. 커서 깜박임 같은 알림은 값이 같아 건너뛴다.
        let observe = cx.observe(&input, |p, input, cx| {
            let value = input.read(cx).value();
            if *value != p.query {
                p.query = value.to_string();
                p.recompute_all_matches(cx);
                p.selected = 0;
                p.scroll_to_top();
                if let Some(history) = p.history.clone() {
                    let query = p.query.clone();
                    history.update(cx, |h, cx| {
                        h.set_query(query, cx);
                        if !h.is_open() {
                            h.search_vault(cx);
                        }
                    });
                }
                cx.notify();
            }
        });
        let mut this = Self {
            language: crate::i18n::language(),
            mode: Mode::Jump,
            placeholder_mode: Mode::Jump,
            matches: search(&items, ""),
            worktrees: Vec::new(),
            worktree_matches: Vec::new(),
            actions: Vec::new(),
            action_matches: Vec::new(),
            quick_commands: Vec::new(),
            quick_matches: Vec::new(),
            items: std::sync::Arc::new(items),
            item_task: None,
            item_generation: 0,
            vault_hits: Vec::new(),
            vault_matches: Vec::new(),
            projects: Vec::new(),
            scope: VaultScope::All,
            agent_filter: None,
            project_filter: None,
            workspace_path: None,
            project_paths: Vec::new(),
            expanded_chats: false,
            expanded_worktrees: false,
            filter_open: false,
            selected: 0,
            query: String::new(),
            input,
            scroll: ScrollHandle::new(),
            quick_scroll: ScrollHandle::new(),
            history: None,
            history_sub: None,
            _observe: observe,
        };
        this.rebuild_projects();
        this
    }

    pub fn connect_history(
        &mut self,
        history: &Entity<crate::agent_history::Panel>,
        cx: &mut Context<Self>,
    ) {
        let initial_hits = history.read(cx).loaded_hits();
        if !initial_hits.is_empty() {
            self.vault_hits = initial_hits;
            self.rebuild_projects();
            self.recompute_all_matches(cx);
        }
        let sub = cx.observe(history, |this, history, cx| {
            let (has_loaded, hits) =
                history.read_with(cx, |h, _| (h.has_loaded(), h.loaded_hits()));
            if has_loaded && this.vault_hits != hits {
                this.set_vault_hits(hits, cx);
            }
        });
        self.history = Some(history.clone());
        self.history_sub = Some(sub);
        let query = self.query.clone();
        let scope = self.scope;
        let agent_filter = self.agent_filter;
        history.update(cx, |h, cx| {
            h.set_query(query, cx);
            h.set_scope(scope, cx);
            h.set_agent_filter(agent_filter, cx);
            if !h.is_open() {
                h.scan(cx);
            }
        });
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// 지금 보이는 순서의 일치 항목.
    pub fn matches(&self) -> impl Iterator<Item = &Item> {
        self.matches.iter().filter_map(|&i| self.items.get(i))
    }

    /// 지금 필터에 일치하는 AI Vault 항목.
    pub fn vault_matches(&self) -> impl Iterator<Item = &VaultHit> {
        self.vault_matches
            .iter()
            .filter_map(|&i| self.vault_hits.get(i))
    }

    /// 지금 필터에 일치하는 작업 공간 항목.
    pub fn worktree_matches(&self) -> impl Iterator<Item = &WorktreeItem> {
        self.worktree_matches
            .iter()
            .filter_map(|&i| self.worktrees.get(i))
    }

    #[must_use]
    pub fn vault_hit(&self, index: usize) -> Option<&VaultHit> {
        self.vault_hits.get(index)
    }

    #[must_use]
    pub fn worktree(&self, index: usize) -> Option<&WorktreeItem> {
        self.worktrees.get(index)
    }

    #[must_use]
    pub fn worktree_entry(&self, index: usize) -> Option<&WorktreeItem> {
        self.worktrees.get(index)
    }

    /// 지금 필터에 일치하는 빠른 액션.
    pub fn action_matches(&self) -> impl Iterator<Item = &ActionEntry> {
        self.action_matches
            .iter()
            .filter_map(|&i| self.actions.get(i))
    }

    /// 지금 필터에 일치하는 빠른 명령.
    pub fn quick_matches(&self) -> impl Iterator<Item = &crate::commands::QuickCommand> {
        self.quick_matches
            .iter()
            .filter_map(|&i| self.quick_commands.get(i))
    }

    #[must_use]
    pub fn scope(&self) -> VaultScope {
        self.scope
    }

    #[must_use]
    pub fn agent_filter(&self) -> Option<VaultAgent> {
        self.agent_filter
    }

    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn set_vault_hits(&mut self, hits: Vec<VaultHit>, cx: &mut Context<Self>) {
        self.vault_hits = hits;
        self.rebuild_projects();
        self.recompute_all_matches(cx);
        cx.notify();
    }

    pub fn set_worktrees<I, T>(&mut self, worktrees: I, cx: &mut Context<Self>)
    where
        I: IntoIterator<Item = T>,
        T: Into<WorktreeItem>,
    {
        self.worktrees = worktrees.into_iter().map(Into::into).collect();
        self.rebuild_projects();
        self.recompute_all_matches(cx);
        cx.notify();
    }

    pub fn set_actions(&mut self, actions: Vec<ActionEntry>, cx: &mut Context<Self>) {
        self.actions = actions;
        self.recompute_all_matches(cx);
        cx.notify();
    }

    pub fn set_quick_commands(
        &mut self,
        quick_commands: Vec<crate::commands::QuickCommand>,
        cx: &mut Context<Self>,
    ) {
        self.quick_commands = quick_commands;
        self.recompute_all_matches(cx);
        cx.notify();
    }

    pub fn set_scope(&mut self, scope: VaultScope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.recompute_all_matches(cx);
            if let Some(history) = self.history.clone() {
                history.update(cx, |h, cx| {
                    h.set_scope(scope, cx);
                    if !h.is_open() {
                        h.search_vault(cx);
                    }
                });
            }
            cx.notify();
        }
    }

    pub fn set_agent_filter(&mut self, agent: Option<VaultAgent>, cx: &mut Context<Self>) {
        if self.agent_filter != agent {
            self.agent_filter = agent;
            self.recompute_all_matches(cx);
            if let Some(history) = self.history.clone() {
                history.update(cx, |h, cx| {
                    h.set_agent_filter(agent, cx);
                    if !h.is_open() {
                        h.search_vault(cx);
                    }
                });
            }
            cx.notify();
        }
    }

    pub fn set_project_filter(&mut self, project: Option<String>, cx: &mut Context<Self>) {
        if self.project_filter != project {
            self.project_filter = project;
            self.recompute_all_matches(cx);
            cx.notify();
        }
    }

    pub fn clear_all_filters(&mut self, cx: &mut Context<Self>) {
        self.project_filter = None;
        self.set_scope(VaultScope::All, cx);
        self.set_agent_filter(None, cx);
        self.recompute_all_matches(cx);
        cx.notify();
    }

    pub fn set_scope_paths(
        &mut self,
        workspace: Option<String>,
        project: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(history) = self.history.clone() {
            let ws = workspace.clone();
            let prj = project.clone();
            history.update(cx, |h, cx| h.update_scope_paths(ws, prj, cx));
        }
        self.workspace_path = workspace;
        self.project_paths = project;
        self.recompute_all_matches(cx);
        cx.notify();
    }

    pub fn resume_vault_at(&mut self, hit_index: usize, cx: &mut Context<Self>) {
        if self.vault_hits.get(hit_index).is_some() {
            cx.emit(Event::ResumeVault(hit_index));
        }
    }

    fn recompute_all_matches(&mut self, cx: &mut Context<Self>) {
        self.item_generation = self.item_generation.wrapping_add(1);
        self.item_task = None;
        let generation = self.item_generation;
        let background = self.items.len() >= 2_048 && !self.query.trim().is_empty();
        let proj = self.project_filter.as_deref();
        let hits = if background {
            Vec::new()
        } else {
            search(&self.items, &self.query)
        };
        self.matches = if let Some(p) = proj {
            hits.into_iter()
                .filter(|&i| {
                    self.items
                        .get(i)
                        .is_some_and(|item| item.repo_name == p || item.worktree_name == p)
                })
                .collect()
        } else {
            hits
        };
        if background {
            let items = std::sync::Arc::clone(&self.items);
            let query = self.query.clone();
            let project = self.project_filter.clone();
            self.item_task = Some(cx.spawn(async move |this, cx| {
                let matches = cx
                    .background_executor()
                    .spawn(async move {
                        let _span =
                            tracing::debug_span!("jump.search", items = items.len()).entered();
                        search(&items, &query)
                            .into_iter()
                            .filter(|&i| {
                                project.as_ref().is_none_or(|p| {
                                    items.get(i).is_some_and(|item| {
                                        item.repo_name == *p || item.worktree_name == *p
                                    })
                                })
                            })
                            .collect()
                    })
                    .await;
                this.update(cx, |this, cx| {
                    if this.item_generation == generation {
                        this.matches = matches;
                        this.clamp_selected();
                        cx.notify();
                    }
                })
                .ok();
            }));
        }
        if self.vault_hits.is_empty() {
            self.vault_matches.clear();
        } else {
            self.vault_matches = filter_vault(
                &self.vault_hits,
                &self.query,
                self.scope,
                self.agent_filter,
                proj,
                self.workspace_path.as_deref(),
                &self.project_paths,
            );
        }
        if self.worktrees.is_empty() {
            self.worktree_matches.clear();
        } else {
            self.worktree_matches = search_worktrees(&self.worktrees, &self.query, proj);
        }
        self.action_matches = action_search(&self.actions, &self.query);
        let all: Vec<Vec<char>> = self
            .quick_commands
            .iter()
            .map(|c| lower_chars(&format!("{} {}", c.name, c.text)))
            .collect();
        let refs: Vec<&[char]> = all.iter().map(Vec::as_slice).collect();
        self.quick_matches = rank(&refs, &vec![false; all.len()], &self.query);
        self.clamp_selected();
    }

    fn clamp_selected(&mut self) {
        let total = self.row_count();
        if total == 0 {
            self.selected = 0;
        } else if self.selected >= total {
            self.selected = total - 1;
        }
    }

    pub fn set_items(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        // 절대 행 번호에서 세션 위치만 살린다(Commands 모드에서는 세션이 뒤에 있다).
        let selected = self
            .row_at(self.selected)
            .filter(|(section, _)| *section == Section::Sessions)
            .and_then(|(_, pos)| self.matches.get(pos))
            .and_then(|&i| self.items.get(i))
            .map(|item| item.id);
        self.items = std::sync::Arc::new(items);
        self.rebuild_projects();
        self.recompute_all_matches(cx);
        let selected_index = self
            .matches()
            .position(|item| Some(item.id) == selected)
            .unwrap_or(0);
        self.selected = selected_index;
        cx.notify();
    }

    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.selected = 0;
            self.scroll_to_top();
            cx.notify();
        }
    }

    /// OWE-862: 빈 상태 문구(⌘P 파일 모드와 그 외를 구분한다).
    #[must_use]
    pub fn empty_text(&self) -> &'static str {
        self.mode.empty_text()
    }

    fn scroll_to_top(&self) {
        self.scroll.scroll_to_item(0);
        self.quick_scroll.scroll_to_item(0);
    }

    fn chat_window(&self) -> (usize, usize, usize) {
        let has_query = !self.query.trim().is_empty();
        let session_base = self.section_base(Section::Sessions);
        let chat_total = self.matches.len() + self.vault_matches.len();
        let in_chat = self.selected >= session_base && self.selected < session_base + chat_total;
        let rel_sel = if in_chat {
            self.selected - session_base
        } else {
            0
        };
        let (start, end) =
            if !has_query && !self.expanded_chats && rel_sel < EMPTY_QUERY_RECENT_TAB_CAP {
                (0, chat_total.min(EMPTY_QUERY_RECENT_TAB_CAP))
            } else if chat_total <= VISIBLE_ROWS {
                (0, chat_total)
            } else {
                let half = VISIBLE_ROWS / 2;
                let s = rel_sel.saturating_sub(half).min(chat_total - VISIBLE_ROWS);
                (s, s + VISIBLE_ROWS)
            };
        (start, end, chat_total.saturating_sub(end))
    }

    fn worktree_window(&self) -> (usize, usize, usize) {
        let has_query = !self.query.trim().is_empty();
        let wt_base = self.section_base(Section::Worktrees);
        let wt_total = self.worktree_matches.len();
        let in_wt = self.selected >= wt_base && self.selected < wt_base + wt_total;
        let rel_sel = if in_wt { self.selected - wt_base } else { 0 };
        let (start, end) =
            if !has_query && !self.expanded_worktrees && rel_sel < EMPTY_QUERY_WORKTREE_CAP {
                (0, wt_total.min(EMPTY_QUERY_WORKTREE_CAP))
            } else if wt_total <= VISIBLE_ROWS {
                (0, wt_total)
            } else {
                let half = VISIBLE_ROWS / 2;
                let s = rel_sel.saturating_sub(half).min(wt_total - VISIBLE_ROWS);
                (s, s + VISIBLE_ROWS)
            };
        (start, end, wt_total.saturating_sub(end))
    }

    fn selected_list_child_index(&self) -> Option<usize> {
        let (section, pos) = self.row_at(self.selected)?;
        let mut idx = 0usize;
        if self.mode == Mode::Commands {
            if !self.action_matches.is_empty() {
                if section == Section::Actions {
                    return Some(idx);
                }
                idx += 1;
            }
            if !self.quick_matches.is_empty() {
                if section == Section::QuickCommands {
                    return Some(idx);
                }
                idx += 1;
            }
        }
        let chat_total = self.matches.len() + self.vault_matches.len();
        if chat_total > 0 {
            let (chat_start, chat_end, chat_overflow) = self.chat_window();
            idx += 1; // section header
            if matches!(section, Section::Sessions | Section::Vault) {
                let chat_pos = self
                    .selected
                    .saturating_sub(self.section_base(Section::Sessions));
                let offset = chat_pos.saturating_sub(chat_start);
                return Some(idx + offset);
            }
            idx += chat_end.saturating_sub(chat_start);
            if chat_overflow > 0 {
                idx += 1;
            }
        }
        let wt_total = self.worktree_matches.len();
        if wt_total > 0 {
            let (wt_start, wt_end, wt_overflow) = self.worktree_window();
            idx += 1; // section header
            if section == Section::Worktrees {
                let offset = pos.saturating_sub(wt_start);
                return Some(idx + offset);
            }
            idx += wt_end.saturating_sub(wt_start);
            if wt_overflow > 0 {
                idx += 1;
            }
        }
        if self.mode != Mode::Commands {
            if !self.action_matches.is_empty() {
                if section == Section::Actions {
                    return Some(idx);
                }
                idx += 1;
            }
            if !self.quick_matches.is_empty() && section == Section::QuickCommands {
                return Some(idx);
            }
        }
        None
    }

    fn section_len(&self, section: Section) -> usize {
        match section {
            Section::Sessions => self.matches.len(),
            Section::Vault => self.vault_matches.len(),
            Section::Worktrees => self.worktree_matches.len(),
            Section::Actions => self.action_matches.len(),
            Section::QuickCommands => self.quick_matches.len(),
        }
    }

    /// 볼트까지 합친 전체 행 수(↑↓ 이동·Enter·⌘1~9 즉시 선택 범위).
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.mode
            .sections()
            .iter()
            .map(|s| self.section_len(*s))
            .sum()
    }

    /// 절대 행 번호를 (섹션, 섹션 안 위치)로 푼다.
    fn row_at(&self, mut row: usize) -> Option<(Section, usize)> {
        for section in self.mode.sections() {
            let len = self.section_len(section);
            if row < len {
                return Some((section, row));
            }
            row -= len;
        }
        None
    }

    /// 섹션 시작 행(렌더·클릭에서 절대 행 번호를 구할 때).
    fn section_base(&self, section: Section) -> usize {
        let mut base = 0;
        for s in self.mode.sections() {
            if s == section {
                return base;
            }
            base += self.section_len(s);
        }
        base
    }

    /// OWE-862: ↑↓는 끝에서 반대쪽으로 순환하고, 고른 행이 속한
    /// 목록(세션·워크트리·빠른 명령·볼트)을 함께 스크롤한다.
    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let total = self.row_count();
        if total == 0 {
            self.selected = 0;
            return;
        }
        let total_isize = isize::try_from(total).unwrap_or(isize::MAX);
        let current = isize::try_from(self.selected).unwrap_or(0);
        let wrapped = current.saturating_add(delta).rem_euclid(total_isize);
        self.selected = usize::try_from(wrapped).unwrap_or(0);
        if let Some(child_idx) = self.selected_list_child_index() {
            self.scroll.scroll_to_item(child_idx);
        }
        if let Some((Section::QuickCommands, pos)) = self.row_at(self.selected) {
            self.quick_scroll.scroll_to_item(pos);
        }
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        match self.row_at(self.selected) {
            Some((Section::Sessions, pos)) => {
                if let Some(item) = self.matches.get(pos).and_then(|&i| self.items.get(i)) {
                    cx.emit(Event::Confirm(item.id));
                }
            }
            Some((Section::Vault, pos)) => {
                if let Some(&vault_idx) = self.vault_matches.get(pos) {
                    cx.emit(Event::ResumeVault(vault_idx));
                }
            }
            Some((Section::Worktrees, pos)) => {
                if let Some(&index) = self.worktree_matches.get(pos)
                    && let Some(entry) = self.worktrees.get(index)
                {
                    if let Some(id) = entry.session_id {
                        cx.emit(Event::Confirm(id));
                    } else {
                        cx.emit(Event::OpenWorktree(index));
                    }
                }
            }
            Some((Section::Actions, pos)) => {
                if let Some(kind) = self
                    .action_matches
                    .get(pos)
                    .and_then(|&i| self.actions.get(i))
                    .map(|a| a.kind)
                {
                    cx.emit(Event::Action(kind));
                }
            }
            Some((Section::QuickCommands, pos)) => {
                if let Some(&index) = self.quick_matches.get(pos) {
                    cx.emit(Event::Action(ActionKind::QuickCommand(index)));
                }
            }
            None => {}
        }
    }

    /// OWE-865: ⌘1~9 행 즉시 선택(Orca digitShortcut). 존재하는 행이면
    /// 선택 후 바로 실행하고 true를 돌려준다.
    pub fn jump_to_row(&mut self, row: usize, cx: &mut Context<Self>) -> bool {
        if self.row_at(row).is_none() {
            return false;
        }
        self.selected = row;
        self.confirm(cx);
        true
    }

    fn rebuild_projects(&mut self) {
        let mut names = Vec::new();
        for wt in &self.worktrees {
            if !wt.repo_name.is_empty() && !names.contains(&wt.repo_name) {
                names.push(wt.repo_name.clone());
            }
        }
        for item in self.items.iter() {
            if !item.repo_name.is_empty() && !names.contains(&item.repo_name) {
                names.push(item.repo_name.clone());
            }
        }
        for hit in &self.vault_hits {
            let (repo, _) = cwd_repo_and_worktree(&hit.cwd);
            if !repo.is_empty() && !names.contains(&repo) {
                names.push(repo);
            }
        }
        self.projects = names;
    }
}

fn render_vault_highlight(text_str: &str, query: &str, p: &crate::ui::Palette) -> gpui::Div {
    let mut row = div().flex().items_center().overflow_hidden();
    for (seg, is_hit) in crate::agent_history::highlight_spans(text_str, query) {
        if is_hit {
            row = row.child(
                div()
                    .px_0p5()
                    .rounded(crate::ui::RADIUS_XS)
                    .bg(p.accent)
                    .text_color(p.accent_foreground)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(seg),
            );
        } else {
            row = row.child(div().truncate().child(seg));
        }
    }
    row
}

fn render_host_badge(label: &str, pal: &crate::ui::Palette) -> gpui::Div {
    div()
        .flex_none()
        .px_1p5()
        .py(px(1.))
        .rounded(crate::ui::RADIUS_XS)
        .border_1()
        .border_color(pal.border)
        .bg(pal.background)
        .text_size(crate::ui::TEXT_2XS)
        .font_weight(FontWeight::MEDIUM)
        .text_color(pal.muted_foreground)
        .child(label.to_owned())
}

fn render_context_badge(label: &str, pal: &crate::ui::Palette) -> gpui::Div {
    div()
        .flex_none()
        .px_1p5()
        .py(px(1.))
        .rounded(crate::ui::RADIUS_XS)
        .border_1()
        .border_color(pal.border)
        .bg(pal.background.opacity(0.5))
        .text_size(crate::ui::TEXT_2XS)
        .font_weight(FontWeight::MEDIUM)
        .text_color(pal.muted_foreground)
        .child(label.to_owned())
}

fn render_location_chip(
    repo_name: &str,
    worktree_name: &str,
    pal: &crate::ui::Palette,
) -> Option<gpui::Div> {
    let show_repo = !repo_name.trim().is_empty();
    let show_worktree = !worktree_name.trim().is_empty();
    if !show_repo && !show_worktree {
        return None;
    }
    let mark_color = repo_badge_color(if show_repo { repo_name } else { worktree_name });
    Some(
        div()
            .flex_none()
            .max_w(px(260.))
            .px_2()
            .py_1()
            .rounded(crate::ui::RADIUS_SM)
            .border_1()
            .border_color(pal.border)
            .bg(pal.muted)
            .flex()
            .items_center()
            .gap_1p5()
            .overflow_hidden()
            .text_size(crate::ui::TEXT_XS)
            .when(show_repo, |d| {
                d.child(
                    div()
                        .size(px(6.))
                        .rounded(px(1.))
                        .flex_none()
                        .bg(mark_color),
                )
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(pal.popover_foreground)
                        .child(repo_name.to_owned()),
                )
            })
            .when(show_repo && show_worktree, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_color(pal.muted_foreground.opacity(0.5))
                        .child("·"),
                )
            })
            .when(show_worktree, |d| {
                d.child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(pal.muted_foreground)
                        .child(worktree_name.to_owned()),
                )
            }),
    )
}

fn render_shortcut_combo(index: usize, pal: &crate::ui::Palette) -> gpui::Div {
    let cap = |k: String| {
        div()
            .px_1()
            .py(px(1.))
            .rounded(crate::ui::RADIUS_XS)
            .border_1()
            .border_color(pal.border)
            .bg(pal.background.opacity(0.45))
            .font_family(crate::ui::FONT_MONO)
            .text_size(crate::ui::TEXT_2XS)
            .text_color(pal.muted_foreground)
            .child(k)
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_0p5()
        .child(cap("⌘".to_owned()))
        .child(cap((index + 1).to_string()))
}

fn render_sliders_glyph(color: Hsla) -> gpui::Div {
    div()
        .flex_none()
        .text_size(crate::ui::TEXT_XS)
        .text_color(color)
        .child("≚")
}

fn render_footer_key(label: &'static str, pal: &crate::ui::Palette) -> gpui::Div {
    div()
        .px_2()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(pal.border)
        .bg(pal.muted.opacity(0.45))
        .text_size(crate::ui::TEXT_2XS)
        .font_weight(FontWeight::MEDIUM)
        .text_color(pal.popover_foreground.opacity(0.85))
        .child(label)
}

fn render_section_header(label: &str, pal: &crate::ui::Palette) -> gpui::Div {
    div()
        .px_3()
        .pt_2()
        .pb_1()
        .text_size(crate::ui::TEXT_XS)
        .font_weight(FontWeight::MEDIUM)
        .text_color(pal.muted_foreground.opacity(0.8))
        .child(label.to_owned())
}

impl Render for Palette {
    #[expect(
        clippy::too_many_lines,
        reason = "Orca WorktreeJumpPalette 패리티 레이아웃(검색창·필터 칩·최근 채팅 및 터미널·최근 작업 공간·빠른 액션/명령·푸터)을 한 뷰에 조립한다"
    )]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != crate::i18n::language() || self.placeholder_mode != self.mode {
            self.language = crate::i18n::language();
            self.placeholder_mode = self.mode;
            let holder = self.mode.placeholder();
            self.input.update(cx, |input, cx| {
                input.set_placeholder(holder, window, cx);
            });
        }
        let pal = crate::ui::palette(cx);
        let (bg, fg, border, accent, accent_fg, muted) = (
            pal.popover,
            pal.popover_foreground,
            pal.border,
            pal.accent,
            pal.accent_foreground,
            pal.muted_foreground,
        );
        let has_query = !self.query.trim().is_empty();
        let now_secs = now_unix_secs();

        let projects = &self.projects;
        let active_filter_count = usize::from(self.scope != VaultScope::All)
            + usize::from(self.agent_filter.is_some())
            + usize::from(self.project_filter.is_some());
        let badge_count = if active_filter_count > 0 {
            active_filter_count
        } else {
            projects.len()
        };

        // 1. 상단 검색 입력창 + 우측 필터 버튼 (Orca CommandInput 패리티)
        let filter_btn = div()
            .id("jump-filter-button")
            .debug_selector(|| "jump-filter-button".into())
            .flex_none()
            .px_2()
            .py_1()
            .rounded(crate::ui::RADIUS_SM)
            .border_1()
            .border_color(border)
            .bg(if active_filter_count > 0 || self.filter_open {
                pal.secondary
            } else {
                pal.background.opacity(0.6)
            })
            .flex()
            .items_center()
            .gap_1p5()
            .cursor_pointer()
            .text_size(crate::ui::TEXT_XS)
            .font_weight(FontWeight::MEDIUM)
            .text_color(fg)
            .hover(|d| d.bg(accent))
            .child(format!("≚ {}", text::FILTER.get()))
            .when(badge_count > 0, |d| {
                d.child(
                    div()
                        .px_1p5()
                        .rounded_full()
                        .bg(pal.primary)
                        .text_size(crate::ui::TEXT_2XS)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(pal.primary_foreground)
                        .child(badge_count.to_string()),
                )
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.filter_open = !this.filter_open;
                cx.notify();
            }));

        let search_bar = div()
            .mx_3()
            .mt_3()
            .h(px(38.))
            .px_3()
            .rounded(crate::ui::RADIUS_MD)
            .border_1()
            .border_color(border)
            .bg(pal.muted.opacity(0.35))
            .flex()
            .items_center()
            .gap_2()
            .text_size(crate::ui::TEXT_SM)
            .text_color(muted)
            .child("⌕")
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.input).appearance(false)),
            )
            .child(filter_btn);

        // 2. 검색창 바로 아래 필터 칩 바 (Orca PaletteFilterChips 패리티)
        let show_vault_filters = !self.vault_hits.is_empty()
            || self.scope != VaultScope::All
            || self.agent_filter.is_some()
            || self.filter_open;
        let show_project_chips = !projects.is_empty() || self.project_filter.is_some();

        let filter_section = (show_project_chips || show_vault_filters).then(|| {
            let mut container = div()
                .mx_3()
                .mt_2()
                .px_2()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(crate::ui::TEXT_XS)
                .text_color(muted);

            if show_project_chips {
                let mut proj_strip = div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(render_sliders_glyph(muted));
                for (idx, proj) in projects.iter().take(8).enumerate() {
                    if idx > 0 {
                        proj_strip =
                            proj_strip.child(div().text_color(muted.opacity(0.5)).child("·"));
                    }
                    let is_active = self.project_filter.as_deref() == Some(proj.as_str());
                    let proj_name = proj.clone();
                    proj_strip = proj_strip.child(
                        div()
                            .id(("jump-project-chip", idx))
                            .debug_selector(move || format!("jump-project-chip-{idx}"))
                            .px_1p5()
                            .py_0p5()
                            .rounded(crate::ui::RADIUS_XS)
                            .cursor_pointer()
                            .when(is_active, |d| d.bg(accent).text_color(fg))
                            .hover(|d| d.text_color(fg))
                            .child(proj.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let next =
                                    if this.project_filter.as_deref() == Some(proj_name.as_str()) {
                                        None
                                    } else {
                                        Some(proj_name.clone())
                                    };
                                this.set_project_filter(next, cx);
                            })),
                    );
                }
                container = container.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(proj_strip)
                        .child(
                            div()
                                .id("jump-filter-clear")
                                .debug_selector(|| "jump-filter-clear".into())
                                .flex_none()
                                .px_1p5()
                                .py_0p5()
                                .rounded(crate::ui::RADIUS_XS)
                                .cursor_pointer()
                                .hover(|d| d.bg(accent).text_color(fg))
                                .child(text::CLEAR_ALL.get())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.clear_all_filters(cx);
                                })),
                        ),
                );
            }

            if show_vault_filters {
                let mut filter_bar = div().flex().flex_wrap().items_center().gap_1();
                if !show_project_chips {
                    filter_bar = filter_bar.child(render_sliders_glyph(muted));
                }
                for (id, scope, label) in [
                    (
                        "jump-scope-all",
                        VaultScope::All,
                        vault_text::SCOPE_ALL.get(),
                    ),
                    (
                        "jump-scope-project",
                        VaultScope::Project,
                        vault_text::SCOPE_PROJECT.get(),
                    ),
                    (
                        "jump-scope-workspace",
                        VaultScope::Workspace,
                        vault_text::SCOPE_WORKSPACE.get(),
                    ),
                ] {
                    let active = self.scope == scope;
                    filter_bar = filter_bar.child(
                        div()
                            .id(id)
                            .debug_selector(move || id.into())
                            .px_1p5()
                            .py_0p5()
                            .rounded(crate::ui::RADIUS_XS)
                            .cursor_pointer()
                            .bg(if active { pal.accent } else { pal.secondary })
                            .text_color(if active {
                                pal.accent_foreground
                            } else {
                                pal.secondary_foreground
                            })
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_scope(scope, cx);
                            })),
                    );
                }
                for (id, agent, label) in [
                    ("jump-agent-all", None, vault_text::AGENT_ALL.get()),
                    ("jump-agent-claude", Some(VaultAgent::Claude), "Claude"),
                    ("jump-agent-codex", Some(VaultAgent::Codex), "Codex"),
                    ("jump-agent-agy", Some(VaultAgent::Agy), "Antigravity"),
                    (
                        "jump-agent-opencode",
                        Some(VaultAgent::OpenCode),
                        "OpenCode",
                    ),
                ] {
                    let active = self.agent_filter == agent;
                    filter_bar = filter_bar.child(
                        div()
                            .id(id)
                            .debug_selector(move || id.into())
                            .px_1p5()
                            .py_0p5()
                            .rounded(crate::ui::RADIUS_XS)
                            .cursor_pointer()
                            .bg(if active { pal.accent } else { pal.secondary })
                            .text_color(if active {
                                pal.accent_foreground
                            } else {
                                pal.secondary_foreground
                            })
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_agent_filter(agent, cx);
                            })),
                    );
                }
                if !show_project_chips && active_filter_count > 0 {
                    filter_bar = filter_bar.child(
                        div()
                            .id("jump-filter-clear-vault")
                            .ml_auto()
                            .px_1p5()
                            .py_0p5()
                            .rounded(crate::ui::RADIUS_XS)
                            .cursor_pointer()
                            .hover(|d| d.bg(accent).text_color(fg))
                            .child(text::CLEAR_ALL.get())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.clear_all_filters(cx);
                            })),
                    );
                }
                container = container.child(filter_bar);
            }

            container
        });

        let action_base = self.section_base(Section::Actions);
        let action_rows: Vec<gpui::Stateful<gpui::Div>> = self
            .action_matches
            .iter()
            .enumerate()
            .filter_map(|(pos, &index)| {
                let action = self.actions.get(index)?;
                let row_index = action_base + pos;
                let is_selected = self.selected == row_index;
                Some(
                    div()
                        .id(("jump-action", index))
                        .debug_selector(move || format!("jump-action-{index}"))
                        .h(px(ROW_HEIGHT))
                        .px_3()
                        .rounded(crate::ui::RADIUS_XS)
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .overflow_hidden()
                        .cursor_pointer()
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(fg)
                        .when(is_selected, |d| d.bg(accent).text_color(accent_fg))
                        .hover(|d| d.bg(accent.opacity(0.6)))
                        .child(div().flex_1().truncate().child(action.label.clone()))
                        .on_click(cx.listener(move |p, _, _, cx| {
                            p.selected = row_index;
                            p.confirm(cx);
                        })),
                )
            })
            .collect();
        let mut action_section = (!self.action_matches.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .child(render_section_header(text::SECTION_ACTIONS.get(), pal))
                .children(action_rows)
        });

        let quick_base = self.section_base(Section::QuickCommands);
        let quick_rows: Vec<gpui::Stateful<gpui::Div>> = self
            .quick_matches
            .iter()
            .enumerate()
            .filter_map(|(pos, &index)| {
                let command = self.quick_commands.get(index)?;
                let row_index = quick_base + pos;
                let is_selected = self.selected == row_index;
                Some(
                    div()
                        .id(("jump-quick-command", index))
                        .debug_selector(move || format!("jump-quick-command-{index}"))
                        .h(px(ROW_HEIGHT))
                        .px_3()
                        .rounded(crate::ui::RADIUS_XS)
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .overflow_hidden()
                        .cursor_pointer()
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(fg)
                        .when(is_selected, |d| d.bg(accent).text_color(accent_fg))
                        .hover(|d| d.bg(accent.opacity(0.6)))
                        .child(div().flex_1().truncate().child(command.name.clone()))
                        .on_click(cx.listener(move |p, _, _, cx| {
                            p.selected = row_index;
                            p.confirm(cx);
                        })),
                )
            })
            .collect();
        let mut quick_section = (!self.quick_matches.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .child(render_section_header(
                    text::SECTION_QUICK_COMMANDS.get(),
                    pal,
                ))
                .child(
                    div()
                        .id("jump-quick-command-list")
                        .flex()
                        .flex_col()
                        .max_h(px(ROW_HEIGHT * 6.))
                        .overflow_y_scroll()
                        .track_scroll(&self.quick_scroll)
                        .children(quick_rows),
                )
        });

        // 3. `최근 채팅 및 터미널` (열린 세션 + AI Vault 대화 기록 통합 섹션)
        let session_base = self.section_base(Section::Sessions);
        let sessions_len = self.matches.len();
        let vault_len = self.vault_matches.len();
        let chat_total = sessions_len + vault_len;
        let (chat_start, chat_end, chat_overflow) = self.chat_window();

        let mut list_body = div()
            .id("jump-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px_2p5()
            .pt_1p5()
            .pb_2()
            .flex()
            .flex_col()
            .gap_0p5();

        if self.row_count() == 0 {
            list_body = list_body.child(
                div()
                    .id("jump-empty")
                    .debug_selector(|| "jump-empty".into())
                    .px_5()
                    .py_8()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_LG)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(fg)
                            .child(self.mode.empty_text()),
                    )
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_SM)
                            .text_color(muted)
                            .child(text::EMPTY_SUBTITLE.get()),
                    ),
            );
        }

        if self.mode == Mode::Commands {
            if let Some(sec) = action_section.take() {
                list_body = list_body.child(sec);
            }
            if let Some(sec) = quick_section.take() {
                list_body = list_body.child(sec);
            }
        }

        if chat_total > 0 {
            list_body = list_body.child(render_section_header(
                if has_query {
                    text::OPEN_TABS.get()
                } else {
                    text::RECENT_CHATS_TERMINALS.get()
                },
                pal,
            ));

            for pos in chat_start..chat_end {
                let abs_row = session_base + pos;
                if pos < sessions_len {
                    let match_idx = pos;
                    let Some(&item_idx) = self.matches.get(match_idx) else {
                        continue;
                    };
                    let Some(item) = self.items.get(item_idx) else {
                        continue;
                    };
                    let is_selected = self.selected == abs_row;
                    let icon_slot = if item.is_file {
                        div()
                            .size(px(18.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Icon::new(IconName::File)
                                    .with_size(crate::ui::ICON_SM)
                                    .text_color(muted),
                            )
                    } else if let Some(pip_state) = item.state {
                        let show_pip = item.unread
                            || item.needs_you
                            || !matches!(pip_state, AgentState::Exited | AgentState::Unknown);
                        div()
                            .relative()
                            .size(px(18.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(crate::ui::status::agent_icon(
                                &item.agent,
                                crate::ui::ICON_SM,
                                muted,
                            ))
                            .when(show_pip, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .right(px(-3.))
                                        .bottom(px(-2.))
                                        .rounded_full()
                                        .bg(bg)
                                        .child(crate::ui::status::status_glyph(
                                            pip_state,
                                            item.unread,
                                            crate::ui::status::Size::Sm,
                                            cx,
                                        )),
                                )
                            })
                    } else {
                        div()
                            .size(crate::ui::STATE_DOT)
                            .flex_none()
                            .rounded_full()
                            .bg(pal.agent_question)
                            .when(!item.needs_you, Styled::invisible)
                    };

                    let title_el = div()
                        .truncate()
                        .text_size(crate::ui::TEXT_LG)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .child(item.title.clone());
                    let primary_line = if item.age.is_empty()
                        && !item.is_current_tab
                        && !item.is_current_worktree
                    {
                        title_el.flex_1().min_w_0()
                    } else {
                        let mut line = div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .overflow_hidden()
                            .child(title_el);
                        if !item.age.is_empty() {
                            line = line.child(
                                div()
                                    .flex_none()
                                    .text_size(crate::ui::TEXT_XS)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(muted.opacity(0.75))
                                    .child(item.age.clone()),
                            );
                        }
                        if item.is_current_tab {
                            line = line.child(render_context_badge(text::CURRENT_TAB.get(), pal));
                        } else if item.is_current_worktree {
                            line =
                                line.child(render_context_badge(text::CURRENT_WORKTREE.get(), pal));
                        }
                        line
                    };

                    let mut row = div()
                        .id(("jump-session-row", match_idx))
                        .debug_selector(move || format!("jump-session-row-{match_idx}"))
                        .px_3()
                        .py_2()
                        .rounded(crate::ui::RADIUS_MD)
                        .border_1()
                        .border_color(if is_selected {
                            border
                        } else {
                            border.opacity(0.)
                        })
                        .when(is_selected, |d| d.bg(accent))
                        .hover(|d| d.bg(accent.opacity(0.6)))
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .cursor_pointer()
                        .child(icon_slot)
                        .child(primary_line);

                    if !item.host_label.is_empty() {
                        row = row.child(render_host_badge(&item.host_label, pal));
                    }
                    if let Some(chip) =
                        render_location_chip(&item.repo_name, &item.worktree_name, pal)
                    {
                        row = row.child(chip);
                    }
                    if !has_query && abs_row < EMPTY_QUERY_RECENT_TAB_CAP && !item.is_file {
                        row = row.child(render_shortcut_combo(abs_row, pal));
                    }

                    list_body = list_body.child(row.on_click(cx.listener(move |p, _, _, cx| {
                        p.selected = abs_row;
                        p.confirm(cx);
                    })));
                } else {
                    let vault_pos = pos - sessions_len;
                    let Some(&hit_idx) = self.vault_matches.get(vault_pos) else {
                        continue;
                    };
                    let Some(hit) = self.vault_hits.get(hit_idx) else {
                        continue;
                    };
                    let is_selected = self.selected == abs_row;
                    let title = if hit.title.is_empty() {
                        hit.id.clone()
                    } else {
                        hit.title.clone()
                    };
                    let age = format_age_secs(hit.modified, now_secs);
                    let (repo_name, worktree_name) = cwd_repo_and_worktree(&hit.cwd);

                    let icon_slot = div()
                        .size(px(18.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(crate::ui::icons::agent_brand_icon(
                            hit.agent.profile(),
                            crate::ui::ICON_SM,
                            muted,
                        ));

                    let mut title_line = div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .overflow_hidden()
                        .text_size(crate::ui::TEXT_LG)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .child(render_vault_highlight(&title, &self.query, pal));
                    if !age.is_empty() {
                        title_line = title_line.child(
                            div()
                                .flex_none()
                                .text_size(crate::ui::TEXT_XS)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(muted.opacity(0.75))
                                .child(age),
                        );
                    }

                    let mut col = div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .child(title_line);
                    if !hit.snippet.is_empty() && has_query {
                        col = col.child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(muted)
                                .child(render_vault_highlight(&hit.snippet, &self.query, pal)),
                        );
                    }

                    let mut right_meta = div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(render_host_badge(text::LOCAL_MAC.get(), pal));
                    if let Some(chip) = render_location_chip(&repo_name, &worktree_name, pal) {
                        right_meta = right_meta.child(chip);
                    }
                    right_meta = right_meta.child(
                        div()
                            .id(("jump-vault-resume", hit_idx))
                            .debug_selector(move || format!("jump-vault-resume-{hit_idx}"))
                            .px_2()
                            .py_0p5()
                            .rounded(crate::ui::RADIUS_XS)
                            .border_1()
                            .border_color(border)
                            .bg(pal.secondary)
                            .text_size(crate::ui::TEXT_2XS)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(pal.secondary_foreground)
                            .cursor_pointer()
                            .child(vault_text::RESUME.get())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = abs_row;
                                this.resume_vault_at(hit_idx, cx);
                            })),
                    );
                    if !has_query && abs_row < EMPTY_QUERY_RECENT_TAB_CAP {
                        right_meta = right_meta.child(render_shortcut_combo(abs_row, pal));
                    }

                    list_body = list_body.child(
                        div()
                            .id(("jump-vault-row", hit_idx))
                            .debug_selector(move || format!("jump-vault-row-{hit_idx}"))
                            .px_3()
                            .py_2()
                            .rounded(crate::ui::RADIUS_MD)
                            .border_1()
                            .border_color(if is_selected {
                                border
                            } else {
                                border.opacity(0.)
                            })
                            .when(is_selected, |d| d.bg(accent))
                            .hover(|d| d.bg(accent.opacity(0.6)))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .cursor_pointer()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_2p5()
                                    .overflow_hidden()
                                    .child(icon_slot)
                                    .child(col),
                            )
                            .child(right_meta)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = abs_row;
                                this.resume_vault_at(hit_idx, cx);
                            })),
                    );
                }
            }

            if chat_overflow > 0 {
                let more_label = text::MORE_COUNT.format(&[("n", &chat_overflow.to_string())]);
                list_body = list_body.child(
                    div()
                        .id("jump-chats-see-more")
                        .debug_selector(|| "jump-chats-see-more".into())
                        .px_3()
                        .py_1p5()
                        .rounded(crate::ui::RADIUS_MD)
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(muted)
                        .hover(|d| d.bg(accent.opacity(0.5)))
                        .child(more_label)
                        .child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded(crate::ui::RADIUS_SM)
                                .border_1()
                                .border_color(pal.input)
                                .bg(pal.background)
                                .text_size(crate::ui::TEXT_SM)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(fg)
                                .child(text::SEE_MORE.get()),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.expanded_chats = true;
                            cx.notify();
                        })),
                );
            }
        }

        // 4. `최근 작업 공간` 섹션 (Orca WorktreeJumpPaletteWorktreeRow 패리티)
        let worktree_base = self.section_base(Section::Worktrees);
        let wt_total = self.worktree_matches.len();
        if wt_total > 0 {
            list_body = list_body.child(render_section_header(
                if has_query {
                    text::WORKSPACES.get()
                } else {
                    text::RECENT_WORKSPACES.get()
                },
                pal,
            ));

            let (wt_start, wt_end, wt_overflow) = self.worktree_window();

            for wt_pos in wt_start..wt_end {
                let Some(&wt_idx) = self.worktree_matches.get(wt_pos) else {
                    continue;
                };
                let Some(wt) = self.worktrees.get(wt_idx) else {
                    continue;
                };
                let row_index = worktree_base + wt_pos;
                let is_selected = self.selected == row_index;

                let status_slot = div()
                    .size(px(18.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        if wt.unread
                            || matches!(
                                wt.state,
                                Some(
                                    AgentState::Working | AgentState::Waiting | AgentState::Blocked
                                )
                            )
                        {
                            crate::ui::status::status_glyph(
                                wt.state.unwrap_or(AgentState::Unknown),
                                wt.unread,
                                crate::ui::status::Size::Sm,
                                cx,
                            )
                            .into_any_element()
                        } else if wt.state.is_some() {
                            div()
                                .size(crate::ui::STATE_DOT)
                                .rounded_full()
                                .bg(pal.success)
                                .into_any_element()
                        } else {
                            div().size(crate::ui::STATE_DOT).into_any_element()
                        },
                    );

                let mut left_line = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .overflow_hidden()
                    .child(
                        div()
                            .truncate()
                            .text_size(crate::ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .child(wt.name.clone()),
                    );
                if !wt.age.is_empty() {
                    left_line = left_line.child(
                        div()
                            .flex_none()
                            .text_size(crate::ui::TEXT_XS)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(muted.opacity(0.75))
                            .child(wt.age.clone()),
                    );
                }
                if wt.is_current {
                    left_line =
                        left_line.child(render_context_badge(text::CURRENT_BADGE.get(), pal));
                }
                if wt.is_main {
                    left_line = left_line.child(
                        div()
                            .flex_none()
                            .px_1p5()
                            .py(px(1.))
                            .rounded(crate::ui::RADIUS_XS)
                            .border_1()
                            .border_color(muted.opacity(0.3))
                            .bg(pal.muted.opacity(0.3))
                            .text_size(crate::ui::TEXT_2XS)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(muted)
                            .child(text::PRIMARY_BADGE.get()),
                    );
                }
                if !wt.branch.is_empty() {
                    left_line = left_line
                        .child(div().flex_none().text_color(muted.opacity(0.45)).child("·"))
                        .child(
                            div()
                                .truncate()
                                .text_size(crate::ui::TEXT_SM)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(muted)
                                .child(wt.branch.clone()),
                        );
                }

                let mut right_meta = div().flex_none().flex().items_center().gap_1p5();
                if !wt.host_label.is_empty() {
                    right_meta = right_meta.child(render_host_badge(&wt.host_label, pal));
                }
                if let Some(chip) = render_location_chip(&wt.repo_name, "", pal) {
                    right_meta = right_meta.child(chip);
                }
                if wt.session_id.is_none() {
                    right_meta = right_meta.child(
                        div()
                            .flex_none()
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(muted)
                            .child(text::WORKTREE_NEW.get()),
                    );
                }

                list_body = list_body.child(
                    div()
                        .id(("jump-worktree-row", wt_idx))
                        .debug_selector(move || format!("jump-worktree-row-{wt_idx}"))
                        .px_3()
                        .py_2()
                        .rounded(crate::ui::RADIUS_MD)
                        .border_1()
                        .border_color(if is_selected {
                            border
                        } else {
                            border.opacity(0.)
                        })
                        .when(is_selected, |d| d.bg(accent))
                        .hover(|d| d.bg(accent.opacity(0.6)))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .cursor_pointer()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2p5()
                                .overflow_hidden()
                                .child(status_slot)
                                .child(left_line),
                        )
                        .child(right_meta)
                        .on_click(cx.listener(move |p, _, _, cx| {
                            p.selected = row_index;
                            p.confirm(cx);
                        })),
                );
            }

            if wt_overflow > 0 {
                let more_label = text::MORE_COUNT.format(&[("n", &wt_overflow.to_string())]);
                list_body = list_body.child(
                    div()
                        .id("jump-worktrees-see-more")
                        .debug_selector(|| "jump-worktrees-see-more".into())
                        .px_3()
                        .py_1p5()
                        .rounded(crate::ui::RADIUS_MD)
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(muted)
                        .hover(|d| d.bg(accent.opacity(0.5)))
                        .child(more_label)
                        .child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded(crate::ui::RADIUS_SM)
                                .border_1()
                                .border_color(pal.input)
                                .bg(pal.background)
                                .text_size(crate::ui::TEXT_SM)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(fg)
                                .child(text::SEE_MORE.get()),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.expanded_worktrees = true;
                            cx.notify();
                        })),
                );
            }
        }

        if self.mode != Mode::Commands {
            if let Some(sec) = action_section {
                list_body = list_body.child(sec);
            }
            if let Some(sec) = quick_section {
                list_body = list_body.child(sec);
            }
        }

        // 5. 하단 단축키 안내 푸터 (Orca FooterKey 패리티: Enter 열기 / Esc 닫기 / ↑↓ 이동 / Tab 필터)
        let footer = div()
            .id("jump-hint")
            .debug_selector(|| "jump-hint".into())
            .px_3p5()
            .py_2()
            .border_t_1()
            .border_color(border)
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .text_size(crate::ui::TEXT_XS)
            .text_color(muted)
            .child(render_footer_key("Enter", pal))
            .child(text::FOOTER_OPEN.get())
            .child(render_footer_key("Esc", pal))
            .child(text::FOOTER_CLOSE.get())
            .child(render_footer_key("↑↓", pal))
            .child(text::FOOTER_MOVE.get())
            .child(render_footer_key("Tab", pal))
            .child(text::FOOTER_FILTER.get());

        let viewport = window.viewport_size();
        let max_w = (viewport.width - crate::ui::SPACE_4 * 2.).max(px(320.));
        let max_h = (viewport.height - px(96.)).max(px(320.));
        let entity = cx.entity();

        // 키는 입력칸보다 먼저(capture) 잡아 단일 행 입력의 위·아래·Enter·Esc·Tab 처리를 대신한다.
        let palette = div()
            .id("jump-palette")
            .debug_selector(|| "jump-palette".into())
            .w(crate::ui::COMMAND_PALETTE_WIDTH)
            .max_w(max_w)
            .max_h(max_h)
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(bg)
            .text_color(fg)
            .border_1()
            .border_color(border)
            .rounded(crate::ui::RADIUS_XL)
            .shadow_lg()
            .capture_action(cx.listener(|p, _: &MoveUp, _, cx| {
                cx.stop_propagation();
                p.step(-1, cx);
            }))
            .capture_action(cx.listener(|p, _: &MoveDown, _, cx| {
                cx.stop_propagation();
                p.step(1, cx);
            }))
            .capture_action(cx.listener(|p, _: &Enter, _, cx| {
                cx.stop_propagation();
                p.confirm(cx);
            }))
            .capture_action(cx.listener(|_, _: &Escape, _, cx| {
                cx.stop_propagation();
                cx.emit(Event::Dismiss);
            }))
            .on_key_down(move |e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "tab" && !e.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    entity.update(cx, |p, cx| {
                        p.filter_open = !p.filter_open;
                        cx.notify();
                    });
                } else if e.keystroke.modifiers.platform
                    && !e.keystroke.modifiers.alt
                    && !e.keystroke.modifiers.control
                    && !e.keystroke.modifiers.shift
                    && let Ok(digit) = e.keystroke.key.parse::<usize>()
                    && (1..=9).contains(&digit)
                {
                    let idx = digit - 1;
                    entity.update(cx, |p, cx| {
                        if p.jump_to_row(idx, cx) {
                            cx.stop_propagation();
                        }
                    });
                }
            })
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Event::Dismiss)))
            .child(search_bar)
            .children(filter_section)
            .child(list_body)
            .child(footer);

        deferred(
            div()
                .id("jump-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(crate::ui::MODAL_BACKDROP)
                .flex()
                .items_start()
                .justify_center()
                .pt(px(64.))
                .child(palette),
        )
    }
}

#[cfg(test)]
#[path = "jump/tests.rs"]
mod tests;
