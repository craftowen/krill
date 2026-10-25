//! OWE-496 워크트리 생성 대화상자(MK-042 후속). 사이드바 레포 행의 "+"로 연다.
//! OWE-687 모양은 Orca `NewWorkspaceComposerModal`처럼 창 가운데 모달(라벨 붙은 입력칸, 오른쪽 아래 버튼).
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
    App, Context, Entity, EventEmitter, Focusable as _, Subscription, Task, Window, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _};
use krill_proto::control::{
    NewWorktree, PrepareWorktree, Reply, Setup, Worktree, WorktreePreparation,
};

use crate::sidebar::Source;

use crate::i18n::catalog::worktree as text;

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

pub struct Dialog {
    language: crate::i18n::Language,
    focus: gpui::FocusHandle,
    repo: PathBuf,
    parent: Option<PathBuf>,
    name: Entity<InputState>,
    base: Entity<InputState>,
    initial_prompt: Entity<InputState>,
    selected_agents: Vec<String>,
    pending_fan_out: usize,
    last_created_agent: Option<String>,
    last_created_prompt: Option<String>,
    phase: Phase,
    source: Arc<dyn Source>,
    task: Option<Task<()>>,
    _enter: [Subscription; 1],
    preparation: Option<Preparation>,
    preparation_task: Option<Task<()>>,
    preparation_ready: bool,
    /// OWE-853: 기준 ref 후보(호출 워크트리 브랜치·데몬 폴백·git refs). 입력값으로 필터해 드롭다운에 보인다.
    candidates: Vec<String>,
    /// OWE-853: 드롭다운 키보드 하이라이트(필터 목록 내 인덱스). `None`이면 Enter가 생성 제출이다.
    highlight: Option<usize>,
    hooks: krill_proto::orchestration::RepoHookSummary,
    hooks_task: Option<Task<()>>,
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

/// OWE-853: 호출 워크트리·레포 브랜치를 앞에 둔 기준 ref 후보.
/// 데몬 폴백 순서(`origin/HEAD` → `origin/main` → … → `HEAD`)와 로컬 git refs를 뒤에 붙인다.
pub fn branch_suggestions(
    parent_branch: Option<&str>,
    repo_branch: Option<&str>,
    git_refs: &[String],
) -> (Option<String>, Vec<String>) {
    let default = parent_branch
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            repo_branch
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        });
    let mut out: Vec<String> = Vec::new();
    let push = |out: &mut Vec<String>, s: &str| {
        let trimmed = s.trim();
        if !trimmed.is_empty()
            && trimmed.len() <= 256
            && !trimmed.chars().any(char::is_control)
            && !out.iter().any(|e| e == trimmed)
        {
            out.push(trimmed.to_owned());
        }
    };
    if let Some(ref d) = default {
        push(&mut out, d);
    }
    if let Some(repo) = repo_branch {
        push(&mut out, repo);
    }
    for fallback in [
        "origin/HEAD",
        "origin/main",
        "origin/master",
        "main",
        "master",
        "HEAD",
    ] {
        push(&mut out, fallback);
    }
    for git_ref in git_refs {
        push(&mut out, git_ref);
        if out.len() >= 100 {
            break;
        }
    }
    (default, out)
}

