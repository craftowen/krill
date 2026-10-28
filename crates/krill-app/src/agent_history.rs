//! MK-082·MK-181: 요청 시 멀티 에이전트 대화 기록을 스캔·전문 검색하고 선택한 대화를 새 탭에서 재개한다.
//! 열기·새로고침·검색·필터 전환 때만 일회성 백그라운드 IO를 실행하며, 감시·폴링·상주 프로세스는 없다.
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Context, Entity, EventEmitter, Subscription, Task, Window, div, prelude::*};
use gpui_component::button::ButtonVariants as _;
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use krill_proto::SessionId;
use krill_proto::agent_history::{
    Action, Entry, FEAT_AGENT_HISTORY, FEAT_AI_VAULT, FEAT_CHAT_POLL, Response, SearchQuery,
    SearchSnapshot, Snapshot, VaultAgent, VaultHit, VaultScope, VaultTarget,
};
use krill_proto::control::{FEAT_CONTROL, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;

use crate::hosts::HostTarget;
use crate::i18n::catalog::agent_history as text;

pub type Call = Arc<dyn Fn(&Request) -> io::Result<Reply> + Send + Sync>;

fn daemon_call() -> Call {
    daemon_call_for(None)
}

pub fn daemon_call_for(target: Option<HostTarget>) -> Call {
    #[cfg(test)]
    if target.is_none() {
        return Arc::new(|_| {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "daemon disabled in unit tests",
            ))
        });
    }
    Arc::new(move |req| {
        let hello = daemon::client_hello(ClientRole::App, "krill-app-history");
        let target = match &target {
            Some(t) => t.clone(),
            None => HostTarget::local_dev()?,
        };
        let conn = target.connect(&hello).map_err(io::Error::other)?;
        let mut sock = conn.stream;
        sock.set_read_timeout(Some(Duration::from_secs(10)))?;
        sock.set_write_timeout(Some(Duration::from_secs(2)))?;
        exchange(conn.negotiated.features, &mut sock, req)
    })
}

fn exchange(features: u64, sock: &mut (impl Read + Write), req: &Request) -> io::Result<Reply> {
    let vault = matches!(
        req,
        Request::AgentHistory(Action::Search(_) | Action::ResumeVault(_))
    );
    let chat_poll = matches!(req, Request::AgentHistory(Action::ChatPoll { .. }));
    let need = if vault {
        FEAT_CONTROL | FEAT_AGENT_HISTORY | FEAT_AI_VAULT
    } else if chat_poll {
        FEAT_CONTROL | FEAT_AGENT_HISTORY | FEAT_CHAT_POLL
    } else {
        FEAT_CONTROL | FEAT_AGENT_HISTORY
    };
    if features & need != need {
        return Err(io::ErrorKind::Unsupported.into());
    }
    sock.write_all(&req.encode()?)?;
    let (kind, body) = ipc::read_frame(sock)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    Ok(Reply::decode(kind, &body)?)
}

