//! 사이드바 워크스페이스 그룹화·정렬·가시성 필터·카드 표시 속성 옵션(MK-177 / OWE-731 / OWE-791).
//!
//! 구성·우선순위·메뉴 구조 참고:
//! `.ref/orca/src/renderer/src/components/sidebar/{SidebarWorkspaceOptionsMenu,workspace-options-menu-items,SidebarGroupByToggle,SidebarWorkspaceFilterSection,WorktreeCardDisplayMenuSection,SidebarRepositoryFilterSection,SidebarProjectFilterPanel,SidebarHostScopeMenuSection,FilterToggleRow,options-filter-count-badge,visible-worktrees,smart-sort}.*`
//! (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gpui::{Context, FontWeight, SharedString, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::SessionId;
use krill_proto::control::AgentState;
use krill_proto::orchestration::WorkspaceStatus;
use serde::{Deserialize, Serialize};

use super::{
    Meta, Preferences, Row, Session, Sidebar, Snapshot, basename, external, needs_you, text,
};
use crate::hosts::{HostId, HostStatus};
use crate::ui;

/// 사이드바 워크트리 목록 그룹화 기준.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GroupBy {
    #[default]
    Repo,
    WorkspaceStatus,
    PrStatus,
    None,
}

impl GroupBy {
    pub const ALL: [Self; 4] = [
        Self::Repo,
        Self::WorkspaceStatus,
        Self::PrStatus,
        Self::None,
    ];

    /// Orca `SidebarGroupByToggle` 세그먼트 순서(없음 · 상태 · PR · 프로젝트).
    pub const SEGMENT_ORDER: [Self; 4] = [
        Self::None,
        Self::WorkspaceStatus,
        Self::PrStatus,
        Self::Repo,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Repo => text::GROUP_BY_REPO.get(),
            Self::WorkspaceStatus => text::GROUP_BY_STATUS.get(),
            Self::PrStatus => text::GROUP_BY_PR.get(),
            Self::None => text::GROUP_BY_NONE.get(),
        }
    }

    #[must_use]
    pub fn short_label(self) -> &'static str {
        match self {
            Self::None => text::GROUP_BY_SHORT_NONE.get(),
            Self::WorkspaceStatus => text::GROUP_BY_SHORT_STATUS.get(),
            Self::PrStatus => text::GROUP_BY_SHORT_PR.get(),
            Self::Repo => text::GROUP_BY_SHORT_REPO.get(),
        }
    }

    #[must_use]
    pub const fn selector(self) -> &'static str {
        match self {
            Self::None => "sidebar-group-by-none",
            Self::WorkspaceStatus => "sidebar-group-by-status",
            Self::PrStatus => "sidebar-group-by-pr",
            Self::Repo => "sidebar-group-by-repo",
        }
    }
}

/// 사이드바 워크트리 정렬 기준.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SortBy {
    /// 스마트 정렬: 승인/주의 필요 → 완료 미확인 → 작업 중 → 완료 → 최근 활동순 → 이름순.
    Smart,
    #[default]
    Recent,
    Name,
    /// 수동으로 옮긴 순서, 나머지는 생성 시각 내림차순(활동해도 순서 고정).
    /// sidebar.bin·모바일(`krilld::mobile::workspaces`)이 변형 인덱스를 읽으므로 끝에만 추가한다.
    Created,
}

impl SortBy {
    pub const ALL: [Self; 4] = [Self::Smart, Self::Recent, Self::Created, Self::Name];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Smart => text::SORT_BY_SMART.get(),
            Self::Recent => text::SORT_BY_RECENT.get(),
            Self::Name => text::SORT_BY_NAME.get(),
            Self::Created => text::SORT_BY_CREATED.get(),
        }
    }

    #[must_use]
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Smart => text::SORT_BY_SHORT_SMART.get(),
            Self::Recent => text::SORT_BY_SHORT_RECENT.get(),
            Self::Name => text::SORT_BY_SHORT_NAME.get(),
            Self::Created => text::SORT_BY_SHORT_CREATED.get(),
        }
    }

    #[must_use]
    pub const fn selector(self) -> &'static str {
        match self {
            Self::Smart => "sidebar-sort-smart",
            Self::Recent => "sidebar-sort-recent",
            Self::Name => "sidebar-sort-name",
            Self::Created => "sidebar-sort-created",
        }
    }
}

/// 워크트리 카드 레이아웃 모드.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CardLayout {
    #[default]
    Detailed,
    Compact,
}

impl CardLayout {
    pub const ALL: [Self; 2] = [Self::Detailed, Self::Compact];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Detailed => text::CARD_LAYOUT_DETAILED.get(),
            Self::Compact => text::CARD_LAYOUT_COMPACT.get(),
        }
    }

    #[must_use]
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Detailed => text::CARD_LAYOUT_SHORT_DETAILED.get(),
            Self::Compact => text::CARD_LAYOUT_SHORT_COMPACT.get(),
        }
    }

    #[must_use]
    pub const fn selector(self) -> &'static str {
        match self {
            Self::Detailed => "sidebar-card-layout-detailed",
            Self::Compact => "sidebar-card-layout-compact",
        }
    }
}

/// 워크트리 카드 개별 표시 속성 식별자.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CardProperty {
    Issue,
    Pr,
    Comment,
    Ports,
    Branch,
    Host,
}

impl CardProperty {
    pub const ALL: [Self; 6] = [
        Self::Issue,
        Self::Pr,
        Self::Comment,
        Self::Ports,
        Self::Branch,
        Self::Host,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Issue => text::PROP_ISSUE.get(),
            Self::Pr => text::PROP_PR.get(),
            Self::Comment => text::PROP_COMMENT.get(),
            Self::Ports => text::PROP_PORTS.get(),
            Self::Branch => text::PROP_BRANCH.get(),
            Self::Host => text::PROP_HOST.get(),
        }
    }

    #[must_use]
    pub const fn selector(self) -> &'static str {
        match self {
            Self::Issue => "sidebar-prop-issue",
            Self::Pr => "sidebar-prop-pr",
            Self::Comment => "sidebar-prop-comment",
            Self::Ports => "sidebar-prop-ports",
            Self::Branch => "sidebar-prop-branch",
            Self::Host => "sidebar-prop-host",
        }
    }
}

/// 워크트리 카드에 표시할 속성 토글(기본 모두 켜짐).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "6 independent card property visibility flags persisted in sidebar.bin"
)]
pub struct CardProperties {
    pub issue: bool,
    pub pr: bool,
    pub comment: bool,
    pub ports: bool,
    pub branch: bool,
    pub host: bool,
}

impl Default for CardProperties {
    fn default() -> Self {
        Self {
            issue: true,
            pr: true,
            comment: true,
            ports: true,
            branch: true,
            host: true,
        }
    }
}

impl CardProperties {
    #[must_use]
    pub const fn get(self, prop: CardProperty) -> bool {
        match prop {
            CardProperty::Issue => self.issue,
            CardProperty::Pr => self.pr,
            CardProperty::Comment => self.comment,
            CardProperty::Ports => self.ports,
            CardProperty::Branch => self.branch,
            CardProperty::Host => self.host,
        }
    }

    pub const fn set(&mut self, prop: CardProperty, enabled: bool) {
        match prop {
            CardProperty::Issue => self.issue = enabled,
            CardProperty::Pr => self.pr = enabled,
            CardProperty::Comment => self.comment = enabled,
            CardProperty::Ports => self.ports = enabled,
            CardProperty::Branch => self.branch = enabled,
            CardProperty::Host => self.host = enabled,
        }
    }

    pub const fn toggle(&mut self, prop: CardProperty) {
        self.set(prop, !self.get(prop));
    }

    #[must_use]
    pub const fn all_enabled(self) -> bool {
        self.issue && self.pr && self.comment && self.ports && self.branch && self.host
    }

    #[must_use]
    pub const fn enabled_count(self) -> usize {
        (self.issue as usize)
            + (self.pr as usize)
            + (self.comment as usize)
            + (self.ports as usize)
            + (self.branch as usize)
            + (self.host as usize)
    }
}

/// 사이드바 가시성 필터 옵션.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "4 independent sidebar visibility filter flags persisted in sidebar.bin"
)]
pub struct WorkspaceFilters {
    pub hide_sleeping: bool,
    pub always_show_default_branch: bool,
    pub hide_automation: bool,
    pub hide_cli: bool,
    /// Project selector entries belong to this app's local host.
    pub repos: BTreeSet<PathBuf>,
    pub hosts: BTreeSet<String>,
}

impl Default for WorkspaceFilters {
    fn default() -> Self {
        Self {
            hide_sleeping: false,
            always_show_default_branch: true,
            hide_automation: false,
            hide_cli: false,
            repos: BTreeSet::new(),
            hosts: BTreeSet::new(),
        }
    }
}

impl WorkspaceFilters {
    #[must_use]
    pub fn active_count(&self) -> usize {
        usize::from(self.hide_sleeping)
            .saturating_add(usize::from(self.hide_automation))
            .saturating_add(usize::from(self.hide_cli))
            .saturating_add(usize::from(!self.repos.is_empty()))
            .saturating_add(usize::from(!self.hosts.is_empty()))
    }

    #[must_use]
    pub fn contains_host(&self, host: &HostId) -> bool {
        self.hosts.is_empty() || self.hosts.contains(host.as_key())
    }

    pub fn toggle_host(&mut self, host: &HostId) {
        let key = host.as_key().to_owned();
        if !self.hosts.remove(&key) {
            self.hosts.insert(key);
        }
    }

    #[must_use]
    pub fn is_repo_selected(&self, repo: &Path) -> bool {
        if self.repos.contains(repo) {
            return true;
        }
        let key = basename(repo);
        !key.is_empty()
            && self
                .repos
                .iter()
                .any(|r| basename(r).eq_ignore_ascii_case(&key))
    }

    #[must_use]
    pub fn contains_repo(&self, repo: &Path) -> bool {
        self.repos.is_empty() || self.is_repo_selected(repo)
    }

