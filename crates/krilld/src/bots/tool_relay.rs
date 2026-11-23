// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Strictly-lazy loopback credential-injecting MCP tool relay (`BotToolRelay`).
//!
//! Security & memory invariants:
//! - Binds `127.0.0.1:0` and spawns a 128 KiB accept thread ONLY when at least one bot with
//!   enabled MCP tools or Composio starts a session or registers a tool route. Stops automatically
//!   when unused or shut down.
//! - Agents receive only `KRILL_BOT_TOOLS_URL` (`http://127.0.0.1:<port>`) and an ephemeral
//!   per-launch `KRILL_BOT_TOOLS_TOKEN` Bearer token. Raw API keys / OAuth tokens live only in
//!   [`SecretVault`] and are injected locally on outbound upstream requests to
//!   `/bot-tools/<cred_id>/mcp`.
//! - Bearer tokens are checked with constant-time byte comparison (`constant_time_eq`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use krill_proto::bots::{
    BotConfig, BotId, BotMcpAuthKind, BotToolRelayStatus, BotToolServerStatus,
};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::bots::http_client::{self, HttpMethod, HttpRequest, SsrfPolicy};
use crate::bots::mcp_oauth;
use crate::bots::secrets::SecretVault;
use crate::session::lock;
use krill_proto::SessionId;

const STACK_SIZE: usize = 128 * 1024;
const ACCEPT_POLL_SLEEP: Duration = Duration::from_millis(25);
const LOCAL_IO_TIMEOUT: Duration = Duration::from_secs(15);
const SSE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_LOCAL_HEADER_BYTES: usize = 64 * 1024;
const MAX_LOCAL_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_RELAY_WORKERS: usize = 32;
pub const BUILTIN_KRILL_CRED_ID: &str = "krill";
pub const BUILTIN_MCP_TOOL_COUNT: u32 = 13;

/// A single registered upstream MCP endpoint served through `/bot-tools/<cred_id>/mcp`.
#[derive(Clone)]
pub struct ToolRoute {
    pub cred_id: String,
    pub bot_id: BotId,
    pub tool_id: String,
    pub name: String,
    pub upstream_url: String,
    pub auth_kind: BotMcpAuthKind,
    pub header_names: Vec<String>,
    pub secret_ref: Option<String>,
    pub fallback_secret_key: String,
}

impl fmt::Debug for ToolRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolRoute")
            .field("cred_id", &self.cred_id)
            .field("bot_id", &self.bot_id)
            .field("tool_id", &self.tool_id)
            .field("auth_kind", &self.auth_kind)
            .finish_non_exhaustive()
    }
}

/// Caller identity bound to a built-in `/bot-tools/krill/mcp` JSON-RPC call.
#[derive(Debug, Clone)]
pub struct BuiltinMcpCallContext {
    pub bot_id: BotId,
    pub session_id: Option<SessionId>,
    pub link: Option<String>,
}

/// Callback handler invoked by `/bot-tools/krill/mcp` for built-in bot tool calls.
pub trait BuiltinMcpHandler: Send + Sync {
    fn call_tool(
        &self,
        ctx: &BuiltinMcpCallContext,
        tool_name: &str,
        arguments: &Value,
    ) -> Result<String, String>;
}

/// Resolved stdio MCP server ready to be handed cleanly to agent launch configuration.
#[derive(Clone)]
pub struct ResolvedStdioMcpTool {
    pub id: String,
    pub name: String,
    pub command: Vec<String>,
    pub env: Vec<(String, Zeroizing<String>)>,
}

impl fmt::Debug for ResolvedStdioMcpTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env_keys: Vec<&str> = self.env.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("ResolvedStdioMcpTool")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("command_len", &self.command.len())
            .field("env_keys", &env_keys)
            .finish()
    }
}

/// Ephemeral per-session credentials handed to a launched bot process.
#[derive(Clone)]
pub struct BotToolLaunchGrant {
    pub base_url: String,
    pub token: Zeroizing<String>,
    pub cred_ids: Vec<String>,
    pub stdio_tools: Vec<ResolvedStdioMcpTool>,
}

impl fmt::Debug for BotToolLaunchGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotToolLaunchGrant")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .field("cred_ids", &self.cred_ids)
            .field("stdio_tools", &self.stdio_tools.len())
            .finish()
    }
}

#[derive(Clone)]
struct TokenGrant {
    bot_id: BotId,
    token: Zeroizing<String>,
    allowed_creds: BTreeSet<String>,
    session_id: Option<SessionId>,
    link_id: Option<String>,
}

struct RelayState {
    routes: BTreeMap<String, ToolRoute>,
    grants: Vec<TokenGrant>,
    last_errors: BTreeMap<String, String>,
    tool_counts: BTreeMap<String, u32>,
    last_relay_error: Option<String>,
    builtin_handler: Option<Arc<dyn BuiltinMcpHandler>>,
    upstream_ssrf_policy: SsrfPolicy,
}

impl Default for RelayState {
    fn default() -> Self {
        Self {
            routes: BTreeMap::new(),
            grants: Vec::new(),
            last_errors: BTreeMap::new(),
            tool_counts: BTreeMap::new(),
            last_relay_error: None,
            builtin_handler: None,
            upstream_ssrf_policy: SsrfPolicy::StrictExternal,
        }
    }
}

struct RunningServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Strictly-lazy loopback MCP credential-injecting relay.
pub struct BotToolRelay {
    secrets: Arc<SecretVault>,
    state: Arc<Mutex<RelayState>>,
    server: Mutex<Option<RunningServer>>,
}

impl fmt::Debug for BotToolRelay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let running = self.server.lock().is_ok_and(|g| g.is_some());
        let route_count = self.state.lock().map_or(0, |s| s.routes.len());
        f.debug_struct("BotToolRelay")
            .field("running", &running)
            .field("routes", &route_count)
            .finish_non_exhaustive()
    }
}

impl Drop for BotToolRelay {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl BotToolRelay {
    /// Creates a lazy relay with zero sockets and zero threads.
    #[must_use]
    pub fn new(secrets: Arc<SecretVault>) -> Self {
        Self {
            secrets,
            state: Arc::new(Mutex::new(RelayState::default())),
            server: Mutex::new(None),
        }
    }

    /// Attaches the built-in `/bot-tools/krill/mcp` handler (called once on daemon startup).
    pub fn set_builtin_handler(&self, handler: Arc<dyn BuiltinMcpHandler>) {
        lock(&self.state).builtin_handler = Some(handler);
    }

    /// Configures outbound SSRF validation policy for proxied remote MCP endpoints (tests only).
    pub fn set_upstream_ssrf_policy(&self, policy: SsrfPolicy) {
        lock(&self.state).upstream_ssrf_policy = policy;
    }

    /// Registers a single upstream MCP route (used by MCP tools and Composio).
    pub fn register_route(&self, route: ToolRoute) {
        let mut st = lock(&self.state);
        st.last_errors.remove(&route.cred_id);
        st.routes.insert(route.cred_id.clone(), route);
    }

    /// Removes a single registered route by `cred_id` and stops the relay if unused.
    pub fn unregister_route(&self, cred_id: &str) {
        {
            let mut st = lock(&self.state);
            st.routes.remove(cred_id);
            st.last_errors.remove(cred_id);
            st.tool_counts.remove(cred_id);
        }
        self.stop_if_unused();
    }

