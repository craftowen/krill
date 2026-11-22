// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Non-blocking RPC transport, response validation, avatar/session metadata, and CSV helpers for Krill Bots UI.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write as _};
use std::sync::Arc;
use std::time::Duration;

use krill_proto::SessionId;
use krill_proto::bots::{
    BotAccessRequest, BotAccessRequestStatus, BotAgentModel, BotConfig, BotId, BotItem,
    BotItemStatus, BotRpcRequest, BotRpcResponse, BotSessionSummary, MAX_AVATAR_PNG_BYTES,
};
use krill_proto::control::{FEAT_CONTROL, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole, FEAT_BOTS};
use krill_proto::screen::Msg as _;

use crate::hosts::HostTarget;
use crate::i18n::catalog::bots as text;
use crate::layout::TabColor;

/// Non-blocking injectable RPC closure for `BotRpcRequest` -> `BotRpcResponse`.
pub type BotRpcCall = Arc<dyn Fn(&BotRpcRequest) -> Result<BotRpcResponse, String> + Send + Sync>;

/// Lightweight UI identity cached for sessions owned by a bot (shown in tabs & sidebar rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotSessionMeta {
    pub bot_id: BotId,
    pub bot_name: String,
    pub emoji: Option<String>,
    pub avatar_color: Option<String>,
}

impl BotSessionMeta {
    /// Builds display metadata from a [`BotConfig`].
    #[must_use]
    pub fn from_bot(bot: &BotConfig) -> Self {
        Self {
            bot_id: bot.id.clone(),
            bot_name: bot.name.clone(),
            emoji: bot
                .emoji
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            avatar_color: bot
                .avatar_color
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
        }
    }

    /// Formats `<emoji> <bot_name>` when emoji is set, or `<bot_name>` otherwise.
    #[must_use]
    pub fn display_prefix(&self) -> String {
        match self
            .emoji
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(emoji) => format!("{emoji} {}", self.bot_name),
            None => self.bot_name.clone(),
        }
    }

    /// Resolves the mapped semantic [`TabColor`] if a known hex/name is set.
    #[must_use]
    pub fn tab_color(&self) -> Option<TabColor> {
        parse_avatar_color(self.avatar_color.as_deref())
    }
}

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
#[must_use]
pub fn parse_avatar_color(raw: Option<&str>) -> Option<TabColor> {
    let trimmed = raw?.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.as_str() {
        "purple" | "#a855f7" | "#8b5cf6" => Some(TabColor::Purple),
        "pink" | "#ec4899" => Some(TabColor::Pink),
        "red" | "#ef4444" => Some(TabColor::Red),
        "orange" | "#f97316" => Some(TabColor::Orange),
        "yellow" | "#eab308" => Some(TabColor::Yellow),
        "green" | "#22c55e" => Some(TabColor::Green),
        "teal" | "#14b8a6" | "#06b6d4" => Some(TabColor::Teal),
        "gray" | "grey" | "#9ca3af" | "#6b7280" => Some(TabColor::Gray),
        _ => Some(TabColor::Blue),
    }
}

/// Supported reasoning effort choices (empty string = runtime default).
pub const BOT_EFFORT_OPTIONS: [&str; 4] = ["", "low", "medium", "high"];

/// Localized display label for a reasoning effort string.
#[must_use]
pub fn effort_label(effort: &str) -> &'static str {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => text::EFFORT_LOW.get(),
        "medium" => text::EFFORT_MEDIUM.get(),
        "high" => text::EFFORT_HIGH.get(),
        _ => text::EFFORT_DEFAULT.get(),
    }
}