    pub fn toggle_repo(&mut self, repo: PathBuf) {
        if self.repos.remove(&repo) {
            return;
        }
        let key = basename(&repo);
        if !key.is_empty() {
            let matching: Vec<PathBuf> = self
                .repos
                .iter()
                .filter(|r| basename(r).eq_ignore_ascii_case(&key))
                .cloned()
                .collect();
            if !matching.is_empty() {
                for m in matching {
                    self.repos.remove(&m);
                }
                return;
            }
        }
        self.repos.insert(repo);
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// 사이드바 전체 워크스페이스 표시 옵션(`sidebar.bin`에 영속화).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceOptions {
    pub group_by: GroupBy,
    pub sort_by: SortBy,
    pub card_layout: CardLayout,
    pub card_properties: CardProperties,
    pub filters: WorkspaceFilters,
}

impl WorkspaceOptions {
    #[must_use]
    pub fn active_filter_count(&self) -> usize {
        self.filters.active_count()
    }

    #[must_use]
    pub fn is_customized(&self) -> bool {
        *self != Self::default()
    }
}

/// PR 상태 그룹(`GroupBy::PrStatus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PrGroup {
    Merged,
    Open,
    DraftOrNoPr,
    Closed,
}

impl PrGroup {
    pub const ALL: [Self; 4] = [Self::Merged, Self::Open, Self::DraftOrNoPr, Self::Closed];

    #[must_use]
    pub const fn from_status(
        status: Option<&crate::pull_request::Status>,
        has_checkpoint_pr: bool,
    ) -> Self {
        match status {
            Some(crate::pull_request::Status::Merged) => Self::Merged,
            Some(crate::pull_request::Status::Open) => Self::Open,
            Some(crate::pull_request::Status::Closed) => Self::Closed,
            Some(
                crate::pull_request::Status::Draft
                | crate::pull_request::Status::NoPr
                | crate::pull_request::Status::Missing
                | crate::pull_request::Status::Failed(_),
            ) => Self::DraftOrNoPr,
            None => {
                if has_checkpoint_pr {
                    Self::Open
                } else {
                    Self::DraftOrNoPr
                }
            }
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Merged => text::PR_MERGED.get(),
            Self::Open => text::PR_OPEN.get(),
            Self::DraftOrNoPr => text::PR_DRAFT_OR_NONE.get(),
            Self::Closed => text::PR_CLOSED.get(),
        }
    }

    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Merged => "pr-merged",
            Self::Open => "pr-open",
            Self::DraftOrNoPr => "pr-draft",
            Self::Closed => "pr-closed",
        }
    }
}

/// 상태별·PR 상태별 그룹 헤더 키.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupKey {
    WorkspaceStatus(WorkspaceStatus),
    PrStatus(PrGroup),
    Pinned,
}

impl PartialOrd for GroupKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for GroupKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.slug().cmp(other.slug())
    }
}

impl std::hash::Hash for GroupKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.slug().hash(state);
    }
}

impl GroupKey {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::WorkspaceStatus(WorkspaceStatus::Todo) => text::STATUS_TODO.get(),
            Self::WorkspaceStatus(WorkspaceStatus::InProgress) => text::STATUS_IN_PROGRESS.get(),
            Self::WorkspaceStatus(WorkspaceStatus::InReview) => text::STATUS_IN_REVIEW.get(),
            Self::WorkspaceStatus(WorkspaceStatus::Completed) => text::STATUS_COMPLETED.get(),
            Self::PrStatus(pr) => pr.label(),
            Self::Pinned => text::PINNED_GROUP.get(),
        }
    }

    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::WorkspaceStatus(WorkspaceStatus::Todo) => "status-todo",
            Self::WorkspaceStatus(WorkspaceStatus::InProgress) => "status-in-progress",
            Self::WorkspaceStatus(WorkspaceStatus::InReview) => "status-in-review",
            Self::WorkspaceStatus(WorkspaceStatus::Completed) => "status-completed",
            Self::PrStatus(pr) => pr.slug(),
            Self::Pinned => "pinned",
        }
    }
}

pub(super) const WORKSPACE_STATUS_ORDER: [WorkspaceStatus; 4] = [
    WorkspaceStatus::Todo,
    WorkspaceStatus::InProgress,
    WorkspaceStatus::InReview,
    WorkspaceStatus::Completed,
];

/// 행 구성·정렬·필터 계산에 필요한 보조 메타데이터 뷰.
#[derive(Default, Clone, Copy)]
pub(super) struct WorktreeContext<'a> {
    pub meta: Option<&'a Meta>,
    pub prs: Option<&'a BTreeMap<PathBuf, crate::pull_request::Status>>,
    pub checkpoint_prs: Option<&'a BTreeSet<PathBuf>>,
    pub statuses: Option<&'a BTreeMap<PathBuf, WorkspaceStatus>>,
    pub updated_at: Option<&'a BTreeMap<PathBuf, u64>>,
    pub created_at: Option<&'a BTreeMap<PathBuf, u64>>,
    pub unread: Option<&'a BTreeSet<SessionId>>,
    pub unread_worktrees: Option<&'a BTreeSet<PathBuf>>,
    pub focused: Option<SessionId>,
    pub awake_worktrees: Option<&'a BTreeSet<PathBuf>>,
    pub automation_worktrees: Option<&'a BTreeSet<PathBuf>>,
    pub cli_worktrees: Option<&'a BTreeSet<PathBuf>>,
    pub parents: Option<&'a BTreeMap<PathBuf, PathBuf>>,
    pub collapsed_lineage: Option<&'a BTreeSet<PathBuf>>,
    pub sleeping_worktrees: Option<&'a BTreeSet<PathBuf>>,
    pub recently_created: Option<&'a BTreeSet<PathBuf>>,
    pub pending_worktrees: &'a [super::PendingWorktree],
    pub removed_projects: Option<&'a BTreeSet<PathBuf>>,
    /// 사용자가 끌어다 놓아 정한 프로젝트 순서. 없는 프로젝트는 그 뒤에 이름순으로 온다.
    pub worktree_order: Option<&'a [PathBuf]>,
    pub project_order: Option<&'a [PathBuf]>,
}

pub(super) fn is_default_branch_worktree(
    repo: &Path,
    path: &Path,
    repo_tree_count: usize,
    meta: Option<&Meta>,
) -> bool {
    if path == repo || repo_tree_count <= 1 {
        return true;
    }
    meta.and_then(|m| m.get(path))
        .and_then(|t| t.branch.as_deref())
        .is_some_and(|b| {
            matches!(
                b.trim(),
                "main" | "master" | "trunk" | "develop" | "default"
            )
        })
}

pub(super) fn is_worktree_awake(
    path: &Path,
    snapshot: &Snapshot,
    prefs: &Preferences,
    ctx: &WorktreeContext<'_>,
) -> bool {
    if ctx.sleeping_worktrees.is_some_and(|s| s.contains(path)) {
        return false;
    }
    if prefs.get(path).is_some_and(|p| p.pinned)
        || ctx.unread_worktrees.is_some_and(|u| u.contains(path))
        || ctx.recently_created.is_some_and(|r| r.contains(path))
    {
        return true;
    }
    if let Some(awake) = ctx.awake_worktrees {
        return awake.contains(path);
    }
    if let Some(focused_id) = ctx.focused
        && snapshot
            .sessions
            .iter()
            .any(|s| s.id == focused_id && s.worktree.as_deref() == Some(path))
    {
        return true;
    }
    snapshot
        .sessions
        .iter()
        .filter(|s| s.worktree.as_deref() == Some(path))
        .any(|s| {
            ctx.unread.is_some_and(|u| u.contains(&s.id))
                || matches!(
                    s.status.state,
                    AgentState::Working
                        | AgentState::Waiting
                        | AgentState::Blocked
                        | AgentState::Done
                )
                || (!s.status.agent.is_empty() && s.status.state != AgentState::Exited)
        })
}

pub(super) fn is_worktree_visible(
    repo: &Path,
    path: &Path,
    repo_tree_count: usize,
    snapshot: &Snapshot,
    prefs: &Preferences,
    options: &WorkspaceOptions,
    ctx: &WorktreeContext<'_>,
) -> bool {
    if options.filters.hide_automation && {
        let lossy = path.to_string_lossy();
        ctx.automation_worktrees.is_some_and(|s| s.contains(path))
            || lossy.contains("/.orca/automations/")
            || lossy.contains("/automations/")
    } {
        return false;
    }
    if options.filters.hide_cli
        && (ctx.cli_worktrees.is_some_and(|s| s.contains(path))
            || external::is_claude_worktree(path))
    {
        return false;
    }
    if options.filters.hide_sleeping {
        if options.filters.always_show_default_branch
            && is_default_branch_worktree(repo, path, repo_tree_count, ctx.meta)
        {
            return true;
        }
        return is_worktree_awake(path, snapshot, prefs, ctx);
    }
    true
}

/// 스마트 정렬 우선순위: 0(승인/주의) < 1(완료 미확인) < 2(작업 중) < 3(완료) < 4(유휴).
fn smart_rank(
    path: &Path,
    sessions_by_wt: &BTreeMap<&Path, Vec<&Session>>,
    ctx: &WorktreeContext<'_>,
) -> u8 {
    let mut has_waiting_or_blocked = false;
    let mut has_unread_done = ctx.unread_worktrees.is_some_and(|u| u.contains(path));
    let mut has_working = false;
    let mut has_done = false;
    if let Some(list) = sessions_by_wt.get(path) {
        for s in list {
            let is_unread = ctx.unread.is_some_and(|u| u.contains(&s.id));
            match s.status.state {
                AgentState::Waiting | AgentState::Blocked => has_waiting_or_blocked = true,
                AgentState::Working => has_working = true,
                AgentState::Done => {
                    has_done = true;
                    if is_unread {
                        has_unread_done = true;
                    }
                }
                AgentState::Exited | AgentState::Unknown => {}
            }
        }
    }
    if has_waiting_or_blocked {
        0
    } else if has_unread_done {
        1
    } else if has_working {
        2
    } else if has_done {
        3
    } else {
        4
    }
}

