// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Non-blocking RPC transport, response validation, avatar/session metadata, and CSV helpers for Krill Bots UI.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use krill_proto::SessionId;
use krill_proto::bots::{
    BotAccessRequest, BotAccessRequestStatus, BotAgentModel, BotConfig, BotId, BotItem,
    BotItemStatus, BotRpcRequest, BotRpcResponse, BotSessionSummary, MAX_RPC_AVATAR_PNG_BYTES,
};
use krill_proto::control::{FEAT_CONTROL, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole, FEAT_BOTS};
use krill_proto::os::net::UnixStream;
use krill_proto::screen::Msg as _;

use crate::hosts::{HostId, HostTarget};
use crate::i18n::catalog::bots as text;

/// Non-blocking injectable RPC closure for `BotRpcRequest` -> `BotRpcResponse`.
pub type BotRpcCall = Arc<dyn Fn(&BotRpcRequest) -> Result<BotRpcResponse, String> + Send + Sync>;

pub use krill_ui::bot_types::{BotSessionMeta, parse_avatar_color};

/// Krill's 8 enabled bot agent runtimes (aligned with Orca's enabled bot agents).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotRuntimeOption {
    pub id: &'static str,
}

impl BotRuntimeOption {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self.id {
            "claude" => text::RUNTIME_CLAUDE.get(),
            "codex" => text::RUNTIME_CODEX.get(),
            "agy" => text::RUNTIME_AGY.get(),
            "opencode" => text::RUNTIME_OPENCODE.get(),
            "cursor" => text::RUNTIME_CURSOR.get(),
            "pi" => text::RUNTIME_PI.get(),
            "grok" => text::RUNTIME_GROK.get(),
            "omp" => text::RUNTIME_OMP.get(),
            _ => self.id,
        }
    }
}

/// All 8 enabled Krill bot agent runtimes.
pub const BOT_RUNTIMES: [BotRuntimeOption; 8] = [
    BotRuntimeOption { id: "claude" },
    BotRuntimeOption { id: "codex" },
    BotRuntimeOption { id: "agy" },
    BotRuntimeOption { id: "opencode" },
    BotRuntimeOption { id: "cursor" },
    BotRuntimeOption { id: "pi" },
    BotRuntimeOption { id: "grok" },
    BotRuntimeOption { id: "omp" },
];

/// Display label for any bot runtime `agent_id`.
#[must_use]
pub fn runtime_display_label(agent_id: &str) -> String {
    let canon = krill_proto::agent::canonical_id(agent_id);
    if let Some(opt) = BOT_RUNTIMES.iter().find(|r| r.id == canon) {
        opt.label().to_owned()
    } else {
        agent_id.to_owned()
    }
}

/// Maps any persisted avatar color name or `#rrggbb` string to a semantic [`TabColor`].
/// Supported reasoning effort choices (empty string = runtime default).
pub const BOT_EFFORT_OPTIONS: [&str; 6] = ["", "low", "medium", "high", "xhigh", "max"];

const CLAUDE_EFFORT_OPTIONS: [&str; 5] = ["", "low", "medium", "high", "max"];
const CODEX_EFFORT_OPTIONS: [&str; 5] = ["", "low", "medium", "high", "xhigh"];

/// Reasoning effort choices accepted by `runtime_id` (empty string = runtime default).
#[must_use]
pub fn effort_options_for_runtime(runtime_id: &str) -> &'static [&'static str] {
    match krill_proto::agent::canonical_id(runtime_id) {
        "claude" | "openclaude" => &CLAUDE_EFFORT_OPTIONS,
        "codex" | "grok" | "pi" | "omp" => &CODEX_EFFORT_OPTIONS,
        _ => &BOT_EFFORT_OPTIONS,
    }
}

/// Localized display label for a reasoning effort string.
#[must_use]
pub fn effort_label(effort: &str) -> &'static str {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => text::EFFORT_LOW.get(),
        "medium" => text::EFFORT_MEDIUM.get(),
        "high" => text::EFFORT_HIGH.get(),
        "xhigh" | "very_high" | "very-high" => text::EFFORT_XHIGH.get(),
        "max" => text::EFFORT_MAX.get(),
        _ => text::EFFORT_DEFAULT.get(),
    }
}

/// Validates and wraps raw PNG bytes into a GPUI [`gpui::Image`] handle.
pub fn decode_png_avatar(bytes: &[u8]) -> Result<Arc<gpui::Image>, String> {
    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() > MAX_RPC_AVATAR_PNG_BYTES.min(512 * 1024) {
        return Err(text::SETTINGS_AVATAR_TOO_LARGE.get().to_owned());
    }
    if bytes.len() < PNG_MAGIC.len() || bytes[..PNG_MAGIC.len()] != PNG_MAGIC {
        return Err(text::SETTINGS_AVATAR_INVALID_PNG.get().to_owned());
    }
    Ok(Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        bytes.to_vec(),
    )))
}

/// Default read timeout for slow/mutating bot daemon RPC calls (OAuth/sync/tidy/look-around/spawn).
const BOT_RPC_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Tighter read timeout for lightweight read-only daemon state queries (prevents hung remote polls).
const BOT_RPC_FAST_READ_TIMEOUT: Duration = Duration::from_secs(8);
const BOT_RPC_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum idle persistent control connections kept per host target.
const MAX_IDLE_CONNS_PER_HOST: usize = 2;
/// Drop pooled idle bot connections after this duration so stale remote streams don't linger.
const POOL_IDLE_TTL: Duration = Duration::from_secs(60);
/// Pipeline read-only frames in one `write_all` only when total encoded payload stays small,
/// preventing socket send-buffer deadlocks on large bodies.
const MAX_PIPELINE_WRITE_BYTES: usize = 16 * 1024;
/// Maximum number of `GetBotAvatar` frames pipelined in one round-trip.
const AVATAR_PIPELINE_CHUNK: usize = 4;

/// Pool lookup key that intentionally excludes `RemoteConnector` so transport resources are not retained.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PoolKey {
    host: HostId,
    socket: PathBuf,
    allow_local_launch: bool,
}

impl PoolKey {
    fn of(target: &HostTarget) -> Self {
        Self {
            host: target.host().clone(),
            socket: target.socket().to_owned(),
            allow_local_launch: target.allow_local_launch(),
        }
    }
}

struct PooledConn {
    stream: UnixStream,
    epoch: u64,
    idle_since: Instant,
}

type PoolMap = Mutex<BTreeMap<PoolKey, VecDeque<PooledConn>>>;

fn connection_pool() -> &'static PoolMap {
    static POOL: OnceLock<PoolMap> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Drops any idle pooled bot RPC connections for `host` (call on host disconnect/target change).
pub fn invalidate_host_connections(host: &HostId) {
    if let Ok(mut pool) = connection_pool().lock() {
        pool.retain(|key, _| &key.host != host);
    }
}

fn checkout_pooled_conn(target: &HostTarget) -> Option<PooledConn> {
    let key = PoolKey::of(target);
    let epoch = target.transport_epoch();
    let now = Instant::now();
    let mut pool = connection_pool().lock().ok()?;
    let queue = pool.get_mut(&key)?;
    while let Some(conn) = queue.pop_front() {
        if conn.epoch == epoch && now.saturating_duration_since(conn.idle_since) < POOL_IDLE_TTL {
            if queue.is_empty() {
                pool.remove(&key);
            }
            return Some(conn);
        }
    }
    pool.remove(&key);
    None
}

