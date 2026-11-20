// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Non-blocking RPC transport, secret-ref naming, runtime catalog, and CSV helpers for Krill Bots UI.

use std::io::{self, Write as _};
use std::sync::Arc;
use std::time::Duration;

use krill_proto::bots::{BotAccessRequestStatus, BotItemStatus, BotRpcRequest, BotRpcResponse};
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
    pub brand_id: &'static str,
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
    BotRuntimeOption {
        id: "claude",
        brand_id: "claude",
    },
    BotRuntimeOption {
        id: "codex",
        brand_id: "codex",
    },
    BotRuntimeOption {
        id: "agy",
        brand_id: "agy",
    },
    BotRuntimeOption {
        id: "opencode",
        brand_id: "opencode",
    },
    BotRuntimeOption {
        id: "cursor",
        brand_id: "cursor",
    },
    BotRuntimeOption {
        id: "pi",
        brand_id: "pi",
    },
    BotRuntimeOption {
        id: "grok",
        brand_id: "grok",
    },
    BotRuntimeOption {
        id: "omp",
        brand_id: "omp",
    },
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

/// Computes total sidebar attention count (`needs_you` open items + `Pending` access requests).
#[must_use]
pub fn fetch_attention_count(call: &BotRpcCall) -> usize {
    let needs_you = match call(&BotRpcRequest::ListItems {
        bot_id: None,
        include_closed: false,
    }) {
        Ok(BotRpcResponse::Items(items)) => items
            .iter()
            .filter(|item| item.status == BotItemStatus::NeedsYou)
            .count(),
        _ => 0,
    };
    let pending_access = match call(&BotRpcRequest::ListAccessRequests { bot_id: None }) {
        Ok(BotRpcResponse::AccessRequests(reqs)) => reqs
            .iter()
            .filter(|req| req.status == BotAccessRequestStatus::Pending)
            .count(),
        _ => 0,
    };
    needs_you.saturating_add(pending_access)
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