    /// Updates `(session_id, link_id)` metadata attached to an active launch grant token.
    pub fn bind_grant_session(&self, token: &str, session_id: SessionId, link_id: Option<&str>) {
        let mut st = lock(&self.state);
        for grant in &mut st.grants {
            if constant_time_eq(grant.token.as_bytes(), token.as_bytes()) {
                grant.session_id = Some(session_id);
                if let Some(id) = link_id {
                    grant.link_id = Some(id.to_owned());
                }
            }
        }
    }

    /// Updates `link_id` on grants matching `(bot_id, session_id)`.
    pub fn update_session_link(&self, bot_id: &BotId, session_id: SessionId, link_id: &str) {
        let mut st = lock(&self.state);
        for grant in &mut st.grants {
            if &grant.bot_id == bot_id && grant.session_id == Some(session_id) {
                grant.link_id = Some(link_id.to_owned());
            }
        }
    }

    /// Resolves enabled stdio [`BotMcpToolConfig`] servers and their secret env vars cleanly.
    #[must_use]
    pub fn resolve_stdio_tools(&self, bot: &BotConfig) -> Vec<ResolvedStdioMcpTool> {
        let mut out = Vec::new();
        for tool in &bot.mcp_tools {
            if !tool.enabled || tool.command.is_empty() {
                continue;
            }
            let mut env = Vec::new();
            if let Ok(Some(secret_val)) = self.secrets.resolve_secret(
                &bot.id,
                tool.secret_ref.as_deref(),
                &format!("mcp:{}", tool.id),
            ) {
                if tool.header_names.is_empty() {
                    let fallback_var = format!(
                        "KRILL_MCP_{}_TOKEN",
                        tool.id
                            .chars()
                            .map(|c| {
                                if c.is_ascii_alphanumeric() {
                                    c.to_ascii_uppercase()
                                } else {
                                    '_'
                                }
                            })
                            .collect::<String>()
                    );
                    env.push((fallback_var, secret_val));
                } else {
                    for env_name in &tool.header_names {
                        let trimmed = env_name.trim();
                        if !trimmed.is_empty() {
                            env.push((trimmed.to_owned(), secret_val.clone()));
                        }
                    }
                }
            }
            out.push(ResolvedStdioMcpTool {
                id: tool.id.clone(),
                name: tool.name.clone(),
                command: tool.command.clone(),
                env,
            });
        }
        out
    }

    /// Synchronizes all enabled HTTP and stdio MCP tool routes for `bot` and mints an ephemeral
    /// launch token if any HTTP/Composio routes, stdio tools, or built-in handler are active.
    pub fn prepare_launch_for_bot(
        &self,
        bot: &BotConfig,
    ) -> io::Result<Option<BotToolLaunchGrant>> {
        let stdio_tools = self.resolve_stdio_tools(bot);
        let mut cred_ids = Vec::new();
        let has_builtin = {
            let mut st = lock(&self.state);
            // Replace non-composio MCP routes for this bot.
            st.routes
                .retain(|_, r| r.bot_id != bot.id || r.tool_id.starts_with("composio"));
            for tool in &bot.mcp_tools {
                if !tool.enabled {
                    continue;
                }
                let Some(url) = tool
                    .endpoint_url
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
                else {
                    continue;
                };
                let cred_id = make_cred_id(&bot.id, &tool.id);
                st.routes.insert(
                    cred_id.clone(),
                    ToolRoute {
                        cred_id,
                        bot_id: bot.id.clone(),
                        tool_id: tool.id.clone(),
                        name: tool.name.clone(),
                        upstream_url: url.to_owned(),
                        auth_kind: tool.auth_kind,
                        header_names: tool.header_names.clone(),
                        secret_ref: tool.secret_ref.clone(),
                        fallback_secret_key: format!("mcp:{}", tool.id),
                    },
                );
            }
            for (cid, r) in &st.routes {
                if r.bot_id == bot.id {
                    cred_ids.push(cid.clone());
                }
            }
            st.builtin_handler.is_some()
        };

        if cred_ids.is_empty() && stdio_tools.is_empty() && !has_builtin {
            self.stop_if_unused();
            return Ok(None);
        }

        let token = mint_ephemeral_token()?;
        let addr = {
            let mut srv = lock(&self.server);
            let addr = Self::ensure_started_locked(&mut srv, &self.state, &self.secrets)?;
            let mut st = lock(&self.state);
            let mut allowed_creds: BTreeSet<String> = cred_ids.iter().cloned().collect();
            allowed_creds.insert(BUILTIN_KRILL_CRED_ID.to_owned());
            st.grants.push(TokenGrant {
                bot_id: bot.id.clone(),
                token: token.clone(),
                allowed_creds,
                session_id: None,
                link_id: None,
            });
            addr
        };

        Ok(Some(BotToolLaunchGrant {
            base_url: format!("http://{addr}"),
            token,
            cred_ids,
            stdio_tools,
        }))
    }

    /// Mints an ephemeral Bearer token for a specific set of already-registered `cred_ids`
    /// and ensures the loopback listener is running.
    pub fn mint_grant_for_creds(
        &self,
        bot_id: &BotId,
        cred_ids: Vec<String>,
    ) -> io::Result<BotToolLaunchGrant> {
        let token = mint_ephemeral_token()?;
        let addr = {
            let mut srv = lock(&self.server);
            let addr = Self::ensure_started_locked(&mut srv, &self.state, &self.secrets)?;
            let mut st = lock(&self.state);
            let mut allowed_creds: BTreeSet<String> = cred_ids.iter().cloned().collect();
            allowed_creds.insert(BUILTIN_KRILL_CRED_ID.to_owned());
            st.grants.push(TokenGrant {
                bot_id: bot_id.clone(),
                token: token.clone(),
                allowed_creds,
                session_id: None,
                link_id: None,
            });
            addr
        };
        Ok(BotToolLaunchGrant {
            base_url: format!("http://{addr}"),
            token,
            cred_ids,
            stdio_tools: Vec::new(),
        })
    }

    /// Revokes a single launch token (called when a bot session ends) and stops the relay if unused.
    pub fn revoke_token(&self, token: &str) {
        {
            let mut st = lock(&self.state);
            st.grants
                .retain(|g| !constant_time_eq(g.token.as_bytes(), token.as_bytes()));
        }
        self.stop_if_unused();
    }

    /// Removes all routes and launch tokens for `bot_id` and stops the relay if unused.
    pub fn unregister_bot(&self, bot_id: &BotId) {
        {
            let mut st = lock(&self.state);
            st.routes.retain(|_, r| &r.bot_id != bot_id);
            st.grants.retain(|g| &g.bot_id != bot_id);
        }
        self.stop_if_unused();
    }