fn return_pooled_conn(target: &HostTarget, stream: UnixStream, epoch: u64) {
    if !target.is_current() || target.transport_epoch() != epoch {
        return;
    }
    let key = PoolKey::of(target);
    let Ok(mut pool) = connection_pool().lock() else {
        return;
    };
    let now = Instant::now();
    let queue = pool.entry(key).or_default();
    queue.retain(|c| {
        c.epoch == epoch && now.saturating_duration_since(c.idle_since) < POOL_IDLE_TTL
    });
    while queue.len() >= MAX_IDLE_CONNS_PER_HOST {
        queue.pop_back();
    }
    queue.push_front(PooledConn {
        stream,
        epoch,
        idle_since: now,
    });
}

fn read_timeout_for(req: &BotRpcRequest) -> Duration {
    if !req.is_read_only() {
        return BOT_RPC_READ_TIMEOUT;
    }
    match req {
        BotRpcRequest::ListAgentModels { .. }
        | BotRpcRequest::ImportExternalBots { .. }
        | BotRpcRequest::McpOAuthStatus { .. }
        | BotRpcRequest::ListComposioConnections { .. } => BOT_RPC_READ_TIMEOUT,
        _ => BOT_RPC_FAST_READ_TIMEOUT,
    }
}

fn is_transient_stream_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}

fn connect_bot_stream(target: &HostTarget) -> io::Result<(UnixStream, u64)> {
    let hello = daemon::client_hello(ClientRole::App, "krill-app-bots");
    let epoch = target.transport_epoch();
    let conn = target.connect(&hello).map_err(io::Error::other)?;
    let need = FEAT_CONTROL | FEAT_BOTS;
    if conn.negotiated.features & need != need {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            unsupported_error(&conn.server.build),
        ));
    }
    Ok((conn.stream, epoch))
}

fn map_socket_io_error(err: io::Error) -> io::Error {
    if err.kind() == io::ErrorKind::WouldBlock {
        io::Error::new(io::ErrorKind::TimedOut, "bot RPC timed out")
    } else {
        err
    }
}

/// Sends one or more `BotRpcRequest` frames on an open control stream.
/// When every frame fits inside [`MAX_PIPELINE_WRITE_BYTES`], writes all frames in a single
/// `write_all` so remote hosts pay only one network RTT instead of N sequential RTTs.
fn exchange_batch_on_stream(
    sock: &mut UnixStream,
    reqs: &[BotRpcRequest],
    read_timeout: Duration,
) -> io::Result<Vec<Result<BotRpcResponse, String>>> {
    sock.set_read_timeout(Some(read_timeout)).ok();
    sock.set_write_timeout(Some(BOT_RPC_WRITE_TIMEOUT)).ok();

    let mut encoded = Vec::with_capacity(reqs.len());
    let mut total_bytes = 0usize;
    for req in reqs {
        let frame = Request::Bot(req.clone()).encode()?;
        total_bytes = total_bytes.saturating_add(frame.len());
        encoded.push(frame);
    }

    let mut replies = Vec::with_capacity(reqs.len());
    if encoded.len() > 1 && total_bytes <= MAX_PIPELINE_WRITE_BYTES {
        let mut combined = Vec::with_capacity(total_bytes);
        for frame in &encoded {
            combined.extend_from_slice(frame);
        }
        sock.write_all(&combined).map_err(map_socket_io_error)?;
        for _ in 0..encoded.len() {
            let (kind, body) = ipc::read_frame(sock)
                .map_err(map_socket_io_error)?
                .ok_or(io::ErrorKind::UnexpectedEof)?;
            match Reply::decode(kind, &body)? {
                Reply::Bot(resp) => replies.push(Ok(resp)),
                Reply::Failed(failure) => replies.push(Err(failure.to_string())),
                _ => return Err(io::ErrorKind::InvalidData.into()),
            }
        }
    } else {
        for frame in &encoded {
            sock.write_all(frame).map_err(map_socket_io_error)?;
            let (kind, body) = ipc::read_frame(sock)
                .map_err(map_socket_io_error)?
                .ok_or(io::ErrorKind::UnexpectedEof)?;
            match Reply::decode(kind, &body)? {
                Reply::Bot(resp) => replies.push(Ok(resp)),
                Reply::Failed(failure) => replies.push(Err(failure.to_string())),
                _ => return Err(io::ErrorKind::InvalidData.into()),
            }
        }
    }
    Ok(replies)
}

/// Capabilities advertised by the Krill Desktop client on bot item reads so `Waiting` items
/// are never downgraded to `Working` (Orca `#47429`).
pub const DESKTOP_BOT_CAPABILITIES: &[&str] = krill_proto::bots::DESKTOP_BOT_CAPABILITIES;

fn needs_desktop_capabilities(req: &BotRpcRequest) -> bool {
    matches!(
        req,
        BotRpcRequest::ListItems { .. }
            | BotRpcRequest::ListItemsPage { .. }
            | BotRpcRequest::GetBotItem { .. }
    )
}

fn wrap_with_desktop_capabilities(req: &BotRpcRequest) -> BotRpcRequest {
    if needs_desktop_capabilities(req) {
        BotRpcRequest::WithCapabilities {
            capabilities: DESKTOP_BOT_CAPABILITIES
                .iter()
                .map(|cap| (*cap).to_owned())
                .collect(),
            request: Box::new(req.clone()),
        }
    } else {
        req.clone()
    }
}

/// Executes a batch of `BotRpcRequest`s against `target` using a pooled persistent control
/// connection and pipelining when possible.
///
/// Advertises [`DESKTOP_BOT_CAPABILITIES`] on item-read requests so `Waiting` items are never
/// downgraded on modern daemons, transparently falling back to raw requests if an older daemon
/// does not understand `WithCapabilities`.
///
/// If a reused pooled connection fails with a transient stream error and all requests are
/// read-only, transparently reconnects once on a fresh stream.
pub fn call_daemon_bot_batch(
    target: &HostTarget,
    reqs: &[BotRpcRequest],
) -> io::Result<Vec<Result<BotRpcResponse, String>>> {
    if reqs.is_empty() {
        return Ok(Vec::new());
    }
    let any_wrapped = reqs.iter().any(needs_desktop_capabilities);
    if any_wrapped {
        let wrapped: Vec<BotRpcRequest> = reqs.iter().map(wrap_with_desktop_capabilities).collect();
        match call_daemon_bot_batch_raw(target, &wrapped) {
            Ok(replies)
                if !reqs
                    .iter()
                    .zip(replies.iter())
                    .any(|(req, slot)| needs_desktop_capabilities(req) && slot.is_err()) =>
            {
                return Ok(replies);
            }
            Ok(replies) if !target.is_current() => return Ok(replies),
            Err(err) if !target.is_current() => return Err(err),
            Ok(_) | Err(_) => {}
        }
    }
    call_daemon_bot_batch_raw(target, reqs)
}

