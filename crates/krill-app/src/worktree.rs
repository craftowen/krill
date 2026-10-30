//! OWE-496 워크트리 생성 대화상자(MK-042 후속). 사이드바 레포 행의 "+"로 연다.
//! OWE-687 모양은 Orca `NewWorkspaceComposerModal`처럼 창 가운데 모달(라벨 붙은 입력칸, 오른쪽 아래 버튼).
//! Orca NewWorkspaceComposer{Modal,Card}, ComposerBaseRefPicker/CreateFromPicker의 선택·검색 분리와 고급 노출을 이식.
//! Copyright (c) 2026 Lovecast Inc. (MIT License; docs/licensing/spdx/MIT.txt).
//! IPC는 사이드바 [`Source`] 뒤에 있어 gpui 테스트는 인메모리 응답을 쓴다.
//! setup 스크립트는 내용과 해시를 보여 주고 사람이 승인한 해시만 돌려보낸다(krilld가 내용 변경 시 다시 묻는다).

use krill_proto::os::ffi::OsStrExt as _;
use ring::rand::{SecureRandom as _, SystemRandom};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{
    App, Context, Entity, EventEmitter, Focusable as _, SharedString, Subscription, Task, Window,
    div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_component::select::{Select, SelectEvent, SelectState};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _};
use krill_proto::control::{
    NewWorktree, PrepareWorktree, Reply, Setup, Worktree, WorktreePreparation,
};

use crate::hosts::HostId;
use crate::sidebar::Source;

use crate::i18n::catalog::worktree as text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRepoOption {
    pub path: PathBuf,
    pub label: String,
    pub branch: Option<String>,
}

#[derive(Clone)]
pub struct WorktreeHostOption {
    pub host: HostId,
    pub label: String,
    pub source: Arc<dyn Source>,
    pub repos: Vec<WorktreeRepoOption>,
}

impl std::fmt::Debug for WorktreeHostOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorktreeHostOption")
            .field("host", &self.host)
            .field("label", &self.label)
            .field("repos", &self.repos)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 생성 제출 즉시 내보낸다. 모달을 닫고 사이드바에 낙관적 대기 카드를 표시한다.
    Submitting {
        repo: PathBuf,
        parent: Option<PathBuf>,
        names: Vec<String>,
    },
    /// 미승인 setup 스크립트 확인 또는 생성 실패로 모달을 다시 띄워야 할 때 내보낸다.
    ModalReopened(Option<String>),
    /// 사이드바를 바로 새로 고친다.
    Created(Worktree),
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Edit(Option<String>),
    Creating,
    Untrusted { script: String, hash: Vec<u8> },
    Done(Worktree),
}

/// `(NewWorktree, agent_id, initial_prompt)` 튜플.
pub type FanOutPlanItem = (NewWorktree, Option<String>, Option<String>);

/// 워크트리 생성 시 선택 가능한 멀티 에이전트(Fan-Out) 프리셋 목록.
pub const FAN_OUT_AGENTS: &[(&str, &str)] = &[
    ("claude", "Claude"),
    ("codex", "Codex"),
    ("agy", "Antigravity"),
    ("opencode", "OpenCode"),
    ("cursor", "Cursor"),
    ("pi", "Pi"),
    ("aider", "Aider"),
];

/// 프로젝트·실행 위치 콤보박스 행: 이름 + 오른쪽 흐린 보조 설명(Orca `ProjectOptionRow`).
#[derive(Clone)]
struct PickItem<V> {
    value: V,
    title: SharedString,
    detail: SharedString,
}

impl<V: Clone + PartialEq> SearchableListItem for PickItem<V> {
    type Value = V;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &V {
        &self.value
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = crate::ui::palette(cx);
        div()
            .flex()
            .w_full()
            .min_w_0()
            .items_baseline()
            .justify_between()
            .gap_2()
            .child(div().min_w_0().truncate().child(self.title.clone()))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(self.detail.clone()),
            )
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.title.to_lowercase().contains(&query) || self.detail.to_lowercase().contains(&query)
    }
}

