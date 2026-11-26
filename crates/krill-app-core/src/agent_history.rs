//! MK-082·MK-181: 요청 시 멀티 에이전트 대화 기록을 스캔·전문 검색하고 선택한 대화를 새 탭에서 재개한다.
//! 열기·새로고침·검색·필터 전환 때만 일회성 백그라운드 IO를 실행하며, 감시·폴링·상주 프로세스는 없다.
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::{Context, Entity, EventEmitter, Subscription, Task, Window, div, prelude::*};
use gpui_component::button::ButtonVariants as _;
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use krill_proto::SessionId;
use krill_proto::agent_history::{
    Action, Entry, FEAT_AGENT_HISTORY, FEAT_AI_VAULT, FEAT_CHAT_POLL, Response, SearchQuery,
    SearchSnapshot, Snapshot, VaultAgent, VaultHit, VaultScope, VaultTarget, is_claude_mem_cwd,
};
use krill_proto::control::{FEAT_CONTROL, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;

use crate::hosts::{HostId, HostTarget};
use crate::i18n::catalog::agent_history as text;

pub type Call = Arc<dyn Fn(&Request) -> io::Result<Reply> + Send + Sync>;
pub type WorkflowCall = Arc<dyn Fn(&str) -> io::Result<String> + Send + Sync>;

fn daemon_call() -> Call {
    daemon_call_for(None)
}

fn should_reconnect_ipc(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
    )
}

