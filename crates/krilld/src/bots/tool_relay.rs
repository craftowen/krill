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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use krill_proto::bots::{
    BotConfig, BotId, BotMcpAuthKind, BotToolRelayStatus, BotToolServerStatus,
};
use ring::rand::{SecureRandom as _, SystemRandom};
use zeroize::Zeroizing;

use crate::bots::http_client::{self, HttpMethod, HttpRequest};
use crate::bots::secrets::SecretVault;
use crate::session::lock;

const STACK_SIZE: usize = 128 * 1024;
const ACCEPT_POLL_SLEEP: Duration = Duration::from_millis(25);
const LOCAL_IO_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_LOCAL_HEADER_BYTES: usize = 64 * 1024;
const MAX_LOCAL_BODY_BYTES: usize = 4 * 1024 * 1024;

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

/// Ephemeral per-session credentials handed to a launched bot process.
#[derive(Clone)]
pub struct BotToolLaunchGrant {
    pub base_url: String,
    pub token: Zeroizing<String>,
    pub cred_ids: Vec<String>,
}

impl fmt::Debug for BotToolLaunchGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotToolLaunchGrant")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .field("cred_ids", &self.cred_ids)
            .finish()
    }
}

#[derive(Clone)]
struct TokenGrant {
    bot_id: BotId,
    token: Zeroizing<String>,
    allowed_creds: BTreeSet<String>,
}

#[derive(Default)]
struct RelayState {
    routes: BTreeMap<String, ToolRoute>,
    grants: Vec<TokenGrant>,
    last_errors: BTreeMap<String, String>,
    last_relay_error: Option<String>,
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

    /// Registers a single upstream MCP route (used by MCP tools and Composio).
    pub fn register_route(&self, route: ToolRoute) {
        let mut st = lock(&self.state);
        st.last_errors.remove(&route.cred_id);
        st.routes.insert(route.cred_id.clone(), route);
    }

    /// Synchronizes all enabled HTTP MCP tool routes for `bot` and mints an ephemeral launch token
    /// if any routes (including Composio) are registered for `bot`. Starts the listener lazily.
    pub fn prepare_launch_for_bot(
        &self,
        bot: &BotConfig,
    ) -> io::Result<Option<BotToolLaunchGrant>> {
        let mut cred_ids = Vec::new();
        {
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
        }

        if cred_ids.is_empty() {
            self.stop_if_unused();
            return Ok(None);
        }

        let addr = self.ensure_started()?;
        let token = mint_ephemeral_token()?;
        {
            let mut st = lock(&self.state);
            st.grants.push(TokenGrant {
                bot_id: bot.id.clone(),
                token: token.clone(),
                allowed_creds: cred_ids.iter().cloned().collect(),
            });
        }

        Ok(Some(BotToolLaunchGrant {
            base_url: format!("http://{addr}"),
            token,
            cred_ids,
        }))
    }