fn worktree_sort_name(path: &Path, prefs: &Preferences, meta: Option<&Meta>) -> String {
    prefs
        .get(path)
        .filter(|v| !v.name.is_empty())
        .map(|v| v.name.clone())
        .or_else(|| {
            meta.and_then(|m| m.get(path))
                .and_then(|t| t.branch.as_deref())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| basename(path))
}

/// 카드에 [기본] 배지가 붙는 워크트리(저장소 본 체크아웃)인지. 배지와 정렬이 이 함수 하나로 판정한다.
pub(super) fn is_primary_worktree(snapshot: &Snapshot, path: &Path) -> bool {
    snapshot.trees.contains_key(path)
}

/// 선택한 정렬 키를 그대로 적용한다. 기본 워크트리 배지는 순서에 영향을 주지 않는다.
pub(super) fn sort_worktrees(
    trees: &mut [&PathBuf],
    _snapshot: &Snapshot,
    sessions_by_wt: &BTreeMap<&Path, Vec<&Session>>,
    prefs: &Preferences,
    sort_by: SortBy,
    ctx: &WorktreeContext<'_>,
) {
    trees.sort_by_cached_key(|a| {
        let name = worktree_sort_name(a, prefs, ctx.meta);
        let updated = ctx.updated_at.and_then(|u| u.get(*a)).copied().unwrap_or(0);
        let created_grace = ctx
            .created_at
            .and_then(|c| c.get(*a))
            .map_or(0, |created| created.saturating_add(300));
        let recent = Reverse(updated.max(created_grace));
        let rank = match sort_by {
            SortBy::Smart => smart_rank(a, sessions_by_wt, ctx),
            SortBy::Recent | SortBy::Name | SortBy::Created => 0,
        };
        let recent_key = match sort_by {
            SortBy::Smart | SortBy::Recent => recent,
            SortBy::Created => {
                Reverse(ctx.created_at.and_then(|c| c.get(*a)).copied().unwrap_or(0))
            }
            SortBy::Name => Reverse(0),
        };
        let manual = if sort_by == SortBy::Created {
            ctx.worktree_order
                .and_then(|order| order.iter().position(|p| p == *a))
                .unwrap_or(usize::MAX)
        } else {
            usize::MAX
        };
        (manual, rank, recent_key, name, *a)
    });
}

fn push_worktree_and_sessions(
    out: &mut Vec<Row>,
    tree: &PathBuf,
    sessions_by_wt: &BTreeMap<&Path, Vec<&Session>>,
    collapsed: &BTreeSet<PathBuf>,
) {
    out.push(Row::Worktree(tree.clone()));
    if !collapsed.contains(tree)
        && let Some(list) = sessions_by_wt.get(tree.as_path())
    {
        for &s in list {
            out.push(Row::Session(s.clone(), true));
        }
    }
}

/// 저장소를 사용자가 정한 순서(`order`)대로, 순서에 없는 것은 그 뒤에 원래(경로) 순서대로 돌려준다.
fn ordered_projects<'a>(
    snapshot: &'a Snapshot,
    order: Option<&[PathBuf]>,
) -> Vec<(&'a PathBuf, &'a Vec<PathBuf>)> {
    let mut repos: Vec<_> = snapshot.trees.iter().collect();
    if let Some(order) = order.filter(|o| !o.is_empty()) {
        repos.sort_by_key(|(repo, _)| {
            let name = basename(repo);
            order
                .iter()
                .position(|o| {
                    o == *repo || (!name.is_empty() && basename(o).eq_ignore_ascii_case(&name))
                })
                .unwrap_or(usize::MAX)
        });
    }
    repos
}

pub(super) fn rows_with_options<'a>(
    snapshot: &'a Snapshot,
    prefs: &Preferences,
    collapsed: &BTreeSet<PathBuf>,
    collapsed_groups: &BTreeSet<GroupKey>,
    shown_external: &BTreeSet<PathBuf>,
    options: &WorkspaceOptions,
    ctx: &WorktreeContext<'_>,
) -> Vec<Row> {
    let mut out = Vec::new();
    let mut sessions_by_wt: BTreeMap<&Path, Vec<&'a Session>> = BTreeMap::new();
    for s in snapshot.sessions.iter().filter(|s| !s.is_exited()) {
        if let Some(wt) = s.worktree.as_deref() {
            sessions_by_wt.entry(wt).or_default().push(s);
        }
    }
    let mut awake: BTreeSet<_> = snapshot
        .sessions
        .iter()
        .filter(|s| {
            ctx.focused == Some(s.id)
                || ctx.unread.is_some_and(|unread| unread.contains(&s.id))
                || matches!(
                    s.status.state,
                    AgentState::Working
                        | AgentState::Waiting
                        | AgentState::Blocked
                        | AgentState::Done
                )
                || (!s.status.agent.is_empty() && s.status.state != AgentState::Exited)
        })
        .filter_map(|s| s.worktree.clone())
        .collect();
    if let Some(recent) = ctx.recently_created {
        for path in recent {
            if !ctx.sleeping_worktrees.is_some_and(|s| s.contains(path)) {
                awake.insert(path.clone());
            }
        }
    }
    let ctx = &WorktreeContext {
        awake_worktrees: Some(&awake),
        ..*ctx
    };
    let mut pinned: Vec<_> = snapshot
        .trees
        .iter()
        .filter(|(repo, _)| {
            !ctx.removed_projects.is_some_and(|r| r.contains(*repo))
                && options.filters.contains_repo(repo)
        })
        .flat_map(|(repo, trees)| {
            trees.iter().filter(move |path| {
                prefs.get(*path).is_some_and(|p| p.pinned)
                    && is_worktree_visible(repo, path, trees.len(), snapshot, prefs, options, ctx)
            })
        })
        .collect();
    pinned.sort();
    pinned.dedup();
    sort_worktrees(
        &mut pinned,
        snapshot,
        &sessions_by_wt,
        prefs,
        options.sort_by,
        ctx,
    );
    if !pinned.is_empty() {
        out.push(Row::GroupHeader {
            key: GroupKey::Pinned,
            count: pinned.len(),
        });
        if !collapsed_groups.contains(&GroupKey::Pinned) {
            for path in pinned {
                push_worktree_and_sessions(&mut out, path, &sessions_by_wt, collapsed);
            }
        }
    }
    let mut waiting = snapshot
        .sessions
        .iter()
        .filter(|s| needs_you(&s.status))
        .peekable();
    if options.group_by != GroupBy::Repo && waiting.peek().is_some() {
        out.push(Row::NeedsYou);
        for s in waiting {
            out.push(Row::Session(s.clone(), false));
        }
    }
    let filters_active = options.filters.active_count() > 0;
    let visible_in_repo = |repo: &PathBuf, trees: &'a [PathBuf]| -> Vec<&'a PathBuf> {
        let n = trees.len();
        trees
            .iter()
            .filter(|p| {
                !prefs.get(*p).is_some_and(|v| v.pinned)
                    && is_worktree_visible(repo, p, n, snapshot, prefs, options, ctx)
            })
            .collect()
    };

    let is_removed = |path: &Path| ctx.removed_projects.is_some_and(|r| r.contains(path));

    match options.group_by {
        GroupBy::Repo => {
            for (repo, trees) in ordered_projects(snapshot, ctx.project_order) {
                if is_removed(repo) || !options.filters.contains_repo(repo) {
                    continue;
                }
                let pending_for_repo: Vec<_> = ctx
                    .pending_worktrees
                    .iter()
                    .filter(|p| &p.repo == repo)
                    .collect();
                let mut vis = visible_in_repo(repo, trees);
                if filters_active
                    && vis.is_empty()
                    && !trees.is_empty()
                    && pending_for_repo.is_empty()
                {
                    continue;
                }
                out.push(Row::Repository(repo.clone()));
                if !collapsed.contains(repo) {
                    for p in pending_for_repo {
                        out.push(Row::PendingWorktree {
                            id: p.id,
                            repo: p.repo.clone(),
                            name: p.name.clone(),
                        });
                    }
                    sort_worktrees(
                        &mut vis,
                        snapshot,
                        &sessions_by_wt,
                        prefs,
                        options.sort_by,
                        ctx,
                    );
                    let (normal, claude_ext) = external::partition_trees(repo, vis);
                    let normal = super::lineage::order_lineage_worktrees(
                        normal,
                        ctx.parents,
                        ctx.collapsed_lineage,
                    );
                    for tree in normal {
                        push_worktree_and_sessions(&mut out, tree, &sessions_by_wt, collapsed);
                    }
                    if !claude_ext.is_empty() {
                        let expanded = shown_external.contains(repo);
                        out.push(Row::HiddenWorktrees(
                            repo.clone(),
                            claude_ext.len(),
                            expanded,
                        ));
                        if expanded {
                            let claude_ext = super::lineage::order_lineage_worktrees(
                                claude_ext,
                                ctx.parents,
                                ctx.collapsed_lineage,
                            );
                            for tree in claude_ext {
                                push_worktree_and_sessions(
                                    &mut out,
                                    tree,
                                    &sessions_by_wt,
                                    collapsed,
                                );
                            }
                        }
                    }
                }
            }
            let mut extra_repos: Vec<PathBuf> = Vec::new();
            for p in ctx.pending_worktrees {
                if !snapshot.trees.contains_key(&p.repo)
                    && !is_removed(&p.repo)
                    && options.filters.contains_repo(&p.repo)
                    && !extra_repos.contains(&p.repo)
                {
                    extra_repos.push(p.repo.clone());
                }
            }
            for repo in extra_repos {
                out.push(Row::Repository(repo.clone()));
                if !collapsed.contains(&repo) {
                    for p in ctx.pending_worktrees.iter().filter(|p| p.repo == repo) {
                        out.push(Row::PendingWorktree {
                            id: p.id,
                            repo: p.repo.clone(),
                            name: p.name.clone(),
                        });
                    }
                }
            }
        }
        GroupBy::WorkspaceStatus => {
            for p in ctx.pending_worktrees {
                if !is_removed(&p.repo) && options.filters.contains_repo(&p.repo) {
                    out.push(Row::PendingWorktree {
                        id: p.id,
                        repo: p.repo.clone(),
                        name: p.name.clone(),
                    });
                }
            }
            let mut buckets: BTreeMap<usize, Vec<&PathBuf>> = BTreeMap::new();
            for (repo, trees) in &snapshot.trees {
                if is_removed(repo) || !options.filters.contains_repo(repo) {
                    continue;
                }
                for tree in visible_in_repo(repo, trees) {
                    let st = ctx
                        .statuses
                        .and_then(|s| s.get(tree))
                        .copied()
                        .unwrap_or(WorkspaceStatus::Todo);
                    let idx = WORKSPACE_STATUS_ORDER
                        .iter()
                        .position(|x| *x == st)
                        .unwrap_or(0);
                    buckets.entry(idx).or_default().push(tree);
                }
            }
            for (idx, status) in WORKSPACE_STATUS_ORDER.into_iter().enumerate() {
                let Some(mut bucket) = buckets.remove(&idx).filter(|b| !b.is_empty()) else {
                    continue;
                };
                sort_worktrees(
                    &mut bucket,
                    snapshot,
                    &sessions_by_wt,
                    prefs,
                    options.sort_by,
                    ctx,
                );
                let key = GroupKey::WorkspaceStatus(status);
                out.push(Row::GroupHeader {
                    key,
                    count: bucket.len(),
                });
                if !collapsed_groups.contains(&key) {
                    let ordered = super::lineage::order_lineage_worktrees(
                        bucket,
                        ctx.parents,
                        ctx.collapsed_lineage,
                    );
                    for tree in ordered {
                        push_worktree_and_sessions(&mut out, tree, &sessions_by_wt, collapsed);
                    }
                }
            }
        }
        GroupBy::PrStatus => {
            for p in ctx.pending_worktrees {
                if !is_removed(&p.repo) && options.filters.contains_repo(&p.repo) {
                    out.push(Row::PendingWorktree {
                        id: p.id,
                        repo: p.repo.clone(),
                        name: p.name.clone(),
                    });
                }
            }
            let mut buckets: BTreeMap<PrGroup, Vec<&PathBuf>> = BTreeMap::new();
            for (repo, trees) in &snapshot.trees {
                if is_removed(repo) || !options.filters.contains_repo(repo) {
                    continue;
                }
                for tree in visible_in_repo(repo, trees) {
                    let status = ctx.prs.and_then(|m| m.get(tree));
                    let has_cp = ctx.checkpoint_prs.is_some_and(|set| set.contains(tree));
                    buckets
                        .entry(PrGroup::from_status(status, has_cp))
                        .or_default()
                        .push(tree);
                }
            }
            for pr_group in PrGroup::ALL {
                let Some(mut bucket) = buckets.remove(&pr_group).filter(|b| !b.is_empty()) else {
                    continue;
                };
                sort_worktrees(
                    &mut bucket,
                    snapshot,
                    &sessions_by_wt,
                    prefs,
                    options.sort_by,
                    ctx,
                );
                let key = GroupKey::PrStatus(pr_group);
                out.push(Row::GroupHeader {
                    key,
                    count: bucket.len(),
                });
                if !collapsed_groups.contains(&key) {
                    let ordered = super::lineage::order_lineage_worktrees(
                        bucket,
                        ctx.parents,
                        ctx.collapsed_lineage,
                    );
                    for tree in ordered {
                        push_worktree_and_sessions(&mut out, tree, &sessions_by_wt, collapsed);
                    }
                }
            }
        }
        GroupBy::None => {
            for p in ctx.pending_worktrees {
                if !is_removed(&p.repo) && options.filters.contains_repo(&p.repo) {
                    out.push(Row::PendingWorktree {
                        id: p.id,
                        repo: p.repo.clone(),
                        name: p.name.clone(),
                    });
                }
            }
            let mut all_trees: Vec<&PathBuf> = Vec::new();
            for (repo, trees) in &snapshot.trees {
                if is_removed(repo) || !options.filters.contains_repo(repo) {
                    continue;
                }
                all_trees.extend(visible_in_repo(repo, trees));
            }
            sort_worktrees(
                &mut all_trees,
                snapshot,
                &sessions_by_wt,
                prefs,
                options.sort_by,
                ctx,
            );
            let ordered = super::lineage::order_lineage_worktrees(
                all_trees,
                ctx.parents,
                ctx.collapsed_lineage,
            );
            for tree in ordered {
                push_worktree_and_sessions(&mut out, tree, &sessions_by_wt, collapsed);
            }
        }
    }

    let mut folders: BTreeMap<&PathBuf, Vec<Row>> = BTreeMap::new();
    for s in snapshot
        .sessions
        .iter()
        .filter(|s| !s.is_exited() && s.worktree.is_none())
    {
        if let Some(cwd) = &s.cwd
            && !is_removed(cwd)
        {
            folders
                .entry(cwd)
                .or_default()
                .push(Row::Session(s.clone(), true));
        }
    }
    for (folder, sessions) in folders {
        out.push(Row::Folder(folder.clone()));
        if !collapsed.contains(folder) {
            out.extend(sessions);
        }
    }
    if options.group_by == GroupBy::Repo {
        super::order_project_rows(&mut out, ctx.project_order.unwrap_or_default());
    }
    let loose: Vec<_> = snapshot
        .sessions
        .iter()
        .filter(|s| !s.is_exited() && s.worktree.is_none() && s.cwd.is_none())
        .collect();
    if !loose.is_empty() {
        out.push(Row::Loose);
        out.extend(loose.into_iter().map(|s| Row::Session(s.clone(), false)));
    }
    out
}

