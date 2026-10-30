//! Opt-in completion webhooks (MK-086). Only a session ID crosses this boundary;
//! terminal output, titles, agent names and paths cannot become message content.
//!
use std::fmt;
use std::io::{self, Write as _};
use std::process::Stdio;

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
    let curl = if cfg!(windows) {
        "curl.exe"
    } else {
        "/usr/bin/curl"
    };
    let discard = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut child = krill_proto::os::background_command(curl)
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
            discard,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn native_curl_posts_completion_to_loopback_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut peer = loop {
                match listener.accept() {
                    Ok((peer, _)) => break peer,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "webhook did not connect");
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("webhook accept failed: {error}"),
                }
            };
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                peer.read_exact(&mut byte).unwrap();
                request.extend_from_slice(&byte);
                assert!(request.len() < 8192);
            }
            let headers = String::from_utf8(request).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            assert!(length < 8192);
            let mut body = vec![0; length];
            peer.read_exact(&mut body).unwrap();
            peer.write_all(
                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
            (headers, body)
        });
        let result = deliver_transition(
            &Endpoint::Discord {
                url: format!("http://{address}/completion"),
            },
            SessionId(42),
            AgentState::Done,
            None,
        );
        let (headers, body) = server.join().unwrap();
        result.unwrap();
        assert!(headers.starts_with("POST /completion HTTP/1.1\r\n"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            serde_json::json!({
                "content": "Krill: session #42 completed.",
                "allowed_mentions": {"parse": []},
            })
        );
    }
}