type ProjectPicker = SelectState<SearchableVec<PickItem<String>>>;
type HostPicker = SelectState<SearchableVec<PickItem<HostId>>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerSync {
    Dirty,
    Current,
}

pub struct Dialog {
    language: crate::i18n::Language,
    focus: gpui::FocusHandle,
    host: HostId,
    host_options: Vec<WorktreeHostOption>,
    repo: PathBuf,
    parent: Option<PathBuf>,
    name: Entity<InputState>,
    base: Entity<InputState>,
    branch_query: Entity<InputState>,
    branch_picker_open: bool,
    advanced_open: bool,
    branch_generation: u64,
    branch_task: Option<Task<()>>,
    branch_error: Option<std::io::ErrorKind>,
    branch_scroll: gpui::UniformListScrollHandle,
    initial_prompt: Entity<InputState>,
    selected_agents: Vec<String>,
    pending_fan_out: usize,
    last_created_agent: Option<String>,
    last_created_prompt: Option<String>,
    phase: Phase,
    source: Arc<dyn Source>,
    task: Option<Task<()>>,
    _enter: [Subscription; 4],
    /// Orca처럼 프로젝트를 먼저 고르고(검색 콤보박스), 그 프로젝트가 있는 호스트 중 실행 위치를 고른다.
    project_picker: Entity<ProjectPicker>,
    host_picker: Entity<HostPicker>,
    /// 호스트·프로젝트가 바뀌어 콤보박스 항목을 다음 렌더에서 다시 채워야 한다.
    picker_sync: PickerSync,
    preparation: Option<Preparation>,
    preparation_task: Option<Task<()>>,
    preparation_ready: bool,
    /// 실제 메타데이터·git refs만 보관한다. 선택한 base와 검색어는 독립적이다.
    candidates: Vec<String>,
    /// OWE-853: 드롭다운 키보드 하이라이트(필터 목록 내 인덱스). `None`이면 Enter가 생성 제출이다.
    highlight: Option<usize>,
    hooks: krill_proto::orchestration::RepoHookSummary,
    hooks_task: Option<Task<()>>,
    /// git이 아닌 폴더인지(레포 경로별 캐시). 폴더면 기준 ref 대신 안내를 보인다.
    folder: Option<(PathBuf, bool)>,
}

struct Preparation {
    req: PrepareWorktree,
    source: Arc<dyn Source>,
    cancelled: Arc<AtomicBool>,
    executor: gpui::BackgroundExecutor,
}

impl Drop for Preparation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let mut req = self.req.clone();
        req.cancel = true;
        let source = self.source.clone();
        // Entity release must never wait for IPC. The token also handles cancellation-before-start.
        self.executor
            .spawn(async move {
                source.prepare_worktree(req).ok();
            })
            .detach();
    }
}

impl EventEmitter<Event> for Dialog {}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialog")
            .field("repo", &self.repo)
            .field("parent", &self.parent)
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

/// 사람이 확인할 스크립트에서 보이지 않는 제어 문자(ESC 등)를 드러낸다. 줄바꿈·탭은 둔다.
fn visible(script: &str) -> String {
    script
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                char::REPLACEMENT_CHARACTER
            } else {
                c
            }
        })
        .collect()
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        write!(s, "{b:02x}").ok();
        s
    })
}

mod branches;
pub use branches::{branch_suggestions, list_git_refs};

impl Dialog {
    pub fn new(
        repo: PathBuf,
        source: Arc<dyn Source>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_options(repo, None, None, Vec::new(), source, window, cx)
    }

    pub fn new_with_parent(
        repo: PathBuf,
        parent: Option<PathBuf>,
        source: Arc<dyn Source>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_options(repo, parent, None, Vec::new(), source, window, cx)
    }