    /// Returns a live [`BotToolRelayStatus`] snapshot without starting the server.
    #[must_use]
    pub fn status(&self) -> BotToolRelayStatus {
        let addr = lock(&self.server).as_ref().map(|s| s.addr);
        let st = lock(&self.state);
        let mut connected_tools: Vec<BotToolServerStatus> = Vec::new();
        if let Some(a) = addr
            && (st.builtin_handler.is_some() || !st.grants.is_empty())
        {
            connected_tools.push(BotToolServerStatus {
                id: BUILTIN_KRILL_CRED_ID.to_owned(),
                name: "Krill Built-in Bot Tools".to_owned(),
                endpoint_url: format!("http://{a}/bot-tools/{BUILTIN_KRILL_CRED_ID}/mcp"),
                connected: true,
                tool_count: BUILTIN_MCP_TOOL_COUNT,
                last_error: None,
            });
        }
        for r in st.routes.values() {
            let err = st.last_errors.get(&r.cred_id).cloned();
            let tool_count = st.tool_counts.get(&r.cred_id).copied().unwrap_or(0);
            connected_tools.push(BotToolServerStatus {
                id: r.cred_id.clone(),
                name: r.name.clone(),
                endpoint_url: addr.map_or_else(
                    || r.upstream_url.clone(),
                    |a| format!("http://{a}/bot-tools/{}/mcp", r.cred_id),
                ),
                connected: addr.is_some() && err.is_none(),
                tool_count,
                last_error: err,
            });
        }
        BotToolRelayStatus {
            running: addr.is_some(),
            listen_addr: addr.map(|a| a.to_string()),
            endpoint_url: addr.map(|a| format!("http://{a}/bot-tools")),
            connected_tools,
            last_error: st.last_relay_error.clone(),
        }
    }

    /// Ensures the loopback listener is bound and serving.
    pub fn ensure_started(&self) -> io::Result<SocketAddr> {
        let mut guard = lock(&self.server);
        Self::ensure_started_locked(&mut guard, &self.state, &self.secrets)
    }

    fn ensure_started_locked(
        guard: &mut Option<RunningServer>,
        state: &Arc<Mutex<RelayState>>,
        secrets: &Arc<SecretVault>,
    ) -> io::Result<SocketAddr> {
        if let Some(ref running) = *guard {
            return Ok(running.addr);
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let state_bg = Arc::clone(state);
        let secrets_bg = Arc::clone(secrets);

        let handle = thread::Builder::new()
            .name("bot-tool-relay".to_owned())
            .stack_size(STACK_SIZE)
            .spawn(move || {
                run_relay_loop(&listener, addr, &stop_bg, &state_bg, &secrets_bg);
            })?;

        *guard = Some(RunningServer {
            addr,
            stop,
            thread: Some(handle),
        });
        Ok(addr)
    }

    /// Stops the listener when no active routes or launch grants remain.
    pub fn stop_if_unused(&self) {
        let running = {
            let mut srv = lock(&self.server);
            let empty = {
                let st = lock(&self.state);
                st.routes.is_empty() && st.grants.is_empty()
            };
            if empty { srv.take() } else { None }
        };
        if let Some(mut srv) = running {
            srv.stop.store(true, Ordering::Release);
            if let Some(handle) = srv.thread.take() {
                handle.join().ok();
            }
        }
    }

    /// Stops the loopback listener and joins its thread cleanly.
    pub fn shutdown(&self) {
        let running = lock(&self.server).take();
        if let Some(mut srv) = running {
            srv.stop.store(true, Ordering::Release);
            if let Some(handle) = srv.thread.take() {
                handle.join().ok();
            }
        }
    }
}

/// Builds a deterministic, URL-safe credential route ID for `(bot_id, tool_id)`.
#[must_use]
pub fn make_cred_id(bot_id: &BotId, tool_id: &str) -> String {
    let mut out = String::with_capacity(bot_id.as_str().len() + tool_id.len() + 1);
    for ch in bot_id.as_str().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out.push('-');
    for ch in tool_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

fn mint_ephemeral_token() -> io::Result<Zeroizing<String>> {
    let mut raw = Zeroizing::new([0u8; 32]);
    SystemRandom::new()
        .fill(raw.as_mut())
        .map_err(|_| io::Error::other("tool relay token RNG failure"))?;
    Ok(Zeroizing::new(crate::mobile::devices::hex(raw.as_ref())))
}

/// Constant-time equality comparison for secrets and Bearer tokens.
#[must_use]
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

struct WorkerSlotGuard(Arc<AtomicUsize>);

impl Drop for WorkerSlotGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn run_relay_loop(
    listener: &TcpListener,
    bound_addr: SocketAddr,
    stop: &Arc<AtomicBool>,
    state: &Arc<Mutex<RelayState>>,
    secrets: &Arc<SecretVault>,
) {
    let active_workers = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _peer)) => {
                let prev = active_workers.fetch_add(1, Ordering::AcqRel);
                if prev >= MAX_RELAY_WORKERS {
                    active_workers.fetch_sub(1, Ordering::AcqRel);
                    stream.set_nonblocking(false).ok();
                    write_simple_response(
                        &mut stream,
                        503,
                        "Service Unavailable",
                        b"relay worker limit reached",
                    )
                    .ok();
                    continue;
                }
                let state_w = Arc::clone(state);
                let secrets_w = Arc::clone(secrets);
                let stop_w = Arc::clone(stop);
                let workers_w = Arc::clone(&active_workers);
                let spawned = thread::Builder::new()
                    .name("bot-tool-relay-worker".to_owned())
                    .stack_size(STACK_SIZE)
                    .spawn(move || {
                        let _slot = WorkerSlotGuard(workers_w);
                        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            handle_connection(stream, bound_addr, &state_w, &secrets_w, &stop_w);
                        }));
                        if res.is_err() {
                            tracing::warn!("bot tool relay handler panicked; continuing");
                        }
                    });
                if let Err(err) = spawned {
                    active_workers.fetch_sub(1, Ordering::AcqRel);
                    tracing::warn!(kind = ?err.kind(), "failed to spawn bot tool relay worker");
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_SLEEP);
            }
            Err(err) => {
                tracing::warn!(kind = ?err.kind(), "bot tool relay accept error");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

struct ParsedLocalRequest {
    method: HttpMethod,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn validate_local_host_and_origin(
    headers: &[(String, String)],
    bound_addr: SocketAddr,
) -> Result<(), &'static str> {
    if headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("origin"))
    {
        return Err("Origin header forbidden on local tool relay");
    }
    let Some((_, host_raw)) = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("host")) else {
        return Err("missing Host header");
    };
    let host = host_raw.trim();
    let port = bound_addr.port();
    let ok = host == format!("127.0.0.1:{port}")
        || host.eq_ignore_ascii_case(&format!("localhost:{port}"))
        || host == format!("[::1]:{port}");
    if !ok {
        return Err("Host header mismatch");
    }
    Ok(())
}