/// Validates and wraps raw PNG bytes into a GPUI [`gpui::Image`] handle.
pub fn decode_png_avatar(bytes: &[u8]) -> Result<Arc<gpui::Image>, String> {
    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() > MAX_AVATAR_PNG_BYTES {
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

/// Default read timeout for bot daemon RPC calls (longer for OAuth/sync/tidy/look-around).
const BOT_RPC_READ_TIMEOUT: Duration = Duration::from_secs(30);
const BOT_RPC_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Builds a live daemon RPC caller talking to `krilld` via `Request::Bot` / `Reply::Bot`.
#[must_use]
pub fn daemon_bot_rpc(target: Option<HostTarget>) -> BotRpcCall {
    #[cfg(test)]
    if target.is_none() {
        return Arc::new(|_| Err(text::DAEMON_UNAVAILABLE.get().to_owned()));
    }
    Arc::new(move |req| call_daemon_bot(target.as_ref(), req).map_err(|e| e.to_string()))
}

fn call_daemon_bot(target: Option<&HostTarget>, req: &BotRpcRequest) -> io::Result<BotRpcResponse> {
    let hello = daemon::client_hello(ClientRole::App, "krill-app-bots");
    let resolved = match target {
        Some(t) => t.clone(),
        None => HostTarget::local_dev()?,
    };
    let conn = resolved.connect(&hello).map_err(io::Error::other)?;
    let need = FEAT_CONTROL | FEAT_BOTS;
    if conn.negotiated.features & need != need {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            unsupported_error(&conn.server.build),
        ));
    }
    let mut sock = conn.stream;
    sock.set_read_timeout(Some(BOT_RPC_READ_TIMEOUT)).ok();
    sock.set_write_timeout(Some(BOT_RPC_WRITE_TIMEOUT)).ok();
    let frame = Request::Bot(req.clone()).encode()?;
    sock.write_all(&frame)?;
    let (kind, body) = ipc::read_frame(&mut sock)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    match Reply::decode(kind, &body)? {
        Reply::Bot(resp) => Ok(resp),
        Reply::Failed(failure) => Err(io::Error::other(failure.to_string())),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
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

/// Invokes a bot RPC call and promotes [`BotRpcResponse::Error`] into `Err(msg)`.
pub fn call_checked(call: &BotRpcCall, req: &BotRpcRequest) -> Result<BotRpcResponse, String> {
    match call(req)? {
        BotRpcResponse::Error(msg) => Err(msg),
        other => Ok(other),
    }
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
    if !matches!(runtime_id, "claude" | "codex" | "agy") {
        return Vec::new();
    }
    crate::agent_models::builtin(runtime_id)
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
                let mut live: Vec<BotSessionSummary> = sessions
                    .iter()
                    .filter(|s| s.bot_id == bot.id && s.live)
                    .cloned()
                    .collect();
                live.sort_by_key(|s| (s.link_id.is_some(), std::cmp::Reverse(s.created_at_ms)));
                SidebarBot {
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
                    sessions: live,
                    updated_at_ms: bot.updated_at_ms,
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
    pub sessions: BTreeMap<SessionId, BotSessionMeta>,
    /// Newly read `avatar.png` bytes: bot, the config time they belong to, PNG (empty when
    /// it couldn't be read).
    pub avatars: Vec<(BotId, u64, Vec<u8>)>,
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
    let Ok(BotRpcResponse::Bots(bots)) = call(&BotRpcRequest::ListBots) else {
        return None;
    };
    let items = match call(&BotRpcRequest::ListItems {
        bot_id: None,
        include_closed: false,
    }) {
        Ok(BotRpcResponse::Items(items)) => items,
        _ => Vec::new(),
    };
    let access = match call(&BotRpcRequest::ListAccessRequests { bot_id: None }) {
        Ok(BotRpcResponse::AccessRequests(reqs)) => reqs,
        _ => Vec::new(),
    };
    let summaries = match call(&BotRpcRequest::ListBotSessions { bot_id: None }) {
        Ok(BotRpcResponse::BotSessions(list)) => list,
        _ => Vec::new(),
    };
    let metas: HashMap<&BotId, BotSessionMeta> = bots
        .iter()
        .map(|b| (&b.id, BotSessionMeta::from_bot(b)))
        .collect();
    let sessions = summaries
        .iter()
        .filter_map(|s| Some((s.session_id, metas.get(&s.bot_id)?.clone())))
        .collect();
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
    Some(SidebarState {
        snapshot: sidebar_snapshot(&bots, &items, &access, &summaries),
        sessions,
        avatars,
    })
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