    pub fn new_with_options(
        repo: PathBuf,
        parent: Option<PathBuf>,
        initial_base: Option<&str>,
        candidates: Vec<String>,
        source: Arc<dyn Source>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(text::NAME.get()));
        let base = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder(text::BASE.get());
            if let Some(default) = initial_base.map(str::trim).filter(|s| !s.is_empty()) {
                state.set_value(default, window, cx);
            }
            state
        });
        let branch_query =
            cx.new(|cx| InputState::new(window, cx).placeholder(text::SEARCH_BRANCHES.get()));
        let initial_prompt = cx.new(|cx| {
            InputState::new(window, cx).placeholder(text::INITIAL_PROMPT_PLACEHOLDER.get())
        });
        name.update(cx, |input, cx| input.focus(window, cx));
        let project_picker = cx.new(|cx| {
            SelectState::new(SearchableVec::new(Vec::new()), None, window, cx).searchable(true)
        });
        let host_picker = cx.new(|cx| {
            SelectState::new(SearchableVec::new(Vec::new()), None, window, cx).searchable(true)
        });
        let subscriptions = [
            cx.subscribe(&base, |d, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    d.prepare(cx);
                }
            }),
            cx.subscribe_in(
                &project_picker,
                window,
                |d, _, event: &SelectEvent<SearchableVec<PickItem<String>>>, window, cx| {
                    let SelectEvent::Confirm(Some(label)) = event else {
                        return;
                    };
                    d.select_project(label, window, cx);
                },
            ),
            cx.subscribe_in(
                &host_picker,
                window,
                |d, _, event: &SelectEvent<SearchableVec<PickItem<HostId>>>, window, cx| {
                    let SelectEvent::Confirm(Some(host)) = event else {
                        return;
                    };
                    d.select_host(host.clone(), window, cx);
                },
            ),
            cx.subscribe_in(
                &branch_query,
                window,
                |d, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        d.highlight = Some(0);
                        d.branch_scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {
                        d.handle_branch_key("enter", window, cx);
                    }
                    _ => {}
                },
            ),
        ];
        let mut dialog = Self {
            language: crate::i18n::language(),
            host: HostId::Local,
            host_options: Vec::new(),
            repo,
            parent,
            name,
            base,
            initial_prompt,
            branch_query,
            branch_picker_open: false,
            advanced_open: false,
            branch_generation: 0,
            branch_task: None,
            branch_error: None,
            branch_scroll: gpui::UniformListScrollHandle::new(),
            selected_agents: Vec::new(),
            focus: cx.focus_handle(),
            pending_fan_out: 0,
            last_created_agent: None,
            last_created_prompt: None,
            phase: Phase::Edit(None),
            source,
            task: None,
            _enter: subscriptions,
            project_picker,
            host_picker,
            picker_sync: PickerSync::Dirty,
            preparation: None,
            preparation_task: None,
            preparation_ready: false,
            candidates: Vec::new(),
            highlight: None,
            hooks: krill_proto::orchestration::RepoHookSummary::default(),
            hooks_task: None,
            folder: None,
        };
        if !candidates.is_empty() {
            dialog.set_branch_candidates(candidates, cx);
        }
        dialog.refresh_repo_hooks(cx);
        dialog.prepare(cx);
        dialog
    }

    #[must_use]
    pub fn host(&self) -> &HostId {
        &self.host
    }

    #[must_use]
    pub fn repo(&self) -> &std::path::Path {
        &self.repo
    }

    pub fn set_host_options(
        &mut self,
        host: HostId,
        options: Vec<WorktreeHostOption>,
        cx: &mut Context<Self>,
    ) {
        self.host_options = options;
        let host_changed = self.host != host;
        self.host = host;
        if let Some(opt) = self
            .host_options
            .iter()
            .find(|opt| opt.host == self.host)
            .cloned()
        {
            self.source = opt.source;
            if !opt.repos.iter().any(|r| r.path == self.repo)
                && let Some(first) = opt.repos.first()
            {
                self.repo = first.path.clone();
            }
        }
        if host_changed {
            self.refresh_repo_hooks(cx);
            self.prepare(cx);
        }
        self.picker_sync = PickerSync::Dirty;
        cx.notify();
    }

    pub fn select_host(&mut self, host: HostId, window: &mut Window, cx: &mut Context<Self>) {
        if self.host == host {
            return;
        }
        let Some(opt) = self.host_options.iter().find(|o| o.host == host).cloned() else {
            return;
        };
        let label = self.repo_label();
        self.host = host;
        self.source = opt.source;
        self.parent = None;
        let next_repo = opt
            .repos
            .iter()
            .find(|r| r.path == self.repo)
            .or_else(|| opt.repos.iter().find(|r| r.label == label))
            .or_else(|| opt.repos.first())
            .cloned();
        if let Some(repo_opt) = next_repo {
            self.repo.clone_from(&repo_opt.path);
            self.apply_repo_branch_options(repo_opt.branch.as_deref(), window, cx);
        } else {
            self.apply_repo_branch_options(None, window, cx);
        }
        self.refresh_repo_hooks(cx);
        self.prepare(cx);
        self.picker_sync = PickerSync::Dirty;
        cx.notify();
    }

    /// 프로젝트 콤보박스 선택: 현재 호스트에 같은 이름의 프로젝트가 있으면 그대로, 없으면 그 프로젝트가 있는 첫 호스트로 옮긴다.
    pub fn select_project(&mut self, label: &str, window: &mut Window, cx: &mut Context<Self>) {
        let on_host = |opt: &WorktreeHostOption| {
            opt.repos
                .iter()
                .find(|r| r.label == label)
                .map(|r| (opt.host.clone(), r.path.clone()))
        };
        let target = self
            .host_options
            .iter()
            .find(|o| o.host == self.host)
            .and_then(on_host)
            .or_else(|| self.host_options.iter().find_map(on_host));
        let Some((host, path)) = target else {
            return;
        };
        self.select_host(host, window, cx);
        self.select_repo(path, window, cx);
    }

    /// 현재 프로젝트 이름(호스트 옵션의 라벨, 없으면 폴더 이름).
    fn repo_label(&self) -> String {
        self.host_options
            .iter()
            .find(|o| o.host == self.host)
            .and_then(|o| o.repos.iter().find(|r| r.path == self.repo))
            .map_or_else(
                || {
                    self.repo
                        .file_name()
                        .unwrap_or(self.repo.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                },
                |r| r.label.clone(),
            )
    }

    /// 프로젝트 목록은 모든 호스트의 프로젝트를 이름으로 묶고(보조 설명에 호스트), 실행 위치는 고른 프로젝트가 있는 호스트만 보인다.
    // ponytail: 호스트 간 같은 프로젝트를 이름으로 묶는다. 한 호스트에 같은 이름이 둘이면 첫 항목만 고를 수 있다.
    fn sync_pickers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker_sync = PickerSync::Current;
        let multi_host = self.host_options.len() > 1;
        let mut projects: Vec<PickItem<String>> = Vec::new();
        for opt in &self.host_options {
            for repo in &opt.repos {
                if let Some(existing) = projects.iter_mut().find(|p| p.value == repo.label) {
                    if multi_host && !existing.detail.split(", ").any(|h| h == opt.label) {
                        existing.detail = format!("{}, {}", existing.detail, opt.label).into();
                    }
                    continue;
                }
                projects.push(PickItem {
                    value: repo.label.clone(),
                    title: repo.label.clone().into(),
                    detail: if multi_host {
                        opt.label.clone().into()
                    } else {
                        repo.path.display().to_string().into()
                    },
                });
            }
        }
        let label = self.repo_label();
        let hosts: Vec<PickItem<HostId>> = self
            .host_options
            .iter()
            .filter(|o| o.repos.iter().any(|r| r.label == label))
            .map(|o| PickItem {
                value: o.host.clone(),
                title: o.label.clone().into(),
                detail: o
                    .repos
                    .iter()
                    .find(|r| r.label == label)
                    .map(|r| r.path.display().to_string().into())
                    .unwrap_or_default(),
            })
            .collect();
        let host = self.host.clone();
        self.project_picker.update(cx, |picker, cx| {
            picker.set_items(SearchableVec::new(projects), window, cx);
            picker.set_selected_value(&label, window, cx);
        });
        self.host_picker.update(cx, |picker, cx| {
            picker.set_items(SearchableVec::new(hosts), window, cx);
            picker.set_selected_value(&host, window, cx);
        });
    }

    pub fn select_repo(&mut self, repo: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.repo == repo {
            return;
        }
        let repo_branch = self
            .host_options
            .iter()
            .find(|o| o.host == self.host)
            .and_then(|o| o.repos.iter().find(|r| r.path == repo))
            .and_then(|r| r.branch.clone());
        self.repo = repo;
        self.parent = None;
        self.apply_repo_branch_options(repo_branch.as_deref(), window, cx);
        self.refresh_repo_hooks(cx);
        self.prepare(cx);
        self.picker_sync = PickerSync::Dirty;
        cx.notify();
    }

    fn refresh_repo_hooks(&mut self, cx: &mut Context<Self>) {
        self.hooks = krill_proto::orchestration::RepoHookSummary::default();
        if !self.host.is_local() {
            self.hooks = krill_proto::orchestration::RepoHookSummary::default();
            self.hooks_task = None;
            return;
        }
        let repo = self.repo.clone();
        let state_dir = crate::settings::state_dir(cx);
        self.hooks_task = Some(cx.spawn(async move |this, cx| {
            let check_repo = repo.clone();
            let hooks = cx
                .background_executor()
                .spawn(async move {
                    krill_proto::orchestration::inspect_repo_hooks(&repo, state_dir.as_deref())
                })
                .await;
            this.update(cx, |dialog, cx| {
                if dialog.host.is_local() && dialog.repo == check_repo {
                    dialog.hooks = hooks;
                }
                dialog.hooks_task = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// 백그라운드 생성 중이거나 생성 완료된 상태인지(`true`이면 모달을 가리고 사이드바 대기 행으로 표시한다).
    #[must_use]
    pub fn is_background(&self) -> bool {
        matches!(self.phase, Phase::Creating | Phase::Done(_))
    }

    #[must_use]
    pub fn is_untrusted(&self) -> bool {
        matches!(self.phase, Phase::Untrusted { .. })
    }

    pub fn focus_on_reopen(&self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.phase {
            Phase::Edit(_) => {
                self.name.update(cx, |input, cx| input.focus(window, cx));
            }
            Phase::Untrusted { .. } => {
                self.focus.focus(window, cx);
            }
            Phase::Creating | Phase::Done(_) => {}
        }
    }

    #[must_use]
    pub fn parent(&self) -> Option<&std::path::Path> {
        self.parent.as_deref()
    }

    #[must_use]
    pub fn selected_agents(&self) -> &[String] {
        &self.selected_agents
    }

    pub fn toggle_agent(&mut self, agent_id: &str, cx: &mut Context<Self>) {
        let trimmed = agent_id.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Some(pos) = self.selected_agents.iter().position(|a| a == trimmed) {
            self.selected_agents.remove(pos);
        } else {
            self.selected_agents.push(trimmed.to_owned());
        }
        cx.notify();
    }

    #[must_use]
    pub fn initial_prompt_value(&self, cx: &App) -> Option<String> {
        let trimmed = self.initial_prompt.read(cx).value().trim().to_owned();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    pub fn set_initial_prompt(
        &mut self,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.initial_prompt.update(cx, |input, cx| {
            input.set_value(prompt, window, cx);
        });
        cx.notify();
    }

    #[must_use]
    pub fn last_created_agent(&self) -> Option<&str> {
        self.last_created_agent.as_deref()
    }

    #[must_use]
    pub fn last_created_prompt(&self) -> Option<&str> {
        self.last_created_prompt.as_deref()
    }

    #[must_use]
    pub fn has_pending_fan_out(&self) -> bool {
        self.pending_fan_out > 0
    }

    /// 로컬 레포가 git 저장소가 아닌지 판정해 캐시한다. 데몬의 폴더 작업공간 판정(`.git` 조상 없음)과 같다.
    fn is_folder(&mut self) -> bool {
        if self.host.is_local()
            && self
                .folder
                .as_ref()
                .is_none_or(|(path, _)| *path != self.repo)
        {
            let folder =
                self.repo.is_dir() && !self.repo.ancestors().any(|a| a.join(".git").exists());
            self.folder = Some((self.repo.clone(), folder));
        }
        self.cached_folder()
    }

    fn cached_folder(&self) -> bool {
        self.host.is_local()
            && self
                .folder
                .as_ref()
                .is_some_and(|(path, folder)| *folder && *path == self.repo)
    }

    fn cancel_preparation(&mut self, cx: &mut Context<Self>) {
        self.preparation = None;
        self.preparation_task = None;
        self.preparation_ready = false;
        cx.notify();
    }

    fn prepare(&mut self, cx: &mut Context<Self>) {
        self.cancel_preparation(cx);
        if !matches!(self.phase, Phase::Edit(_)) {
            return;
        }
        let mut token = [0; 8];
        if SystemRandom::new().fill(&mut token).is_err() {
            return;
        }
        let base = self.base.read(cx).value().trim().to_owned();
        let req = PrepareWorktree {
            token: u64::from_le_bytes(token),
            repo: self.repo.as_os_str().as_bytes().to_vec(),
            base: (!base.is_empty()).then_some(base),
            cancel: false,
        };
        let source = self.source.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.preparation = Some(Preparation {
            req: req.clone(),
            source: source.clone(),
            cancelled: cancelled.clone(),
            executor: cx.background_executor().clone(),
        });
        self.preparation_task = Some(cx.spawn(async move |this, cx| {
            // Base edits debounce; opening the dialog is the only automatic preparation trigger.
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            loop {
                let (source, req, cancelled) = (source.clone(), req.clone(), cancelled.clone());
                let reply = cx
                    .background_executor()
                    .spawn(async move {
                        if cancelled.load(Ordering::Acquire) {
                            return None;
                        }
                        let result = source.prepare_worktree(req.clone());
                        if cancelled.load(Ordering::Acquire) {
                            let mut cancel = req;
                            cancel.cancel = true;
                            source.prepare_worktree(cancel).ok();
                            return None;
                        }
                        Some(result)
                    })
                    .await;
                match reply {
                    Some(Ok(Reply::WorktreePreparation(
                        WorktreePreparation::Preparing | WorktreePreparation::Busy,
                    ))) => {}
                    Some(Ok(Reply::WorktreePreparation(WorktreePreparation::Ready))) => {
                        this.update(cx, |d, cx| {
                            d.preparation_ready = true;
                            cx.notify();
                        })
                        .ok();
                    }
                    _ => {
                        this.update(cx, Dialog::cancel_preparation).ok();
                        return;
                    }
                }
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            }
        }));
    }

    fn request(&self, setup: Setup, cx: &Context<Self>) -> Result<NewWorktree, &'static str> {
        let mut name: String = self
            .name
            .read(cx)
            .value()
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        if name.is_empty() {
            // 비어 있으면 충돌 가능성이 작은 자동 이름으로 생성한다. 서버가 최종 중복 검사를 한다.
            let suffix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            name = format!("workspace-{suffix:x}");
        }
        if name.len() > 255 {
            return Err(text::NAME_TOO_LONG.get());
        }
        let base = self.base.read(cx).value().trim().to_owned();
        Ok(NewWorktree {
            repo: self.repo.as_os_str().as_bytes().to_vec(),
            name,
            // 폴더 작업공간은 기준 ref가 없다(데몬이 거부한다).
            base: (!base.is_empty() && !self.cached_folder()).then_some(base),
            setup,
        })
    }

    /// OWE-924: 멀티 에이전트 Fan-Out 요청 목록을 만든다.
    /// 에이전트를 2개 이상 고르면 `<name>-<agent>` 이름으로 N개 워크트리를 만들고,
    /// 1개만 고르면 원래 `<name>`에 그 에이전트를 지정한다.
    pub fn fan_out_requests(
        &self,
        setup: Setup,
        cx: &Context<Self>,
    ) -> Result<Vec<FanOutPlanItem>, &'static str> {
        let base_req = self.request(setup, cx)?;
        let prompt = self.initial_prompt_value(cx);
        if self.selected_agents.is_empty() {
            return Ok(vec![(base_req, None, prompt)]);
        }
        if let [only] = self.selected_agents.as_slice() {
            return Ok(vec![(base_req, Some(only.clone()), prompt)]);
        }
        let mut out = Vec::with_capacity(self.selected_agents.len());
        for agent in &self.selected_agents {
            let mut req = base_req.clone();
            req.name = format!("{}-{agent}", base_req.name);
            out.push((req, Some(agent.clone()), prompt.clone()));
        }
        Ok(out)
    }

    /// 입력값으로 요청을 보낸다. setup 승인·건너뛰기는 같은 입력으로 다시 보낸다(입력은 그동안 숨겨져 있다).
    fn submit(&mut self, setup: Setup, cx: &mut Context<Self>) {
        let requests = match self.fan_out_requests(setup, cx) {
            Ok(reqs) => reqs,
            Err(e) => {
                self.phase = Phase::Edit(Some(e.to_owned()));
                cx.notify();
                return;
            }
        };
        let names = requests
            .iter()
            .map(|(req, _, _)| req.name.clone())
            .collect();
        self.pending_fan_out = requests.len();
        self.phase = Phase::Creating;
        cx.emit(Event::Submitting {
            repo: self.repo.clone(),
            parent: self.parent.clone(),
            names,
        });
        cx.notify();
        let source = self.source.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            for (req, agent, prompt) in requests {
                let source = source.clone();
                let reply = bg.spawn(async move { source.new_worktree(req) }).await;
                let cont = this
                    .update(cx, |d, cx| d.finish_step(reply, agent, prompt, cx))
                    .unwrap_or(false);
                if !cont {
                    break;
                }
            }
        }));
    }

    fn finish_step(
        &mut self,
        reply: std::io::Result<Reply>,
        agent: Option<String>,
        prompt: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.pending_fan_out = self.pending_fan_out.saturating_sub(1);
        match reply {
            Ok(Reply::WorktreeCreated(w)) => {
                self.cancel_preparation(cx);
                self.last_created_agent = agent;
                self.last_created_prompt = prompt;
                self.phase = Phase::Done(w.clone());
                cx.emit(Event::Created(w));
                cx.notify();
                self.pending_fan_out > 0
            }
            Ok(Reply::SetupUntrusted { script, hash }) => {
                self.pending_fan_out = 0;
                self.phase = Phase::Untrusted { script, hash };
                cx.emit(Event::ModalReopened(None));
                cx.notify();
                false
            }
            Ok(Reply::WorktreeFailed(e)) => {
                self.pending_fan_out = 0;
                self.phase = Phase::Edit(Some(e.clone()));
                cx.emit(Event::ModalReopened(Some(e)));
                cx.notify();
                false
            }
            Ok(Reply::Failed(f)) => {
                let msg = f.to_string();
                self.pending_fan_out = 0;
                self.phase = Phase::Edit(Some(msg.clone()));
                cx.emit(Event::ModalReopened(Some(msg)));
                cx.notify();
                false
            }
            Ok(_) => {
                let msg = text::UNEXPECTED.get().to_owned();
                self.pending_fan_out = 0;
                self.phase = Phase::Edit(Some(msg.clone()));
                cx.emit(Event::ModalReopened(Some(msg)));
                cx.notify();
                false
            }
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                let msg = text::UNSUPPORTED.get().to_owned();
                self.pending_fan_out = 0;
                self.phase = Phase::Edit(Some(msg.clone()));
                cx.emit(Event::ModalReopened(Some(msg)));
                cx.notify();
                false
            }
            Err(e) => {
                let msg = e.to_string();
                self.pending_fan_out = 0;
                self.phase = Phase::Edit(Some(msg.clone()));
                cx.emit(Event::ModalReopened(Some(msg)));
                cx.notify();
                false
            }
        }
    }
}

mod render;

#[cfg(test)]
#[path = "worktree/tests.rs"]
mod tests;