fn handle_connection(
    mut stream: TcpStream,
    bound_addr: SocketAddr,
    state: &Mutex<RelayState>,
    secrets: &SecretVault,
    stop: &AtomicBool,
) {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(LOCAL_IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(LOCAL_IO_TIMEOUT)).ok();

    let Ok(req) = read_local_request(&mut stream) else {
        write_simple_response(&mut stream, 400, "Bad Request", b"bad request").ok();
        return;
    };

    if let Err(reason) = validate_local_host_and_origin(&req.headers, bound_addr) {
        write_simple_response(&mut stream, 403, "Forbidden", reason.as_bytes()).ok();
        return;
    }

    let Some((cred_id, suffix_and_query)) = parse_relay_target(&req.target) else {
        write_simple_response(&mut stream, 404, "Not Found", b"unknown tool route").ok();
        return;
    };

    let Some(token) = extract_bearer_token(&req.headers) else {
        write_simple_response(&mut stream, 401, "Unauthorized", b"missing bearer token").ok();
        return;
    };

    let (grant_opt, route_opt, builtin_handler, upstream_policy) = {
        let st = lock(state);
        let grant = st
            .grants
            .iter()
            .find(|g| {
                constant_time_eq(g.token.as_bytes(), token.as_bytes())
                    && (cred_id == BUILTIN_KRILL_CRED_ID || g.allowed_creds.contains(cred_id))
            })
            .cloned();
        let route = if grant.is_some() && cred_id != BUILTIN_KRILL_CRED_ID {
            st.routes.get(cred_id).cloned()
        } else {
            None
        };
        (
            grant,
            route,
            st.builtin_handler.clone(),
            st.upstream_ssrf_policy,
        )
    };

    let Some(grant) = grant_opt else {
        write_simple_response(
            &mut stream,
            401,
            "Unauthorized",
            b"invalid token or tool route",
        )
        .ok();
        return;
    };

    if cred_id == BUILTIN_KRILL_CRED_ID {
        handle_builtin_mcp_request(&mut stream, &req, &grant, builtin_handler.as_deref());
        return;
    }

    let Some(route) = route_opt else {
        write_simple_response(
            &mut stream,
            401,
            "Unauthorized",
            b"invalid token or tool route",
        )
        .ok();
        return;
    };

    let is_tools_list = request_is_tools_list(&req.body);
    let upstream_url = combine_upstream_url(&route.upstream_url, suffix_and_query);
    let mut upstream_req = HttpRequest::new(req.method, upstream_url)
        .with_body(req.body)
        .with_ssrf_policy(upstream_policy);

    for (name, value) in &req.headers {
        if is_hop_by_hop_or_auth_header(name) {
            continue;
        }
        upstream_req.headers.push((name.clone(), value.clone()));
    }

    if route.auth_kind != BotMcpAuthKind::None {
        let secret_res = if route.auth_kind == BotMcpAuthKind::BrowserOAuth {
            mcp_oauth::resolve_or_refresh_token(
                secrets,
                &route.bot_id,
                &route.tool_id,
                route.secret_ref.as_deref(),
                &route.fallback_secret_key,
                upstream_policy,
            )
        } else {
            secrets.resolve_secret(
                &route.bot_id,
                route.secret_ref.as_deref(),
                &route.fallback_secret_key,
            )
        };
        match secret_res {
            Ok(Some(secret_val)) => {
                inject_auth_header(&mut upstream_req.headers, &route, &secret_val);
            }
            Ok(None) => {
                if matches!(
                    route.auth_kind,
                    BotMcpAuthKind::BearerHeader | BotMcpAuthKind::BrowserOAuth
                ) {
                    lock(state)
                        .last_errors
                        .insert(route.cred_id.clone(), "missing tool credential".to_owned());
                    write_simple_response(
                        &mut stream,
                        502,
                        "Bad Gateway",
                        b"configured tool credential missing in vault",
                    )
                    .ok();
                    return;
                }
            }
            Err(_) => {
                lock(state)
                    .last_errors
                    .insert(route.cred_id.clone(), "vault read error".to_owned());
                write_simple_response(
                    &mut stream,
                    502,
                    "Bad Gateway",
                    b"failed to read tool credential",
                )
                .ok();
                return;
            }
        }
    }

    match http_client::send_streaming(&upstream_req) {
        Ok(mut streaming) => {
            if (300..400).contains(&streaming.status) {
                lock(state).last_errors.insert(
                    route.cred_id.clone(),
                    format!("upstream redirect blocked (HTTP {})", streaming.status),
                );
                write_simple_response(
                    &mut stream,
                    502,
                    "Bad Gateway",
                    b"upstream MCP redirect blocked",
                )
                .ok();
                return;
            }
            if streaming.is_event_stream() {
                lock(state).last_errors.remove(&route.cred_id);
                if write_streaming_response_head(&mut stream, streaming.status, &streaming.headers)
                    .is_err()
                {
                    return;
                }
                while !stop.load(Ordering::Acquire) {
                    match streaming.read_next_stream_chunk(SSE_IDLE_TIMEOUT, Some(stop)) {
                        Ok(Some(chunk)) => {
                            if stream.write_all(&chunk).is_err() || stream.flush().is_err() {
                                break;
                            }
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
                return;
            }
            match streaming.into_response(MAX_LOCAL_BODY_BYTES) {
                Ok(resp) => {
                    if is_tools_list && let Some(count) = extract_tools_list_count(&resp.body) {
                        lock(state).tool_counts.insert(route.cred_id.clone(), count);
                    }
                    lock(state).last_errors.remove(&route.cred_id);
                    write_forwarded_response(&mut stream, resp.status, &resp.headers, &resp.body)
                        .ok();
                }
                Err(err) => {
                    lock(state).last_errors.insert(
                        route.cred_id.clone(),
                        format!("upstream read error: {}", err.kind()),
                    );
                    write_simple_response(
                        &mut stream,
                        502,
                        "Bad Gateway",
                        b"upstream MCP response error",
                    )
                    .ok();
                }
            }
        }
        Err(err) => {
            lock(state).last_errors.insert(
                route.cred_id.clone(),
                format!("upstream error: {}", err.kind()),
            );
            write_simple_response(
                &mut stream,
                502,
                "Bad Gateway",
                b"upstream MCP server unreachable",
            )
            .ok();
        }
    }
}

fn handle_builtin_mcp_request(
    stream: &mut TcpStream,
    req: &ParsedLocalRequest,
    grant: &TokenGrant,
    handler: Option<&dyn BuiltinMcpHandler>,
) {
    let mcp_session_id = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("mcp-session-id"))
        .map_or_else(
            || format!("krill-{}", grant.bot_id.as_str()),
            |(_, v)| v.clone(),
        );
    let extra_headers = vec![
        ("Content-Type".to_owned(), "application/json".to_owned()),
        ("Mcp-Session-Id".to_owned(), mcp_session_id),
        ("Cache-Control".to_owned(), "no-store".to_owned()),
    ];

    if req.method != HttpMethod::Post {
        let err = json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": { "code": -32600, "message": "HTTP POST required for /bot-tools/krill/mcp" }
        });
        let body = serde_json::to_vec(&err).unwrap_or_default();
        write_forwarded_response(stream, 405, &extra_headers, &body).ok();
        return;
    }

    let Ok(rpc) = serde_json::from_slice::<Value>(&req.body) else {
        let err = json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": { "code": -32700, "message": "Parse error" }
        });
        let body = serde_json::to_vec(&err).unwrap_or_default();
        write_forwarded_response(stream, 400, &extra_headers, &body).ok();
        return;
    };

    let id = rpc.get("id").cloned();
    let method = rpc
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = rpc.get("params").cloned().unwrap_or_else(|| json!({}));

    if (method.starts_with("notifications/")
        || id.is_none()
        || id.as_ref().is_some_and(Value::is_null))
        && (method == "notifications/initialized" || id.is_none())
    {
        write_forwarded_response(stream, 202, &extra_headers, b"").ok();
        return;
    }
    let resp_id = id.unwrap_or(Value::Null);

    let payload = match method {
        "initialize" => {
            let client_ver = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            json!({
                "jsonrpc": "2.0",
                "id": resp_id,
                "result": {
                    "protocolVersion": client_ver,
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "krill-bot-mcp",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }
            })
        }
        "ping" => json!({ "jsonrpc": "2.0", "id": resp_id, "result": {} }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": resp_id,
            "result": { "tools": builtin_tools_schema() }
        }),
        "tools/call" => {
            let tool_name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if tool_name.is_empty() {
                json!({
                    "jsonrpc": "2.0",
                    "id": resp_id,
                    "error": { "code": -32602, "message": "missing tool name" }
                })
            } else if !is_builtin_tool_name(tool_name) {
                json!({
                    "jsonrpc": "2.0",
                    "id": resp_id,
                    "error": { "code": -32602, "message": format!("unknown built-in tool: {tool_name}") }
                })
            } else if let Some(h) = handler {
                let ctx = BuiltinMcpCallContext {
                    bot_id: grant.bot_id.clone(),
                    session_id: grant.session_id,
                    link: grant.link_id.clone(),
                };
                match h.call_tool(&ctx, tool_name, &arguments) {
                    Ok(text) => json!({
                        "jsonrpc": "2.0",
                        "id": resp_id,
                        "result": {
                            "content": [{ "type": "text", "text": text }],
                            "isError": false
                        }
                    }),
                    Err(err_text) => json!({
                        "jsonrpc": "2.0",
                        "id": resp_id,
                        "result": {
                            "content": [{ "type": "text", "text": err_text }],
                            "isError": true
                        }
                    }),
                }
            } else {
                json!({
                    "jsonrpc": "2.0",
                    "id": resp_id,
                    "error": { "code": -32603, "message": "built-in MCP handler not attached" }
                })
            }
        }
        other => json!({
            "jsonrpc": "2.0",
            "id": resp_id,
            "error": { "code": -32601, "message": format!("Method not found: {other}") }
        }),
    };

    let body = serde_json::to_vec(&payload).unwrap_or_default();
    write_forwarded_response(stream, 200, &extra_headers, &body).ok();
}