/// Reuse the merged workspace projection while unrelated session/hover state
/// changes. Exact source equality also covers direct fixture mutations, without
/// relying on every caller to increment an invalidation counter.
#[derive(Default)]
pub(super) struct LocalRowsCache {
    base_statuses: BTreeMap<PathBuf, WorkspaceStatus>,
    base_updated: BTreeMap<PathBuf, u64>,
    store_statuses: BTreeMap<String, String>,
    store_updated: BTreeMap<String, u64>,
    store_unread: BTreeMap<String, bool>,
    store_prs: BTreeMap<String, u64>,
    statuses: BTreeMap<PathBuf, WorkspaceStatus>,
    updated_at: BTreeMap<PathBuf, u64>,
    unread_worktrees: BTreeSet<PathBuf>,
    checkpoint_prs: BTreeSet<PathBuf>,
}

impl LocalRowsCache {
    fn refresh(&mut self, sidebar: &Sidebar) {
        let store = &sidebar.disk.store;
        if self.base_statuses != sidebar.worktree_statuses || self.store_statuses != store.statuses
        {
            self.base_statuses.clone_from(&sidebar.worktree_statuses);
            self.store_statuses.clone_from(&store.statuses);
            self.statuses.clone_from(&self.base_statuses);
            for (path, value) in &self.store_statuses {
                if let Ok(status) = WorkspaceStatus::parse(value) {
                    self.statuses.entry(PathBuf::from(path)).or_insert(status);
                }
            }
        }
        if self.base_updated != sidebar.worktree_updated_at
            || self.store_updated != store.updated_at
        {
            self.base_updated.clone_from(&sidebar.worktree_updated_at);
            self.store_updated.clone_from(&store.updated_at);
            self.updated_at.clone_from(&self.base_updated);
            for (path, &updated) in &self.store_updated {
                self.updated_at
                    .entry(PathBuf::from(path))
                    .or_insert(updated);
            }
        }
        if self.store_unread != store.unread {
            self.store_unread.clone_from(&store.unread);
            self.unread_worktrees = store
                .unread
                .iter()
                .filter(|(_, unread)| **unread)
                .map(|(path, _)| PathBuf::from(path))
                .collect();
        }
        if self.store_prs != store.prs {
            self.store_prs.clone_from(&store.prs);
            self.checkpoint_prs = store
                .prs
                .iter()
                .filter(|(_, pr)| **pr > 0)
                .map(|(path, _)| PathBuf::from(path))
                .collect();
        }
    }
}

impl Sidebar {
    /// Use the same status and checkpoint fallbacks as the rendered host rows.
    pub(super) fn group_for_path(&self, host: &HostId, path: &Path) -> Option<GroupKey> {
        match self.options.group_by {
            GroupBy::WorkspaceStatus => {
                let status = if host.is_local() {
                    let mut derived = self.local_rows_cache.borrow_mut();
                    derived.refresh(self);
                    derived.statuses.get(path).copied()
                } else {
                    self.worktree_statuses.get(path).copied()
                };
                Some(GroupKey::WorkspaceStatus(
                    status.unwrap_or(WorkspaceStatus::Todo),
                ))
            }
            GroupBy::PrStatus => {
                let checkpoint = if host.is_local() {
                    let mut derived = self.local_rows_cache.borrow_mut();
                    derived.refresh(self);
                    derived.checkpoint_prs.contains(path)
                } else {
                    false
                };
                Some(GroupKey::PrStatus(PrGroup::from_status(
                    self.prs.get(path),
                    checkpoint,
                )))
            }
            GroupBy::Repo | GroupBy::None => None,
        }
    }

    pub(super) fn local_rows(&self) -> Vec<Row> {
        let mut derived = self.local_rows_cache.borrow_mut();
        derived.refresh(self);
        let parents = self.merged_parents();
        let pending: Vec<_> = self
            .pending_worktrees
            .iter()
            .filter(|p| p.host.is_local())
            .cloned()
            .collect();
        let ctx = WorktreeContext {
            meta: Some(&self.meta),
            prs: Some(&self.prs),
            checkpoint_prs: Some(&derived.checkpoint_prs),
            statuses: Some(&derived.statuses),
            updated_at: Some(&derived.updated_at),
            created_at: Some(&self.disk.worktree_created_at),
            unread: Some(&self.unread),
            unread_worktrees: Some(&derived.unread_worktrees),
            focused: self.focused,
            awake_worktrees: None,
            automation_worktrees: Some(&self.automation_worktrees),
            cli_worktrees: Some(&self.cli_worktrees),
            parents: Some(&parents),
            collapsed_lineage: Some(&self.collapsed_lineage),
            sleeping_worktrees: Some(&self.sleeping_worktrees),
            recently_created: Some(&self.recently_created_worktrees),
            pending_worktrees: &pending,
            removed_projects: Some(&self.removed_projects),
            project_order: Some(&self.project_order),
            worktree_order: Some(&self.worktree_order),
        };
        rows_with_options(
            &self.snapshot,
            &self.prefs,
            &self.collapsed,
            &self.collapsed_groups,
            &self.shown_external,
            &self.options,
            &ctx,
        )
    }