    /// Mints an ephemeral Bearer token for a specific set of already-registered `cred_ids`
    /// and ensures the loopback listener is running.
    pub fn mint_grant_for_creds(
        &self,
        bot_id: &BotId,
        cred_ids: Vec<String>,
    ) -> io::Result<BotToolLaunchGrant> {
        let addr = self.ensure_started()?;
        let token = mint_ephemeral_token()?;
        {
            let mut st = lock(&self.state);
            st.grants.push(TokenGrant {
                bot_id: bot_id.clone(),
                token: token.clone(),
                allowed_creds: cred_ids.iter().cloned().collect(),
            });
        }
        Ok(BotToolLaunchGrant {
            base_url: format!("http://{addr}"),
            token,
            cred_ids,
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
        let connected_tools = st
            .routes
            .values()
            .map(|r| {
                let err = st.last_errors.get(&r.cred_id).cloned();
                BotToolServerStatus {
                    id: r.cred_id.clone(),
                    name: r.name.clone(),
                    endpoint_url: addr.map_or_else(
                        || r.upstream_url.clone(),
                        |a| format!("http://{a}/bot-tools/{}/mcp", r.cred_id),
                    ),
                    connected: addr.is_some() && err.is_none(),
                    tool_count: 0,
                    last_error: err,
                }
            })
            .collect();
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
        if let Some(ref running) = *guard {
            return Ok(running.addr);
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let state_bg = Arc::clone(&self.state);
        let secrets_bg = Arc::clone(&self.secrets);

        let handle = thread::Builder::new()
            .name("bot-tool-relay".to_owned())
            .stack_size(STACK_SIZE)
            .spawn(move || {
                run_relay_loop(&listener, &stop_bg, &state_bg, &secrets_bg);
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
        let empty = {
            let st = lock(&self.state);
            st.routes.is_empty() && st.grants.is_empty()
        };
        if empty {
            self.shutdown();
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

fn run_relay_loop(
    listener: &TcpListener,
    stop: &AtomicBool,
    state: &Arc<Mutex<RelayState>>,
    secrets: &Arc<SecretVault>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                let state = Arc::clone(state);
                let secrets = Arc::clone(secrets);
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handle_connection(stream, &state, &secrets);
                }));
                if res.is_err() {
                    tracing::warn!("bot tool relay handler panicked; continuing");
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

fn handle_connection(mut stream: TcpStream, state: &Mutex<RelayState>, secrets: &SecretVault) {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(LOCAL_IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(LOCAL_IO_TIMEOUT)).ok();

    let Ok(req) = read_local_request(&mut stream) else {
        write_simple_response(&mut stream, 400, "Bad Request", b"bad request").ok();
        return;
    };

    let Some((cred_id, suffix_and_query)) = parse_relay_target(&req.target) else {
        write_simple_response(&mut stream, 404, "Not Found", b"unknown tool route").ok();
        return;
    };

    let Some(token) = extract_bearer_token(&req.headers) else {
        write_simple_response(&mut stream, 401, "Unauthorized", b"missing bearer token").ok();
        return;
    };

    let route = {
        let st = lock(state);
        let authorized = st.grants.iter().any(|g| {
            g.allowed_creds.contains(cred_id)
                && constant_time_eq(g.token.as_bytes(), token.as_bytes())
        });
        if authorized {
            st.routes.get(cred_id).cloned()
        } else {
            None
        }
    };

    let Some(route) = route else {
        write_simple_response(
            &mut stream,
            401,
            "Unauthorized",
            b"invalid token or tool route",
        )
        .ok();
        return;
    };

    let upstream_url = combine_upstream_url(&route.upstream_url, suffix_and_query);
    let mut upstream_req = HttpRequest::new(req.method, upstream_url).with_body(req.body);

    for (name, value) in &req.headers {
        if is_hop_by_hop_or_auth_header(name) {
            continue;
        }
        upstream_req.headers.push((name.clone(), value.clone()));
    }

    if route.auth_kind != BotMcpAuthKind::None {
        match secrets.resolve_secret(
            &route.bot_id,
            route.secret_ref.as_deref(),
            &route.fallback_secret_key,
        ) {
            Ok(Some(secret_val)) => {
                inject_auth_header(&mut upstream_req.headers, &route, &secret_val);
            }
            Ok(None) => {
                if route.auth_kind == BotMcpAuthKind::BearerHeader {
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

    match http_client::send(&upstream_req) {
        Ok(resp) => {
            lock(state).last_errors.remove(&route.cred_id);
            write_forwarded_response(&mut stream, resp.status, &resp.headers, &resp.body).ok();
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

    let content_len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if content_len > MAX_LOCAL_BODY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "local relay request body too large",
        ));
    }

    let body_start = header_end.saturating_add(4);
    let mut body = buf.get(body_start..).unwrap_or_default().to_vec();
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

    Ok(ParsedLocalRequest {
        method,
        target,
        headers,
        body,
    })
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

fn write_forwarded_response(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> io::Result<()> {
    let mut head = String::with_capacity(256);
    write!(head, "HTTP/1.1 {status} OK\r\n").ok();
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("transfer-encoding")
            || k.eq_ignore_ascii_case("content-length")
            || k.eq_ignore_ascii_case("connection")
        {
            continue;
        }
        write!(head, "{k}: {v}\r\n").ok();
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
        assert!(!relay.status().running);

        // Mock upstream MCP server capturing received Authorization / x-api-key header.
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(String::new()));
        let captured_bg = Arc::clone(&captured);
        let upstream_thread = thread::spawn(move || {
            let (mut sock, _) = upstream.accept().unwrap();
            let mut req_buf = [0u8; 4096];
            let n = sock.read(&mut req_buf).unwrap();
            *lock(&captured_bg) =
                String::from_utf8_lossy(req_buf.get(..n).unwrap_or_default()).into_owned();
            let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).unwrap();
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

        // Rejects wrong bearer token.
        let bad = http_client::send(
            &HttpRequest::post_json(&relay_url, b"{}".to_vec())
                .with_header("Authorization", "Bearer wrong-token"),
        )
        .unwrap();
        assert_eq!(bad.status, 401);

        // Accepts valid launch token and injects `x-api-key` without forwarding launch token.
        let ok = http_client::send(
            &HttpRequest::post_json(&relay_url, br#"{"jsonrpc":"2.0"}"#.to_vec())
                .with_header("Authorization", format!("Bearer {}", *grant.token)),
        )
        .unwrap();
        assert_eq!(ok.status, 200);
        assert!(ok.text().contains("tools"));

        upstream_thread.join().unwrap();
        let seen = lock(&captured).clone();
        assert!(seen.contains("x-api-key: upstream-api-key-xyz"));
        assert!(!seen.contains(&*grant.token));

        // Cleanup stops relay when unused.
        relay.unregister_bot(&bot_id);
        assert!(!relay.status().running);

        fs::remove_dir_all(&dir).ok();
    }
}