fn call_daemon_bot_batch_raw(
    target: &HostTarget,
    reqs: &[BotRpcRequest],
) -> io::Result<Vec<Result<BotRpcResponse, String>>> {
    if reqs.is_empty() {
        return Ok(Vec::new());
    }
    if !target.is_current() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    let read_timeout = reqs
        .iter()
        .map(read_timeout_for)
        .max()
        .unwrap_or(BOT_RPC_FAST_READ_TIMEOUT);
    let all_read_only = reqs.iter().all(BotRpcRequest::is_read_only);

    let (mut stream, mut epoch, mut from_pool) = if let Some(pooled) = checkout_pooled_conn(target)
    {
        (pooled.stream, pooled.epoch, true)
    } else {
        let (stream, epoch) = connect_bot_stream(target)?;
        (stream, epoch, false)
    };

    let mut result = exchange_batch_on_stream(&mut stream, reqs, read_timeout);
    if let Err(ref err) = result
        && from_pool
        && all_read_only
        && is_transient_stream_error(err)
        && target.is_current()
    {
        drop(stream);
        let (fresh_stream, fresh_epoch) = connect_bot_stream(target)?;
        stream = fresh_stream;
        epoch = fresh_epoch;
        from_pool = false;
        result = exchange_batch_on_stream(&mut stream, reqs, read_timeout);
    }

    if !target.is_current() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    match result {
        Ok(replies) => {
            target.record_outcome(true, None);
            return_pooled_conn(target, stream, epoch);
            Ok(replies)
        }
        Err(err) => {
            if !from_pool
                && err.kind() != io::ErrorKind::Unsupported
                && err.kind() != io::ErrorKind::Interrupted
            {
                target.record_outcome(false, Some(&err));
            }
            Err(err)
        }
    }
}

/// Builds a live daemon RPC caller talking to `krilld` via `Request::Bot` / `Reply::Bot`.
#[must_use]
pub fn daemon_bot_rpc(target: Option<HostTarget>) -> BotRpcCall {
    #[cfg(test)]
    if target.is_none() {
        return Arc::new(|_| Err(text::DAEMON_UNAVAILABLE.get().to_owned()));
    }
    Arc::new(move |req| call_daemon_bot(target.as_ref(), req).map_err(|e| e.to_string()))
}

/// Builds a live daemon RPC caller bound to `host`, refusing to fall back to the local daemon
/// when `host` is remote and no `target` is currently available.
#[must_use]
pub fn daemon_bot_rpc_for_host(host: &HostId, target: Option<HostTarget>) -> BotRpcCall {
    if host.is_remote() && target.is_none() {
        return Arc::new(|_| Err(text::DAEMON_UNAVAILABLE.get().to_owned()));
    }
    daemon_bot_rpc(target)
}

fn call_daemon_bot(target: Option<&HostTarget>, req: &BotRpcRequest) -> io::Result<BotRpcResponse> {
    let resolved = match target {
        Some(t) => t.clone(),
        None => HostTarget::local_dev()?,
    };
    let mut replies = call_daemon_bot_batch(&resolved, std::slice::from_ref(req))?;
    match replies.pop() {
        Some(Ok(resp)) => Ok(resp),
        Some(Err(msg)) => Err(io::Error::other(msg)),
        None => Err(io::ErrorKind::InvalidData.into()),
    }
}

/// Executes `reqs` as a single pipelined daemon batch when `target` is `Some`, or sequentially
/// through `fallback` otherwise (mock RPCs pass `target = None`). Promotes
/// [`BotRpcResponse::Error`] and [`BotRpcResponse::SessionRefused`] per slot just like
/// [`call_checked_on`].
pub fn call_checked_batch_on(
    target: Option<&HostTarget>,
    fallback: &BotRpcCall,
    reqs: &[BotRpcRequest],
    host_name: Option<&str>,
) -> Result<Vec<Result<BotRpcResponse, String>>, String> {
    if reqs.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(target) = target {
        let raw = call_daemon_bot_batch(target, reqs).map_err(|e| e.to_string())?;
        return Ok(raw
            .into_iter()
            .map(|slot| match slot {
                Ok(BotRpcResponse::Error(msg)) | Err(msg) => Err(msg),
                Ok(BotRpcResponse::SessionRefused(refusal)) => {
                    Err(bot_refusal_text(&refusal, host_name))
                }
                Ok(other) => Ok(other),
            })
            .collect());
    }
    Ok(reqs
        .iter()
        .map(|req| call_checked_on(fallback, req, host_name))
        .collect())
}

/// Start of the error a daemon without bots answers with; [`unsupported_version`] reads it back.
// ponytail: the RPC error is a plain String, so the "no bots here" case rides in its text.
const UNSUPPORTED: &str = "krilld does not support FEAT_BOTS";

pub(super) fn unsupported_error(build: &str) -> String {
    format!("{UNSUPPORTED} (krilld {build})")
}

/// The daemon's version when `err` says that daemon has no bots (`""` if it didn't say).
#[must_use]
pub fn unsupported_version(err: &str) -> Option<&str> {
    let rest = err.strip_prefix(UNSUPPORTED)?;
    let build = rest
        .trim()
        .strip_prefix("(krilld ")
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or("");
    Some(build.split_once('+').map_or(build, |(version, _)| version))
}

/// One banner line for several failed loads: each distinct message once, in first-seen order.
#[must_use]
pub fn join_errors(errors: &[String]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for err in errors {
        if !seen.contains(&err.as_str()) {
            seen.push(err);
        }
    }
    seen.join(" | ")
}

/// Localized name for the local computer when wording host-side bot refusals.
#[must_use]
pub fn local_host_label() -> &'static str {
    crate::i18n::catalog::bots::LOCAL_HOST_NAME.get()
}

/// Formats a structured [`krill_proto::bots::BotSessionRefusal`] from the reader's perspective,
/// naming `host_name` (e.g. `"this computer"` / `"이 컴퓨터"` locally, or the paired server's
/// display name remotely, or `None` for unnamed paired servers so a remote host is never called
/// "this computer"). Mirrors Orca `botRefusalText` (`b10792b6`, `e1779f39`).
#[must_use]
pub fn bot_refusal_text(
    refusal: &krill_proto::bots::BotSessionRefusal,
    host_name: Option<&str>,
) -> String {
    match crate::i18n::language() {
        crate::i18n::Language::Ko => crate::i18n::format_bot_refusal_ko(refusal, host_name),
        crate::i18n::Language::En => refusal.message_en(host_name),
    }
}

/// Invokes a bot RPC call and promotes [`BotRpcResponse::Error`] and
/// [`BotRpcResponse::SessionRefused`] into `Err(msg)`, wording refusals for `host_name`.
pub fn call_checked_on(
    call: &BotRpcCall,
    req: &BotRpcRequest,
    host_name: Option<&str>,
) -> Result<BotRpcResponse, String> {
    match call(req)? {
        BotRpcResponse::Error(msg) => Err(msg),
        BotRpcResponse::SessionRefused(refusal) => Err(bot_refusal_text(&refusal, host_name)),
        other => Ok(other),
    }
}

/// Invokes a bot RPC call and promotes [`BotRpcResponse::Error`] and
/// [`BotRpcResponse::SessionRefused`] into `Err(msg)`.
pub fn call_checked(call: &BotRpcCall, req: &BotRpcRequest) -> Result<BotRpcResponse, String> {
    call_checked_on(call, req, Some(local_host_label()))
}

/// Unwraps a [`BotRpcResponse`] where only side-effect success (`Ok` / non-Error reply) matters.
pub fn expect_ok(call: &BotRpcCall, req: &BotRpcRequest) -> Result<(), String> {
    call_checked(call, req).map(|_| ())
}

/// Extracts a strongly-typed variant from a checked RPC call or returns a clear error.
pub fn expect_variant<T>(
    call: &BotRpcCall,
    req: &BotRpcRequest,
    extract: impl FnOnce(BotRpcResponse) -> Option<T>,
    expected: &'static str,
) -> Result<T, String> {
    let resp = call_checked(call, req)?;
    extract(resp).ok_or_else(|| format!("Unexpected daemon response (expected {expected})"))
}

