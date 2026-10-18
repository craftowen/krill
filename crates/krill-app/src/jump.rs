//! MK-057 Jump 팔레트(Cmd+J) + MK-181 AI Vault 통합 검색:
//! 열린 세션 퍼지 검색(`List`·`States`·`Sidebar`)과 AI Vault 대화 기록(워크스페이스/프로젝트/전체 범위 필터,
//! 에이전트 종류 필터, 스니펫 하이라이트, 원클릭 Resume)을 함께 제공한다.
//!
//! 검색어는 조합 중인 한글(IME preedit)까지 포함해 즉시 거른다: gpui-component `Input`은 조합 중에 `Change`를 내지
//! 않으므로 입력 상태를 직접 관찰한다.

use std::cmp::Reverse;

use gpui::{
    Context, Entity, EventEmitter, ScrollHandle, Subscription, UniformListScrollHandle, Window,
    deferred, div, prelude::*, px, uniform_list,
};
use gpui_component::input::{Enter, Escape, Input, InputState, MoveDown, MoveUp};
use krill_proto::SessionId;
use krill_proto::agent_history::{VaultAgent, VaultHit, VaultScope};

use crate::i18n::catalog::agent_history as vault_text;
use crate::i18n::catalog::jump as text;

const ROW_HEIGHT: f32 = 28.;
const VISIBLE_ROWS: usize = 12;

/// 팔레트 한 줄. `label`의 소문자 글자를 미리 펼쳐 둬 키 입력마다 할당하지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: SessionId,
    pub label: String,
    pub needs_you: bool,
    pub agent: String,
    chars: Vec<char>,
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
        let chars = label.chars().flat_map(char::to_lowercase).collect();
        Self {
            id,
            label,
            needs_you,
            agent,
            chars,
        }
    }

    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = agent.into();
        self
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
    rank(&all, &boost, query)
}

fn lower_chars(text: &str) -> Vec<char> {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// Jump 팔레트의 워크트리 한 줄(OWE-865): 세션이 열린 워크트리(열기)와
/// 아직 세션이 없는 워크트리·폴더(새 세션 만들기)를 함께 검색한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: std::path::PathBuf,
    pub label: String,
    pub detail: Option<String>,
    pub session: Option<SessionId>,
    chars: Vec<char>,
}

