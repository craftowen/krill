//! 사이드바 워크스페이스 그룹화·정렬·가시성 필터·카드 표시 속성 옵션(MK-177 / OWE-731).
//!
//! 구성·우선순위·메뉴 구조 참고:
//! `.ref/orca/src/renderer/src/components/sidebar/{workspace-options-menu-items,SidebarWorkspaceFilterSection,WorktreeCardDisplayMenuSection,visible-worktrees,smart-sort}.*`
//! (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gpui::{Context, div, prelude::*};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::{Icon, Sizable as _};
use krill_proto::SessionId;
use krill_proto::control::AgentState;
use krill_proto::orchestration::WorkspaceStatus;
use serde::{Deserialize, Serialize};

use super::{Meta, Preferences, Row, Sidebar, Snapshot, basename, external, needs_you, text};
use crate::hosts::HostId;
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

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Repo => text::GROUP_BY_REPO.get(),
            Self::WorkspaceStatus => text::GROUP_BY_STATUS.get(),
            Self::PrStatus => text::GROUP_BY_PR.get(),
            Self::None => text::GROUP_BY_NONE.get(),
        }
    }
}

/// 사이드바 워크트리 정렬 기준.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SortBy {
    /// 스마트 정렬: 승인/주의 필요 → 완료 미확인 → 작업 중 → 완료 → 최근 활동순 → 이름순.
    #[default]
    Smart,
    Recent,
    Name,
}

impl SortBy {
    pub const ALL: [Self; 3] = [Self::Smart, Self::Recent, Self::Name];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Smart => text::SORT_BY_SMART.get(),
            Self::Recent => text::SORT_BY_RECENT.get(),
            Self::Name => text::SORT_BY_NAME.get(),
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
    pub fn contains_repo(&self, repo: &Path) -> bool {
        self.repos.is_empty() || self.repos.contains(repo)
    }