/// Built-in models the New bot picker offers until krilld reports the agent's own list
/// ([`BotRpcRequest::ListAgentModels`]). Same list as the chat composer's; "Default" is `None` here.
#[must_use]
pub fn runtime_models(runtime_id: &str) -> Vec<BotAgentModel> {
    let canon = krill_proto::agent::canonical_id(runtime_id);
    if !matches!(canon, "claude" | "codex" | "agy") {
        return Vec::new();
    }
    crate::agent_models::builtin(canon)
        .0
        .iter()
        .filter(|m| m.id != "default")
        .map(|m| BotAgentModel {
            id: m.id.to_owned(),
            label: m.label.to_owned(),
        })
        .collect()
}

/// One bot row in the main window sidebar's Bots section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarBot {
    /// Which Krill host owns and runs this bot (`Local` or `Remote(id)`).
    pub host: HostId,
    pub id: BotId,
    pub name: String,
    pub emoji: Option<String>,
    pub avatar: super::avatar::Avatar,
    pub avatar_color: Option<String>,
    /// SOUL-free one-liner shown when nothing else is going on (Orca's fallback activity line).
    pub description: String,
    /// Folder name under the bots root (`<state>/bots/<slug>`).
    pub slug: String,
    pub paused: bool,
    /// The agent runtime its chats launch (`claude`, `codex`, …).
    pub agent_id: String,
    /// Whether its chats ask before edits and commands (shown in the chat composer).
    pub permission: krill_proto::agent::Permission,
    /// The folder its chats run in when it isn't the bot's own home.
    pub workspace: Option<String>,
    /// `needs_you` items plus pending access requests owned by this bot.
    pub attention: usize,
    /// Its `needs_you` items (the hover card's "items it flagged for you").
    pub items_need_you: usize,
    /// Who asked for access and waits (the hover card's "Asked for access" lines).
    pub access_requesters: Vec<String>,
    /// Live chats, Home Chat (no external link) first, then newest.
    pub sessions: Vec<BotSessionSummary>,
    pub updated_at_ms: u64,
    /// Its schedules' names and prompts: a turn one started reads as Orca's "Scheduled · name".
    pub schedules: Vec<(String, String)>,
}

impl SidebarBot {
    /// The bot's Home Chat: its live session that no Slack/Discord/… thread owns.
    #[must_use]
    pub fn home_session(&self) -> Option<&BotSessionSummary> {
        self.sessions.iter().find(|s| s.link_id.is_none())
    }

    #[must_use]
    pub fn working(&self) -> usize {
        self.sessions.iter().filter(|s| s.working).count()
    }

    /// Identity the sidebar and tabs use to label this bot's sessions.
    #[must_use]
    pub fn session_meta(&self) -> BotSessionMeta {
        BotSessionMeta {
            bot_id: self.id.clone(),
            bot_name: self.name.clone(),
            emoji: self.emoji.clone(),
            avatar_color: self.avatar_color.clone(),
        }
    }
}

/// What the sidebar shows for bots: the rows and the total attention badge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarSnapshot {
    pub bots: Vec<SidebarBot>,
    pub attention: usize,
}

#[must_use]
pub fn sidebar_snapshot(
    bots: &[BotConfig],
    items: &[BotItem],
    access: &[BotAccessRequest],
    sessions: &[BotSessionSummary],
) -> SidebarSnapshot {
    sidebar_snapshot_on(&HostId::Local, bots, items, access, sessions)
}

#[must_use]
pub fn sidebar_snapshot_on(
    host: &HostId,
    bots: &[BotConfig],
    items: &[BotItem],
    access: &[BotAccessRequest],
    sessions: &[BotSessionSummary],
) -> SidebarSnapshot {
    let needs: Vec<&BotId> = items
        .iter()
        .filter(|item| item.status == BotItemStatus::NeedsYou)
        .map(|item| &item.owner_bot_id)
        .chain(
            access
                .iter()
                .filter(|req| req.status == BotAccessRequestStatus::Pending)
                .map(|req| &req.bot_id),
        )
        .collect();
    SidebarSnapshot {
        bots: bots
            .iter()
            .map(|bot| {
                let mut chats: Vec<BotSessionSummary> = sessions
                    .iter()
                    .filter(|s| {
                        s.bot_id == bot.id
                            && krill_proto::bots::session_belongs_to_bot(
                                bot.created_at_ms,
                                s.created_at_ms,
                            )
                    })
                    .cloned()
                    .collect();
                chats.sort_by_key(|s| {
                    (
                        s.link_id.is_some(),
                        !s.live,
                        std::cmp::Reverse(s.created_at_ms),
                    )
                });
                SidebarBot {
                    host: host.clone(),
                    id: bot.id.clone(),
                    name: bot.name.clone(),
                    emoji: bot.emoji.clone().filter(|e| !e.trim().is_empty()),
                    avatar: super::avatar::Avatar::of(bot),
                    avatar_color: bot.avatar_color.clone(),
                    description: bot.description.trim().to_owned(),
                    slug: bot.slug.as_str().to_owned(),
                    paused: bot.paused,
                    agent_id: bot.agent_id.clone(),
                    permission: bot.permission_mode,
                    workspace: bot
                        .workspace
                        .as_deref()
                        .map(str::trim)
                        .filter(|w| !w.is_empty())
                        .map(str::to_owned),
                    attention: needs.iter().filter(|id| **id == &bot.id).count(),
                    items_need_you: items
                        .iter()
                        .filter(|i| i.status == BotItemStatus::NeedsYou && i.owner_bot_id == bot.id)
                        .count(),
                    access_requesters: access
                        .iter()
                        .filter(|r| {
                            r.status == BotAccessRequestStatus::Pending && r.bot_id == bot.id
                        })
                        .map(|r| {
                            if r.sender_name.trim().is_empty() {
                                r.sender_id.clone()
                            } else {
                                r.sender_name.clone()
                            }
                        })
                        .collect(),
                    sessions: chats,
                    updated_at_ms: bot.updated_at_ms,
                    schedules: bot
                        .schedules
                        .iter()
                        .map(|s| (s.name.clone(), s.prompt.clone()))
                        .collect(),
                }
            })
            .collect(),
        attention: needs.len(),
    }
}

/// Everything the sidebar Bots section shows, read in one pass.
#[derive(Debug, Clone, Default)]
pub struct SidebarState {
    pub snapshot: SidebarSnapshot,
    pub groups: Vec<krill_proto::bots::BotGroupSummary>,
    pub sessions: BTreeMap<SessionId, BotSessionMeta>,
    /// Newly read `avatar.png` bytes: bot, the config time they belong to, PNG (empty when
    /// it couldn't be read).
    pub avatars: Vec<(BotId, u64, Vec<u8>)>,
}

/// Failure classification when refreshing one host's sidebar bots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarFetchError {
    /// Host's `krilld` does not advertise `FEAT_BOTS` (contains daemon version or `""`).
    Unsupported(String),
    /// Transient transport/timeout/RPC error — keep previously loaded bots and retry with backoff.
    Transient(String),
}

fn classify_sidebar_error(err: String) -> SidebarFetchError {
    if let Some(version) = unsupported_version(&err) {
        SidebarFetchError::Unsupported(version.to_owned())
    } else {
        SidebarFetchError::Transient(err)
    }
}