fn is_builtin_tool_name(name: &str) -> bool {
    matches!(
        name,
        "items_list"
            | "items_create"
            | "items_update"
            | "memory_get"
            | "memory_update"
            | "memory_forget"
            | "memory_history"
            | "mail_send"
            | "group_list"
            | "group_send"
            | "access_list"
            | "ask_owner"
            | "reply"
    )
}

fn builtin_tools_schema() -> Value {
    json!([
        {
            "name": "items_list",
            "description": "List tracked deliverables and owner-decision items for this bot.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "include_closed": { "type": "boolean" },
                    "include_snoozed": { "type": "boolean" },
                    "limit": { "type": "integer" },
                    "cursor": { "type": "string" }
                }
            }
        },
        {
            "name": "items_create",
            "description": "Create a tracked deliverable/work item for this bot.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "done_means": { "type": "string" },
                    "status": { "type": "string", "enum": ["planned", "working", "needs_you", "done", "canceled"] }
                },
                "required": ["title"]
            }
        },
        {
            "name": "items_update",
            "description": "Update an existing tracked item (status, comment, title, description, done_means, close_note).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "item_id": { "type": "string" },
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "done_means": { "type": "string" },
                    "status": { "type": "string", "enum": ["planned", "working", "needs_you", "done", "canceled"] },
                    "comment": { "type": "string" },
                    "close_note": { "type": "string" }
                },
                "required": ["item_id"]
            }
        },
        {
            "name": "memory_get",
            "description": "Read this bot's durable MEMORY.md contents.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "memory_update",
            "description": "Update this bot's MEMORY.md (deletions blocked unless allow_deletions=true).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": { "type": "string" },
                    "allow_deletions": { "type": "boolean" }
                },
                "required": ["content"]
            }
        },
        {
            "name": "memory_forget",
            "description": "Explicitly forget facts matching a text query from MEMORY.md.",
            "inputSchema": {
                "type": "object",
                "properties": { "fact": { "type": "string" } },
                "required": ["fact"]
            }
        },
        {
            "name": "memory_history",
            "description": "Inspect audit history entries for MEMORY.md changes.",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer" } }
            }
        },
        {
            "name": "mail_send",
            "description": "Send asynchronous bot-to-bot mail to another bot.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to_bot": { "type": "string" },
                    "subject": { "type": "string" },
                    "body": { "type": "string" },
                    "thread_id": { "type": "string" }
                },
                "required": ["to_bot", "subject", "body"]
            }
        },
        {
            "name": "group_list",
            "description": "List bot group chat rooms available to this bot.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "group_send",
            "description": "Post a message into a multi-bot group chat room.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "group_id": { "type": "string" },
                    "text": { "type": "string" },
                    "thread_id": { "type": "string" },
                    "reply_mode": { "type": "string", "enum": ["mentions", "all", "selected"] }
                },
                "required": ["group_id", "text"]
            }
        },
        {
            "name": "access_list",
            "description": "List pending or resolved external access requests for this bot.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "ask_owner",
            "description": "Escalate a question or structured decision gate (NeedsYou item) to the human owner.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "item_id": { "type": "string" },
                    "title": { "type": "string" },
                    "question": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" } },
                    "recommended": { "type": "integer" },
                    "done_means": { "type": "string" }
                },
                "required": ["question"]
            }
        },
        {
            "name": "reply",
            "description": "Send an explicit reply, file attachment, or reaction back to the active external conversation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "files": { "type": "array", "items": { "type": "string" } },
                    "reaction": { "type": "string" },
                    "link_id": { "type": "string" }
                }
            }
        }
    ])
}

fn request_is_tools_list(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("method")
                .and_then(Value::as_str)
                .map(|m| m == "tools/list")
        })
        .unwrap_or(false)
}

fn extract_tools_list_count(body: &[u8]) -> Option<u32> {
    let val = serde_json::from_slice::<Value>(body).ok()?;
    let tools = val.get("result")?.get("tools")?.as_array()?;
    u32::try_from(tools.len()).ok()
}

fn inject_auth_header(
    headers: &mut Vec<(String, String)>,
    route: &ToolRoute,
    secret_val: &Zeroizing<String>,
) {
    let header_name = route
        .header_names
        .first()
        .map(String::as_str)
        .filter(|h| !h.trim().is_empty())
        .unwrap_or("Authorization");
    headers.retain(|(k, _)| !k.eq_ignore_ascii_case(header_name));

    let header_value = if header_name.eq_ignore_ascii_case("authorization") {
        let trimmed = secret_val.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("bearer ")
            || lower.starts_with("basic ")
            || lower.starts_with("token ")
        {
            trimmed.to_owned()
        } else {
            format!("Bearer {trimmed}")
        }
    } else {
        secret_val.trim().to_owned()
    };
    headers.push((header_name.to_owned(), header_value));
}