impl WorktreeEntry {
    pub fn new(
        path: std::path::PathBuf,
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

/// 워크트리 목록에서 `query`와 일치하는 인덱스(점수 높은 순, 원래 순서는 유지).
pub fn worktree_search(entries: &[WorktreeEntry], query: &str) -> Vec<usize> {
    let all: Vec<&[char]> = entries.iter().map(|entry| entry.chars.as_slice()).collect();
    rank(&all, &vec![false; entries.len()], query)
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
    Worktrees,
    Actions,
    QuickCommands,
    Vault,
}

impl Mode {
    fn sections(self) -> [Section; 5] {
        match self {
            Mode::Jump | Mode::Files => [
                Section::Sessions,
                Section::Worktrees,
                Section::Actions,
                Section::QuickCommands,
                Section::Vault,
            ],
            Mode::Commands => [
                Section::Actions,
                Section::QuickCommands,
                Section::Sessions,
                Section::Worktrees,
                Section::Vault,
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
    items: Vec<Item>,
    matches: Vec<usize>,
    worktrees: Vec<WorktreeEntry>,
    worktree_matches: Vec<usize>,
    actions: Vec<ActionEntry>,
    action_matches: Vec<usize>,
    quick_commands: Vec<crate::commands::QuickCommand>,
    quick_matches: Vec<usize>,
    vault_hits: Vec<VaultHit>,
    vault_matches: Vec<usize>,
    scope: VaultScope,
    agent_filter: Option<VaultAgent>,
    workspace_path: Option<String>,
    project_paths: Vec<String>,
    selected: usize,
    query: String,
    input: Entity<InputState>,
    scroll: UniformListScrollHandle,
    worktree_scroll: ScrollHandle,
    quick_scroll: ScrollHandle,
    vault_scroll: ScrollHandle,
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
                p.matches = search(&p.items, &p.query);
                p.recompute_entry_matches();
                p.recompute_vault_matches();
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
        Self {
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
            items,
            vault_hits: Vec::new(),
            vault_matches: Vec::new(),
            scope: VaultScope::All,
            agent_filter: None,
            workspace_path: None,
            project_paths: Vec::new(),
            selected: 0,
            query: String::new(),
            input,
            scroll: UniformListScrollHandle::new(),
            worktree_scroll: ScrollHandle::new(),
            quick_scroll: ScrollHandle::new(),
            vault_scroll: ScrollHandle::new(),
            history: None,
            history_sub: None,
            _observe: observe,
        }
    }

    pub fn connect_history(
        &mut self,
        history: &Entity<crate::agent_history::Panel>,
        cx: &mut Context<Self>,
    ) {
        let initial_hits = history.read(cx).loaded_hits();
        if !initial_hits.is_empty() {
            self.vault_hits = initial_hits;
            self.recompute_vault_matches();
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

    #[must_use]
    pub fn vault_hit(&self, index: usize) -> Option<&VaultHit> {
        self.vault_hits.get(index)
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
        self.recompute_vault_matches();
        cx.notify();
    }

    pub fn set_scope(&mut self, scope: VaultScope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.recompute_vault_matches();
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
            self.recompute_vault_matches();
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

    pub fn set_scope_paths(
        &mut self,
        workspace: Option<String>,
        project: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(history) = self.history.clone() {
            let ws = workspace.clone();
            let prj = project.clone();
            history.update(cx, |h, _| h.set_scope_paths(ws, prj));
        }
        self.workspace_path = workspace;
        self.project_paths = project;
        self.recompute_vault_matches();
        cx.notify();
    }

    pub fn resume_vault_at(&mut self, hit_index: usize, cx: &mut Context<Self>) {
        if self.vault_hits.get(hit_index).is_some() {
            cx.emit(Event::ResumeVault(hit_index));
        }
    }

    fn recompute_vault_matches(&mut self) {
        self.vault_matches = filter_vault(
            &self.vault_hits,
            &self.query,
            self.scope,
            self.agent_filter,
            self.workspace_path.as_deref(),
            &self.project_paths,
        );
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

    /// 검색어가 바뀌면 워크트리·액션·빠른 명령 일치 목록도 다시 고른다.
    fn recompute_entry_matches(&mut self) {
        self.worktree_matches = worktree_search(&self.worktrees, &self.query);
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

    pub fn set_items(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        // 절대 행 번호에서 세션 위치만 살린다(Commands 모드에서는 세션이 뒤에 있다).
        let selected = self
            .row_at(self.selected)
            .filter(|(section, _)| *section == Section::Sessions)
            .and_then(|(_, pos)| self.matches.get(pos))
            .and_then(|&i| self.items.get(i))
            .map(|item| item.id);
        self.items = items;
        self.matches = search(&self.items, &self.query);
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
        self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        self.worktree_scroll.scroll_to_item(0);
        self.quick_scroll.scroll_to_item(0);
        self.vault_scroll.scroll_to_item(0);
    }

    pub fn set_worktrees(&mut self, worktrees: Vec<WorktreeEntry>, cx: &mut Context<Self>) {
        self.worktrees = worktrees;
        self.recompute_entry_matches();
        cx.notify();
    }

    pub fn set_actions(&mut self, actions: Vec<ActionEntry>, cx: &mut Context<Self>) {
        self.actions = actions;
        self.recompute_entry_matches();
        cx.notify();
    }

    pub fn set_quick_commands(
        &mut self,
        quick_commands: Vec<crate::commands::QuickCommand>,
        cx: &mut Context<Self>,
    ) {
        self.quick_commands = quick_commands;
        self.recompute_entry_matches();
        cx.notify();
    }

    /// 지금 필터에 일치하는 워크트리 항목.
    pub fn worktree_matches(&self) -> impl Iterator<Item = &WorktreeEntry> {
        self.worktree_matches
            .iter()
            .filter_map(|&i| self.worktrees.get(i))
    }

    #[must_use]
    pub fn worktree_entry(&self, index: usize) -> Option<&WorktreeEntry> {
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

    fn section_len(&self, section: Section) -> usize {
        match section {
            Section::Sessions => self.matches.len(),
            Section::Worktrees => self.worktree_matches.len(),
            Section::Actions => self.action_matches.len(),
            Section::QuickCommands => self.quick_matches.len(),
            Section::Vault => self.vault_matches.len(),
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
        match self.row_at(self.selected) {
            Some((Section::Sessions, pos)) => {
                self.scroll
                    .scroll_to_item(pos, gpui::ScrollStrategy::Center);
            }
            Some((Section::Worktrees, pos)) => {
                self.worktree_scroll.scroll_to_item(pos);
            }
            Some((Section::QuickCommands, pos)) => {
                self.quick_scroll.scroll_to_item(pos);
            }
            Some((Section::Vault, pos)) => {
                self.vault_scroll.scroll_to_item(pos);
            }
            Some((Section::Actions, _)) | None => {}
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
            Some((Section::Worktrees, pos)) => {
                if let Some(&index) = self.worktree_matches.get(pos)
                    && let Some(entry) = self.worktrees.get(index)
                {
                    // 세션이 이미 있으면 세션 행과 똑같이 열기만 한다.
                    if let Some(id) = entry.session {
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
            Some((Section::Vault, pos)) => {
                if let Some(&vault_idx) = self.vault_matches.get(pos) {
                    cx.emit(Event::ResumeVault(vault_idx));
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
}

fn render_vault_highlight(text_str: &str, query: &str, p: &crate::ui::Palette) -> gpui::Div {
    let mut row = div().flex().flex_wrap().items_center();
    for (seg, is_hit) in crate::agent_history::highlight_spans(text_str, query) {
        if is_hit {
            row = row.child(
                div()
                    .px_0p5()
                    .rounded(crate::ui::RADIUS_XS)
                    .bg(p.accent)
                    .text_color(p.accent_foreground)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(seg),
            );
        } else {
            row = row.child(div().child(seg));
        }
    }
    row
}

impl Render for Palette {
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
        let (bg, fg, border, accent, warning, muted) = (
            pal.popover,
            pal.popover_foreground,
            pal.border,
            pal.accent,
            pal.agent_question,
            pal.muted_foreground,
        );
        let rows = self.matches.len();
        // Commands 모드에서는 세션 섹션이 뒤에 오므로 하이라이트·클릭에 절대 행 번호를 쓴다.
        let session_base = self.section_base(Section::Sessions);
        #[expect(
            clippy::cast_precision_loss,
            reason = "VISIBLE_ROWS 이하라 정확히 표현된다"
        )]
        let height = rows.min(VISIBLE_ROWS) as f32 * ROW_HEIGHT;
        let list = uniform_list(
            "jump-list",
            rows,
            cx.processor(move |p, range: std::ops::Range<usize>, _, cx| {
                range
                    .filter_map(|i| {
                        let item = p.items.get(*p.matches.get(i)?)?;
                        Some(
                            div()
                                .id(i)
                                .h(px(ROW_HEIGHT))
                                .px_2()
                                .rounded(crate::ui::RADIUS_XS)
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .overflow_hidden()
                                .cursor_pointer()
                                .text_size(crate::ui::TEXT_SM)
                                .text_color(fg)
                                .when(session_base + i == p.selected, |d| d.bg(accent))
                                .child(
                                    div()
                                        .text_size(crate::ui::TEXT_2XS)
                                        .text_color(warning)
                                        .when(!item.needs_you, Styled::invisible)
                                        .child("●"),
                                )
                                .children((!item.agent.is_empty()).then(|| {
                                    crate::ui::icons::agent_brand_icon(
                                        &item.agent,
                                        crate::ui::ICON_SM,
                                        fg,
                                    )
                                }))
                                .child(div().flex_1().truncate().child(item.label.clone()))
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    p.selected = session_base + i;
                                    p.confirm(cx);
                                })),
                        )
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll)
        .h(px(height));

        // OWE-865: 세션 밖 워크트리·빠른 액션·빠른 명령 섹션. 세션 목록과 같은
        // 28px 행에 선택 강조·클릭 실행을 맞추고, 길어지면 볼트처럼 안에서 스크롤한다.
        let header = |label: &str| {
            div()
                .px_2()
                .pt_1()
                .text_size(crate::ui::TEXT_XS)
                .text_color(muted)
                .child(label.to_owned())
        };
        let worktree_base = self.section_base(Section::Worktrees);
        let worktree_rows: Vec<gpui::Stateful<gpui::Div>> = self
            .worktree_matches
            .iter()
            .enumerate()
            .filter_map(|(pos, &index)| {
                let entry = self.worktrees.get(index)?;
                let row_index = worktree_base + pos;
                let is_selected = self.selected == row_index;
                let mut row = div()
                    .id(("jump-worktree", index))
                    .debug_selector(move || format!("jump-worktree-{index}"))
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .rounded(crate::ui::RADIUS_XS)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .overflow_hidden()
                    .cursor_pointer()
                    .text_size(crate::ui::TEXT_SM)
                    .text_color(fg)
                    .when(is_selected, |d| d.bg(accent))
                    .child(div().flex_1().truncate().child(entry.label.clone()));
                if entry.session.is_none() {
                    row = row.child(
                        div()
                            .flex_none()
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(muted)
                            .child(text::WORKTREE_NEW.get()),
                    );
                }
                Some(row.on_click(cx.listener(move |p, _, _, cx| {
                    p.selected = row_index;
                    p.confirm(cx);
                })))
            })
            .collect();
        let worktree_section = (!self.worktree_matches.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .child(header(text::SECTION_WORKTREES.get()))
                .child(
                    div()
                        .id("jump-worktree-list")
                        .flex()
                        .flex_col()
                        .max_h(px(ROW_HEIGHT * 6.))
                        .overflow_y_scroll()
                        .track_scroll(&self.worktree_scroll)
                        .children(worktree_rows),
                )
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
                        .px_2()
                        .rounded(crate::ui::RADIUS_XS)
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .overflow_hidden()
                        .cursor_pointer()
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(fg)
                        .when(is_selected, |d| d.bg(accent))
                        .child(div().flex_1().truncate().child(action.label.clone()))
                        .on_click(cx.listener(move |p, _, _, cx| {
                            p.selected = row_index;
                            p.confirm(cx);
                        })),
                )
            })
            .collect();
        let action_section = (!self.action_matches.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .child(header(text::SECTION_ACTIONS.get()))
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
                        .px_2()
                        .rounded(crate::ui::RADIUS_XS)
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .overflow_hidden()
                        .cursor_pointer()
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(fg)
                        .when(is_selected, |d| d.bg(accent))
                        .child(div().flex_1().truncate().child(command.name.clone()))
                        .on_click(cx.listener(move |p, _, _, cx| {
                            p.selected = row_index;
                            p.confirm(cx);
                        })),
                )
            })
            .collect();
        let quick_section = (!self.quick_matches.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .child(header(text::SECTION_QUICK_COMMANDS.get()))
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

        let show_vault = !self.vault_hits.is_empty()
            || self.scope != VaultScope::All
            || self.agent_filter.is_some();
        let vault_section = show_vault.then(|| {
            let mut filter_bar = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .pt_1()
                .border_t_1()
                .border_color(border)
                .text_size(crate::ui::TEXT_XS);
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
                        .flex()
                        .items_center()
                        .gap_1()
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
                        .children(agent.map(|ag| {
                            crate::ui::icons::agent_brand_icon(ag.profile(), crate::ui::ICON_XS, fg)
                        }))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_agent_filter(agent, cx);
                        })),
                );
            }

            let mut rows_div = div()
                .id("jump-vault-list")
                .flex()
                .flex_col()
                .gap_1()
                .max_h(px(ROW_HEIGHT * 6.))
                .overflow_y_scroll()
                .track_scroll(&self.vault_scroll)
                .text_size(crate::ui::TEXT_XS);
            // 볼트 행의 절대 번호(워크트리·액션·빠른 명령이 앞에 올 수 있다).
            let vault_base = self.section_base(Section::Vault);
            for (vault_pos, &hit_idx) in self.vault_matches.iter().enumerate() {
                if let Some(hit) = self.vault_hits.get(hit_idx) {
                    let row_index = vault_base + vault_pos;
                    let is_selected = self.selected == row_index;
                    let title = if hit.title.is_empty() {
                        format!("{} · {}", hit.agent.profile(), hit.id)
                    } else {
                        format!("{} · {}", hit.agent.profile(), hit.title)
                    };
                    let mut col = div().flex_1().flex().flex_col().overflow_hidden().child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(crate::ui::icons::agent_brand_icon(
                                hit.agent.profile(),
                                crate::ui::ICON_SM,
                                fg,
                            ))
                            .child(render_vault_highlight(&title, &self.query, pal)),
                    );
                    if !hit.snippet.is_empty() {
                        col = col.child(div().text_color(muted).child(render_vault_highlight(
                            &hit.snippet,
                            &self.query,
                            pal,
                        )));
                    }
                    rows_div = rows_div.child(
                        div()
                            .id(("jump-vault-row", hit_idx))
                            .debug_selector(move || format!("jump-vault-row-{hit_idx}"))
                            .px_2()
                            .py_1()
                            .rounded(crate::ui::RADIUS_XS)
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .when(is_selected, |d| d.bg(accent))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = row_index;
                                this.resume_vault_at(hit_idx, cx);
                            }))
                            .child(col)
                            .child(
                                div()
                                    .id(("jump-vault-resume", hit_idx))
                                    .debug_selector(move || format!("jump-vault-resume-{hit_idx}"))
                                    .px_2()
                                    .py_0p5()
                                    .rounded(crate::ui::RADIUS_XS)
                                    .bg(pal.secondary)
                                    .text_color(pal.secondary_foreground)
                                    .cursor_pointer()
                                    .child(vault_text::RESUME.get())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selected = row_index;
                                        this.resume_vault_at(hit_idx, cx);
                                    })),
                            ),
                    );
                }
            }
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(filter_bar)
                .child(rows_div)
        });

        // OWE-862: 콘텐츠 높이에 맞춘 상단 플로팅 모달. 전체 높이는 max-h로 묶고,
        // 하단 힌트(↑↓ 이동·Enter 열기·Esc 닫기)를 고정한다.
        let hint_item = |keys: &str, label: &str| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(crate::ui::keycap(pal, keys))
                .child(div().child(label.to_owned()))
        };
        let hint = div()
            .id("jump-hint")
            .debug_selector(|| "jump-hint".into())
            .flex()
            .items_center()
            .gap_3()
            .pt_1p5()
            .border_t_1()
            .border_color(border)
            .text_size(crate::ui::TEXT_XS)
            .text_color(muted)
            .child(hint_item("↑↓", text::HINT_MOVE.get()))
            .child(hint_item("Enter", text::HINT_OPEN.get()))
            .child(hint_item("Esc", text::HINT_CLOSE.get()));
        // 키는 입력칸보다 먼저(capture) 잡아 단일 행 입력의 위·아래·Enter·Esc 처리를 대신한다.
        let mut palette = div()
            .w(crate::ui::COMMAND_PALETTE_WIDTH)
            .p_2()
            .flex()
            .flex_col()
            .gap_1p5()
            .bg(bg)
            .text_color(fg)
            .border_1()
            .border_color(border)
            .rounded(crate::ui::RADIUS_LG)
            .shadow_lg()
            .max_h(px(480.))
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
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Event::Dismiss)))
            .child(Input::new(&self.input));
        if self.row_count() == 0 {
            palette = palette.child(
                div()
                    .id("jump-empty")
                    .debug_selector(|| "jump-empty".into())
                    .p_2()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(muted)
                    .child(self.mode.empty_text()),
            );
        }
        // OWE-865: ⇧⌘P 명령 팔레트에서는 액션부터, ⌘J·⌘P에서는 세션부터 보여준다.
        match self.mode {
            Mode::Commands => {
                palette = palette
                    .children(action_section)
                    .children(quick_section)
                    .child(list)
                    .children(worktree_section)
                    .children(vault_section);
            }
            Mode::Jump | Mode::Files => {
                palette = palette
                    .child(list)
                    .children(worktree_section)
                    .children(action_section)
                    .children(quick_section)
                    .children(vault_section);
            }
        }
        palette = palette.child(hint);
        deferred(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                .pt(px(80.))
                .bg(crate::ui::MODAL_BACKDROP)
                .child(palette),
        )
    }
}

#[cfg(test)]
#[path = "jump/tests.rs"]
mod tests;