    pub(super) fn filterable_project_repos(&self) -> Vec<PathBuf> {
        let mut repos: Vec<PathBuf> = Vec::new();
        let mut seen_keys: BTreeSet<String> = BTreeSet::new();
        let mut push_repo = |repo: &PathBuf| {
            if self.removed_projects.contains(repo) {
                return;
            }
            let key = basename(repo).to_lowercase();
            let dedup_key = if key.is_empty() {
                repo.to_string_lossy().into_owned()
            } else {
                key
            };
            if seen_keys.insert(dedup_key) {
                repos.push(repo.clone());
            }
        };
        for (repo, _) in ordered_projects(&self.snapshot, Some(&self.project_order)) {
            push_repo(repo);
        }
        for remote in &self.remote_hosts {
            if remote.status == HostStatus::Connected
                || !remote.snapshot.sessions.is_empty()
                || !remote.snapshot.trees.is_empty()
                || self.remote_info.contains_key(&remote.id)
            {
                for (repo, _) in ordered_projects(&remote.snapshot, Some(&self.project_order)) {
                    push_repo(repo);
                }
            }
        }
        repos
    }

    pub(super) fn remote_rows(
        &self,
        remote: &super::RemoteHost,
        collapsed: &BTreeSet<PathBuf>,
    ) -> Vec<Row> {
        let pending: Vec<_> = self
            .pending_worktrees
            .iter()
            .filter(|p| p.host == HostId::Remote(remote.id.clone()))
            .cloned()
            .collect();
        let ctx = WorktreeContext {
            pending_worktrees: &pending,
            meta: Some(&remote.meta),
            prs: Some(&self.prs),
            statuses: Some(&self.worktree_statuses),
            updated_at: Some(&self.worktree_updated_at),
            focused: self.focused,
            awake_worktrees: None,
            automation_worktrees: Some(&self.automation_worktrees),
            cli_worktrees: Some(&self.cli_worktrees),
            // Lineage belongs to the local store. Matching absolute paths on a remote
            // host do not make them descendants of local worktrees.
            sleeping_worktrees: Some(&self.sleeping_worktrees),
            removed_projects: Some(&self.removed_projects),
            project_order: Some(&self.project_order),
            worktree_order: Some(&self.worktree_order),
            ..WorktreeContext::default()
        };
        rows_with_options(
            &remote.snapshot,
            &self.prefs,
            collapsed,
            &self.collapsed_groups,
            &self.shown_external,
            &self.options,
            &ctx,
        )
    }

    #[must_use]
    pub fn options(&self) -> &WorkspaceOptions {
        &self.options
    }