const MAX_IDLE_CONNS_PER_KEY: usize = 4;
const POOL_IDLE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PoolChannel {
    Workflow,
    History,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PoolKey {
    host: HostId,
    socket: PathBuf,
    allow_local_launch: bool,
    channel: PoolChannel,
}

impl PoolKey {
    fn of(target: &HostTarget, channel: PoolChannel) -> Self {
        Self {
            host: target.host().clone(),
            socket: target.socket().to_owned(),
            allow_local_launch: target.allow_local_launch(),
            channel,
        }
    }
}

struct PooledConn {
    conn: daemon::Connection,
    epoch: u64,
    idle_since: Instant,
}

type PoolMap = Mutex<BTreeMap<PoolKey, VecDeque<PooledConn>>>;

fn connection_pool() -> &'static PoolMap {
    static POOL: OnceLock<PoolMap> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Drops any idle pooled history/workflow IPC connections for `host`
/// (call on host disconnect or target change).
pub fn invalidate_host_connections(host: &HostId) {
    if let Ok(mut pool) = connection_pool().lock() {
        pool.retain(|key, _| &key.host != host);
    }
}

fn checkout_pooled_conn(target: &HostTarget, channel: PoolChannel) -> Option<PooledConn> {
    let key = PoolKey::of(target, channel);
    let epoch = target.transport_epoch();
    let now = Instant::now();
    let mut pool = connection_pool().lock().ok()?;
    let queue = pool.get_mut(&key)?;
    while let Some(item) = queue.pop_front() {
        if item.epoch == epoch && now.saturating_duration_since(item.idle_since) < POOL_IDLE_TTL {
            if queue.is_empty() {
                pool.remove(&key);
            }
            return Some(item);
        }
    }
    pool.remove(&key);
    None
}

fn return_pooled_conn(
    target: &HostTarget,
    channel: PoolChannel,
    conn: daemon::Connection,
    epoch: u64,
) {
    if !target.is_current() || target.transport_epoch() != epoch {
        return;
    }
    let key = PoolKey::of(target, channel);
    let Ok(mut pool) = connection_pool().lock() else {
        return;
    };
    let now = Instant::now();
    let queue = pool.entry(key).or_default();
    queue.retain(|c| {
        c.epoch == epoch && now.saturating_duration_since(c.idle_since) < POOL_IDLE_TTL
    });
    while queue.len() >= MAX_IDLE_CONNS_PER_KEY {
        queue.pop_back();
    }
    queue.push_front(PooledConn {
        conn,
        epoch,
        idle_since: now,
    });
}

pub fn workflow_call_for(target: Option<HostTarget>) -> WorkflowCall {
    workflow_call_with_timeouts(target, Duration::from_secs(15), Duration::from_secs(5))
}

fn workflow_call_with_timeouts(
    target: Option<HostTarget>,
    read_timeout: Duration,
    write_timeout: Duration,
) -> WorkflowCall {
    #[cfg(test)]
    if target.is_none() {
        return Arc::new(|_| {
            Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "daemon disabled in unit tests",
            ))
        });
    }
    Arc::new(move |json| {
        let req = krill_proto::mobile::MobileRequest::Workflow(json.to_owned());
        let encoded = req.encode()?;
        let resolved = match &target {
            Some(t) => t.clone(),
            None => HostTarget::local_dev()?,
        };
        for attempt in 0..2 {
            let (mut conn, epoch, from_pool) =
                if let Some(pooled) = checkout_pooled_conn(&resolved, PoolChannel::Workflow) {
                    (pooled.conn, pooled.epoch, true)
                } else {
                    let mut hello = daemon::client_hello(ClientRole::App, "krill-app-composer");
                    hello.features = krill_proto::mobile::FEAT_MOBILE;
                    let epoch = resolved.transport_epoch();
                    let conn = resolved.connect(&hello).map_err(io::Error::other)?;
                    if conn.negotiated.features & krill_proto::mobile::FEAT_MOBILE == 0 {
                        return Err(io::ErrorKind::Unsupported.into());
                    }
                    (conn, epoch, false)
                };
            if let Err(err) = conn
                .stream
                .set_read_timeout(Some(read_timeout))
                .and_then(|()| conn.stream.set_write_timeout(Some(write_timeout)))
            {
                if attempt == 0 && (from_pool || should_reconnect_ipc(&err)) {
                    continue;
                }
                return Err(err);
            }
            let result = (|| -> io::Result<Result<String, String>> {
                conn.stream.write_all(&encoded)?;
                let (kind, body) =
                    ipc::read_frame(&mut conn.stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
                match krill_proto::mobile::MobileReply::decode(kind, &body)? {
                    krill_proto::mobile::MobileReply::Workflow(out) => Ok(Ok(out)),
                    krill_proto::mobile::MobileReply::Failed(err) => Ok(Err(err)),
                    _ => Err(io::ErrorKind::InvalidData.into()),
                }
            })();
            match result {
                Ok(Ok(out)) => {
                    return_pooled_conn(&resolved, PoolChannel::Workflow, conn, epoch);
                    return Ok(out);
                }
                Ok(Err(err)) => {
                    return_pooled_conn(&resolved, PoolChannel::Workflow, conn, epoch);
                    return Err(io::Error::other(err));
                }
                Err(err) => {
                    drop(conn);
                    if attempt == 0 && should_reconnect_ipc(&err) {
                        continue;
                    }
                    return Err(err);
                }
            }
        }
        Err(io::ErrorKind::NotConnected.into())
    })
}

pub fn daemon_call_for(target: Option<HostTarget>) -> Call {
    daemon_call_with_timeouts(target, Duration::from_secs(10), Duration::from_secs(2))
}