fn build_sidebar_state(
    host: &HostId,
    bots: &[BotConfig],
    items: &[BotItem],
    access: &[BotAccessRequest],
    summaries: &[BotSessionSummary],
    groups: Vec<krill_proto::bots::BotGroupSummary>,
    avatars: Vec<(BotId, u64, Vec<u8>)>,
) -> SidebarState {
    let metas: HashMap<&BotId, (u64, BotSessionMeta)> = bots
        .iter()
        .map(|b| (&b.id, (b.created_at_ms, BotSessionMeta::from_bot(b))))
        .collect();
    let sessions = summaries
        .iter()
        .filter_map(|s| {
            let (bot_created, meta) = metas.get(&s.bot_id)?;
            krill_proto::bots::session_belongs_to_bot(*bot_created, s.created_at_ms)
                .then(|| (s.session_id, meta.clone()))
        })
        .collect();
    SidebarState {
        snapshot: sidebar_snapshot_on(host, bots, items, access, summaries),
        groups,
        sessions,
        avatars,
    }
}

fn wrap_legacy_groups(
    raw: Vec<krill_proto::bots::BotGroupChat>,
) -> Vec<krill_proto::bots::BotGroupSummary> {
    raw.into_iter()
        .map(|group| krill_proto::bots::BotGroupSummary {
            group,
            reply_audience: krill_proto::bots::BotGroupReplyAudience::default(),
            threads: Vec::new(),
            latest_message: None,
            active_turns: Vec::new(),
        })
        .collect()
}

/// Pipelined, single-connection sidebar state fetch for a live [`HostTarget`].
///
/// Pipelines `ListBots`, `ListItems`, `ListAccessRequests`, `ListBotSessions`, and
/// `ListGroupSummaries` in one round-trip over a pooled control connection, falling back to
/// `ListGroups` only when an older daemon rejects `ListGroupSummaries`. Never substitutes empty
/// collections on a transport error (preserving previously loaded chats and badges).
pub fn fetch_sidebar_state_for_target<S: std::hash::BuildHasher>(
    host: &HostId,
    target: &HostTarget,
    known_avatars: &HashMap<BotId, u64, S>,
) -> Result<SidebarState, SidebarFetchError> {
    let primary = [
        BotRpcRequest::ListBots,
        BotRpcRequest::ListItems {
            bot_id: None,
            include_closed: false,
        },
        BotRpcRequest::ListAccessRequests { bot_id: None },
        BotRpcRequest::ListBotSessions { bot_id: None },
        BotRpcRequest::ListGroupSummaries,
    ];
    let mut replies = call_daemon_bot_batch(target, &primary)
        .map_err(|e| classify_sidebar_error(e.to_string()))?
        .into_iter();

    let bots = match replies.next() {
        Some(Ok(BotRpcResponse::Bots(bots))) => bots,
        Some(Ok(BotRpcResponse::Error(msg)) | Err(msg)) => {
            return Err(classify_sidebar_error(msg));
        }
        other => {
            return Err(SidebarFetchError::Transient(format!(
                "unexpected ListBots reply: {other:?}"
            )));
        }
    };
    let items = match replies.next() {
        Some(Ok(BotRpcResponse::Items(items))) => items,
        Some(Ok(BotRpcResponse::Ok)) => Vec::new(),
        Some(Ok(BotRpcResponse::Error(msg)) | Err(msg)) => {
            return Err(SidebarFetchError::Transient(msg));
        }
        other => {
            return Err(SidebarFetchError::Transient(format!(
                "unexpected ListItems reply: {other:?}"
            )));
        }
    };
    let access = match replies.next() {
        Some(Ok(BotRpcResponse::AccessRequests(reqs))) => reqs,
        Some(Ok(BotRpcResponse::Ok)) => Vec::new(),
        Some(Ok(BotRpcResponse::Error(msg)) | Err(msg)) => {
            return Err(SidebarFetchError::Transient(msg));
        }
        other => {
            return Err(SidebarFetchError::Transient(format!(
                "unexpected ListAccessRequests reply: {other:?}"
            )));
        }
    };
    let summaries = match replies.next() {
        Some(Ok(BotRpcResponse::BotSessions(list))) => list,
        Some(Ok(BotRpcResponse::Ok)) => Vec::new(),
        Some(Ok(BotRpcResponse::Error(msg)) | Err(msg)) => {
            return Err(SidebarFetchError::Transient(msg));
        }
        other => {
            return Err(SidebarFetchError::Transient(format!(
                "unexpected ListBotSessions reply: {other:?}"
            )));
        }
    };
    let groups = match replies.next() {
        Some(Ok(BotRpcResponse::GroupSummaries(groups))) => groups,
        Some(Ok(BotRpcResponse::Ok)) => Vec::new(),
        // Older `FEAT_BOTS` daemon predates `ListGroupSummaries` (answers BadRequest): fall back
        // once to `ListGroups` on the same pooled connection.
        Some(Err(_) | Ok(BotRpcResponse::Error(_))) => {
            match call_daemon_bot_batch(target, &[BotRpcRequest::ListGroups])
                .map_err(|e| SidebarFetchError::Transient(e.to_string()))?
                .into_iter()
                .next()
            {
                Some(Ok(BotRpcResponse::Groups(raw))) => wrap_legacy_groups(raw),
                Some(Ok(BotRpcResponse::Ok)) => Vec::new(),
                Some(Ok(BotRpcResponse::Error(msg)) | Err(msg)) => {
                    return Err(SidebarFetchError::Transient(msg));
                }
                other => {
                    return Err(SidebarFetchError::Transient(format!(
                        "unexpected ListGroups reply: {other:?}"
                    )));
                }
            }
        }
        other => {
            return Err(SidebarFetchError::Transient(format!(
                "unexpected ListGroupSummaries reply: {other:?}"
            )));
        }
    };

    let missing_avatars: Vec<(BotId, u64)> = bots
        .iter()
        .filter(|b| super::avatar::Avatar::of(b) == super::avatar::Avatar::Upload)
        .filter(|b| known_avatars.get(&b.id) != Some(&b.updated_at_ms))
        .map(|b| (b.id.clone(), b.updated_at_ms))
        .collect();
    let mut avatars = Vec::with_capacity(missing_avatars.len());
    for chunk in missing_avatars.chunks(AVATAR_PIPELINE_CHUNK) {
        let reqs: Vec<BotRpcRequest> = chunk
            .iter()
            .map(|(bot_id, _)| BotRpcRequest::GetBotAvatar {
                bot_id: bot_id.clone(),
            })
            .collect();
        // A transient transport error fetching avatars should not discard the bot list itself;
        // omit them so the next refresh retries cleanly.
        let Ok(chunk_replies) = call_daemon_bot_batch(target, &reqs) else {
            break;
        };
        for ((bot_id, updated_at_ms), reply) in chunk.iter().zip(chunk_replies) {
            match reply {
                Ok(BotRpcResponse::BotAvatar(data)) => {
                    avatars.push((bot_id.clone(), *updated_at_ms, data.png_bytes));
                }
                // Missing/unreadable avatar file on daemon: cache empty so we stop re-fetching
                // until `updated_at_ms` changes.
                Ok(BotRpcResponse::Error(_) | BotRpcResponse::Ok) => {
                    avatars.push((bot_id.clone(), *updated_at_ms, Vec::new()));
                }
                _ => {}
            }
        }
    }

    Ok(build_sidebar_state(
        host, &bots, &items, &access, &summaries, groups, avatars,
    ))
}