    pub fn set_group_by(&mut self, group_by: GroupBy, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.touch("group_by");
        }
        if self.options.group_by != group_by || !self.disk.initialized {
            self.options.group_by = group_by;
            self.persist(cx);
        }
    }

    pub fn set_sort_by(&mut self, sort_by: SortBy, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.touch("sort_by");
        }
        if self.options.sort_by != sort_by || !self.disk.initialized {
            self.options.sort_by = sort_by;
            self.persist(cx);
        }
    }

    pub fn set_card_layout(&mut self, layout: CardLayout, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.touch("card_layout");
        }
        if self.options.card_layout != layout || !self.disk.initialized {
            self.options.card_layout = layout;
            self.persist(cx);
        }
    }

    pub fn toggle_card_property(&mut self, prop: CardProperty, cx: &mut Context<Self>) {
        self.options.card_properties.toggle(prop);
        self.persist(cx);
    }

    pub fn set_card_property(&mut self, prop: CardProperty, enabled: bool, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            let field = match prop {
                CardProperty::Issue => "card_properties.issue",
                CardProperty::Pr => "card_properties.pr",
                CardProperty::Comment => "card_properties.comment",
                CardProperty::Ports => "card_properties.ports",
                CardProperty::Branch => "card_properties.branch",
                CardProperty::Host => "card_properties.host",
            };
            self.disk.startup.touch(field);
        }
        if self.options.card_properties.get(prop) != enabled || !self.disk.initialized {
            self.options.card_properties.set(prop, enabled);
            self.persist(cx);
        }
    }

    pub fn toggle_hide_sleeping(&mut self, cx: &mut Context<Self>) {
        self.options.filters.hide_sleeping = !self.options.filters.hide_sleeping;
        self.persist(cx);
    }

    pub fn toggle_always_show_default_branch(&mut self, cx: &mut Context<Self>) {
        self.options.filters.always_show_default_branch =
            !self.options.filters.always_show_default_branch;
        self.persist(cx);
    }

    pub fn toggle_hide_automation(&mut self, cx: &mut Context<Self>) {
        self.options.filters.hide_automation = !self.options.filters.hide_automation;
        self.persist(cx);
    }

    pub fn toggle_hide_cli(&mut self, cx: &mut Context<Self>) {
        self.options.filters.hide_cli = !self.options.filters.hide_cli;
        self.persist(cx);
    }

    pub fn toggle_repo_filter(&mut self, repo: PathBuf, cx: &mut Context<Self>) {
        self.options.filters.toggle_repo(repo);
        self.persist(cx);
    }

    pub fn clear_repo_filters(&mut self, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.touch("repos.clear");
        }
        if !self.options.filters.repos.is_empty() || !self.disk.initialized {
            self.options.filters.repos.clear();
            self.persist(cx);
        }
    }

    pub fn toggle_host_filter(&mut self, host: &HostId, cx: &mut Context<Self>) {
        self.options.filters.toggle_host(host);
        self.persist(cx);
    }

    pub fn clear_host_filters(&mut self, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.touch("hosts.clear");
        }
        if !self.options.filters.hosts.is_empty() || !self.disk.initialized {
            self.options.filters.hosts.clear();
            self.persist(cx);
        }
    }

    pub fn reset_filters(&mut self, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.reset_filters();
        }
        if self.options.filters != WorkspaceFilters::default() || !self.disk.initialized {
            self.options.filters.reset();
            self.persist(cx);
        }
    }

    pub fn fold_group(&mut self, key: GroupKey, open: Option<bool>, cx: &mut Context<Self>) {
        if !self.disk.initialized {
            self.disk.startup.ui.collapsed_groups.insert(key);
        }
        let closed = self.collapsed_groups.contains(&key);
        if open.unwrap_or(closed) {
            self.collapsed_groups.remove(&key);
        } else {
            self.collapsed_groups.insert(key);
        }
        self.persist(cx);
    }

    pub fn set_worktree_status(
        &mut self,
        path: PathBuf,
        status: WorkspaceStatus,
        cx: &mut Context<Self>,
    ) {
        if self.worktree_statuses.insert(path, status) != Some(status) {
            self.rebuild(cx);
        }
    }

    pub fn set_worktree_updated_at(&mut self, path: PathBuf, ts: u64, cx: &mut Context<Self>) {
        if self.worktree_updated_at.insert(path, ts) != Some(ts) {
            self.rebuild(cx);
        }
    }

    pub fn set_worktree_issue(
        &mut self,
        path: PathBuf,
        issue: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let issue = issue.into();
        if issue.trim().is_empty() {
            self.issues.remove(&path);
        } else {
            self.issues.insert(path, issue);
        }
        cx.notify();
    }

    pub fn set_worktree_ports(&mut self, path: PathBuf, ports: Vec<u16>, cx: &mut Context<Self>) {
        if ports.is_empty() {
            self.ports.remove(&path);
        } else {
            self.ports.insert(path, ports);
        }
        cx.notify();
    }

    pub fn set_worktree_host_label(
        &mut self,
        path: PathBuf,
        label: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let label = label.into();
        if label.trim().is_empty() {
            self.worktree_hosts.remove(&path);
        } else {
            self.worktree_hosts.insert(path, label);
        }
        cx.notify();
    }

    pub fn mark_automation_worktree(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.automation_worktrees.insert(path) {
            self.rebuild(cx);
        }
    }

    pub fn mark_cli_worktree(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.cli_worktrees.insert(path) {
            self.rebuild(cx);
        }
    }

    pub(super) fn issue_for(&self, path: &Path) -> Option<String> {
        if let Some(custom) = self.issues.get(path) {
            return Some(custom.clone());
        }
        self.disk
            .store
            .issues
            .get(&path.to_string_lossy().into_owned())
            .cloned()
    }

    pub(super) fn ports_for(&self, host: &HostId, path: &Path) -> Vec<u16> {
        let mut set = BTreeSet::new();
        if let Some(list) = self.ports.get(path) {
            for &port in list {
                set.insert(port);
            }
        }
        if !self.titles.is_empty() {
            for s in self
                .snapshot_for(host)
                .sessions
                .iter()
                .filter(|s| s.worktree.as_deref() == Some(path))
            {
                if let Some(title) = self.titles.get(&s.id) {
                    for entry in
                        krill_proto::orchestration::extract_local_urls(s.id.0, title, false)
                    {
                        set.insert(entry.port);
                    }
                }
            }
        }
        set.into_iter().collect()
    }

    pub(super) fn host_badge_for(&self, host: &HostId, path: &Path) -> Option<String> {
        if let Some(custom) = self.worktree_hosts.get(path) {
            return Some(custom.clone());
        }
        // 프로젝트 기준 보기에는 PC 단계가 없으니 원격 PC 것만 배지로 구분한다.
        if self.options.group_by == GroupBy::Repo && host.is_local() {
            return None;
        }
        match host {
            HostId::Remote(id) => Some(
                self.remote_hosts
                    .iter()
                    .find(|r| r.id == *id)
                    .map_or_else(|| id.clone(), |r| r.label.clone()),
            ),
            HostId::Local => (self.options.group_by != GroupBy::Repo
                && self.has_connected_remote_hosts())
            .then(|| {
                text::HOST_LOCAL
                    .mac_or(crate::i18n::catalog::other_os::HOST_LOCAL)
                    .get()
                    .to_owned()
            }),
        }
    }

    pub(super) fn render_options_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let active_filters = self.options.active_filter_count();
        let tooltip_label: SharedString = if active_filters > 0 {
            text::WORKSPACE_OPTIONS_ACTIVE
                .format(&[("n", &active_filters.to_string())])
                .into()
        } else {
            text::WORKSPACE_OPTIONS.get().into()
        };
        let this = cx.weak_entity();
        let btn = Button::new("sidebar-options")
            .ghost()
            .xsmall()
            .toggled(self.options.is_customized())
            .icon(Icon::default().data(ui::icons::SLIDERS_HORIZONTAL_SVG))
            .accessibility_label(tooltip_label.clone())
            .tooltip(tooltip_label)
            .dropdown_menu(move |menu, window, cx| build_options_menu(menu, &this, window, cx));

        div()
            .id("sidebar-options-wrap")
            .debug_selector(|| "sidebar-options".into())
            .flex_none()
            .flex()
            .items_center()
            .gap_0p5()
            .child(btn)
            .when(active_filters > 0, |d| {
                d.child(
                    div()
                        .id("sidebar-options-badge")
                        .debug_selector(|| "sidebar-options-badge".into())
                        .min_w(px(14.))
                        .h(px(14.))
                        .px_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(p.primary)
                        .font_family(ui::FONT_MONO)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(ui::TEXT_2XS)
                        .text_color(p.primary_foreground)
                        .child(format_filter_badge_count(active_filters)),
                )
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OptionsSubmenu {
    Hosts,
    Projects,
    SortBy,
    CardLayout,
    CardProperties,
}

#[derive(Clone, Debug)]
struct HostMenuEntry {
    id: HostId,
    label: String,
    status_label: &'static str,
}

#[must_use]
pub(super) fn format_filter_badge_count(count: usize) -> String {
    if count > 9 {
        "9+".to_owned()
    } else {
        count.to_string()
    }
}

#[must_use]
fn host_status_label(status: HostStatus) -> &'static str {
    match status {
        HostStatus::Connected => text::HOST_CONNECTED.get(),
        HostStatus::Connecting => text::HOST_CONNECTING.get(),
        HostStatus::Disconnected => text::HOST_DISCONNECTED.get(),
    }
}

#[must_use]
pub(super) fn host_visibility_summary(
    filters: &WorkspaceFilters,
    hosts: &[(HostId, String)],
) -> String {
    if filters.hosts.is_empty() || hosts.is_empty() {
        return text::ALL_HOSTS.get().to_owned();
    }
    let selected: Vec<&str> = hosts
        .iter()
        .filter(|(id, _)| filters.hosts.contains(id.as_key()))
        .map(|(_, label)| label.as_str())
        .collect();
    if selected.is_empty() || selected.len() == hosts.len() {
        text::ALL_HOSTS.get().to_owned()
    } else if let [only] = selected.as_slice() {
        (*only).to_owned()
    } else {
        text::HOSTS_SELECTED_COUNT.format(&[("n", &selected.len().to_string())])
    }
}

#[must_use]
pub(super) fn project_visibility_summary(filters: &WorkspaceFilters, repos: &[PathBuf]) -> String {
    if filters.repos.is_empty() || repos.is_empty() {
        return text::ALL_PROJECTS.get().to_owned();
    }
    let selected: Vec<&PathBuf> = repos
        .iter()
        .filter(|r| filters.is_repo_selected(r))
        .collect();
    if selected.is_empty() || selected.len() == repos.len() {
        text::ALL_PROJECTS.get().to_owned()
    } else if let [only] = selected.as_slice() {
        basename(only)
    } else {
        text::PROJECTS_SELECTED_COUNT.format(&[("n", &selected.len().to_string())])
    }
}

#[must_use]
pub(super) fn properties_summary(options: &WorkspaceOptions) -> String {
    if options.card_layout == CardLayout::Compact {
        text::SHOW_PROPERTIES_HOVER.get().to_owned()
    } else {
        let count = options.card_properties.enabled_count();
        if count > 0 {
            count.to_string()
        } else {
            String::new()
        }
    }
}

/// 서브메뉴 배치·포커스·바깥 클릭 처리는 `PopupMenu`가 맡는다.
fn build_options_menu(
    menu: PopupMenu,
    this: &gpui::WeakEntity<Sidebar>,
    window: &mut gpui::Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let menu_weak = cx.weak_entity();
    if let Some(sidebar) = this.upgrade() {
        cx.observe(&sidebar, |_, _, cx| cx.notify()).detach();
    }
    let has_hosts = this
        .upgrade()
        .is_some_and(|s| s.read(cx).has_connected_remote_hosts());
    let has_projects = this
        .upgrade()
        .is_some_and(|s| !s.read(cx).filterable_project_repos().is_empty());
    let mut menu = menu
        .min_w(px(284.))
        .max_w(px(296.))
        .scrollable(true)
        .max_h(px(540.))
        .item(
            PopupMenuItem::element(|_, cx| {
                div()
                    .id("sidebar-options-menu")
                    .debug_selector(|| "sidebar-options-menu".into())
                    .w(px(272.))
                    .child(
                        div()
                            .id("sidebar-options-header")
                            .debug_selector(|| "sidebar-options-header".into())
                            .py_1()
                            .text_size(ui::TEXT_XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui::palette(cx).popover_foreground)
                            .child(text::WORKSPACE_OPTIONS.get()),
                    )
            })
            .disabled(true),
        );
    if has_hosts || has_projects {
        menu = menu.item(
            PopupMenuItem::element(|_, cx| {
                section_label(text::SHOW_SECTION.get(), ui::palette(cx))
            })
            .disabled(true),
        );
        if has_hosts {
            menu = add_options_submenu(menu, OptionsSubmenu::Hosts, this, window, cx);
        }
        if has_projects {
            menu = add_options_submenu(menu, OptionsSubmenu::Projects, this, window, cx);
        }
        menu = menu.separator();
    }
    let sb = this.clone();
    let mn = menu_weak.clone();
    menu = menu
        .item(
            PopupMenuItem::element(move |_, cx| {
                let group = sb
                    .upgrade()
                    .map_or(GroupBy::default(), |s| s.read(cx).options.group_by);
                render_group_by_section(&sb, &mn, group, ui::palette(cx))
            })
            .disabled(true),
        )
        .separator();
    for target in [
        OptionsSubmenu::SortBy,
        OptionsSubmenu::CardLayout,
        OptionsSubmenu::CardProperties,
    ] {
        menu = add_options_submenu(menu, target, this, window, cx);
    }
    let sb = this.clone();
    menu.separator().item(
        PopupMenuItem::element(move |_, cx| {
            let options = sb
                .upgrade()
                .map_or_else(WorkspaceOptions::default, |s| s.read(cx).options.clone());
            render_filter_section(&sb, &menu_weak, &options, ui::palette(cx))
        })
        .disabled(true),
    )
}

fn add_options_submenu(
    menu: PopupMenu,
    target: OptionsSubmenu,
    sidebar: &gpui::WeakEntity<Sidebar>,
    window: &mut gpui::Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let sb = sidebar.clone();
    let submenu = PopupMenu::build(window, cx, move |menu, _, cx| {
        if let Some(sidebar) = sb.upgrade() {
            cx.observe(&sidebar, |_, _, cx| cx.notify()).detach();
        }
        let mn = cx.weak_entity();
        let sb = sb.clone();
        menu.min_w(px(260.))
            .max_w(px(296.))
            .scrollable(true)
            .max_h(px(360.))
            .item(
                PopupMenuItem::element(move |_, cx| render_options_submenu(target, &sb, &mn, cx))
                    .disabled(true),
            )
    });
    let sb = sidebar.clone();
    menu.item(
        PopupMenuItem::submenu(target.label(), submenu).submenu_label(move |_, cx| {
            let summary = sb.upgrade().map_or_else(String::new, |sidebar| {
                let sidebar = sidebar.read(cx);
                match target {
                    OptionsSubmenu::Hosts => {
                        let hosts = std::iter::once((
                            HostId::Local,
                            text::HOST_LOCAL
                                .mac_or(crate::i18n::catalog::other_os::HOST_LOCAL)
                                .get()
                                .to_owned(),
                        ))
                        .chain(
                            sidebar
                                .remote_hosts
                                .iter()
                                .filter(|h| h.status == HostStatus::Connected)
                                .map(|h| (HostId::Remote(h.id.clone()), h.label.clone())),
                        )
                        .collect::<Vec<_>>();
                        host_visibility_summary(&sidebar.options.filters, &hosts)
                    }
                    OptionsSubmenu::Projects => project_visibility_summary(
                        &sidebar.options.filters,
                        &sidebar.filterable_project_repos(),
                    ),
                    OptionsSubmenu::SortBy => sidebar.options.sort_by.short_label().to_owned(),
                    OptionsSubmenu::CardLayout => {
                        sidebar.options.card_layout.short_label().to_owned()
                    }
                    OptionsSubmenu::CardProperties => properties_summary(&sidebar.options),
                }
            });
            render_submenu_label(target, summary, ui::palette(cx))
        }),
    )
}

impl OptionsSubmenu {
    fn label(self) -> &'static str {
        match self {
            Self::Hosts => text::HOSTS_ROW.get(),
            Self::Projects => text::PROJECTS.get(),
            Self::SortBy => text::SORT_BY.get(),
            Self::CardLayout => text::CARD_LAYOUT.get(),
            Self::CardProperties => text::CARD_PROPERTIES.get(),
        }
    }

    const fn selectors(self) -> (&'static str, &'static str) {
        match self {
            Self::Hosts => ("sidebar-submenu-hosts", "sidebar-hosts-summary"),
            Self::Projects => ("sidebar-submenu-projects", "sidebar-projects-summary"),
            Self::SortBy => ("sidebar-submenu-sort", "sidebar-sort-summary"),
            Self::CardLayout => ("sidebar-submenu-card-layout", "sidebar-card-layout-summary"),
            Self::CardProperties => (
                "sidebar-submenu-card-properties",
                "sidebar-card-properties-summary",
            ),
        }
    }
}