fn parse_relay_target(target: &str) -> Option<(&str, &str)> {
    let rest = target.strip_prefix("/bot-tools/")?;
    let slash = rest.find('/')?;
    let cred_id = rest.get(..slash)?;
    if cred_id.is_empty() {
        return None;
    }
    let after = rest.get(slash..)?;
    if let Some(tail) = after.strip_prefix("/mcp")
        && (tail.is_empty() || tail.starts_with('/') || tail.starts_with('?'))
    {
        return Some((cred_id, tail));
    }
    None
}

fn combine_upstream_url(base: &str, suffix_and_query: &str) -> String {
    if suffix_and_query.is_empty() {
        return base.to_owned();
    }
    if suffix_and_query.starts_with('?') {
        if base.contains('?') {
            let trimmed = suffix_and_query.trim_start_matches('?');
            format!("{base}&{trimmed}")
        } else {
            format!("{base}{suffix_and_query}")
        }
    } else {
        format!("{}{suffix_and_query}", base.trim_end_matches('/'))
    }
}

fn extract_bearer_token(headers: &[(String, String)]) -> Option<&str> {
    let raw = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.trim())?;
    let prefix = raw.get(..7)?;
    if !prefix.eq_ignore_ascii_case("bearer ") {
        return None;
    }
    let tok = raw.get(7..)?.trim();
    (!tok.is_empty()).then_some(tok)
}

fn is_hop_by_hop_or_auth_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case("host")
        || name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("content-length")
        || name.eq_ignore_ascii_case("transfer-encoding")
        || name.eq_ignore_ascii_case("keep-alive")
        || name.eq_ignore_ascii_case("proxy-connection")
}

fn read_local_request(stream: &mut TcpStream) -> io::Result<ParsedLocalRequest> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() >= MAX_LOCAL_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "local relay request headers too large",
            ));
        }
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "EOF reading local relay request",
            ));
        }
        buf.extend_from_slice(tmp.get(..n).unwrap_or_default());
    };

    let head = std::str::from_utf8(buf.get(..header_end).unwrap_or_default())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 header"))?;
    let mut lines = head.split("\r\n");
    let req_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut parts = req_line.split_whitespace();
    let method_str = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing method"))?;
    let target = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing target"))?
        .to_owned();
    let method = HttpMethod::parse(method_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unsupported HTTP method"))?;

    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }

    let is_chunked = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding")
            && v.split(',')
                .any(|tok| tok.trim().eq_ignore_ascii_case("chunked"))
    });
    let body_start = header_end.saturating_add(4);
    let initial = buf.get(body_start..).unwrap_or_default().to_vec();

    let body = if is_chunked {
        read_local_chunked_body(stream, initial, MAX_LOCAL_BODY_BYTES)?
    } else {
        let content_len = match headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        {
            Some((_, v)) => v.trim().parse::<usize>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid local Content-Length")
            })?,
            None => 0,
        };
        if content_len > MAX_LOCAL_BODY_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "local relay request body too large",
            ));
        }

        let mut body = initial;
        while body.len() < content_len {
            let remaining = content_len.saturating_sub(body.len());
            let chunk_len = remaining.min(tmp.len());
            let n = stream.read(tmp.get_mut(..chunk_len).unwrap_or(&mut []))?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated local relay body",
                ));
            }
            body.extend_from_slice(tmp.get(..n).unwrap_or_default());
        }
        body.truncate(content_len);
        body
    };

    Ok(ParsedLocalRequest {
        method,
        target,
        headers,
        body,
    })
}

fn read_local_chunked_body(
    stream: &mut TcpStream,
    mut raw: Vec<u8>,
    max_body_bytes: usize,
) -> io::Result<Vec<u8>> {
    const MAX_CHUNK_LINE_BYTES: usize = 1024;
    let mut cursor = 0usize;
    let mut out = Vec::new();
    let mut tmp = [0u8; 4096];

    loop {
        if cursor >= 8192 {
            raw.drain(..cursor);
            cursor = 0;
        }
        let crlf_rel = loop {
            let slice = raw.get(cursor..).unwrap_or_default();
            if let Some(pos) = slice.windows(2).position(|w| w == b"\r\n") {
                break pos;
            }
            if slice.len() > MAX_CHUNK_LINE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "chunk header line too long",
                ));
            }
            let n = stream.read(&mut tmp)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF reading local chunked request size",
                ));
            }
            raw.extend_from_slice(tmp.get(..n).unwrap_or_default());
        };

        let line_bytes = raw
            .get(cursor..cursor.saturating_add(crlf_rel))
            .unwrap_or_default();
        let line = std::str::from_utf8(line_bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid chunk line UTF-8"))?;
        let hex_part = line.split(';').next().unwrap_or(line).trim();
        let chunk_len = usize::from_str_radix(hex_part, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid chunk size hex"))?;
        cursor = cursor.saturating_add(crlf_rel).saturating_add(2);

        if chunk_len == 0 {
            let mut trailer_bytes = 0usize;
            loop {
                let t_crlf = loop {
                    let slice = raw.get(cursor..).unwrap_or_default();
                    if let Some(pos) = slice.windows(2).position(|w| w == b"\r\n") {
                        break pos;
                    }
                    if trailer_bytes.saturating_add(slice.len()) > MAX_LOCAL_HEADER_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "local chunked trailers exceed limit",
                        ));
                    }
                    let n = stream.read(&mut tmp)?;
                    if n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "EOF reading local chunked trailers",
                        ));
                    }
                    raw.extend_from_slice(tmp.get(..n).unwrap_or_default());
                };
                cursor = cursor.saturating_add(t_crlf).saturating_add(2);
                trailer_bytes = trailer_bytes.saturating_add(t_crlf).saturating_add(2);
                if trailer_bytes > MAX_LOCAL_HEADER_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "local chunked trailers exceed limit",
                    ));
                }
                if t_crlf == 0 {
                    break;
                }
            }
            break;
        }

        if out.len().saturating_add(chunk_len) > max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "local relay chunked body too large",
            ));
        }

        let needed = chunk_len.saturating_add(2);
        while raw.len().saturating_sub(cursor) < needed {
            let n = stream.read(&mut tmp)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated local chunked request body",
                ));
            }
            raw.extend_from_slice(tmp.get(..n).unwrap_or_default());
        }
        let data_end = cursor.saturating_add(chunk_len);
        out.extend_from_slice(raw.get(cursor..data_end).unwrap_or_default());
        if raw.get(data_end..data_end.saturating_add(2)) != Some(b"\r\n".as_slice()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing CRLF after local HTTP chunk",
            ));
        }
        cursor = data_end.saturating_add(2);
    }

    Ok(out)
}

fn write_simple_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &[u8],
) -> io::Result<()> {
    let mut head = String::new();
    write!(
        head,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .ok();
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn http_status_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Response",
    }
}

fn sanitize_header_token(raw: &str) -> String {
    raw.chars()
        .filter(|c| !matches!(c, '\r' | '\n' | '\0'))
        .collect()
}