    pub fn toggle_repo(&mut self, repo: PathBuf) {
        if !self.repos.remove(&repo) {
            self.repos.insert(repo);
        }
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
                | crate::pull_request::Status::Failed,
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
#[derive(Default)]
pub(super) struct WorktreeContext<'a> {
    pub meta: Option<&'a Meta>,
    pub prs: Option<&'a BTreeMap<PathBuf, crate::pull_request::Status>>,
    pub checkpoint_prs: Option<&'a BTreeSet<PathBuf>>,
    pub statuses: Option<&'a BTreeMap<PathBuf, WorkspaceStatus>>,
    pub updated_at: Option<&'a BTreeMap<PathBuf, u64>>,
    pub unread: Option<&'a BTreeSet<SessionId>>,
    pub unread_worktrees: Option<&'a BTreeSet<PathBuf>>,
    pub focused: Option<SessionId>,
    pub automation_worktrees: Option<&'a BTreeSet<PathBuf>>,
    pub cli_worktrees: Option<&'a BTreeSet<PathBuf>>,
    pub parents: Option<&'a BTreeMap<PathBuf, PathBuf>>,
    pub collapsed_lineage: Option<&'a BTreeSet<PathBuf>>,
    pub sleeping_worktrees: Option<&'a BTreeSet<PathBuf>>,
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
    {
        return true;
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
fn smart_rank(path: &Path, snapshot: &Snapshot, ctx: &WorktreeContext<'_>) -> u8 {
    let mut has_waiting_or_blocked = false;
    let mut has_unread_done = ctx.unread_worktrees.is_some_and(|u| u.contains(path));
    let mut has_working = false;
    let mut has_done = false;
    for s in snapshot
        .sessions
        .iter()
        .filter(|s| s.worktree.as_deref() == Some(path))
    {
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

pub(super) fn sort_worktrees(
    trees: &mut [&PathBuf],
    snapshot: &Snapshot,
    prefs: &Preferences,
    sort_by: SortBy,
    ctx: &WorktreeContext<'_>,
) {
    trees.sort_by(|a, b| {
        let pin_a = !prefs.get(*a).is_some_and(|v| v.pinned);
        let pin_b = !prefs.get(*b).is_some_and(|v| v.pinned);
        let name_a = worktree_sort_name(a, prefs, ctx.meta);
        let name_b = worktree_sort_name(b, prefs, ctx.meta);
        let recent_a = Reverse(ctx.updated_at.and_then(|u| u.get(*a)).copied().unwrap_or(0));
        let recent_b = Reverse(ctx.updated_at.and_then(|u| u.get(*b)).copied().unwrap_or(0));
        match sort_by {
            SortBy::Smart => (pin_a, smart_rank(a, snapshot, ctx), recent_a, &name_a, *a).cmp(&(
                pin_b,
                smart_rank(b, snapshot, ctx),
                recent_b,
                &name_b,
                *b,
            )),
            SortBy::Recent => (pin_a, recent_a, &name_a, *a).cmp(&(pin_b, recent_b, &name_b, *b)),
            SortBy::Name => (pin_a, &name_a, *a).cmp(&(pin_b, &name_b, *b)),
        }
    });
}

fn push_worktree_and_sessions(
    out: &mut Vec<Row>,
    tree: &PathBuf,
    snapshot: &Snapshot,
    collapsed: &BTreeSet<PathBuf>,
) {
    out.push(Row::Worktree(tree.clone()));
    if !collapsed.contains(tree) {
        for s in snapshot
            .sessions
            .iter()
            .filter(|s| s.worktree.as_ref() == Some(tree))
        {
            out.push(Row::Session(s.clone(), true));
        }
    }
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
    let mut waiting = snapshot
        .sessions
        .iter()
        .filter(|s| needs_you(&s.status))
        .peekable();
    if waiting.peek().is_some() {
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
            .filter(|p| is_worktree_visible(repo, p, n, snapshot, prefs, options, ctx))
            .collect()
    };

    match options.group_by {
        GroupBy::Repo => {
            for (repo, trees) in &snapshot.trees {
                if !options.filters.contains_repo(repo) {
                    continue;
                }
                let mut vis = visible_in_repo(repo, trees);
                if filters_active && vis.is_empty() && !trees.is_empty() {
                    continue;
                }
                out.push(Row::Repository(repo.clone()));
                sort_worktrees(&mut vis, snapshot, prefs, options.sort_by, ctx);
                let (normal, claude_ext) = external::partition_trees(repo, vis);
                let normal = super::lineage::order_lineage_worktrees(
                    normal,
                    ctx.parents,
                    ctx.collapsed_lineage,
                );
                for tree in normal {
                    push_worktree_and_sessions(&mut out, tree, snapshot, collapsed);
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
                            push_worktree_and_sessions(&mut out, tree, snapshot, collapsed);
                        }
                    }
                }
            }
        }
        GroupBy::WorkspaceStatus => {
            let mut buckets: BTreeMap<usize, Vec<&PathBuf>> = BTreeMap::new();
            for (repo, trees) in &snapshot.trees {
                if !options.filters.contains_repo(repo) {
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
                sort_worktrees(&mut bucket, snapshot, prefs, options.sort_by, ctx);
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
                        push_worktree_and_sessions(&mut out, tree, snapshot, collapsed);
                    }
                }
            }
        }
        GroupBy::PrStatus => {
            let mut buckets: BTreeMap<PrGroup, Vec<&PathBuf>> = BTreeMap::new();
            for (repo, trees) in &snapshot.trees {
                if !options.filters.contains_repo(repo) {
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
                sort_worktrees(&mut bucket, snapshot, prefs, options.sort_by, ctx);
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
                        push_worktree_and_sessions(&mut out, tree, snapshot, collapsed);
                    }
                }
            }
        }
        GroupBy::None => {
            let mut all_trees: Vec<&PathBuf> = Vec::new();
            for (repo, trees) in &snapshot.trees {
                if !options.filters.contains_repo(repo) {
                    continue;
                }
                all_trees.extend(visible_in_repo(repo, trees));
            }
            sort_worktrees(&mut all_trees, snapshot, prefs, options.sort_by, ctx);
            let ordered = super::lineage::order_lineage_worktrees(
                all_trees,
                ctx.parents,
                ctx.collapsed_lineage,
            );
            for tree in ordered {
                push_worktree_and_sessions(&mut out, tree, snapshot, collapsed);
            }
        }
    }

    let mut folders: BTreeMap<&PathBuf, Vec<Row>> = BTreeMap::new();
    for s in snapshot.sessions.iter().filter(|s| s.worktree.is_none()) {
        if let Some(cwd) = &s.cwd {
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
    let loose: Vec<_> = snapshot
        .sessions
        .iter()
        .filter(|s| s.worktree.is_none() && s.cwd.is_none())
        .collect();
    if !loose.is_empty() {
        out.push(Row::Loose);
        out.extend(loose.into_iter().map(|s| Row::Session(s.clone(), false)));
    }
    out
}

impl Sidebar {
    pub(super) fn local_rows(&self) -> Vec<Row> {
        let state_dir = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty());
        let mut statuses = self.worktree_statuses.clone();
        let mut updated_at = self.worktree_updated_at.clone();
        let mut unread_worktrees = BTreeSet::new();
        let mut checkpoint_prs = BTreeSet::new();
        if let Some(dir) = state_dir {
            let store = krill_proto::orchestration::WorktreeStore::load_cached(dir);
            for (k, v) in &store.statuses {
                if let Ok(st) = WorkspaceStatus::parse(v) {
                    statuses.entry(PathBuf::from(k)).or_insert(st);
                }
            }
            for (k, &ts) in &store.updated_at {
                updated_at.entry(PathBuf::from(k)).or_insert(ts);
            }
            for (k, &u) in &store.unread {
                if u {
                    unread_worktrees.insert(PathBuf::from(k));
                }
            }
            for (k, &pr) in &store.prs {
                if pr > 0 {
                    checkpoint_prs.insert(PathBuf::from(k));
                }
            }
        }
        let parents = self.merged_parents();
        let ctx = WorktreeContext {
            meta: Some(&self.meta),
            prs: Some(&self.prs),
            checkpoint_prs: Some(&checkpoint_prs),
            statuses: Some(&statuses),
            updated_at: Some(&updated_at),
            unread: Some(&self.unread),
            unread_worktrees: Some(&unread_worktrees),
            focused: self.focused,
            automation_worktrees: Some(&self.automation_worktrees),
            cli_worktrees: Some(&self.cli_worktrees),
            parents: Some(&parents),
            collapsed_lineage: Some(&self.collapsed_lineage),
            sleeping_worktrees: Some(&self.sleeping_worktrees),
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

    pub(super) fn remote_rows(&self, remote: &super::RemoteHost) -> Vec<Row> {
        let parents = self.merged_parents();
        let ctx = WorktreeContext {
            meta: Some(&remote.meta),
            prs: Some(&self.prs),
            statuses: Some(&self.worktree_statuses),
            updated_at: Some(&self.worktree_updated_at),
            focused: self.focused,
            automation_worktrees: Some(&self.automation_worktrees),
            cli_worktrees: Some(&self.cli_worktrees),
            parents: Some(&parents),
            collapsed_lineage: Some(&self.collapsed_lineage),
            sleeping_worktrees: Some(&self.sleeping_worktrees),
            ..WorktreeContext::default()
        };
        rows_with_options(
            &remote.snapshot,
            &self.prefs,
            &self.collapsed,
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
        if self.options.group_by != group_by {
            self.options.group_by = group_by;
            self.persist(cx);
        }
    }

    pub fn set_sort_by(&mut self, sort_by: SortBy, cx: &mut Context<Self>) {
        if self.options.sort_by != sort_by {
            self.options.sort_by = sort_by;
            self.persist(cx);
        }
    }

    pub fn set_card_layout(&mut self, layout: CardLayout, cx: &mut Context<Self>) {
        if self.options.card_layout != layout {
            self.options.card_layout = layout;
            self.persist(cx);
        }
    }

    pub fn toggle_card_property(&mut self, prop: CardProperty, cx: &mut Context<Self>) {
        self.options.card_properties.toggle(prop);
        self.persist(cx);
    }

    pub fn set_card_property(&mut self, prop: CardProperty, enabled: bool, cx: &mut Context<Self>) {
        if self.options.card_properties.get(prop) != enabled {
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

    pub fn toggle_host_filter(&mut self, host: &HostId, cx: &mut Context<Self>) {
        self.options.filters.toggle_host(host);
        self.persist(cx);
    }

    pub fn reset_filters(&mut self, cx: &mut Context<Self>) {
        if self.options.filters.active_count() > 0 {
            self.options.filters.reset();
            self.persist(cx);
        }
    }

    pub fn fold_group(&mut self, key: GroupKey, open: Option<bool>, cx: &mut Context<Self>) {
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
        let dir = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty())?;
        let store = krill_proto::orchestration::WorktreeStore::load_cached(dir);
        store
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
        for s in self
            .snapshot_for(host)
            .sessions
            .iter()
            .filter(|s| s.worktree.as_deref() == Some(path))
        {
            if let Some(title) = self.titles.get(&s.id) {
                for entry in krill_proto::orchestration::extract_local_urls(s.id.0, title, false) {
                    set.insert(entry.port);
                }
            }
        }
        set.into_iter().collect()
    }

    pub(super) fn host_badge_for(&self, host: &HostId, path: &Path) -> Option<String> {
        if let Some(custom) = self.worktree_hosts.get(path) {
            return Some(custom.clone());
        }
        match host {
            HostId::Remote(id) => Some(
                self.remote_hosts
                    .iter()
                    .find(|r| r.id == *id)
                    .map_or_else(|| id.clone(), |r| r.label.clone()),
            ),
            HostId::Local => {
                (!self.remote_hosts.is_empty()).then(|| text::HOST_LOCAL.get().to_owned())
            }
        }
    }

    pub(super) fn render_options_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let options = self.options.clone();
        let active_filters = options.active_filter_count();
        let repos: Vec<PathBuf> = self.snapshot.trees.keys().cloned().collect();
        let hosts: Vec<(HostId, String)> = if self.remote_hosts.is_empty() {
            Vec::new()
        } else {
            std::iter::once((HostId::Local, text::HOST_LOCAL.get().to_owned()))
                .chain(
                    self.remote_hosts
                        .iter()
                        .map(|r| (HostId::Remote(r.id.clone()), r.label.clone())),
                )
                .collect()
        };
        let this = cx.weak_entity();
        let btn = Button::new("sidebar-options")
            .ghost()
            .xsmall()
            .toggled(options.is_customized())
            .icon(Icon::default().data(ui::icons::SLIDERS_HORIZONTAL_SVG))
            .accessibility_label(text::WORKSPACE_OPTIONS.get())
            .tooltip(text::WORKSPACE_OPTIONS.get())
            .dropdown_menu(move |menu, window, cx| {
                build_options_menu(menu, &this, &options, &repos, &hosts, window, cx)
            });

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
                        .px_1()
                        .rounded(ui::RADIUS_XS)
                        .bg(p.sidebar_accent)
                        .font_family(ui::FONT_MONO)
                        .text_size(ui::TEXT_2XS)
                        .text_color(p.accent_foreground)
                        .child(active_filters.to_string()),
                )
            })
    }
}

fn build_options_menu(
    mut menu: PopupMenu,
    this: &gpui::WeakEntity<Sidebar>,
    options: &WorkspaceOptions,
    repos: &[PathBuf],
    hosts: &[(HostId, String)],
    window: &mut gpui::Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    menu = menu.min_w(gpui::px(230.)).label(text::GROUP_BY.get());
    for group in GroupBy::ALL {
        let this = this.clone();
        menu = menu.item(
            PopupMenuItem::new(group.label())
                .checked(options.group_by == group)
                .on_click(move |_, _, cx| {
                    this.update(cx, |s, cx| s.set_group_by(group, cx)).ok();
                }),
        );
    }
    menu = menu.separator().label(text::SORT_BY.get());
    for sort in SortBy::ALL {
        let this = this.clone();
        menu = menu.item(
            PopupMenuItem::new(sort.label())
                .checked(options.sort_by == sort)
                .on_click(move |_, _, cx| {
                    this.update(cx, |s, cx| s.set_sort_by(sort, cx)).ok();
                }),
        );
    }
    menu = menu.separator().label(text::FILTERS.get());
    {
        let this = this.clone();
        menu = menu.item(
            PopupMenuItem::new(text::FILTER_HIDE_SLEEPING.get())
                .checked(options.filters.hide_sleeping)
                .on_click(move |_, _, cx| {
                    this.update(cx, Sidebar::toggle_hide_sleeping).ok();
                }),
        );
    }
    if options.filters.hide_sleeping {
        let this = this.clone();
        menu = menu.item(
            PopupMenuItem::new(text::FILTER_ALWAYS_SHOW_DEFAULT.get())
                .checked(options.filters.always_show_default_branch)
                .on_click(move |_, _, cx| {
                    this.update(cx, Sidebar::toggle_always_show_default_branch)
                        .ok();
                }),
        );
    }
    {
        let this = this.clone();
        menu = menu.item(
            PopupMenuItem::new(text::FILTER_HIDE_AUTOMATION.get())
                .checked(options.filters.hide_automation)
                .on_click(move |_, _, cx| {
                    this.update(cx, Sidebar::toggle_hide_automation).ok();
                }),
        );
    }
    {
        let this = this.clone();
        menu = menu.item(
            PopupMenuItem::new(text::FILTER_HIDE_CLI.get())
                .checked(options.filters.hide_cli)
                .on_click(move |_, _, cx| {
                    this.update(cx, Sidebar::toggle_hide_cli).ok();
                }),
        );
    }
    if repos.len() > 1 {
        let (this, repo_list, selected) =
            (this.clone(), repos.to_vec(), options.filters.repos.clone());
        menu = menu.submenu(
            text::FILTER_REPOS.get(),
            window,
            cx,
            move |mut sub, _, _| {
                for repo in &repo_list {
                    let (this, target) = (this.clone(), repo.clone());
                    sub = sub.item(
                        PopupMenuItem::new(basename(repo))
                            .checked(selected.contains(repo))
                            .on_click(move |_, _, cx| {
                                this.update(cx, |s, cx| s.toggle_repo_filter(target.clone(), cx))
                                    .ok();
                            }),
                    );
                }
                sub
            },
        );
    }
    if !hosts.is_empty() {
        let (this, host_list, selected) =
            (this.clone(), hosts.to_vec(), options.filters.hosts.clone());
        menu = menu.submenu(
            text::FILTER_HOSTS.get(),
            window,
            cx,
            move |mut sub, _, _| {
                for (host_id, label) in &host_list {
                    let (this, target) = (this.clone(), host_id.clone());
                    sub = sub.item(
                        PopupMenuItem::new(label.clone())
                            .checked(selected.contains(host_id.as_key()))
                            .on_click(move |_, _, cx| {
                                this.update(cx, |s, cx| s.toggle_host_filter(&target, cx))
                                    .ok();
                            }),
                    );
                }
                sub
            },
        );
    }
    if options.filters.active_count() > 0 {
        let this = this.clone();
        menu = menu.item(
            PopupMenuItem::new(text::FILTER_RESET.get()).on_click(move |_, _, cx| {
                this.update(cx, Sidebar::reset_filters).ok();
            }),
        );
    }
    menu = menu.separator().label(text::CARD_LAYOUT.get());
    for layout in CardLayout::ALL {
        let this = this.clone();
        menu = menu.item(
            PopupMenuItem::new(layout.label())
                .checked(options.card_layout == layout)
                .on_click(move |_, _, cx| {
                    this.update(cx, |s, cx| s.set_card_layout(layout, cx)).ok();
                }),
        );
    }
    menu = menu.separator().label(text::CARD_PROPERTIES.get());
    for prop in CardProperty::ALL {
        let this = this.clone();
        menu = menu.item(
            PopupMenuItem::new(prop.label())
                .checked(options.card_properties.get(prop))
                .on_click(move |_, _, cx| {
                    this.update(cx, |s, cx| s.toggle_card_property(prop, cx))
                        .ok();
                }),
        );
    }
    menu
}