fn render_options_submenu(
    target: OptionsSubmenu,
    sidebar: &gpui::WeakEntity<Sidebar>,
    menu: &gpui::WeakEntity<PopupMenu>,
    cx: &gpui::App,
) -> gpui::Stateful<gpui::Div> {
    let p = ui::palette(cx);
    let Some(sidebar_entity) = sidebar.upgrade() else {
        return div().id("sidebar-options-unavailable");
    };
    let s = sidebar_entity.read(cx);
    match target {
        OptionsSubmenu::Hosts => {
            let hosts = std::iter::once(HostMenuEntry {
                id: HostId::Local,
                label: text::HOST_LOCAL
                    .mac_or(crate::i18n::catalog::other_os::HOST_LOCAL)
                    .get()
                    .to_owned(),
                status_label: host_status_label(s.local_status),
            })
            .chain(
                s.remote_hosts
                    .iter()
                    .filter(|h| h.status == HostStatus::Connected)
                    .map(|h| HostMenuEntry {
                        id: HostId::Remote(h.id.clone()),
                        label: h.label.clone(),
                        status_label: host_status_label(h.status),
                    }),
            )
            .collect::<Vec<_>>();
            render_hosts_panel(sidebar, menu, &s.options.filters, &hosts, p)
        }
        OptionsSubmenu::Projects => {
            let repos = s.filterable_project_repos();
            render_projects_panel(sidebar, menu, s, &repos, p)
        }
        OptionsSubmenu::SortBy => render_sort_panel(sidebar, menu, s.options.sort_by, p),
        OptionsSubmenu::CardLayout => {
            render_card_layout_panel(sidebar, menu, s.options.card_layout, p)
        }
        OptionsSubmenu::CardProperties => {
            render_card_properties_panel(sidebar, menu, &s.options, p)
        }
    }
}

fn render_hosts_panel(
    sidebar_weak: &gpui::WeakEntity<Sidebar>,
    menu_weak: &gpui::WeakEntity<PopupMenu>,
    filters: &WorkspaceFilters,
    hosts: &[HostMenuEntry],
    p: &'static ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let all_checked = filters.hosts.is_empty();
    let clear_sidebar = sidebar_weak.clone();
    let clear_menu = menu_weak.clone();
    let mut panel = submenu_panel_frame("sidebar-hosts-panel", p)
        .child(
            div()
                .id("sidebar-host-all")
                .debug_selector(|| "sidebar-host-all".into())
                .px_2()
                .py_1()
                .rounded(ui::RADIUS_XS)
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .hover(move |s| s.bg(p.accent).text_color(p.accent_foreground))
                .on_click(move |_, _, cx| {
                    clear_sidebar.update(cx, Sidebar::clear_host_filters).ok();
                    clear_menu.update(cx, |_, cx| cx.notify()).ok();
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(check_slot(all_checked, p))
                        .child(
                            div()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.popover_foreground)
                                .child(text::ALL_HOSTS.get()),
                        ),
                )
                .child(
                    div()
                        .text_size(ui::TEXT_2XS)
                        .text_color(p.muted_foreground)
                        .child(text::ALL_HOSTS_DETAIL.get()),
                ),
        )
        .child(menu_separator(p));

    for host in hosts {
        let checked = !filters.hosts.is_empty() && filters.hosts.contains(host.id.as_key());
        let row_id = format!("sidebar-host-item-{}", host.id.as_key());
        let debug_id = row_id.clone();
        let target_host = host.id.clone();
        let label = host.label.clone();
        let status = host.status_label;
        let sb = sidebar_weak.clone();
        let mn = menu_weak.clone();
        panel = panel.child(
            div()
                .id(SharedString::from(row_id))
                .debug_selector(move || debug_id.clone())
                .px_2()
                .py_1()
                .rounded(ui::RADIUS_XS)
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .hover(move |s| s.bg(p.accent).text_color(p.accent_foreground))
                .on_click(move |_, _, cx| {
                    sb.update(cx, |s, cx| s.toggle_host_filter(&target_host, cx))
                        .ok();
                    mn.update(cx, |_, cx| cx.notify()).ok();
                })
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(check_slot(checked, p))
                        .child(
                            div()
                                .truncate()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.popover_foreground)
                                .child(label),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(ui::TEXT_2XS)
                        .text_color(p.muted_foreground)
                        .child(status),
                ),
        );
    }
    panel
}

fn render_projects_panel(
    sidebar_weak: &gpui::WeakEntity<Sidebar>,
    menu_weak: &gpui::WeakEntity<PopupMenu>,
    sidebar: &Sidebar,
    repos: &[PathBuf],
    p: &'static ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let filters = &sidebar.options.filters;
    let has_selection = !filters.repos.is_empty();
    let clear_sb = sidebar_weak.clone();
    let clear_mn = menu_weak.clone();
    let mut panel = submenu_panel_frame("sidebar-projects-panel", p)
        .child(
            div()
                .px_2()
                .py_0p5()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.muted_foreground)
                        .child(text::PROJECTS.get()),
                )
                .child(
                    div()
                        .id("sidebar-projects-clear")
                        .debug_selector(|| "sidebar-projects-clear".into())
                        .px_1()
                        .py_0p5()
                        .rounded(ui::RADIUS_XS)
                        .cursor_pointer()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .hover(move |s| s.bg(p.accent).text_color(p.accent_foreground))
                        .on_click(move |_, _, cx| {
                            clear_sb.update(cx, Sidebar::clear_repo_filters).ok();
                            clear_mn.update(cx, |_, cx| cx.notify()).ok();
                        })
                        .child(if has_selection {
                            text::CLEAR_SELECTION.get()
                        } else {
                            text::SELECT_ALL.get()
                        }),
                ),
        )
        .child(menu_separator(p));

    for (idx, repo) in repos.iter().enumerate() {
        let checked = filters.is_repo_selected(repo);
        let name = basename(repo);
        let display_name = sidebar.project_header_name(repo);
        let row_id = format!("sidebar-project-item-{idx}");
        let debug_id = format!("sidebar-project-item-{name}");
        let target = repo.clone();
        let sb = sidebar_weak.clone();
        let mn = menu_weak.clone();
        panel = panel.child(
            div()
                .id(SharedString::from(row_id))
                .debug_selector(move || debug_id.clone())
                .px_2()
                .py_1()
                .rounded(ui::RADIUS_XS)
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_1p5()
                .hover(move |s| s.bg(p.accent).text_color(p.accent_foreground))
                .on_click(move |_, _, cx| {
                    sb.update(cx, |s, cx| s.toggle_repo_filter(target.clone(), cx))
                        .ok();
                    mn.update(cx, |_, cx| cx.notify()).ok();
                })
                .child(check_slot(checked, p))
                .child(ui::icons::project_header_icon(
                    sidebar.project_icon(repo),
                    sidebar.project_badge_color(repo),
                    ui::ICON_XS,
                    p.muted_foreground,
                ))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.popover_foreground)
                        .child(display_name),
                ),
        );
    }
    panel
}

fn render_group_by_section(
    sidebar_weak: &gpui::WeakEntity<Sidebar>,
    menu_weak: &gpui::WeakEntity<PopupMenu>,
    current: GroupBy,
    p: &'static ui::Palette,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(section_label(text::GROUP_BY.get(), p))
        .child(
            div()
                .id("sidebar-group-by-toggle")
                .debug_selector(|| "sidebar-group-by-toggle".into())
                .mx_1()
                .child(ui::segmented_control(
                    p,
                    GroupBy::SEGMENT_ORDER.into_iter().map(|group| {
                        let active = current == group;
                        let selector = group.selector();
                        let sb = sidebar_weak.clone();
                        let mn = menu_weak.clone();
                        ui::segment(p, selector, group.short_label(), active)
                            .debug_selector(move || selector.into())
                            .on_click(move |_, _, cx| {
                                sb.update(cx, |s, cx| s.set_group_by(group, cx)).ok();
                                mn.update(cx, |_, cx| cx.notify()).ok();
                            })
                    }),
                )),
        )
}

fn render_sort_panel(
    sidebar_weak: &gpui::WeakEntity<Sidebar>,
    menu_weak: &gpui::WeakEntity<PopupMenu>,
    current: SortBy,
    p: &'static ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let mut panel = submenu_panel_frame("sidebar-sort-panel", p);
    for sort in SortBy::ALL {
        let checked = current == sort;
        let selector = sort.selector();
        let sb = sidebar_weak.clone();
        let mn = menu_weak.clone();
        panel = panel.child(
            div()
                .id(selector)
                .debug_selector(move || selector.into())
                .px_2()
                .py_1()
                .rounded(ui::RADIUS_XS)
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_1p5()
                .hover(move |s| s.bg(p.accent).text_color(p.accent_foreground))
                .on_click(move |_, _, cx| {
                    sb.update(cx, |s, cx| s.set_sort_by(sort, cx)).ok();
                    mn.update(cx, |_, cx| cx.notify()).ok();
                })
                .child(check_slot(checked, p))
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.popover_foreground)
                        .child(sort.label()),
                ),
        );
    }
    panel
}

fn render_card_layout_panel(
    sidebar_weak: &gpui::WeakEntity<Sidebar>,
    menu_weak: &gpui::WeakEntity<PopupMenu>,
    current: CardLayout,
    p: &'static ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let mut panel = submenu_panel_frame("sidebar-card-layout-panel", p);
    for layout in CardLayout::ALL {
        let checked = current == layout;
        let selector = layout.selector();
        let sb = sidebar_weak.clone();
        let mn = menu_weak.clone();
        panel = panel.child(
            div()
                .id(selector)
                .debug_selector(move || selector.into())
                .px_2()
                .py_1()
                .rounded(ui::RADIUS_XS)
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_1p5()
                .hover(move |s| s.bg(p.accent).text_color(p.accent_foreground))
                .on_click(move |_, _, cx| {
                    sb.update(cx, |s, cx| s.set_card_layout(layout, cx)).ok();
                    mn.update(cx, |_, cx| cx.notify()).ok();
                })
                .child(check_slot(checked, p))
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.popover_foreground)
                        .child(layout.label()),
                ),
        );
    }
    panel
}