/// 검색어 토큰과 대소문자 무관하게 일치하는 구간을 `(문자열, 하이라이트 여부)`로 나눈다.
#[must_use]
pub fn highlight_spans(text: &str, query: &str) -> Vec<(String, bool)> {
    let terms: Vec<Vec<char>> = query
        .split_whitespace()
        .filter(|tok| {
            !tok.starts_with("agent:") && !tok.starts_with("path:") && !tok.starts_with("repo:")
        })
        .map(|tok| {
            tok.chars()
                .flat_map(char::to_lowercase)
                .collect::<Vec<char>>()
        })
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() || text.is_empty() {
        return vec![(text.to_owned(), false)];
    }
    let orig_chars: Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = orig_chars
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let mut matched = vec![false; orig_chars.len()];
    for term in &terms {
        let tlen = term.len();
        if tlen == 0 || tlen > lower_chars.len() {
            continue;
        }
        for start in 0..=lower_chars.len().saturating_sub(tlen) {
            if lower_chars.get(start..start + tlen) == Some(term.as_slice()) {
                for flag in matched.iter_mut().skip(start).take(tlen) {
                    *flag = true;
                }
            }
        }
    }
    let mut spans: Vec<(String, bool)> = Vec::new();
    for (ch, is_hit) in orig_chars.into_iter().zip(matched) {
        if let Some(last) = spans.last_mut()
            && last.1 == is_hit
        {
            last.0.push(ch);
        } else {
            spans.push((ch.to_string(), is_hit));
        }
    }
    spans
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

fn path_matches_scope(cwd: &str, filter_paths: &[String]) -> bool {
    if filter_paths.is_empty() {
        return true;
    }
    let norm = normalize_path(cwd);
    filter_paths.iter().any(|f| {
        let f_norm = normalize_path(f);
        if f_norm.is_empty() {
            return true;
        }
        norm == f_norm
            || norm
                .strip_prefix(f_norm.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// 성공한 새 세션만 셸에 알려 새 탭을 만든다. 실패하면 기존 탭은 그대로다.
#[derive(Debug, Clone, Copy)]
pub struct Open(pub SessionId);

/// 상태바에서 우측 세션 기록 탭을 열거나 접는다.
#[derive(Debug, Clone, Copy)]
pub struct Toggle;

#[expect(
    clippy::struct_excessive_bools,
    reason = "패널 열림·처리 중·재검색 대기·열림 포커스 예약의 독립 UI 상태"
)]
pub struct Panel {
    call: Call,
    generation: u64,
    open: bool,
    active: bool,
    busy: bool,
    pending_search: bool,
    data: Option<Snapshot>,
    vault: Option<SearchSnapshot>,
    query: String,
    scope: VaultScope,
    agent_filter: Option<VaultAgent>,
    workspace_path: Option<String>,
    project_paths: Vec<String>,
    input: Option<Entity<InputState>>,
    input_sub: Option<Subscription>,
    focus_on_open: bool,
    language: crate::i18n::Language,
    error: Option<String>,
    task: Option<Task<()>>,
    list_state: gpui::ListState,
    list_hits: Arc<Vec<VaultHit>>,
}

impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Panel")
            .field("open", &self.open)
            .field("busy", &self.busy)
            .field("pending_search", &self.pending_search)
            .field("scope", &self.scope)
            .field("agent_filter", &self.agent_filter)
            .finish_non_exhaustive()
    }
}
impl EventEmitter<Open> for Panel {}
impl EventEmitter<Toggle> for Panel {}
impl Default for Panel {
    fn default() -> Self {
        Self::new(daemon_call())
    }
}
impl Panel {
    #[must_use]
    pub fn new(call: Call) -> Self {
        Self {
            call,
            generation: 0,
            open: false,
            active: false,
            busy: false,
            pending_search: false,
            data: None,
            vault: None,
            query: String::new(),
            scope: VaultScope::All,
            agent_filter: None,
            workspace_path: None,
            project_paths: Vec::new(),
            input: None,
            input_sub: None,
            focus_on_open: false,
            language: crate::i18n::language(),
            error: None,
            task: None,
            list_state: gpui::ListState::new(0, gpui::ListAlignment::Top, gpui::px(100.)),
            list_hits: Arc::default(),
        }
    }

    pub fn set_call(&mut self, call: Call) {
        self.generation = self.generation.wrapping_add(1);
        self.task = None;
        self.busy = false;
        self.pending_search = false;
        self.data = None;
        self.vault = None;
        self.call = call;
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.close(cx);
            return;
        }
        self.open = true;
        // OWE-857: 다음 렌더에서 검색창에 자동 포커스한다.
        self.focus_on_open = true;
        self.scan(cx);
    }

    pub fn scan(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        if self.has_vault_filters() {
            self.search_vault(cx);
        } else {
            self.run(Action::Scan, cx);
        }
    }

    /// 패널을 접거나 다른 탭으로 전환하면 적재한 기록과 진행 중인 요청을 해제한다.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open || self.active {
            self.active = false;
            self.generation = self.generation.wrapping_add(1);
            self.open = false;
            self.busy = false;
            self.pending_search = false;
            self.focus_on_open = false;
            self.data = None;
            self.vault = None;
            self.task = None;
            cx.notify();
        }
    }

    /// 현재 워크스페이스·프로젝트 범위 경로를 설정한다.
    pub fn set_scope_paths(&mut self, workspace: Option<String>, project: Vec<String>) {
        if self.workspace_path != workspace || self.project_paths != project {
            self.generation = self.generation.wrapping_add(1);
            self.task = None;
            self.busy = false;
            self.data = None;
            self.vault = None;
        }
        self.workspace_path = workspace;
        self.project_paths = project;
    }

    /// Update an active consumer's paths and refresh its search, including a hidden palette consumer.
    pub fn update_scope_paths(
        &mut self,
        workspace: Option<String>,
        project: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.workspace_path == workspace && self.project_paths == project {
            return;
        }
        self.set_scope_paths(workspace, project);
        if self.active {
            self.scan(cx);
        }
        cx.notify();
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
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_scope(&mut self, scope: VaultScope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            if self.active {
                self.search_vault(cx);
            } else {
                cx.notify();
            }
        }
    }

    pub fn set_agent_filter(&mut self, agent: Option<VaultAgent>, cx: &mut Context<Self>) {
        if self.agent_filter != agent {
            self.agent_filter = agent;
            if self.active {
                self.search_vault(cx);
            } else {
                cx.notify();
            }
        }
    }

    pub fn set_query(&mut self, query: impl Into<String>, cx: &mut Context<Self>) {
        let next = query.into();
        if self.query != next {
            self.query = next;
            if self.active {
                self.search_vault(cx);
            } else {
                cx.notify();
            }
        }
    }

    pub fn search_vault(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        if self.busy {
            self.pending_search = true;
            cx.notify();
            return;
        }
        self.pending_search = false;
        let req = self.build_search_query();
        self.run(Action::Search(req), cx);
    }

    #[must_use]
    pub fn build_search_query(&self) -> SearchQuery {
        SearchQuery {
            query: self.query.clone(),
            agents: self.agent_filter.into_iter().collect(),
            paths: self.active_scope_paths(),
            limit: 50,
        }
    }

    fn active_scope_paths(&self) -> Vec<String> {
        match self.scope {
            VaultScope::All => Vec::new(),
            VaultScope::Project => {
                if self.project_paths.is_empty() {
                    self.workspace_path.iter().cloned().collect()
                } else {
                    self.project_paths.clone()
                }
            }
            VaultScope::Workspace => self.workspace_path.iter().cloned().collect(),
        }
    }

    fn has_vault_filters(&self) -> bool {
        !self.query.trim().is_empty()
            || self.agent_filter.is_some()
            || (self.scope != VaultScope::All && !self.active_scope_paths().is_empty())
    }

    #[must_use]
    pub fn has_loaded(&self) -> bool {
        self.vault.is_some() || self.data.is_some()
    }

    /// 스캔·검색으로 적재된 볼트 항목 목록(범위·에이전트·검색어 재필터링 전).
    #[must_use]
    pub fn loaded_hits(&self) -> Vec<VaultHit> {
        if let Some(vault) = &self.vault {
            vault.hits.clone()
        } else if let Some(data) = &self.data {
            data.entries.iter().map(VaultHit::from).collect()
        } else {
            Vec::new()
        }
    }

    /// 현재 범위·에이전트·검색어 필터가 적용된 결과 목록을 돌려준다.
    #[must_use]
    pub fn visible_hits(&self) -> Vec<VaultHit> {
        let fallback;
        let base = if let Some(vault) = &self.vault {
            &vault.hits
        } else {
            fallback = self
                .data
                .as_ref()
                .map(|d| d.entries.iter().map(VaultHit::from).collect::<Vec<_>>())
                .unwrap_or_default();
            &fallback
        };
        if base.is_empty() {
            return Vec::new();
        }
        let scope_paths = self.active_scope_paths();
        let terms: Vec<String> = self
            .query
            .split_whitespace()
            .filter(|tok| {
                !tok.starts_with("agent:") && !tok.starts_with("path:") && !tok.starts_with("repo:")
            })
            .map(str::to_lowercase)
            .collect();
        base.iter()
            .filter(|hit| self.agent_filter.is_none_or(|a| hit.agent == a))
            .filter(|hit| path_matches_scope(&hit.cwd, &scope_paths))
            .filter(|hit| {
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
            .cloned()
            .collect()
    }

    /// 상태바 토글 버튼(OWE-856: 버튼 역할·이름 노출).
    pub fn render_toggle(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let tooltip_text = match (&self.vault, &self.data) {
            (Some(v), _) => format!("{} ({})", text::TITLE.get(), v.hits.len()),
            (None, Some(d)) => format!("{} ({})", text::TITLE.get(), d.entries.len()),
            (None, None) => text::TITLE.get().to_owned(),
        };
        div()
            .id("agent-history-toggle")
            .debug_selector(|| "agent-history-toggle".into())
            .role(gpui::Role::Button)
            .aria_label(tooltip_text.clone())
            .flex()
            .items_center()
            .justify_center()
            .px_1()
            .h(crate::ui::STATUS_BAR_BUTTON_HEIGHT)
            .text_size(crate::ui::TEXT_XS)
            .text_color(if self.open {
                p.foreground
            } else {
                p.muted_foreground
            })
            .rounded(crate::ui::RADIUS_XS)
            .cursor_pointer()
            .hover(|d| d.bg(p.accent).text_color(p.accent_foreground))
            .child(
                gpui_component::Icon::default()
                    .data(crate::ui::icons::HISTORY_SVG)
                    .with_size(gpui_component::Size::XSmall),
            )
            .tooltip(move |window, cx| {
                crate::ui::tooltip_view("agent-history-toggle", tooltip_text.clone(), window, cx)
            })
            .on_click(cx.listener(|_, _, _, cx| cx.emit(Toggle)))
    }

    /// OWE-857: 열린 패널의 검색창에 포커스를 둔다. 입력 엔티티가 아직
    /// 없으면 다음 렌더의 `focus_on_open` 소비가 대신 수행한다.
    pub fn focus_search(&self, window: &mut Window, cx: &mut App) {
        if self.open
            && let Some(input) = &self.input
        {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    /// OWE-652: 열림 상태(셸·테스트에서 읽는다).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn run(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.active = true;
        self.busy = true;
        self.error = None;
        let call = Arc::clone(&self.call);
        let generation = self.generation;
        self.task = Some(cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move { call(&Request::AgentHistory(action)) })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.busy = false;
                if this.pending_search {
                    this.search_vault(cx);
                    return;
                }
                match got {
                    Ok(Reply::AgentHistory(Response::Scanned(data))) => {
                        this.data = Some(data);
                        this.vault = None;
                    }
                    Ok(Reply::AgentHistory(Response::Searched(vault))) => {
                        this.vault = Some(vault);
                    }
                    Ok(Reply::AgentHistory(Response::Resumed { session })) => {
                        this.open = false;
                        this.pending_search = false;
                        this.data = None;
                        this.vault = None;
                        cx.emit(Open(session));
                    }
                    Ok(Reply::Failed(e)) => this.error = Some(e.to_string()),
                    Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                        this.error = Some(text::OUTDATED.get().into());
                    }
                    Err(e) => this.error = Some(e.to_string()),
                    Ok(_) => this.error = Some(io::ErrorKind::InvalidData.to_string()),
                }
                if this.pending_search {
                    this.search_vault(cx);
                } else {
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn resume(&mut self, entry: Entry, cx: &mut Context<Self>) {
        self.run(Action::Resume(entry), cx);
    }

    pub fn resume_vault(&mut self, target: VaultTarget, cx: &mut Context<Self>) {
        self.run(Action::ResumeVault(target), cx);
    }
}

impl Panel {
    fn render_hit(&mut self, i: usize, hit: &VaultHit, cx: &mut Context<Self>) -> gpui::Div {
        #[cfg(target_os = "macos")]
        let _work = crate::responsiveness::work("history.row");
        let p = crate::ui::palette(cx);
        let from_vault = self.vault.is_some();
        let matching_entry = (!from_vault)
            .then(|| {
                self.data
                    .as_ref()
                    .into_iter()
                    .flat_map(|d| d.entries.iter())
                    .find(|e| e.id == hit.id && VaultAgent::from(e.agent) == hit.agent)
                    .cloned()
            })
            .flatten();
        let header_line = if hit.title.is_empty() {
            format!("{} · {}", hit.agent.profile(), hit.id)
        } else {
            format!("{} · {} ({})", hit.agent.profile(), hit.title, hit.id)
        };
        let mut detail = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(crate::ui::TEXT_MD)
                    .child(crate::ui::icons::agent_brand_icon(
                        hit.agent.profile(),
                        crate::ui::ICON_SM,
                        p.foreground,
                    ))
                    .child(render_highlighted(&header_line, &self.query, p)),
            );
        if !hit.snippet.is_empty() {
            detail = detail.child(
                div()
                    .text_color(p.muted_foreground)
                    .child(render_highlighted(&hit.snippet, &self.query, p)),
            );
        }
        detail = detail.child(
            div()
                .truncate()
                .font_family(crate::ui::FONT_MONO)
                .text_size(crate::ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(hit.cwd.clone()),
        );
        let row = div()
            .flex_none()
            .flex()
            .flex_col()
            .items_start()
            .gap_1()
            .pb_2()
            .border_b_1()
            .border_color(p.border)
            .child(detail.w_full());
        let busy = self.busy;
        let target = hit.target();
        row.child(
            crate::ui::secondary(("agent-history-resume", i), text::RESUME.get())
                .debug_selector(move || format!("agent-history-resume-{i}"))
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !busy {
                        if let Some(entry) = &matching_entry {
                            this.resume(entry.clone(), cx);
                        } else {
                            this.resume_vault(target.clone(), cx);
                        }
                    }
                })),
        )
    }
}