/// OWE-853: 로컬 `git for-each-ref`로 브랜치·태그를 읽는다(새 IPC 없이).
/// 실패·타임아웃 시 빈 목록으로 조용히 넘어간다. 호출자는 백그라운드에서 부른다.
pub fn list_git_refs(repo: &Path) -> Vec<String> {
    let mut command = crate::platform::background_command("git");
    command
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "for-each-ref",
            "--count=50",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ]);
    let output =
        crate::pull_request::execute(&mut command, Duration::from_millis(1500)).unwrap_or_default();
    let text = String::from_utf8_lossy(&output);
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.len() > 256
            || trimmed.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            continue;
        }
        if !out.iter().any(|e: &String| e == trimmed) {
            out.push(trimmed.to_owned());
        }
        if out.len() >= 50 {
            break;
        }
    }
    out
}

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
        let initial_prompt = cx.new(|cx| {
            InputState::new(window, cx).placeholder(text::INITIAL_PROMPT_PLACEHOLDER.get())
        });
        name.update(cx, |input, cx| input.focus(window, cx));
        let subscriptions = [cx.subscribe(&base, |d, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                d.prepare(cx);
            }
        })];
        let mut dialog = Self {
            language: crate::i18n::language(),
            repo,
            parent,
            name,
            base,
            initial_prompt,
            selected_agents: Vec::new(),
            focus: cx.focus_handle(),
            pending_fan_out: 0,
            last_created_agent: None,
            last_created_prompt: None,
            phase: Phase::Edit(None),
            source,
            task: None,
            _enter: subscriptions,
            preparation: None,
            preparation_task: None,
            preparation_ready: false,
            candidates: Vec::new(),
            highlight: None,
            hooks: krill_proto::orchestration::RepoHookSummary::default(),
            hooks_task: None,
        };
        if !candidates.is_empty() {
            dialog.set_branch_candidates(candidates, cx);
        }
        let repo = dialog.repo.clone();
        let state_dir = crate::settings::state_dir(cx);
        dialog.hooks_task = Some(cx.spawn(async move |this, cx| {
            let hooks = cx
                .background_executor()
                .spawn(async move {
                    krill_proto::orchestration::inspect_repo_hooks(&repo, state_dir.as_deref())
                })
                .await;
            this.update(cx, |dialog, cx| {
                dialog.hooks = hooks;
                dialog.hooks_task = None;
                cx.notify();
            })
            .ok();
        }));
        dialog.prepare(cx);
        dialog
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

    /// OWE-853: 기준 ref 후보를 교체한다(중복·빈값 제거, 최대 100개). 입력 필터는 렌더 시 계산한다.
    pub fn set_branch_candidates(&mut self, candidates: Vec<String>, cx: &mut Context<Self>) {
        let mut seen = std::collections::BTreeSet::new();
        let mut cleaned = Vec::new();
        for raw in candidates {
            let trimmed = raw.trim().to_owned();
            if trimmed.is_empty()
                || trimmed.len() > 256
                || trimmed.chars().any(char::is_control)
                || !seen.insert(trimmed.clone())
            {
                continue;
            }
            cleaned.push(trimmed);
            if cleaned.len() >= 100 {
                break;
            }
        }
        self.candidates = cleaned;
        self.highlight = None;
        cx.notify();
    }

    /// OWE-853: 호출한 워크트리의 현재 브랜치를 기준 ref 기본값으로 넣는다(입력済み면 덮지 않는다).
    pub fn set_base_default(
        &mut self,
        base: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(default) = base.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) else {
            return;
        };
        let current = self.base.read(cx).value().trim().to_owned();
        if current.is_empty() {
            self.base.update(cx, |input, cx| {
                input.set_value(&default, window, cx);
            });
            self.highlight = None;
            self.prepare(cx);
            cx.notify();
        }
    }

    /// OWE-853: 현재 입력값으로 필터한 후보(최대 8개). 빈 입력이면 상위 후보를 그대로 보인다.
    fn filtered_bases(&self, cx: &App) -> Vec<String> {
        let query = self.base.read(cx).value().trim().to_lowercase();
        let mut out = Vec::new();
        for candidate in &self.candidates {
            if query.is_empty() || candidate.to_lowercase().contains(query.as_str()) {
                out.push(candidate.clone());
                if out.len() >= 8 {
                    break;
                }
            }
        }
        out
    }

    fn base_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.base.read(cx).focus_handle(cx).is_focused(window)
    }

    /// OWE-853: 드롭다운 키보드 처리. 하이라이트를 소비하면 `true`(전파 중단).
    fn on_key(
        &mut self,
        e: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(self.phase, Phase::Edit(_)) {
            return false;
        }
        if e.keystroke.key == "enter"
            && (e.keystroke.modifiers.platform
                || (!cfg!(target_os = "macos") && e.keystroke.modifiers.control))
        {
            self.submit(Setup::Ask, cx);
            return true;
        }
        if e.keystroke.key == "enter" && self.name.read(cx).focus_handle(cx).is_focused(window) {
            self.base.update(cx, |input, cx| input.focus(window, cx));
            return true;
        }
        match e.keystroke.key.as_str() {
            "escape" => {
                if self.highlight.is_some() {
                    self.highlight = None;
                    cx.notify();
                    return true;
                }
                false
            }
            "down" if self.base_is_focused(window, cx) => {
                let filtered = self.filtered_bases(cx);
                if filtered.is_empty() {
                    return false;
                }
                let next = self
                    .highlight
                    .map_or(0, |i| if i + 1 >= filtered.len() { 0 } else { i + 1 });
                self.highlight = Some(next);
                cx.notify();
                true
            }
            "up" if self.base_is_focused(window, cx) => {
                let filtered = self.filtered_bases(cx);
                if filtered.is_empty() {
                    return false;
                }
                let next = self
                    .highlight
                    .map_or(filtered.len().saturating_sub(1), |i| {
                        if i == 0 { filtered.len() - 1 } else { i - 1 }
                    });
                self.highlight = Some(next);
                cx.notify();
                true
            }
            "enter" if self.base_is_focused(window, cx) => {
                if let Some(index) = self.highlight {
                    let filtered = self.filtered_bases(cx);
                    if let Some(picked) = filtered.get(index).cloned() {
                        self.base.update(cx, |input, cx| {
                            input.set_value(&picked, window, cx);
                        });
                        self.highlight = None;
                        self.prepare(cx);
                        cx.notify();
                        return true;
                    }
                    self.highlight = None;
                    cx.notify();
                }
                self.initial_prompt
                    .update(cx, |input, cx| input.focus(window, cx));
                true
            }
            _ => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn base_value(&self, cx: &App) -> String {
        self.base.read(cx).value().to_string()
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
            base: (!base.is_empty()).then_some(base),
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

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != crate::i18n::language() {
            self.language = crate::i18n::language();
            self.name.update(cx, |input, cx| {
                input.set_placeholder(text::NAME.get(), window, cx);
            });
            self.base.update(cx, |input, cx| {
                input.set_placeholder(text::BASE.get(), window, cx);
            });
            self.initial_prompt.update(cx, |input, cx| {
                input.set_placeholder(text::INITIAL_PROMPT_PLACEHOLDER.get(), window, cx);
            });
        }
        let p = crate::ui::palette(cx);
        let (warning, muted) = (p.warning, p.muted_foreground);
        let close = Button::new("worktree-close")
            .disabled(matches!(self.phase, Phase::Creating))
            .ghost()
            .label(text::CLOSE.get())
            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close)));
        let footer = || div().flex_none().flex().flex_wrap().justify_end().gap_2();
        let repo_hooks = &self.hooks;
        let body = div()
            .id("worktree-dialog-body")
            .debug_selector(|| "worktree-dialog-body".into())
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4();
        let filtered = match &self.phase {
            Phase::Edit(_) => self.filtered_bases(cx),
            _ => Vec::new(),
        };
        let highlight = self.highlight;
        let base_candidates = filtered.clone();
        let selected_agents = self.selected_agents.clone();
        let body = match &self.phase {
            Phase::Edit(error) => body
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(crate::ui::field_label(text::NAME_LABEL.get(), None, p))
                        .child(Input::new(&self.name)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(crate::ui::field_label(
                            text::BASE_LABEL.get(),
                            Some(text::OPTIONAL.get()),
                            p,
                        ))
                        .child(Input::new(&self.base))
                        .when(!base_candidates.is_empty(), |d| {
                            d.child(
                                div()
                                    .id("worktree-base-options")
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .mt_1()
                                    .rounded(crate::ui::RADIUS_SM)
                                    .border_1()
                                    .border_color(p.border)
                                    .bg(p.background)
                                    .p_1()
                                    .children(base_candidates.iter().enumerate().map(
                                        |(index, candidate)| {
                                            let label = candidate.clone();
                                            let picked = candidate.clone();
                                            let is_active = highlight == Some(index);
                                            div()
                                                .id(("worktree-base-option", index))
                                                .debug_selector(move || {
                                                    format!("worktree-base-option-{label}")
                                                })
                                                .flex()
                                                .items_center()
                                                .px_2()
                                                .py_1()
                                                .rounded(crate::ui::RADIUS_XS)
                                                .font_family(crate::ui::FONT_MONO)
                                                .text_size(crate::ui::TEXT_SM)
                                                .text_color(if is_active {
                                                    p.accent_foreground
                                                } else {
                                                    p.muted_foreground
                                                })
                                                .when(is_active, |row| row.bg(p.accent))
                                                .cursor_pointer()
                                                .child(picked.clone())
                                                .on_click(cx.listener(move |d, _, window, cx| {
                                                    d.base.update(cx, |input, cx| {
                                                        input.set_value(&picked, window, cx);
                                                    });
                                                    d.highlight = None;
                                                    d.prepare(cx);
                                                    cx.notify();
                                                }))
                                        },
                                    )),
                            )
                        })
                        .when(self.preparation.is_some(), |d| {
                            d.child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .items_center()
                                    .text_size(crate::ui::TEXT_2XS)
                                    .text_color(muted)
                                    .child(if self.preparation_ready {
                                        text::PREPARED.get()
                                    } else {
                                        text::PREPARING.get()
                                    })
                                    .child(
                                        Button::new("cancel-preparation")
                                            .ghost()
                                            .xsmall()
                                            .label(text::CANCEL_PREPARATION.get())
                                            .on_click(
                                                cx.listener(|d, _, _, cx| d.cancel_preparation(cx)),
                                            ),
                                    ),
                            )
                        }),
                )
                .child(
                    div()
                        .id("worktree-fanout-agents")
                        .debug_selector(|| "worktree-fanout-agents".into())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(crate::ui::field_label(
                            text::AGENTS_LABEL.get(),
                            Some(text::OPTIONAL.get()),
                            p,
                        ))
                        .child(
                            div().flex().flex_wrap().gap_1().children(
                                FAN_OUT_AGENTS.iter().enumerate().map(
                                    |(idx, &(agent_id, label))| {
                                        let selected =
                                            selected_agents.iter().any(|a| a == agent_id);
                                        let btn = Button::new(("worktree-agent", idx))
                                            .xsmall()
                                            .icon(crate::ui::icons::agent_menu_icon(agent_id))
                                            .label(label)
                                            .selected(selected)
                                            .toggled(selected);
                                        let btn = btn.ghost();
                                        btn.on_click(cx.listener(move |d, _, _, cx| {
                                            d.toggle_agent(agent_id, cx);
                                        }))
                                    },
                                ),
                            ),
                        ),
                )
                .child(
                    div()
                        .id("worktree-initial-prompt")
                        .debug_selector(|| "worktree-initial-prompt".into())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(crate::ui::field_label(
                            text::INITIAL_PROMPT_LABEL.get(),
                            Some(text::OPTIONAL.get()),
                            p,
                        ))
                        .child(Input::new(&self.initial_prompt)),
                )
                .when(!repo_hooks.effective_symlinks.is_empty(), |d| {
                    let paths = repo_hooks.effective_symlinks.join(", ");
                    d.child(
                        div()
                            .id("worktree-symlinks-preview")
                            .debug_selector(|| "worktree-symlinks-preview".into())
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_SM)
                            .text_color(muted)
                            .child(text::SYMLINKS_PREVIEW.format(&[("paths", &paths)])),
                    )
                })
                .when_some(repo_hooks.effective_setup.clone(), |d, script| {
                    d.child(
                        div()
                            .id("worktree-setup-preview")
                            .debug_selector(|| "worktree-setup-preview".into())
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_SM)
                            .text_color(muted)
                            .child(text::SETUP_PREVIEW.format(&[("script", &visible(&script))])),
                    )
                })
                .when_some(repo_hooks.effective_archive.clone(), |d, script| {
                    d.child(
                        div()
                            .id("worktree-archive-preview")
                            .debug_selector(|| "worktree-archive-preview".into())
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_SM)
                            .text_color(muted)
                            .child(text::ARCHIVE_PREVIEW.format(&[("script", &visible(&script))])),
                    )
                })
                .children(
                    error
                        .clone()
                        .map(|e| crate::ui::error_banner(p, crate::i18n::cached_notice(&e))),
                ),
            Phase::Creating => body.child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(muted)
                    .child(text::CREATING.get()),
            ),
            Phase::Untrusted { script, hash } => body
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(warning)
                        .child(text::UNTRUSTED.get()),
                )
                .child(
                    div()
                        .id("setup-script")
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded(crate::ui::RADIUS_XS)
                        .bg(p.muted)
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_SM)
                        .child(visible(script)),
                )
                .child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_2XS)
                        .text_color(muted)
                        .child(
                            crate::i18n::catalog::general::HASH
                                .format(&[("hash", &hex_string(hash))]),
                        ),
                ),
            Phase::Done(w) => body
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.success)
                        .child(text::CREATED.get()),
                )
                .child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(muted)
                        .child(
                            PathBuf::from(std::ffi::OsStr::from_bytes(&w.path))
                                .display()
                                .to_string(),
                        ),
                )
                .child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_SM)
                        .child(format!("{} ← {}", w.branch, w.base)),
                )
                .children(w.setup.map(|id| format!("{} {}", text::SETUP.get(), id.0)))
                .children(w.warnings.iter().map(|e| {
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(warning)
                        .child(e.clone())
                })),
        };
        let actions = match &self.phase {
            Phase::Edit(_) => footer().child(close).child(
                Button::new("worktree-create")
                    .debug_selector(|| "worktree-create".into())
                    .primary()
                    .label(text::CREATE.get())
                    .on_click(cx.listener(|d, _, _, cx| d.submit(Setup::Ask, cx))),
            ),
            Phase::Untrusted { hash, .. } => {
                let approve = hash.clone();
                footer()
                    .child(close)
                    .child(
                        Button::new("setup-skip")
                            .ghost()
                            .label(text::SKIP.get())
                            .on_click(cx.listener(|d, _, _, cx| d.submit(Setup::Skip, cx))),
                    )
                    .child(
                        Button::new("setup-approve")
                            .danger()
                            .label(text::APPROVE.get())
                            .on_click(cx.listener(move |d, _, _, cx| {
                                d.submit(Setup::Approve(approve.clone()), cx);
                            })),
                    )
            }
            Phase::Creating | Phase::Done(_) => footer().child(close),
        };
        let card = div()
            .id("worktree-dialog")
            .debug_selector(|| "worktree-dialog".into())
            .occlude()
            .w(crate::ui::DIALOG_WIDTH)
            .max_w((window.viewport_size().width - crate::ui::SPACE_4 * 2.).max(px(0.)))
            .max_h((window.viewport_size().height - crate::ui::SPACE_4 * 2.).max(px(0.)))
            .role(gpui::Role::Dialog)
            .aria_label(text::TITLE.get())
            .p_6()
            .rounded(crate::ui::RADIUS_LG)
            .bg(p.popover)
            .font_family(crate::ui::FONT_UI)
            .text_size(crate::ui::TEXT_MD)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .capture_key_down(cx.listener(|d, e: &gpui::KeyDownEvent, window, cx| {
                if d.on_key(e, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    if !matches!(d.phase, Phase::Creating) {
                        cx.emit(Event::Close);
                    }
                }
            }))
            .on_mouse_down_out(cx.listener(|d, _, _, cx| {
                if !matches!(d.phase, Phase::Creating) {
                    cx.emit(Event::Close);
                }
            }))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text::TITLE.get()),
                    )
                    .child(
                        div().text_size(crate::ui::TEXT_XS).text_color(muted).child(
                            self.repo
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned(),
                        ),
                    )
                    .when_some(self.parent.as_ref(), |d, parent| {
                        let parent_name = parent
                            .file_name()
                            .unwrap_or(parent.as_os_str())
                            .to_string_lossy()
                            .into_owned();
                        d.child(
                            div()
                                .id("worktree-parent")
                                .debug_selector(|| "worktree-parent".into())
                                .text_size(crate::ui::TEXT_2XS)
                                .text_color(muted)
                                .child(format!("{}: {parent_name}", text::PARENT_LABEL.get())),
                        )
                    }),
            )
            .child(body)
            .child(actions);
        crate::ui::modal_backdrop(
            window.viewport_size(),
            crate::ui::modal_focus_scope(card, &self.focus),
        )
    }
}

#[cfg(test)]
#[path = "worktree/tests.rs"]
mod tests;
