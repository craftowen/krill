// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Non-blocking RPC transport, secret-ref naming, runtime catalog, and CSV helpers for Krill Bots UI.

use std::io::{self, Write as _};
use std::sync::Arc;
use std::time::Duration;

use krill_proto::bots::{
    BotAccessRequest, BotAccessRequestStatus, BotConfig, BotId, BotItem, BotItemStatus,
    BotRpcRequest, BotRpcResponse,
};
use krill_proto::control::{FEAT_CONTROL, Reply, Request};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole, FEAT_BOTS};
use krill_proto::screen::Msg as _;

use crate::hosts::HostTarget;
use crate::i18n::catalog::bots as text;

/// Non-blocking injectable RPC closure for `BotRpcRequest` -> `BotRpcResponse`.
pub type BotRpcCall = Arc<dyn Fn(&BotRpcRequest) -> Result<BotRpcResponse, String> + Send + Sync>;

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
            "krilld does not support FEAT_BOTS",
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

/// Models offered per runtime in the New bot picker. Ids are what each CLI's `--model` accepts.
// ponytail: mirrors the chat composer fallback list (shell/chat/composer.rs); move both to one
// catalog when a daemon-side model list exists.
#[must_use]
pub fn runtime_models(runtime_id: &str) -> &'static [(&'static str, &'static str)] {
    match runtime_id {
        "claude" => &[
            ("opus", "Opus 5.5"),
            ("sonnet", "Sonnet 5.5"),
            ("haiku", "Haiku 5.5"),
            ("fable", "Fable 5.1"),
        ],
        "codex" => &[
            ("gpt-5.4", "GPT-5.4"),
            ("gpt-5.3-codex", "GPT-5.3 Codex"),
            ("gpt-5.4-mini", "GPT-5.4 Mini"),
        ],
        "agy" => &[
            ("gemini-2.5-pro", "Gemini 2.5 Pro"),
            ("gemini-2.5-flash", "Gemini 2.5 Flash"),
        ],
        _ => &[],
    }
}

/// One bot row in the main window sidebar's Bots section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarBot {
    pub id: BotId,
    pub name: String,
    pub emoji: Option<String>,
    pub paused: bool,
    /// `needs_you` items plus pending access requests owned by this bot.
    pub attention: usize,
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
            .map(|bot| SidebarBot {
                id: bot.id.clone(),
                name: bot.name.clone(),
                emoji: bot.emoji.clone().filter(|e| !e.trim().is_empty()),
                paused: bot.paused,
                attention: needs.iter().filter(|id| **id == &bot.id).count(),
            })
            .collect(),
        attention: needs.len(),
    }
}

/// Reads bots, open items and access requests for the sidebar. `None` when the bot list is
/// unavailable, so the caller keeps what it last showed instead of flashing an empty section.
#[must_use]
pub fn fetch_sidebar_snapshot(call: &BotRpcCall) -> Option<SidebarSnapshot> {
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
    Some(sidebar_snapshot(&bots, &items, &access))
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