fn render_highlighted(text_str: &str, query: &str, p: &crate::ui::Palette) -> gpui::Div {
    let mut row = div().flex().flex_wrap().items_center();
    for (seg, is_hit) in highlight_spans(text_str, query) {
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

impl Render for Panel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::ui::palette(cx);
        if self.open && cx.has_global::<gpui_component::Theme>() {
            let input = self
                .input
                .get_or_insert_with(|| {
                    let query = self.query.clone();
                    cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(text::SEARCH_PLACEHOLDER.get())
                            .default_value(query)
                    })
                })
                .clone();
            if self.input_sub.is_none() {
                self.input_sub = Some(cx.observe(&input, |this, input, cx| {
                    let val = input.read(cx).value().to_string();
                    if val != this.query {
                        this.set_query(val, cx);
                    }
                }));
            }
            if self.language != crate::i18n::language() {
                self.language = crate::i18n::language();
                input.update(cx, |s, cx| {
                    s.set_placeholder(text::SEARCH_PLACEHOLDER.get(), window, cx);
                });
            }
            // OWE-857: 열림 요청(`toggle`)이 남긴 포커스 예약을 여기서 소비한다.
            if self.focus_on_open {
                self.focus_on_open = false;
                input.update(cx, |s, cx| s.focus(window, cx));
            }
        }
        let panel = self.open.then(|| {
            let search_bar = if let Some(input) = &self.input {
                div().id("vault-search-box").py_1().child(Input::new(input))
            } else {
                div()
                    .id("vault-search-fallback")
                    .px_2()
                    .py_1()
                    .rounded(crate::ui::RADIUS_XS)
                    .border_1()
                    .border_color(p.border)
                    .text_color(if self.query.is_empty() {
                        p.muted_foreground
                    } else {
                        p.popover_foreground
                    })
                    .child(if self.query.is_empty() {
                        text::SEARCH_PLACEHOLDER.get().to_owned()
                    } else {
                        self.query.clone()
                    })
            };

            let scope_bar = div().py_1().child(
                crate::ui::segmented_control(
                    p,
                    [
                        ("vault-scope-all", VaultScope::All, text::SCOPE_ALL.get()),
                        (
                            "vault-scope-project",
                            VaultScope::Project,
                            text::SCOPE_PROJECT.get(),
                        ),
                        (
                            "vault-scope-workspace",
                            VaultScope::Workspace,
                            text::SCOPE_WORKSPACE.get(),
                        ),
                    ]
                    .into_iter()
                    .map(|(id, scope, label)| {
                        let active = self.scope == scope;
                        crate::ui::segment(p, id, label, active)
                            .debug_selector(move || id.into())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_scope(scope, cx);
                            }))
                    }),
                )
                .flex_wrap(),
            );

            let agent_bar = div().flex().flex_wrap().gap_1().pb_1().children(
                [
                    ("vault-agent-all", None, text::AGENT_ALL.get()),
                    ("vault-agent-claude", Some(VaultAgent::Claude), "Claude"),
                    ("vault-agent-codex", Some(VaultAgent::Codex), "Codex"),
                    ("vault-agent-agy", Some(VaultAgent::Agy), "Antigravity"),
                    (
                        "vault-agent-opencode",
                        Some(VaultAgent::OpenCode),
                        "OpenCode",
                    ),
                ]
                .into_iter()
                .map(|(id, agent, label)| {
                    let active = self.agent_filter == agent;
                    let btn =
                        crate::ui::segment(p, id, label, active).debug_selector(move || id.into());
                    let btn = match agent {
                        Some(ag) => btn.icon(crate::ui::icons::agent_menu_icon(ag.profile())),
                        None => btn,
                    };
                    btn.on_click(cx.listener(move |this, _, _, cx| {
                        this.set_agent_filter(agent, cx);
                    }))
                }),
            );

            let mut content = div()
                .id("agent-history-list")
                .debug_selector(|| "agent-history-list".into())
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2();

            let hits = self.visible_hits();
            let has_loaded = self.vault.is_some() || self.data.is_some();
            if has_loaded && hits.is_empty() {
                content = content.child(text::EMPTY.get());
            }

            let count = hits.len();
            // Keep list identity and scroll state while only constructing visible rows.
            if self.list_hits.as_ref() != &hits {
                self.list_state.reset(count);
                self.list_hits = Arc::new(hits);
            }
            let hits = Arc::clone(&self.list_hits);
            let weak = cx.entity().downgrade();
            content = content.child(
                gpui::list(self.list_state.clone(), move |i, _, cx| {
                    weak.update(cx, |this, cx| {
                        hits.get(i)
                            .map_or_else(div, |hit| this.render_hit(i, hit, cx))
                    })
                    .unwrap_or_else(|_| div())
                    .into_any_element()
                })
                .flex_1(),
            );

            let truncated = self.vault.as_ref().map_or_else(
                || self.data.as_ref().is_some_and(|d| d.truncated),
                |v| v.truncated,
            );
            let skipped = self.vault.as_ref().map_or_else(
                || self.data.as_ref().is_some_and(|d| d.skipped),
                |v| v.skipped,
            );
            if truncated {
                content = content.child(text::LIMITED.get());
            }
            if skipped {
                content = content.child(text::SKIPPED.get());
            }

            div()
                .id("agent-history-panel")
                .debug_selector(|| "agent-history-panel".into())
                .capture_key_down(cx.listener(|_, e: &gpui::KeyDownEvent, _, cx| {
                    if e.keystroke.key == "escape" && !e.keystroke.modifiers.modified() {
                        cx.stop_propagation();
                        cx.emit(Toggle);
                    }
                }))
                .size_full()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .p_2()
                .bg(p.sidebar)
                .font_family(crate::ui::FONT_UI)
                .text_color(p.sidebar_foreground)
                .text_size(crate::ui::TEXT_XS)
                .child(
                    div()
                        .id("agent-history-header")
                        .debug_selector(|| "agent-history-header".into())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .pb_1()
                        .child(
                            div()
                                .id("agent-history-title")
                                .debug_selector(|| "agent-history-title".into())
                                .text_size(crate::ui::TEXT_SM)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(p.foreground)
                                .child(text::TITLE.get()),
                        )
                        .child(
                            crate::ui::button_with_reserved_label(
                                "agent-history-refresh",
                                text::REFRESH.get(),
                                text::BUSY.get(),
                                self.busy,
                            )
                            .icon(gpui_component::IconName::RefreshCw)
                            .ghost()
                            .xsmall()
                            .debug_selector(|| "agent-history-refresh".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.has_vault_filters() || this.vault.is_some() {
                                    this.search_vault(cx);
                                } else {
                                    this.run(Action::Scan, cx);
                                }
                            })),
                        ),
                )
                .child(search_bar)
                .child(scope_bar)
                .child(agent_bar)
                .child(content)
                .children(self.error.as_deref().map(crate::i18n::cached_notice))
        });
        div().size_full().min_h_0().children(panel)
    }
}

#[cfg(test)]
mod tests;