fn render_card_properties_panel(
    sidebar_weak: &gpui::WeakEntity<Sidebar>,
    menu_weak: &gpui::WeakEntity<PopupMenu>,
    options: &WorkspaceOptions,
    p: &'static ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let mut panel = submenu_panel_frame("sidebar-card-properties-panel", p).when(
        options.card_layout == CardLayout::Compact,
        |d| {
            d.child(
                div()
                    .id("sidebar-card-properties-hover-hint")
                    .debug_selector(|| "sidebar-card-properties-hover-hint".into())
                    .px_2()
                    .py_0p5()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::COMPACT_HOVER_HINT.get()),
            )
            .child(menu_separator(p))
        },
    );
    for prop in CardProperty::ALL {
        let checked = options.card_properties.get(prop);
        let selector = prop.selector();
        let sb = sidebar_weak.clone();
        let mn = menu_weak.clone();
        panel = panel.child(
            div()
                .id(selector)
                .debug_selector(move || selector.into())
                .px_2()
                .py_1()
                .rounded(ui::RADIUS_XS)
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .hover(move |s| s.bg(p.accent).text_color(p.accent_foreground))
                .on_click(move |_, _, cx| {
                    sb.update(cx, |s, cx| s.toggle_card_property(prop, cx)).ok();
                    mn.update(cx, |_, cx| cx.notify()).ok();
                })
                .child(
                    div().flex().items_center().gap_1p5().child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.popover_foreground)
                            .child(prop.label()),
                    ),
                )
                .child(switch_face(checked, p)),
        );
    }
    panel
}

fn render_filter_section(
    sidebar_weak: &gpui::WeakEntity<Sidebar>,
    menu_weak: &gpui::WeakEntity<PopupMenu>,
    options: &WorkspaceOptions,
    p: &'static ui::Palette,
) -> gpui::Div {
    let active_filters = options.filters.active_count();
    let sleep_sb = sidebar_weak.clone();
    let sleep_mn = menu_weak.clone();
    let def_sb = sidebar_weak.clone();
    let def_mn = menu_weak.clone();
    let auto_sb = sidebar_weak.clone();
    let auto_mn = menu_weak.clone();
    let cli_sb = sidebar_weak.clone();
    let cli_mn = menu_weak.clone();
    let reset_sb = sidebar_weak.clone();
    let reset_mn = menu_weak.clone();

    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(section_label(text::FILTERS.get(), p))
        .child(render_filter_toggle_row(
            "sidebar-filter-hide-sleeping",
            ui::icons::MOON_SVG,
            text::FILTER_HIDE_SLEEPING.get(),
            options.filters.hide_sleeping,
            false,
            p,
            move |cx| {
                sleep_sb.update(cx, Sidebar::toggle_hide_sleeping).ok();
                sleep_mn.update(cx, |_, cx| cx.notify()).ok();
            },
        ))
        .when(options.filters.hide_sleeping, |d| {
            d.child(render_filter_toggle_row(
                "sidebar-filter-keep-default-branch",
                ui::icons::GIT_BRANCH_SVG,
                text::FILTER_ALWAYS_SHOW_DEFAULT.get(),
                options.filters.always_show_default_branch,
                true,
                p,
                move |cx| {
                    def_sb
                        .update(cx, Sidebar::toggle_always_show_default_branch)
                        .ok();
                    def_mn.update(cx, |_, cx| cx.notify()).ok();
                },
            ))
        })
        .child(render_filter_toggle_row(
            "sidebar-filter-hide-automation",
            ui::icons::CALENDAR_CLOCK_SVG,
            text::FILTER_HIDE_AUTOMATION.get(),
            options.filters.hide_automation,
            false,
            p,
            move |cx| {
                auto_sb.update(cx, Sidebar::toggle_hide_automation).ok();
                auto_mn.update(cx, |_, cx| cx.notify()).ok();
            },
        ))
        .child(render_filter_toggle_row(
            "sidebar-filter-hide-cli",
            ui::icons::SQUARE_TERMINAL_SVG,
            text::FILTER_HIDE_CLI.get(),
            options.filters.hide_cli,
            false,
            p,
            move |cx| {
                cli_sb.update(cx, Sidebar::toggle_hide_cli).ok();
                cli_mn.update(cx, |_, cx| cx.notify()).ok();
            },
        ))
        .when(active_filters > 0, |d| {
            d.child(menu_separator(p)).child(
                div()
                    .id("sidebar-filter-reset")
                    .debug_selector(|| "sidebar-filter-reset".into())
                    .px_2()
                    .py_1p5()
                    .rounded(ui::RADIUS_SM)
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .hover(move |s| s.bg(p.accent).text_color(p.accent_foreground))
                    .on_click(move |_, _, cx| {
                        reset_sb.update(cx, Sidebar::reset_filters).ok();
                        reset_mn.update(cx, |_, cx| cx.notify()).ok();
                    })
                    .child(ui::icons::svg_box(
                        ui::icons::ROTATE_CCW_SVG,
                        ui::ICON_SM,
                        p.muted_foreground,
                    ))
                    .child(text::FILTER_RESET.get()),
            )
        })
}

fn render_filter_toggle_row(
    id: &'static str,
    icon_bytes: &'static [u8],
    label: &'static str,
    checked: bool,
    indented: bool,
    p: &'static ui::Palette,
    on_toggle: impl Fn(&mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let text_color = if indented {
        p.muted_foreground
    } else {
        p.popover_foreground
    };
    div()
        .id(id)
        .debug_selector(move || id.into())
        .when(indented, |d| d.pl(px(22.)).pr_2())
        .when(!indented, gpui::Styled::px_2)
        .py_1p5()
        .rounded(ui::RADIUS_SM)
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .hover(move |s| s.bg(p.accent).text_color(p.accent_foreground))
        .on_click(move |_, _, cx| on_toggle(cx))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .gap_2()
                .child(ui::icons::svg_box(
                    icon_bytes,
                    ui::ICON_SM,
                    p.muted_foreground,
                ))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(ui::TEXT_XS)
                        .text_color(text_color)
                        .child(label),
                ),
        )
        .child(switch_face(checked, p))
}

fn render_submenu_label(
    target: OptionsSubmenu,
    summary: String,
    p: &'static ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let (id, summary_id) = target.selectors();
    div()
        .id(id)
        .debug_selector(move || id.into())
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .text_size(ui::TEXT_XS)
        .text_color(p.popover_foreground)
        .child(target.label())
        .child(
            div()
                .id(summary_id)
                .debug_selector(move || summary_id.into())
                .max_w(px(140.))
                .truncate()
                .text_color(p.muted_foreground)
                .child(summary),
        )
}

/// OWE-888: `ui::switch` 정본 토큰(트랙 `SWITCH_WIDTH`×`SWITCH_HEIGHT`, 썸 `SWITCH_THUMB`,
/// 켜짐 `primary`/꺼짐 `muted` + 테두리, 썸 `primary_foreground`/`muted_foreground`)을 그대로 쓰는
/// 비상호작용 스위치 외형. 행 전체가 클릭 토글을 단일 소유하므로 여기서 중복
/// 토글 핸들러를 달지 않는다(행 `on_click` 한 번만 토글된다).
fn switch_face(checked: bool, p: &'static ui::Palette) -> gpui::Div {
    let track_bg = if checked { p.primary } else { p.muted };
    let track_border = if checked { p.primary } else { p.border };
    let thumb_bg = if checked {
        p.primary_foreground
    } else {
        p.muted_foreground
    };
    div()
        .w(ui::SWITCH_WIDTH)
        .h(ui::SWITCH_HEIGHT)
        .flex_none()
        .rounded_full()
        .p(px(2.))
        .flex()
        .items_center()
        .when(checked, Styled::justify_end)
        .when(!checked, Styled::justify_start)
        .border_1()
        .border_color(track_border)
        .bg(track_bg)
        .child(div().size(ui::SWITCH_THUMB).rounded_full().bg(thumb_bg))
}

fn check_slot(checked: bool, p: &'static ui::Palette) -> gpui::Div {
    div()
        .size(px(13.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .when(checked, |d| {
            d.child(
                Icon::new(IconName::Check)
                    .with_size(ui::ICON_XS)
                    .text_color(p.primary),
            )
        })
}

fn submenu_panel_frame(id: &'static str, p: &'static ui::Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .mx(px(-4.))
        .w(px(248.))
        .font_family(ui::FONT_UI)
        .text_size(ui::TEXT_XS)
        .text_color(p.popover_foreground)
        .flex()
        .flex_col()
        .gap_0p5()
}

fn section_label(label: &'static str, p: &'static ui::Palette) -> gpui::Div {
    div()
        .px_2()
        .pt_1()
        .pb_0p5()
        .text_size(ui::TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted_foreground)
        .child(label)
}

fn menu_separator(p: &'static ui::Palette) -> gpui::Div {
    div().my_1().h(px(1.)).w_full().bg(p.border)
}

#[cfg(all(test, unix))]
mod projection_tests {
    use super::*;

    #[test]
    fn merged_projection_reuses_maps_and_updates_only_when_sources_change() {
        let mut sidebar = Sidebar::new(None);
        let path = PathBuf::from("/repo/tree");
        sidebar
            .disk
            .store
            .statuses
            .insert("/repo/tree".into(), "done".into());
        sidebar
            .disk
            .store
            .updated_at
            .insert("/repo/tree".into(), 12);
        sidebar.disk.store.unread.insert("/repo/tree".into(), true);
        sidebar.disk.store.prs.insert("/repo/tree".into(), 44);
        sidebar
            .worktree_statuses
            .insert(path.clone(), WorkspaceStatus::InProgress);
        let mut cache = LocalRowsCache::default();
        cache.refresh(&sidebar);
        assert_eq!(cache.statuses[&path], WorkspaceStatus::InProgress);
        assert_eq!(cache.updated_at[&path], 12);
        assert!(cache.unread_worktrees.contains(&path));
        assert!(cache.checkpoint_prs.contains(&path));
        let status = &raw const cache.statuses[&path];
        cache.refresh(&sidebar);
        assert_eq!(status, &raw const cache.statuses[&path]);
        sidebar.worktree_statuses.remove(&path);
        sidebar
            .disk
            .store
            .updated_at
            .insert("/repo/tree".into(), 15);
        sidebar.disk.store.unread.insert("/repo/tree".into(), false);
        sidebar.disk.store.prs.remove("/repo/tree");
        cache.refresh(&sidebar);
        assert_eq!(cache.statuses[&path], WorkspaceStatus::Completed);
        assert_eq!(cache.updated_at[&path], 15);
        assert!(!cache.unread_worktrees.contains(&path));
        assert!(!cache.checkpoint_prs.contains(&path));
    }
}