/// Reads bots, open items, access requests and bot sessions for the sidebar via an injectable
/// [`BotRpcCall`] (used by tests and mock transports).
pub fn fetch_sidebar_state_detailed_with_rpc<S: std::hash::BuildHasher>(
    host: &HostId,
    call: &BotRpcCall,
    known_avatars: &HashMap<BotId, u64, S>,
) -> Result<SidebarState, SidebarFetchError> {
    let bots = match call(&BotRpcRequest::ListBots) {
        Ok(BotRpcResponse::Bots(bots)) => bots,
        Ok(BotRpcResponse::Error(msg)) | Err(msg) => return Err(classify_sidebar_error(msg)),
        other => {
            return Err(SidebarFetchError::Transient(format!(
                "unexpected ListBots reply: {other:?}"
            )));
        }
    };
    let items = match call(&BotRpcRequest::ListItems {
        bot_id: None,
        include_closed: false,
    }) {
        Ok(BotRpcResponse::Items(items)) => items,
        Ok(BotRpcResponse::Error(msg)) | Err(msg) => {
            return Err(SidebarFetchError::Transient(msg));
        }
        _ => Vec::new(),
    };
    let access = match call(&BotRpcRequest::ListAccessRequests { bot_id: None }) {
        Ok(BotRpcResponse::AccessRequests(reqs)) => reqs,
        Ok(BotRpcResponse::Error(msg)) | Err(msg) => {
            return Err(SidebarFetchError::Transient(msg));
        }
        _ => Vec::new(),
    };
    let summaries = match call(&BotRpcRequest::ListBotSessions { bot_id: None }) {
        Ok(BotRpcResponse::BotSessions(list)) => list,
        Ok(BotRpcResponse::Error(msg)) | Err(msg) => {
            return Err(SidebarFetchError::Transient(msg));
        }
        _ => Vec::new(),
    };
    let groups = match call(&BotRpcRequest::ListGroupSummaries) {
        Ok(BotRpcResponse::GroupSummaries(groups)) => groups,
        Ok(BotRpcResponse::Ok) => Vec::new(),
        _ => match call(&BotRpcRequest::ListGroups) {
            Ok(BotRpcResponse::Groups(raw)) => wrap_legacy_groups(raw),
            _ => Vec::new(),
        },
    };
    let avatars = bots
        .iter()
        .filter(|b| super::avatar::Avatar::of(b) == super::avatar::Avatar::Upload)
        .filter(|b| known_avatars.get(&b.id) != Some(&b.updated_at_ms))
        // An unreadable picture comes back empty, so the caller remembers it and stops asking.
        .map(|b| {
            let png = match call(&BotRpcRequest::GetBotAvatar {
                bot_id: b.id.clone(),
            }) {
                Ok(BotRpcResponse::BotAvatar(data)) => data.png_bytes,
                _ => Vec::new(),
            };
            (b.id.clone(), b.updated_at_ms, png)
        })
        .collect();
    Ok(build_sidebar_state(
        host, &bots, &items, &access, &summaries, groups, avatars,
    ))
}

/// Reads bots, open items, access requests and bot sessions for the sidebar, listing bots once.
/// Uploaded pictures are read only for bots whose config time isn't in `known_avatars`.
/// `None` when the bot list is unavailable, so the caller keeps what it last showed instead of
/// flashing an empty section.
#[must_use]
pub fn fetch_sidebar_state<S: std::hash::BuildHasher>(
    call: &BotRpcCall,
    known_avatars: &HashMap<BotId, u64, S>,
) -> Option<SidebarState> {
    fetch_sidebar_state_detailed_with_rpc(&HostId::Local, call, known_avatars).ok()
}

/// Canonical scoped keychain secret reference (`<bot_id>:<key>`) aligned with daemon `SecretVault::scoped_ref`.
#[must_use]
pub fn connector_secret_ref(bot_id: &str, key: &str) -> String {
    format!("{bot_id}:{key}")
}

/// Canonical scoped keychain secret reference (`<bot_id>:mcp-<tool_id>`) for an MCP tool credential.
#[must_use]
pub fn mcp_secret_ref(bot_id: &str, tool_id: &str) -> String {
    format!("{bot_id}:mcp-{tool_id}")
}

/// Parses comma/newline-separated string IDs, dropping blanks and deduplicating while preserving order.
#[must_use]
pub fn parse_csv_strings(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in raw.split([',', '\n']) {
        let trimmed = part.trim();
        if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_owned());
        }
    }
    out
}

/// Parses comma/newline-separated signed 64-bit integer IDs (for Telegram chat/user IDs).
#[must_use]
pub fn parse_csv_i64s(raw: &str) -> Vec<i64> {
    let mut out = Vec::new();
    for part in raw.split([',', '\n']) {
        if let Ok(val) = part.trim().parse::<i64>()
            && !out.contains(&val)
        {
            out.push(val);
        }
    }
    out
}

/// Formats a slice of string IDs as a comma-separated string.
#[must_use]
pub fn format_csv_strings(items: &[String]) -> String {
    items.join(", ")
}

