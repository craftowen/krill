//! Opt-in completion webhooks (MK-086). Only a session ID crosses this boundary;
//! terminal output, titles, agent names and paths cannot become message content.
//!
use std::fmt;
use std::io::{self, Write as _};
use std::process::{Command, Stdio};

use krill_proto::{SessionId, control::AgentState};
use serde::{Deserialize, Serialize};

/// A user-configured destination. Telegram URLs must include `/sendMessage`.
/// Credentials are deliberately omitted from Debug and transport diagnostics.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum Endpoint {
    Ntfy { url: String },
    Telegram { url: String, chat_id: String },
    Discord { url: String },
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ntfy { .. } => "Ntfy([redacted])",
            Self::Telegram { .. } => "Telegram([redacted])",
            Self::Discord { .. } => "Discord([redacted])",
        })
    }
}

impl Endpoint {
    #[must_use]
    pub fn url(&self) -> &str {
        match self {
            Self::Ntfy { url } | Self::Telegram { url, .. } | Self::Discord { url } => url,
        }
    }

    fn payload(
        &self,
        id: SessionId,
        state: AgentState,
        deep_link: Option<&str>,
    ) -> (&'static str, String) {
        let status = match state {
            AgentState::Waiting => "waiting for input",
            AgentState::Blocked => "blocked",
            _ => "completed",
        };
        let mut message = format!("Krill: session #{} {status}.", id.0);
        if let Some(link) = deep_link {
            message.push('\n');
            message.push_str(link);
        }
        match self {
            Self::Ntfy { .. } => ("text/plain; charset=utf-8", message),
            Self::Telegram { chat_id, .. } => (
                "application/json",
                serde_json::json!({"chat_id": chat_id, "text": message}).to_string(),
            ),
            Self::Discord { .. } => (
                "application/json",
                serde_json::json!({"content": message, "allowed_mentions": {"parse": []}})
                    .to_string(),
            ),
        }
    }
}

/// At most two POST attempts, including transport and non-2xx HTTP failures.
/// A failed delivery cannot affect a PTY or session. No persistent queue exists.
pub fn deliver(endpoint: &Endpoint, id: SessionId) -> io::Result<()> {
    deliver_transition(endpoint, id, AgentState::Done, None)
}

/// Fixed transition metadata and a credential-free gateway URL only.
pub fn deliver_transition(
    endpoint: &Endpoint,
    id: SessionId,
    state: AgentState,
    deep_link: Option<&str>,
) -> io::Result<()> {
    let url = validated_url(endpoint.url())?;
    let (content_type, body) = endpoint.payload(id, state, deep_link);
    let config = format!(
        "url = {}\nheader = {}\ndata-binary = {}\n",
        config_quote(url),
        config_quote(&format!("Content-Type: {content_type}")),
        config_quote(&body),
    );
    for _ in 0..2 {
        if post(&config).is_ok() {
            return Ok(());
        }
    }
    Err(io::Error::other("completion webhook delivery failed"))
}

pub(super) fn validated_url(raw: &str) -> io::Result<&str> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "invalid webhook URL");
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) || raw.contains(['\\', '#']) {
        return Err(invalid());
    }
    let (scheme, rest) = raw.split_once("://").ok_or_else(invalid)?;
    let authority = rest.split(['/', '?']).next().ok_or_else(invalid)?;
    if authority.is_empty() || authority.contains('@') {
        return Err(invalid());
    }
    let host = if authority.starts_with('[') {
        let end = authority.find(']').ok_or_else(invalid)?;
        authority.get(..=end).ok_or_else(invalid)?
    } else {
        authority.split(':').next().ok_or_else(invalid)?
    };
    let loopback = matches!(host, "127.0.0.1" | "localhost" | "[::1]");
    if host.is_empty() || !(scheme == "https" || scheme == "http" && loopback) {
        return Err(invalid());
    }
    Ok(raw)
}

/// curl config is sent through stdin so secret URLs never appear in argv.
fn config_quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
            .replace('\t', "\\t")
    )
}

fn post(config: &str) -> io::Result<()> {
    // Disable user curlrc, proxies and URL globbing. Do not follow redirects:
    // neither credentials nor payload may move to another endpoint. Discard
    // response bodies and diagnostics; only the fixed-size HTTP status is read.
    let mut child = Command::new("/usr/bin/curl")
        .args([
            "--disable",
            "--silent",
            "--globoff",
            "--noproxy",
            "*",
            "--proto",
            "=https,http",
            "--connect-timeout",
            "3",
            "--max-time",
            "5",
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            "--config",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let written = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(config.as_bytes()),
        None => Err(io::Error::other("webhook stdin unavailable")),
    };
    // Always reap our child, including a broken stdin pipe.
    let result = child.wait_with_output()?;
    written?;
    let code = std::str::from_utf8(&result.stdout)
        .ok()
        .and_then(|s| s.parse::<u16>().ok());
    if result.status.success() && code.is_some_and(|c| (200..300).contains(&c)) {
        Ok(())
    } else {
        Err(io::Error::other("webhook POST failed"))
    }
}