fn write_streaming_response_head(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(String, String)],
) -> io::Result<()> {
    let reason = http_status_reason(status);
    let mut head = String::with_capacity(256);
    write!(head, "HTTP/1.1 {status} {reason}\r\n").ok();
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("transfer-encoding")
            || k.eq_ignore_ascii_case("content-length")
            || k.eq_ignore_ascii_case("connection")
        {
            continue;
        }
        let ck = sanitize_header_token(k);
        let cv = sanitize_header_token(v);
        if !ck.is_empty() {
            write!(head, "{ck}: {cv}\r\n").ok();
        }
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    stream.flush()
}

fn write_forwarded_response(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> io::Result<()> {
    let reason = http_status_reason(status);
    let mut head = String::with_capacity(256);
    write!(head, "HTTP/1.1 {status} {reason}\r\n").ok();
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("transfer-encoding")
            || k.eq_ignore_ascii_case("content-length")
            || k.eq_ignore_ascii_case("connection")
        {
            continue;
        }
        let ck = sanitize_header_token(k);
        let cv = sanitize_header_token(v);
        if !ck.is_empty() {
            write!(head, "{ck}: {cv}\r\n").ok();
        }
    }
    write!(
        head,
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .ok();
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::BotMcpToolConfig;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-relay-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn relay_is_lazy_validates_bearer_and_injects_vault_secret_upstream() {
        let dir = temp_dir("inject");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x55; 32]));
        let relay = BotToolRelay::new(Arc::clone(&secrets));
        relay.set_upstream_ssrf_policy(SsrfPolicy::AllowLoopback);
        assert!(!relay.status().running);

        // Mock upstream MCP server capturing received Authorization / x-api-key header.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(String::new()));
        let captured_bg = Arc::clone(&captured);
        let upstream_thread = thread::spawn(move || {
            for idx in 0..2 {
                let (mut sock, _) = upstream.accept().unwrap();
                let mut req_buf = [0u8; 4096];
                let n = sock.read(&mut req_buf).unwrap();
                let txt =
                    String::from_utf8_lossy(req_buf.get(..n).unwrap_or_default()).into_owned();
                if idx == 0 {
                    *lock(&captured_bg) = txt;
                }
                let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"search"},{"name":"create"}]}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: up-sess-42\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                sock.write_all(resp.as_bytes()).unwrap();
            }
        });

        let bot_id = BotId::new("bot-mcp").unwrap();
        secrets
            .set_ref(
                "mcp-secret-1",
                Zeroizing::new("upstream-api-key-xyz".to_owned()),
            )
            .unwrap();

        let mut bot = BotConfig::new(bot_id.clone(), "MCP Bot", "claude", 1_000).unwrap();
        bot.mcp_tools.push(BotMcpToolConfig {
            id: "linear".to_owned(),
            name: "Linear".to_owned(),
            endpoint_url: Some(format!("http://{upstream_addr}/mcp")),
            command: Vec::new(),
            auth_kind: BotMcpAuthKind::BearerHeader,
            header_names: vec!["x-api-key".to_owned()],
            secret_ref: Some("mcp-secret-1".to_owned()),
            enabled: true,
        });

        let grant = relay.prepare_launch_for_bot(&bot).unwrap().unwrap();
        assert!(relay.status().running);
        let cred_id = grant.cred_ids.first().unwrap();
        let relay_url = format!("{}/bot-tools/{cred_id}/mcp", grant.base_url);

        // Rejects wrong Host or present Origin with 403 Forbidden.
        let origin_bad = http_client::send(
            &HttpRequest::post_json(&relay_url, b"{}".to_vec())
                .with_header("Authorization", format!("Bearer {}", *grant.token))
                .with_header("Origin", "https://evil.example")
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(origin_bad.status, 403);

        // Rejects wrong bearer token.
        let bad = http_client::send(
            &HttpRequest::post_json(&relay_url, b"{}".to_vec())
                .with_header("Authorization", "Bearer wrong-token")
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(bad.status, 401);

        // Accepts valid launch token, forwards Mcp-Session-Id, injects key, updates tool_count.
        let ok = http_client::send(
            &HttpRequest::post_json(
                &relay_url,
                br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_vec(),
            )
            .with_header("Authorization", format!("Bearer {}", *grant.token))
            .with_header("Mcp-Session-Id", "client-sess-9")
            .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(ok.status, 200);
        assert_eq!(ok.header("Mcp-Session-Id"), Some("up-sess-42"));
        assert!(ok.text().contains("tools"));

        // Verify tool_count updated to 2 for linear route.
        let st = relay.status();
        let linear_st = st
            .connected_tools
            .iter()
            .find(|t| t.id == *cred_id)
            .unwrap();
        assert_eq!(linear_st.tool_count, 2);

        // Also send second request so mock finishes cleanly.
        let ok2 = http_client::send(
            &HttpRequest::post_json(
                &relay_url,
                br#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#.to_vec(),
            )
            .with_header("Authorization", format!("Bearer {}", *grant.token))
            .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(ok2.status, 200);

        upstream_thread.join().unwrap();
        let seen = lock(&captured).clone();
        assert!(seen.contains("x-api-key: upstream-api-key-xyz"));
        assert!(seen.contains("Mcp-Session-Id: client-sess-9"));
        assert!(!seen.contains(&*grant.token));

        relay.unregister_bot(&bot_id);
        assert!(!relay.status().running);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn relay_builtin_krill_mcp_redirect_block_sse_stream_and_stdio_work() {
        struct MockBuiltin;
        impl BuiltinMcpHandler for MockBuiltin {
            fn call_tool(
                &self,
                ctx: &BuiltinMcpCallContext,
                tool_name: &str,
                arguments: &Value,
            ) -> Result<String, String> {
                Ok(format!(
                    "bot={} link={:?} tool={} args={arguments}",
                    ctx.bot_id.as_str(),
                    ctx.link,
                    tool_name
                ))
            }
        }

        let dir = temp_dir("builtin-sse");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x77; 32]));
        let relay = BotToolRelay::new(Arc::clone(&secrets));
        relay.set_builtin_handler(Arc::new(MockBuiltin));

        // Mock upstream that serves a 302 redirect on first call and SSE stream on second call.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let up_addr = upstream.local_addr().unwrap();
        let up_thread = thread::spawn(move || {
            for idx in 0..2 {
                let (mut sock, _) = upstream.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                assert!(n > 0);
                if idx == 0 {
                    sock.write_all(
                        b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\n\r\n",
                    )
                    .unwrap();
                } else {
                    sock.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: sse-1\r\nTransfer-Encoding: chunked\r\n\r\n19\r\nevent: message\ndata: ok\n\n\r\n0\r\n\r\n",
                    )
                    .unwrap();
                }
            }
        });

        let bot_id = BotId::new("bot-k").unwrap();
        secrets
            .set_ref("stdio-tok", Zeroizing::new("std-secret-val".to_owned()))
            .unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "Krill Builtin Bot", "claude", 1_000).unwrap();
        bot.mcp_tools.push(BotMcpToolConfig {
            id: "redir".to_owned(),
            name: "Redir".to_owned(),
            endpoint_url: Some(format!("http://{up_addr}/mcp")),
            command: Vec::new(),
            auth_kind: BotMcpAuthKind::None,
            header_names: Vec::new(),
            secret_ref: None,
            enabled: true,
        });
        bot.mcp_tools.push(BotMcpToolConfig {
            id: "local-fs".to_owned(),
            name: "Local FS".to_owned(),
            endpoint_url: None,
            command: vec!["npx".to_owned(), "-y".to_owned(), "mcp-fs".to_owned()],
            auth_kind: BotMcpAuthKind::BearerHeader,
            header_names: vec!["FS_API_TOKEN".to_owned()],
            secret_ref: Some("stdio-tok".to_owned()),
            enabled: true,
        });

        let grant = relay.prepare_launch_for_bot(&bot).unwrap().unwrap();
        assert_eq!(grant.stdio_tools.len(), 1);
        assert_eq!(grant.stdio_tools[0].command, vec!["npx", "-y", "mcp-fs"]);
        assert_eq!(grant.stdio_tools[0].env[0].0, "FS_API_TOKEN");
        assert_eq!(&*grant.stdio_tools[0].env[0].1, "std-secret-val");
        relay.bind_grant_session(&grant.token, SessionId(55), Some("link-abc"));

        // 1. StrictExternal blocks loopback upstream by default -> 502 Bad Gateway.
        let cred_id = make_cred_id(&bot_id, "redir");
        let blocked = http_client::send(
            &HttpRequest::post_json(
                format!("{}/bot-tools/{cred_id}/mcp", grant.base_url),
                b"{}".to_vec(),
            )
            .with_header("Authorization", format!("Bearer {}", *grant.token))
            .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(blocked.status, 502);

        // 2. Allow loopback on upstream -> 302 redirect still blocked with 502; SSE streamed cleanly.
        relay.set_upstream_ssrf_policy(SsrfPolicy::AllowLoopback);
        let redir = http_client::send(
            &HttpRequest::post_json(
                format!("{}/bot-tools/{cred_id}/mcp", grant.base_url),
                b"{}".to_vec(),
            )
            .with_header("Authorization", format!("Bearer {}", *grant.token))
            .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(redir.status, 502);
        assert!(redir.text().contains("redirect blocked"));

        let sse = http_client::send(
            &HttpRequest::get(format!("{}/bot-tools/{cred_id}/mcp", grant.base_url))
                .with_header("Authorization", format!("Bearer {}", *grant.token))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(sse.status, 200);
        assert_eq!(sse.header("Mcp-Session-Id"), Some("sse-1"));
        assert_eq!(sse.text(), "event: message\ndata: ok\n\n");

        // 3. Built-in /bot-tools/krill/mcp JSON-RPC initialize, tools/list, tools/call.
        let krill_url = format!("{}/bot-tools/krill/mcp", grant.base_url);
        let list_resp = http_client::send(
            &HttpRequest::post_json(
                &krill_url,
                br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_vec(),
            )
            .with_header("Authorization", format!("Bearer {}", *grant.token))
            .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(list_resp.status, 200);
        let list_json: Value = list_resp.json().unwrap();
        assert_eq!(
            list_json["result"]["tools"].as_array().unwrap().len(),
            BUILTIN_MCP_TOOL_COUNT as usize
        );

        let call_resp = http_client::send(
            &HttpRequest::post_json(
                &krill_url,
                br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"reply","arguments":{"text":"hi"}}}"#.to_vec(),
            )
            .with_header("Authorization", format!("Bearer {}", *grant.token))
            .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(call_resp.status, 200);
        let call_json: Value = call_resp.json().unwrap();
        let text = call_json["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("bot=bot-k link=Some(\"link-abc\") tool=reply"));

        up_thread.join().unwrap();
        relay.unregister_bot(&bot_id);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn local_relay_accepts_chunked_post_and_shutdown_interrupts_idle_sse_worker() {
        let dir = temp_dir("chunked-sse-stop");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x5a; 32]));
        let relay = BotToolRelay::new(Arc::clone(&secrets));
        relay.set_upstream_ssrf_policy(SsrfPolicy::AllowLoopback);

        // Upstream that holds an SSE stream open until closed.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let up_addr = upstream.local_addr().unwrap();
        let sse_connected = Arc::new(AtomicBool::new(false));
        let sse_connected_bg = Arc::clone(&sse_connected);
        let up_stop = Arc::new(AtomicBool::new(false));
        let up_stop_bg = Arc::clone(&up_stop);
        let up_thread = thread::spawn(move || {
            let (mut sock, _) = upstream.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).unwrap_or(0);
            assert!(n > 0);
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
            )
            .ok();
            sock.write_all(b"b\r\ndata: init\n\r\n").ok();
            sock.flush().ok();
            sse_connected_bg.store(true, Ordering::Release);
            while !up_stop_bg.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(20));
            }
        });

        let bot_id = BotId::new("bot-chunk").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "ChunkBot", "claude", 1_000).unwrap();
        bot.mcp_tools.push(BotMcpToolConfig {
            id: "sse".to_owned(),
            name: "SSE".to_owned(),
            endpoint_url: Some(format!("http://{up_addr}/mcp")),
            command: Vec::new(),
            auth_kind: BotMcpAuthKind::None,
            header_names: Vec::new(),
            secret_ref: None,
            enabled: true,
        });
        let grant = relay.prepare_launch_for_bot(&bot).unwrap().unwrap();
        let host = grant.base_url.trim_start_matches("http://");

        // 1. Send chunked POST JSON-RPC `ping` to built-in `/bot-tools/krill/mcp`.
        let mut client = TcpStream::connect(host).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let p1 = br#"{"jsonrpc":"2.0","id":7,"#;
        let p2 = br#""method":"ping"}"#;
        let wire = format!(
            "POST /bot-tools/krill/mcp HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            *grant.token,
            p1.len(),
            std::str::from_utf8(p1).unwrap(),
            p2.len(),
            std::str::from_utf8(p2).unwrap(),
        );
        client.write_all(wire.as_bytes()).unwrap();
        client.flush().unwrap();
        let mut resp_bytes = Vec::new();
        client.read_to_end(&mut resp_bytes).unwrap();
        let resp_str = String::from_utf8_lossy(&resp_bytes);
        assert!(resp_str.starts_with("HTTP/1.1 200 OK"), "{resp_str}");
        assert!(resp_str.contains("\"id\":7"), "{resp_str}");

        // 2. Open an SSE stream to the upstream and confirm `shutdown()` cancels it promptly (<2s).
        let cred_id = grant.cred_ids.first().unwrap().clone();
        let mut sse_client = TcpStream::connect(host).unwrap();
        sse_client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let sse_req = format!(
            "GET /bot-tools/{cred_id}/mcp HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\n\r\n",
            *grant.token
        );
        sse_client.write_all(sse_req.as_bytes()).unwrap();
        sse_client.flush().unwrap();
        let mut head_buf = [0u8; 512];
        let n = sse_client.read(&mut head_buf).unwrap();
        assert!(String::from_utf8_lossy(&head_buf[..n]).contains("200 OK"));

        let started = std::time::Instant::now();
        relay.shutdown();
        let mut tail = Vec::new();
        sse_client.read_to_end(&mut tail).ok();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "SSE worker was not interrupted promptly on shutdown: {:?}",
            started.elapsed()
        );

        up_stop.store(true, Ordering::Release);
        up_thread.join().ok();
        fs::remove_dir_all(&dir).ok();
    }
}
