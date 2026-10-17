//! MK-082·MK-181: 요청 시 멀티 에이전트 대화 기록을 스캔·전문 검색하고 선택한 대화를 새 탭에서 재개한다.
//! 열기·새로고침·검색·필터 전환 때만 일회성 백그라운드 IO를 실행하며, 감시·폴링·상주 프로세스는 없다.
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Entity, EventEmitter, Subscription, Task, Window, div, prelude::*};
use gpui_component::input::{Input, InputState};
use krill_proto::SessionId;
use krill_proto::agent_history::{
    Action, Entry, FEAT_AGENT_HISTORY, FEAT_AI_VAULT, Response, SearchQuery, SearchSnapshot,
    Snapshot, VaultAgent, VaultHit, VaultScope, VaultTarget,
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
    let need = if vault {
        FEAT_CONTROL | FEAT_AGENT_HISTORY | FEAT_AI_VAULT
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

pub struct Panel {
    call: Call,
    open: bool,
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
    language: crate::i18n::Language,
    error: Option<String>,
    task: Option<Task<()>>,
    /// OWE-652: 바깥 클릭으로 닫힌 시각. 같은 제스처의 헤더 클릭이 다시 열지 않게 한다.
    dismissed_at: Option<std::time::Instant>,
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
            open: false,
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
            language: crate::i18n::language(),
            error: None,
            task: None,
            dismissed_at: None,
        }
    }

    pub fn set_call(&mut self, call: Call) {
        self.call = call;
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.close(cx);
            return;
        }
        self.open = true;
        self.scan(cx);
    }

    pub fn scan(&mut self, cx: &mut Context<Self>) {
        if self.has_vault_filters() {
            self.search_vault(cx);
        } else {
            self.run(Action::Scan, cx);
        }
    }

    /// OWE-652: 바깥 클릭·다른 팝오버가 열릴 때 닫는다.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            self.busy = false;
            self.pending_search = false;
            self.data = None;
            self.vault = None;
            self.task = None;
            cx.notify();
        }
    }

    /// 현재 워크스페이스·프로젝트 범위 경로를 설정한다.
    pub fn set_scope_paths(&mut self, workspace: Option<String>, project: Vec<String>) {
        self.workspace_path = workspace;
        self.project_paths = project;
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
            if self.open {
                self.search_vault(cx);
            } else {
                cx.notify();
            }
        }
    }

    pub fn set_agent_filter(&mut self, agent: Option<VaultAgent>, cx: &mut Context<Self>) {
        if self.agent_filter != agent {
            self.agent_filter = agent;
            if self.open {
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
            if self.open {
                self.search_vault(cx);
            } else {
                cx.notify();
            }
        }
    }

    pub fn search_vault(&mut self, cx: &mut Context<Self>) {
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
        let base = self.loaded_hits();
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
        base.into_iter()
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
            .collect()
    }

    /// OWE-652: 팝오버 밖 마우스 눌림에 닫는다. 같은 제스처의 헤더 클릭이
    /// 다시 열지 않게 시각을 남긴다(클릭은 마우스 뗌 뒤에 온다).
    fn on_outside_down(&mut self, cx: &mut Context<Self>) {
        self.close(cx);
        self.dismissed_at = Some(std::time::Instant::now());
    }

    /// OWE-652: 헤더 클릭. 바깥 닫기와 같은 제스처(약 150ms 안)면 닫힌 채로 둔다.
    fn on_toggle_click(&mut self, cx: &mut Context<Self>) {
        let same_gesture = self
            .dismissed_at
            .take()
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(150));
        if !same_gesture {
            self.toggle(cx);
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
        self.busy = true;
        self.error = None;
        let call = Arc::clone(&self.call);
        self.task = Some(cx.spawn(async move |this, cx| {
            let got = cx
                .background_executor()
                .spawn(async move { call(&Request::AgentHistory(action)) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
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
                    cx.new(|cx| {
                        InputState::new(window, cx).placeholder(text::SEARCH_PLACEHOLDER.get())
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
        }

        let header = div()
            .id("agent-history-toggle")
            .debug_selector(|| "agent-history-toggle".into())
            .px_1p5()
            .py_0p5()
            .text_size(crate::ui::TEXT_XS)
            .rounded(crate::ui::RADIUS_XS)
            .cursor_pointer()
            .hover(|d| d.bg(p.accent).text_color(p.accent_foreground))
            .child(text::TITLE.get())
            .on_click(cx.listener(|this, _, _, cx| this.on_toggle_click(cx)));

        let popover = self.open.then(|| {
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

            let scope_bar = {
                let mut bar = div().flex().items_center().gap_1().py_1();
                for (id, scope, label) in [
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
                ] {
                    let active = self.scope == scope;
                    bar = bar.child(
                        div()
                            .id(id)
                            .debug_selector(move || id.into())
                            .px_1p5()
                            .py_0p5()
                            .rounded(crate::ui::RADIUS_XS)
                            .cursor_pointer()
                            .bg(if active { p.accent } else { p.secondary })
                            .text_color(if active {
                                p.accent_foreground
                            } else {
                                p.secondary_foreground
                            })
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_scope(scope, cx);
                            })),
                    );
                }
                bar
            };

            let agent_bar = {
                let mut bar = div().flex().flex_wrap().items_center().gap_1().pb_1();
                for (id, agent, label) in [
                    ("vault-agent-all", None, text::AGENT_ALL.get()),
                    ("vault-agent-claude", Some(VaultAgent::Claude), "Claude"),
                    ("vault-agent-codex", Some(VaultAgent::Codex), "Codex"),
                    ("vault-agent-agy", Some(VaultAgent::Agy), "Antigravity"),
                    (
                        "vault-agent-opencode",
                        Some(VaultAgent::OpenCode),
                        "OpenCode",
                    ),
                ] {
                    let active = self.agent_filter == agent;
                    bar = bar.child(
                        div()
                            .id(id)
                            .debug_selector(move || id.into())
                            .px_1p5()
                            .py_0p5()
                            .rounded(crate::ui::RADIUS_XS)
                            .cursor_pointer()
                            .bg(if active { p.accent } else { p.secondary })
                            .text_color(if active {
                                p.accent_foreground
                            } else {
                                p.secondary_foreground
                            })
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_agent_filter(agent, cx);
                            })),
                    );
                }
                bar
            };

            let mut content = div()
                .id("agent-history-list")
                .flex()
                .flex_col()
                .gap_2()
                .max_h(crate::ui::AGENT_HISTORY_MAX_HEIGHT)
                .overflow_y_scroll();

            let hits = self.visible_hits();
            let has_loaded = self.vault.is_some() || self.data.is_some();
            if has_loaded && hits.is_empty() {
                content = content.child(text::EMPTY.get());
            }

            let from_vault = self.vault.is_some();
            let raw_entries = self
                .data
                .as_ref()
                .map(|d| d.entries.clone())
                .unwrap_or_default();

            for (i, hit) in hits.into_iter().enumerate() {
                let matching_entry = (!from_vault)
                    .then(|| {
                        raw_entries
                            .iter()
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
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(render_highlighted(&header_line, &self.query, p));
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
                        .text_color(p.muted_foreground)
                        .child(hit.cwd.clone()),
                );
                let row = div().flex().items_center().gap_2().child(detail);
                let busy = self.busy;
                let target = hit.target();
                content = content.child(
                    row.child(
                        div()
                            .id(("agent-history-resume", i))
                            .debug_selector(move || format!("agent-history-resume-{i}"))
                            .px_2()
                            .py_1()
                            .rounded(crate::ui::RADIUS_XS)
                            .bg(p.secondary)
                            .text_color(p.secondary_foreground)
                            .cursor_pointer()
                            .child(text::RESUME.get())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !busy {
                                    if let Some(entry) = &matching_entry {
                                        this.resume(entry.clone(), cx);
                                    } else {
                                        this.resume_vault(target.clone(), cx);
                                    }
                                }
                            })),
                    ),
                );
            }

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
                .id("agent-history-popover")
                .debug_selector(|| "agent-history-popover".into())
                .absolute()
                .bottom(crate::ui::STATUS_BAR_HEIGHT + crate::ui::SPACE_1)
                .right_0()
                .w(crate::ui::AGENT_HISTORY_WIDTH)
                .p_3()
                .rounded(crate::ui::RADIUS_MD)
                .bg(p.popover)
                .text_color(p.popover_foreground)
                .border_1()
                .border_color(p.border)
                .shadow_md()
                .text_size(crate::ui::TEXT_XS)
                // OWE-652: 바깥 클릭·다른 팝오버가 열리면 닫는다.
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.on_outside_down(cx)))
                .child(
                    div()
                        .id("agent-history-refresh")
                        .py_1()
                        .cursor_pointer()
                        .child(if self.busy {
                            text::BUSY.get()
                        } else {
                            text::REFRESH.get()
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.has_vault_filters() || this.vault.is_some() {
                                this.search_vault(cx);
                            } else {
                                this.run(Action::Scan, cx);
                            }
                        })),
                )
                .child(search_bar)
                .child(scope_bar)
                .child(agent_bar)
                .child(content)
                .children(self.error.as_deref().map(crate::i18n::cached_notice))
        });
        div().relative().child(header).children(popover)
    }
}

#[cfg(test)]
mod tests;