fn daemon_call_with_timeouts(
    target: Option<HostTarget>,
    read_timeout: Duration,
    write_timeout: Duration,
) -> Call {
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
        let resolved = match &target {
            Some(t) => t.clone(),
            None => HostTarget::local_dev()?,
        };
        for attempt in 0..2 {
            let (mut conn, epoch, from_pool) =
                if let Some(pooled) = checkout_pooled_conn(&resolved, PoolChannel::History) {
                    (pooled.conn, pooled.epoch, true)
                } else {
                    let hello = daemon::client_hello(ClientRole::App, "krill-app-history");
                    let epoch = resolved.transport_epoch();
                    let conn = resolved.connect(&hello).map_err(io::Error::other)?;
                    (conn, epoch, false)
                };
            if let Err(err) = conn
                .stream
                .set_read_timeout(Some(read_timeout))
                .and_then(|()| conn.stream.set_write_timeout(Some(write_timeout)))
            {
                if attempt == 0 && (from_pool || should_reconnect_ipc(&err)) {
                    continue;
                }
                return Err(err);
            }
            let features = conn.negotiated.features;
            match exchange(features, &mut conn.stream, req) {
                Ok(reply) => {
                    return_pooled_conn(&resolved, PoolChannel::History, conn, epoch);
                    return Ok(reply);
                }
                Err(err) => {
                    drop(conn);
                    if attempt == 0 && should_reconnect_ipc(&err) {
                        continue;
                    }
                    return Err(err);
                }
            }
        }
        Err(io::ErrorKind::NotConnected.into())
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
        .map(crate::jump::normalize_search_chars)
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() || text.is_empty() {
        return vec![(text.to_owned(), false)];
    }
    let orig_chars: Vec<char> = text.chars().collect();
    let norm = crate::jump::normalize_search_chars_with_ranges(text);
    let mut matched = vec![false; orig_chars.len()];
    for term in &terms {
        let tlen = term.len();
        if tlen == 0 || tlen > norm.len() {
            continue;
        }
        for start in 0..=norm.len().saturating_sub(tlen) {
            if norm
                .get(start..start + tlen)
                .is_some_and(|slice| slice.iter().map(|(c, _)| *c).eq(term.iter().copied()))
            {
                for (_, orig_range) in &norm[start..start + tlen] {
                    for flag in matched
                        .iter_mut()
                        .take(orig_range.end)
                        .skip(orig_range.start)
                    {
                        *flag = true;
                    }
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

fn path_matches_scope(cwd: &str, filter_paths: &[String]) -> bool {
    filter_paths.is_empty()
        || filter_paths.iter().any(|root| {
            root.is_empty() || krill_proto::agent_history::path_inside_or_equal(cwd, root)
        })
}

/// 성공한 새 세션만 셸에 알려 새 탭을 만든다. 실패하면 기존 탭은 그대로다.
#[derive(Debug, Clone)]
pub struct Open {
    pub host: HostId,
    pub session: SessionId,
}

/// 상태바에서 우측 세션 기록 탭을 열거나 접는다.
#[derive(Debug, Clone, Copy)]
pub struct Toggle;

#[expect(
    clippy::struct_excessive_bools,
    reason = "패널 열림·처리 중·재검색 대기·열림 포커스 예약의 독립 UI 상태"
)]
pub struct Panel {
    call: Call,
    host: HostId,
    target: Option<HostTarget>,
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
            host: HostId::Local,
            target: None,
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
        self.error = None;
        self.list_hits = Arc::default();
        self.call = call;
    }

    #[must_use]
    pub fn host(&self) -> &HostId {
        &self.host
    }

    #[must_use]
    pub fn context(&self) -> (HostId, u64) {
        (self.host.clone(), self.generation)
    }

    /// Bind history and resume to the workspace's daemon. An unavailable remote
    /// must never fall back to the local daemon, even when paths or IDs coincide.
    pub fn set_host(&mut self, host: HostId, target: Option<HostTarget>, cx: &mut Context<Self>) {
        self.set_context(
            host,
            target,
            self.workspace_path.clone(),
            self.project_paths.clone(),
            cx,
        );
    }

    /// Update host and path filters together before starting the next request.
    pub fn set_context(
        &mut self,
        host: HostId,
        target: Option<HostTarget>,
        workspace: Option<String>,
        project: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let host_changed = self.host != host || self.target != target;
        if !host_changed && self.workspace_path == workspace && self.project_paths == project {
            return;
        }
        if host_changed {
            let call = if target.is_none() && host.is_remote() {
                Arc::new(|_: &Request| Err(io::ErrorKind::NotConnected.into())) as Call
            } else {
                daemon_call_for(target.clone())
            };
            self.set_call(call);
            self.host = host;
            self.target = target;
        }
        self.set_scope_paths(workspace, project);
        if self.active {
            self.scan(cx);
        }
        cx.notify();
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
            self.pending_search = false;
            self.data = None;
            self.vault = None;
            self.error = None;
            self.list_hits = Arc::default();
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
            vault
                .hits
                .iter()
                .filter(|hit| !is_claude_mem_cwd(&hit.cwd))
                .cloned()
                .collect()
        } else if let Some(data) = &self.data {
            data.entries
                .iter()
                .filter(|entry| !is_claude_mem_cwd(&entry.cwd))
                .map(VaultHit::from)
                .collect()
        } else {
            Vec::new()
        }
    }

    /// 현재 범위·에이전트·검색어 필터가 적용된 결과 목록을 돌려준다.
    #[must_use]
    pub fn visible_hits(&self) -> Vec<VaultHit> {
        // Search is evaluated against complete messages on the daemon host. A snippet cannot
        // reproduce keyword matches, and local filesystem rules cannot classify remote paths.
        if let Some(vault) = &self.vault {
            return vault
                .hits
                .iter()
                .filter(|hit| !is_claude_mem_cwd(&hit.cwd))
                .cloned()
                .collect();
        }
        let base = self
            .data
            .as_ref()
            .map(|d| {
                d.entries
                    .iter()
                    .filter(|entry| !is_claude_mem_cwd(&entry.cwd))
                    .map(VaultHit::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
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
            .map(crate::jump::normalize_search_str)
            .filter(|t| !t.is_empty())
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
                    crate::jump::normalize_search_str(&hit.title),
                    crate::jump::normalize_search_str(&hit.summary),
                    crate::jump::normalize_search_str(&hit.snippet),
                    crate::jump::normalize_search_str(&hit.cwd),
                    crate::jump::normalize_search_str(&hit.id)
                );
                terms.iter().all(|t| hay.contains(t))
            })
            .cloned()
            .collect()
    }

    pub(crate) fn input_is_composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let app: &mut gpui::App = cx;
        self.input
            .as_ref()
            .is_some_and(|input| crate::jump::entity_input_is_composing(input, window, app))
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
        let host = self.host.clone();
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
                let resumed = matches!(got, Ok(Reply::AgentHistory(Response::Resumed { .. })));
                if this.pending_search && !resumed {
                    this.search_vault(cx);
                    return;
                }
                match got {
                    Ok(Reply::AgentHistory(Response::Scanned(mut data))) => {
                        data.entries.retain(|e| !is_claude_mem_cwd(&e.cwd));
                        this.data = Some(data);
                        this.vault = None;
                    }
                    Ok(Reply::AgentHistory(Response::Searched(mut vault))) => {
                        vault.hits.retain(|h| !is_claude_mem_cwd(&h.cwd));
                        this.vault = Some(vault);
                    }
                    Ok(Reply::AgentHistory(Response::Resumed { session })) => {
                        this.open = false;
                        this.pending_search = false;
                        this.data = None;
                        this.vault = None;
                        cx.emit(Open { host, session });
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
        let context = self.context();
        let target = hit.target();
        row.child(
            crate::ui::secondary(("agent-history-resume", i), text::RESUME.get())
                .debug_selector(move || format!("agent-history-resume-{i}"))
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !busy && this.context() == context {
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
                .capture_action(cx.listener(
                    |this, _: &gpui_component::input::Escape, window, cx| {
                        if this.input_is_composing(window, cx) {
                            window.prevent_default();
                        }
                    },
                ))
                .capture_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, window, cx| {
                    if e.keystroke.key == "escape"
                        && !e.keystroke.modifiers.modified()
                        && !window.default_prevented()
                        && !this.input_is_composing(window, cx)
                    {
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

#[cfg(all(test, unix))]
mod reconnect_tests {
    use super::*;
    use krill_proto::ipc::{ClientHello, ServerHello};
    use krill_proto::os::net::UnixListener;

    fn handshake(stream: &mut (impl Read + Write), features: u64) -> io::Result<()> {
        let hello = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features,
            build: "reconnect-test".into(),
            instance_id: 1,
        }
        .encode()?;
        stream.write_all(&hello)?;
        let (kind, body) = ipc::read_frame(stream)?
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let _client = ClientHello::decode(kind, &body)?;
        Ok(())
    }

    #[test]
    fn daemon_call_drops_cached_connection_after_timeout_and_invalid_data() {
        let dir = std::env::temp_dir().join(format!(
            "krill-history-drop-conn-daemon-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let features = FEAT_CONTROL | FEAT_AGENT_HISTORY;

        let server = std::thread::spawn(move || {
            // Connection 1: delay past read timeout, then send a stale reply so reusing conn 1 would desync.
            let (mut s1, _) = listener.accept().unwrap();
            s1.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            handshake(&mut s1, features).unwrap();
            let _ = ipc::read_frame(&mut s1).unwrap().unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).ok();
            s1.write_all(
                &Reply::AgentHistory(Response::Resumed {
                    session: SessionId(111),
                })
                .encode()
                .unwrap(),
            )
            .ok();

            // Connection 2: return an invalid/unexpected frame kind (triggers InvalidData), keep open.
            let (mut s2, _) = listener.accept().unwrap();
            s2.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            handshake(&mut s2, features).unwrap();
            let _ = ipc::read_frame(&mut s2).unwrap().unwrap();
            s2.write_all(
                &ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features,
                    build: "wrong-frame".into(),
                    instance_id: 2,
                }
                .encode()
                .unwrap(),
            )
            .unwrap();

            // Connection 3: valid response proves both TimedOut and InvalidData dropped the cached stream.
            let (mut s3, _) = listener.accept().unwrap();
            s3.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            handshake(&mut s3, features).unwrap();
            let _ = ipc::read_frame(&mut s3).unwrap().unwrap();
            s3.write_all(
                &Reply::AgentHistory(Response::Resumed {
                    session: SessionId(333),
                })
                .encode()
                .unwrap(),
            )
            .unwrap();
        });

        let call = daemon_call_with_timeouts(
            Some(HostTarget::remote("studio", socket)),
            Duration::from_millis(60),
            Duration::from_secs(1),
        );
        let req = Request::AgentHistory(Action::Scan);

        let err1 = call(&req).unwrap_err();
        assert!(
            matches!(
                err1.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "expected timeout, got {err1:?}"
        );
        release_tx.send(()).ok();

        let err2 = call(&req).unwrap_err();
        assert_eq!(err2.kind(), io::ErrorKind::InvalidData);

        let reply3 = call(&req).unwrap();
        assert_eq!(
            reply3,
            Reply::AgentHistory(Response::Resumed {
                session: SessionId(333),
            })
        );

        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn workflow_call_drops_cached_connection_after_timeout_and_invalid_data() {
        let dir = std::env::temp_dir().join(format!(
            "krill-history-drop-conn-workflow-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("workflow.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let features = krill_proto::mobile::FEAT_MOBILE;

        let server = std::thread::spawn(move || {
            // Connection 1: time out, then write a stale Workflow reply.
            let (mut s1, _) = listener.accept().unwrap();
            s1.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            handshake(&mut s1, features).unwrap();
            let _ = ipc::read_frame(&mut s1).unwrap().unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).ok();
            s1.write_all(
                &krill_proto::mobile::MobileReply::Workflow("stale".into())
                    .encode()
                    .unwrap(),
            )
            .ok();

            // Connection 2: write an unexpected frame kind (InvalidData), keep open.
            let (mut s2, _) = listener.accept().unwrap();
            s2.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            handshake(&mut s2, features).unwrap();
            let _ = ipc::read_frame(&mut s2).unwrap().unwrap();
            s2.write_all(
                &ServerHello {
                    proto_min: ipc::PROTO_MIN,
                    proto_max: ipc::PROTO_MAX,
                    features,
                    build: "wrong-frame".into(),
                    instance_id: 2,
                }
                .encode()
                .unwrap(),
            )
            .unwrap();

            // Connection 3: fresh valid reply.
            let (mut s3, _) = listener.accept().unwrap();
            s3.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            handshake(&mut s3, features).unwrap();
            let _ = ipc::read_frame(&mut s3).unwrap().unwrap();
            s3.write_all(
                &krill_proto::mobile::MobileReply::Workflow("fresh".into())
                    .encode()
                    .unwrap(),
            )
            .unwrap();
        });

        let call = workflow_call_with_timeouts(
            Some(HostTarget::remote("studio", socket)),
            Duration::from_millis(60),
            Duration::from_secs(1),
        );

        let err1 = call("{}").unwrap_err();
        assert!(
            matches!(
                err1.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "expected timeout, got {err1:?}"
        );
        release_tx.send(()).ok();

        let err2 = call("{}").unwrap_err();
        assert_eq!(err2.kind(), io::ErrorKind::InvalidData);

        assert_eq!(call("{}").unwrap(), "fresh");

        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn perf_bench_workflow_and_daemon_calls() {
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        use krill_proto::agent_history::ChatPollSnapshot;

        let dir = std::env::temp_dir().join(format!(
            "krill-perf-ipc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let accepts = Arc::new(AtomicUsize::new(0));
        let accepts_bg = Arc::clone(&accepts);
        let features =
            FEAT_CONTROL | FEAT_AGENT_HISTORY | FEAT_CHAT_POLL | krill_proto::mobile::FEAT_MOBILE;

        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            while !stop_bg.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(_) => break,
                };
                if stop_bg.load(Ordering::Relaxed) {
                    break;
                }
                accepts_bg.fetch_add(1, Ordering::Relaxed);
                workers.push(std::thread::spawn(move || {
                    stream.set_nonblocking(false).ok();
                    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                    if handshake(&mut stream, features).is_err() {
                        return;
                    }
                    while let Ok(Some((rk, rb))) = ipc::read_frame(&mut stream) {
                        if krill_proto::mobile::MobileRequest::decode(rk, &rb).is_ok() {
                            let out =
                                r#"{"ok":true,"data":{"next_offset":10,"items":[]}}"#.to_owned();
                            if stream
                                .write_all(
                                    &krill_proto::mobile::MobileReply::Workflow(out)
                                        .encode()
                                        .unwrap(),
                                )
                                .is_err()
                            {
                                break;
                            }
                        } else if Request::decode(rk, &rb).is_ok() {
                            let rep = Reply::AgentHistory(Response::ChatPolled(
                                ChatPollSnapshot::default(),
                            ));
                            if stream.write_all(&rep.encode().unwrap()).is_err() {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }));
            }
            for w in workers {
                w.join().ok();
            }
        });

        let target = HostTarget::remote("perf-host", socket);
        let t0 = std::time::Instant::now();
        for _ in 0..50 {
            let wf = workflow_call_for(Some(target.clone()));
            let out = wf(r#"{"action":"chat_poll","session":1,"offset":0}"#).unwrap();
            assert!(out.contains("\"ok\":true"));
        }
        let wf_elapsed = t0.elapsed();
        let wf_accepts = accepts.load(Ordering::Relaxed);

        let t1 = std::time::Instant::now();
        for _ in 0..50 {
            let dc = daemon_call_for(Some(target.clone()));
            let rep = dc(&Request::AgentHistory(Action::ChatPoll {
                session: SessionId(1),
                offset: 0,
            }))
            .unwrap();
            assert!(matches!(rep, Reply::AgentHistory(Response::ChatPolled(_))));
        }
        let dc_elapsed = t1.elapsed();
        let dc_accepts = accepts.load(Ordering::Relaxed) - wf_accepts;

        eprintln!(
            "[BENCH] workflow_call_for x50: {:.3} ms, accepts={wf_accepts} | daemon_call_for x50: {:.3} ms, accepts={dc_accepts}",
            wf_elapsed.as_secs_f64() * 1000.0,
            dc_elapsed.as_secs_f64() * 1000.0,
        );
        assert_eq!(
            wf_accepts, 1,
            "workflow_call_for must reuse a single pooled stream"
        );
        assert_eq!(
            dc_accepts, 1,
            "daemon_call_for must reuse a single pooled stream"
        );

        // Host invalidation must drop pooled connections so the next call reconnects cleanly.
        invalidate_host_connections(target.host());
        let wf = workflow_call_for(Some(target.clone()));
        assert!(
            wf(r#"{"action":"chat_poll","session":1,"offset":0}"#)
                .unwrap()
                .contains("\"ok\":true")
        );
        assert_eq!(
            accepts.load(Ordering::Relaxed),
            3,
            "invalidate_host_connections must force a fresh stream"
        );
        invalidate_host_connections(target.host());

        stop.store(true, Ordering::Relaxed);
        // Wake accept loop if needed
        krill_proto::os::net::UnixStream::connect(target.socket()).ok();
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