/// Formats a slice of `i64` IDs as a comma-separated string.
#[must_use]
pub fn format_csv_i64s(items: &[i64]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use krill_proto::bots::{BotAvatarData, BotGroupChat, BotRpcRequest, BotRpcResponse};
    use krill_proto::control::{Failure, Reply, Request};
    use krill_proto::ipc::{ClientHello, ServerHello};
    use krill_proto::os::net::UnixListener;

    use super::*;

    type MockBotHandler = Arc<dyn Fn(usize, &BotRpcRequest) -> Option<Reply> + Send + Sync>;

    static TEST_SOCK_SEQ: AtomicUsize = AtomicUsize::new(0);

    struct MockBotDaemon {
        socket: PathBuf,
        accepted: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl MockBotDaemon {
        #[expect(clippy::expect_used, reason = "test helper binds ephemeral socket")]
        fn start(features: u64, build: &str, handler: MockBotHandler) -> Self {
            let seq = TEST_SOCK_SEQ.fetch_add(1, Ordering::Relaxed);
            let socket = std::env::temp_dir()
                .join(format!("krill-bot-rpc-{}-{seq}.sock", std::process::id()));
            std::fs::remove_file(&socket).ok();
            let listener = UnixListener::bind(&socket).expect("bind unix socket");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let accepted = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let accepted_bg = Arc::clone(&accepted);
            let stop_bg = Arc::clone(&stop);
            let build = build.to_owned();
            let thread = std::thread::spawn(move || {
                while !stop_bg.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let conn_idx = accepted_bg.fetch_add(1, Ordering::AcqRel) + 1;
                            let handler = Arc::clone(&handler);
                            let build_conn = build.clone();
                            std::thread::spawn(move || {
                                stream.set_nonblocking(false).ok();
                                stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
                                let hello = ServerHello {
                                    proto_min: ipc::PROTO_MIN,
                                    proto_max: ipc::PROTO_MAX,
                                    features,
                                    build: build_conn,
                                    instance_id: 99,
                                };
                                let Ok(hello_frame) = hello.encode() else {
                                    return;
                                };
                                if stream.write_all(&hello_frame).is_err() {
                                    return;
                                }
                                let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) else {
                                    return;
                                };
                                if ClientHello::decode(kind, &body).is_err() {
                                    return;
                                }
                                while let Ok(Some((k, payload))) = ipc::read_frame(&mut stream) {
                                    let Ok(Request::Bot(bot_req)) = Request::decode(k, &payload)
                                    else {
                                        break;
                                    };
                                    let Some(reply) = handler(conn_idx, &bot_req) else {
                                        // Simulate abrupt connection drop mid-stream.
                                        break;
                                    };
                                    let Ok(frame) = reply.encode() else {
                                        break;
                                    };
                                    if stream.write_all(&frame).is_err() {
                                        break;
                                    }
                                    stream.flush().ok();
                                }
                            });
                        }
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                socket,
                accepted,
                stop,
                thread: Some(thread),
            }
        }

        fn target(&self, id: &str) -> HostTarget {
            HostTarget::remote(id, self.socket.clone())
        }
    }

    impl Drop for MockBotDaemon {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            UnixStream::connect(&self.socket).ok();
            if let Some(t) = self.thread.take() {
                t.join().ok();
            }
            std::fs::remove_file(&self.socket).ok();
        }
    }

    #[test]
    fn pooled_connection_reused_and_sidebar_fetch_pipelines_over_single_stream() {
        let mut mascot_bot = BotConfig::new(
            BotId::new("bot-mascot").unwrap(),
            "Mascot Bot",
            "claude",
            1_700_000_000_000,
        )
        .unwrap();
        mascot_bot.avatar = Some("A1".to_owned());

        let mut upload_bot = BotConfig::new(
            BotId::new("bot-upload").unwrap(),
            "Upload Bot",
            "claude",
            1_700_000_000_000,
        )
        .unwrap();
        upload_bot.avatar = Some(crate::bots::avatar::UPLOAD_AVATAR.to_owned());
        upload_bot.updated_at_ms = 42;

        let avatar_calls = Arc::new(AtomicUsize::new(0));
        let avatar_calls_bg = Arc::clone(&avatar_calls);
        let bots = vec![mascot_bot.clone(), upload_bot.clone()];
        let session = BotSessionSummary {
            session_id: SessionId(701),
            bot_id: mascot_bot.id.clone(),
            link_id: None,
            pid: 500,
            live: true,
            working: false,
            label: "Home".to_owned(),
            created_at_ms: 1_700_000_000_100,
        };
        let sessions = vec![session];

        let daemon = MockBotDaemon::start(
            FEAT_CONTROL | FEAT_BOTS,
            "0.3.24+test",
            Arc::new(move |_conn, req| {
                let resp = match req {
                    BotRpcRequest::ListBots => BotRpcResponse::Bots(bots.clone()),
                    BotRpcRequest::ListItems { .. } => BotRpcResponse::Items(Vec::new()),
                    BotRpcRequest::ListAccessRequests { .. } => {
                        BotRpcResponse::AccessRequests(Vec::new())
                    }
                    BotRpcRequest::ListBotSessions { .. } => {
                        BotRpcResponse::BotSessions(sessions.clone())
                    }
                    BotRpcRequest::ListGroupSummaries => BotRpcResponse::GroupSummaries(Vec::new()),
                    BotRpcRequest::GetBotAvatar { bot_id } => {
                        avatar_calls_bg.fetch_add(1, Ordering::AcqRel);
                        assert_eq!(
                            bot_id, &upload_bot.id,
                            "built-in mascot must never trigger GetBotAvatar"
                        );
                        BotRpcResponse::BotAvatar(BotAvatarData {
                            bot_id: bot_id.clone(),
                            mime: "image/png".to_owned(),
                            png_bytes: vec![0x89, b'P', b'N', b'G'],
                        })
                    }
                    _ => BotRpcResponse::Ok,
                };
                Some(Reply::Bot(resp))
            }),
        );

        let host_id = "pool-reuse-macmini";
        let host = HostId::Remote(host_id.to_owned());
        let target = daemon.target(host_id);
        invalidate_host_connections(&host);

        let first = fetch_sidebar_state_for_target(&host, &target, &HashMap::new())
            .expect("first pipelined sidebar fetch");
        assert_eq!(first.snapshot.bots.len(), 2);
        assert!(first.snapshot.bots.iter().all(|b| b.host == host));
        assert_eq!(first.sessions.len(), 1);
        assert_eq!(first.avatars.len(), 1);
        assert_eq!(avatar_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            daemon.accepted.load(Ordering::Acquire),
            1,
            "all sidebar RPCs + avatar batch must share 1 connection"
        );

        let known: HashMap<BotId, u64> = first
            .avatars
            .iter()
            .map(|(id, at, _)| (id.clone(), *at))
            .collect();
        let second = fetch_sidebar_state_for_target(&host, &target, &known)
            .expect("second pooled sidebar fetch");
        assert_eq!(second.snapshot.bots.len(), 2);
        assert!(second.avatars.is_empty());
        assert_eq!(
            avatar_calls.load(Ordering::Acquire),
            1,
            "known avatar timestamp must skip re-fetch"
        );
        assert_eq!(
            daemon.accepted.load(Ordering::Acquire),
            1,
            "second refresh must reuse the pooled connection with zero new handshakes"
        );

        invalidate_host_connections(&host);
        let third = fetch_sidebar_state_for_target(&host, &target, &known)
            .expect("post-invalidation fetch");
        assert_eq!(third.snapshot.bots.len(), 2);
        assert_eq!(
            daemon.accepted.load(Ordering::Acquire),
            2,
            "invalidate_host_connections must drop pooled streams"
        );
        invalidate_host_connections(&host);
    }

    #[test]
    fn pooled_connection_retries_once_on_stale_eof_for_read_only_only() {
        let conn1_reqs = Arc::new(AtomicUsize::new(0));
        let conn1_reqs_bg = Arc::clone(&conn1_reqs);
        let daemon = MockBotDaemon::start(
            FEAT_CONTROL | FEAT_BOTS,
            "0.3.24+test",
            Arc::new(move |conn, req| match (conn, req) {
                (1, BotRpcRequest::ListBots) => {
                    let n = conn1_reqs_bg.fetch_add(1, Ordering::AcqRel);
                    if n == 0 {
                        Some(Reply::Bot(BotRpcResponse::Bots(Vec::new())))
                    } else {
                        // Drop conn #1 when reused from the pool.
                        None
                    }
                }
                (2, BotRpcRequest::ListBots) => Some(Reply::Bot(BotRpcResponse::Bots(Vec::new()))),
                (2, BotRpcRequest::DeleteBot { .. }) => {
                    // Drop conn #2 mid-mutation: client MUST NOT retry a mutating RPC.
                    None
                }
                _ => Some(Reply::Bot(BotRpcResponse::Ok)),
            }),
        );

        let host_id = "pool-retry-macmini";
        let host = HostId::Remote(host_id.to_owned());
        let target = daemon.target(host_id);
        invalidate_host_connections(&host);

        // Prime pool with conn #1.
        let r1 = call_daemon_bot_batch(&target, &[BotRpcRequest::ListBots]).expect("prime");
        assert_eq!(r1.len(), 1);
        assert_eq!(daemon.accepted.load(Ordering::Acquire), 1);

        // Reuse dead conn #1 on a read-only call -> must retry transparently on conn #2.
        let r2 = call_daemon_bot_batch(&target, &[BotRpcRequest::ListBots])
            .expect("read-only must retry once on fresh connection");
        assert_eq!(r2.len(), 1);
        assert_eq!(daemon.accepted.load(Ordering::Acquire), 2);

        // Mutating call on pooled conn #2 that drops mid-flight -> MUST error, NOT open conn #3.
        let mut_res = call_daemon_bot_batch(
            &target,
            &[BotRpcRequest::DeleteBot {
                bot_id: BotId::new("bot-x").unwrap(),
                delete_home: false,
            }],
        );
        assert!(mut_res.is_err(), "dropped mutation must return error");
        assert_eq!(
            daemon.accepted.load(Ordering::Acquire),
            2,
            "mutating RPC must never auto-retry on a fresh connection"
        );
        invalidate_host_connections(&host);
    }

    #[test]
    fn remote_host_without_target_never_falls_back_to_local_daemon() {
        let rpc = daemon_bot_rpc_for_host(&HostId::Remote("offline-macmini".into()), None);
        let err = rpc(&BotRpcRequest::ListBots).expect_err("must fail without remote target");
        assert!(!err.is_empty());
    }

    #[test]
    fn sidebar_fetch_classifies_unsupported_transient_and_legacy_groups() {
        // 1. Missing FEAT_BOTS -> SidebarFetchError::Unsupported(version)
        let old_daemon = MockBotDaemon::start(
            FEAT_CONTROL,
            "0.3.6+old",
            Arc::new(|_, _| Some(Reply::Failed(Failure::BadRequest))),
        );
        let old_host = HostId::Remote("old-macmini".into());
        let err = fetch_sidebar_state_for_target(
            &old_host,
            &old_daemon.target("old-macmini"),
            &HashMap::new(),
        )
        .expect_err("missing FEAT_BOTS");
        assert_eq!(err, SidebarFetchError::Unsupported("0.3.6".to_owned()));

        // 2. Partial sub-RPC failure (ListBotSessions error) -> Transient (never wipes sessions)
        let bot = BotConfig::new(BotId::new("bot-1").unwrap(), "Shop", "claude", 1_000).unwrap();
        let bots_for_partial = vec![bot.clone()];
        let partial_daemon = MockBotDaemon::start(
            FEAT_CONTROL | FEAT_BOTS,
            "0.3.24+test",
            Arc::new(move |_, req| match req {
                BotRpcRequest::ListBots => {
                    Some(Reply::Bot(BotRpcResponse::Bots(bots_for_partial.clone())))
                }
                BotRpcRequest::ListItems { .. } => {
                    Some(Reply::Bot(BotRpcResponse::Items(Vec::new())))
                }
                BotRpcRequest::ListAccessRequests { .. } => {
                    Some(Reply::Bot(BotRpcResponse::AccessRequests(Vec::new())))
                }
                BotRpcRequest::ListBotSessions { .. } => {
                    Some(Reply::Bot(BotRpcResponse::Error("db busy".into())))
                }
                BotRpcRequest::ListGroupSummaries => {
                    Some(Reply::Bot(BotRpcResponse::GroupSummaries(Vec::new())))
                }
                _ => Some(Reply::Bot(BotRpcResponse::Ok)),
            }),
        );
        let partial_host = HostId::Remote("partial-macmini".into());
        let err = fetch_sidebar_state_for_target(
            &partial_host,
            &partial_daemon.target("partial-macmini"),
            &HashMap::new(),
        )
        .expect_err("partial sub-RPC error must fail batch instead of returning empty sessions");
        assert!(matches!(err, SidebarFetchError::Transient(_)));
        invalidate_host_connections(&partial_host);

        // 3. Legacy daemon without ListGroupSummaries falls back to ListGroups on same pooled stream
        let legacy_called = Arc::new(AtomicUsize::new(0));
        let legacy_called_bg = Arc::clone(&legacy_called);
        let bots_for_legacy = vec![bot.clone()];
        let legacy_group = BotGroupChat {
            id: "grp-legacy".into(),
            name: "Ops".into(),
            bot_ids: vec![bot.id.clone()],
            description: String::new(),
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        };
        let legacy_daemon = MockBotDaemon::start(
            FEAT_CONTROL | FEAT_BOTS,
            "0.3.20+legacy",
            Arc::new(move |_, req| match req {
                BotRpcRequest::ListBots => {
                    Some(Reply::Bot(BotRpcResponse::Bots(bots_for_legacy.clone())))
                }
                BotRpcRequest::ListItems { .. } => {
                    Some(Reply::Bot(BotRpcResponse::Items(Vec::new())))
                }
                BotRpcRequest::ListAccessRequests { .. } => {
                    Some(Reply::Bot(BotRpcResponse::AccessRequests(Vec::new())))
                }
                BotRpcRequest::ListBotSessions { .. } => {
                    Some(Reply::Bot(BotRpcResponse::BotSessions(Vec::new())))
                }
                BotRpcRequest::ListGroupSummaries => Some(Reply::Failed(Failure::BadRequest)),
                BotRpcRequest::ListGroups => {
                    legacy_called_bg.fetch_add(1, Ordering::AcqRel);
                    Some(Reply::Bot(BotRpcResponse::Groups(vec![
                        legacy_group.clone(),
                    ])))
                }
                _ => Some(Reply::Bot(BotRpcResponse::Ok)),
            }),
        );
        let legacy_host = HostId::Remote("legacy-macmini".into());
        let state = fetch_sidebar_state_for_target(
            &legacy_host,
            &legacy_daemon.target("legacy-macmini"),
            &HashMap::new(),
        )
        .expect("legacy group fallback must succeed");
        assert_eq!(legacy_called.load(Ordering::Acquire), 1);
        assert_eq!(state.groups.len(), 1);
        assert_eq!(state.groups[0].group.id, "grp-legacy");
        assert_eq!(
            legacy_daemon.accepted.load(Ordering::Acquire),
            1,
            "legacy ListGroups fallback must reuse the same pooled connection"
        );
        invalidate_host_connections(&legacy_host);
    }

    #[test]
    fn would_block_socket_error_maps_to_timed_out() {
        let mapped = map_socket_io_error(io::Error::from(io::ErrorKind::WouldBlock));
        assert_eq!(mapped.kind(), io::ErrorKind::TimedOut);
        assert_eq!(mapped.to_string(), "bot RPC timed out");

        let eof = map_socket_io_error(io::Error::from(io::ErrorKind::UnexpectedEof));
        assert_eq!(eof.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn sidebar_snapshot_keeps_dormant_post_reboot_sessions_and_sorts_live_first() {
        let bot = BotConfig::new(
            BotId::new("bot-reboot").unwrap(),
            "크릴",
            "agy",
            1_700_000_000_000,
        )
        .unwrap();
        let dormant_home = BotSessionSummary {
            session_id: SessionId(41_800),
            bot_id: bot.id.clone(),
            link_id: None,
            pid: 0,
            live: false,
            working: false,
            label: "크릴".to_owned(),
            created_at_ms: 1_700_000_001_000,
        };
        let live_other = BotSessionSummary {
            session_id: SessionId(41_900),
            bot_id: bot.id.clone(),
            link_id: Some("slack-1".to_owned()),
            pid: 5002,
            live: true,
            working: false,
            label: "Slack".to_owned(),
            created_at_ms: 1_700_000_002_000,
        };

        let snap = sidebar_snapshot(&[bot], &[], &[], &[live_other, dormant_home]);
        assert_eq!(snap.bots.len(), 1);
        let row = &snap.bots[0];
        assert_eq!(
            row.sessions.len(),
            2,
            "dormant post-reboot chat must stay visible in sidebar instead of disappearing"
        );
        assert_eq!(
            row.home_session().map(|s| s.session_id),
            Some(SessionId(41_800)),
            "dormant Home Chat must be found by home_session() so clicking resumes it"
        );
        assert_eq!(row.sessions[0].session_id, SessionId(41_800));
        assert_eq!(row.sessions[1].session_id, SessionId(41_900));
    }
}
