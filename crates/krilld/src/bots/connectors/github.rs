// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Outbound GitHub issue/PR comment [`PlatformSender`] for Krill Bots external conversations.
//!
//! Enables immediate reply delivery while helper workers run (Orca #47532) and restart-cut
//! interruption notices (Orca #47898) on GitHub-linked bot conversations.

use std::io;
use std::time::Duration;

use krill_proto::bots::ExternalSource;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::bots::connectors::policy_for_api_base;
use crate::bots::connectors::router::{DeliveryTarget, PlatformSender, sanitize_error_for_status};
use crate::bots::http_client::{self, HttpMethod, HttpRequest, SsrfPolicy};

/// Production GitHub REST API base URL.
pub const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com";
/// Safe single-comment length for GitHub issue/PR comments (GitHub hard cap is 65,536 chars).
pub const GITHUB_MAX_COMMENT_CHARS: usize = 60_000;
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// GitHub REST API v3 client implementing [`PlatformSender`] for issue/PR comments.
pub struct GitHubSender {
    api_base: String,
    token: Zeroizing<String>,
    ssrf_policy: SsrfPolicy,
}

impl std::fmt::Debug for GitHubSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubSender")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl GitHubSender {
    #[must_use]
    pub fn new(api_base: &str, token: Zeroizing<String>) -> Self {
        let base = api_base.trim_end_matches('/').to_owned();
        let ssrf_policy = policy_for_api_base(&base);
        Self {
            api_base: base,
            token,
            ssrf_policy,
        }
    }

    fn send_json(&self, method: HttpMethod, path: &str, payload: &Value) -> io::Result<Value> {
        let body = serde_json::to_vec(payload).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "failed to encode GitHub API payload",
            )
        })?;
        let url = format!("{}{path}", self.api_base);
        let req = HttpRequest::new(method, url)
            .with_header("Authorization", format!("Bearer {}", self.token.as_str()))
            .with_header("Accept", "application/vnd.github+json")
            .with_header("X-GitHub-Api-Version", "2022-11-28")
            .with_header("Content-Type", "application/json")
            .with_body(body)
            .with_timeout(API_REQUEST_TIMEOUT)
            .with_ssrf_policy(self.ssrf_policy);
        let resp = http_client::send(&req)
            .map_err(|err| io::Error::new(err.kind(), "GitHub REST transport error"))?;
        let parsed: Value = resp.json().unwrap_or(Value::Null);
        if resp.is_success() {
            Ok(parsed)
        } else {
            let msg = parsed
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("github_api_error");
            Err(io::Error::other(format!(
                "HTTP {}: {}",
                resp.status,
                sanitize_error_for_status(msg)
            )))
        }
    }
}

/// Parses `(owner, repo, issue_number)` from a GitHub [`DeliveryTarget`].
///
/// Accepts either `channel_id = "owner/repo#42"` or `channel_id = "owner/repo"` with
/// `thread_id = Some("42")` / `Some("#42")`.
pub(crate) fn parse_github_target(target: &DeliveryTarget) -> io::Result<(&str, &str, u64)> {
    let channel = target.channel_id.trim();
    let (repo_part, raw_num) = if let Some((repo, num)) = channel.split_once('#') {
        (repo.trim(), num.trim())
    } else if let Some(tid) = target.thread_id.as_deref() {
        (channel, tid.trim().trim_start_matches('#'))
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "GitHub target requires owner/repo#number or thread_id",
        ));
    };
    let (owner, repo) = repo_part.split_once('/').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "GitHub target channel must be owner/repo",
        )
    })?;
    let valid_ident = |s: &str| {
        !s.is_empty()
            && !s.contains("..")
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    if !valid_ident(owner) || !valid_ident(repo) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid GitHub owner/repo identifier",
        ));
    }
    let number = raw_num
        .parse::<u64>()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid GitHub issue/PR number",
            )
        })?;
    Ok((owner, repo, number))
}

impl PlatformSender for GitHubSender {
    fn source(&self) -> ExternalSource {
        ExternalSource::GitHub
    }

    fn max_chunk_chars(&self) -> usize {
        GITHUB_MAX_COMMENT_CHARS
    }

    fn send_message(&self, target: &DeliveryTarget, text: &str) -> io::Result<String> {
        let (owner, repo, number) = parse_github_target(target)?;
        let path = format!("/repos/{owner}/{repo}/issues/{number}/comments");
        let parsed = self.send_json(HttpMethod::Post, &path, &json!({ "body": text }))?;
        let id = parsed
            .get("id")
            .and_then(|v| {
                v.as_u64()
                    .map(|n| n.to_string())
                    .or_else(|| v.as_str().map(ToOwned::to_owned))
            })
            .unwrap_or_else(|| "0".to_owned());
        Ok(id)
    }

    fn edit_message(
        &self,
        target: &DeliveryTarget,
        message_id: &str,
        text: &str,
    ) -> io::Result<()> {
        let (owner, repo, _number) = parse_github_target(target)?;
        let comment_id = message_id.trim();
        if comment_id.is_empty() || !comment_id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid GitHub comment id",
            ));
        }
        let path = format!("/repos/{owner}/{repo}/issues/comments/{comment_id}");
        self.send_json(HttpMethod::Patch, &path, &json!({ "body": text }))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn github_sender_posts_and_edits_issue_comments_and_rejects_bad_targets() {
        let bad = DeliveryTarget {
            source: ExternalSource::GitHub,
            workspace_id: None,
            channel_id: "../evil/repo#1".to_owned(),
            thread_id: None,
            reply_to_message_id: None,
        };
        assert!(parse_github_target(&bad).is_err());

        let t1 = DeliveryTarget {
            source: ExternalSource::GitHub,
            workspace_id: None,
            channel_id: "craftowen/krill#42".to_owned(),
            thread_id: None,
            reply_to_message_id: None,
        };
        assert_eq!(
            parse_github_target(&t1).unwrap(),
            ("craftowen", "krill", 42)
        );

        let t2 = DeliveryTarget {
            source: ExternalSource::GitHub,
            workspace_id: None,
            channel_id: "craftowen/krill".to_owned(),
            thread_id: Some("99".to_owned()),
            reply_to_message_id: None,
        };
        assert_eq!(
            parse_github_target(&t2).unwrap(),
            ("craftowen", "krill", 99)
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = thread::spawn(move || {
            let mut seen = Vec::new();
            for idx in 0..2 {
                let (mut sock, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                seen.push(req);
                let body = if idx == 0 {
                    r#"{"id":55501}"#
                } else {
                    r#"{"id":55501,"body":"updated"}"#
                };
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
            }
            seen
        });

        let sender = GitHubSender::new(
            &format!("http://{addr}"),
            Zeroizing::new("ghp_test".to_owned()),
        );
        let id = sender.send_message(&t1, "Started helper worker.").unwrap();
        assert_eq!(id, "55501");
        sender.edit_message(&t1, &id, "Updated note.").unwrap();

        let seen = srv.join().unwrap();
        assert!(seen[0].starts_with("POST /repos/craftowen/krill/issues/42/comments "));
        assert!(seen[0].contains("Started helper worker."));
        assert!(seen[1].starts_with("PATCH /repos/craftowen/krill/issues/comments/55501 "));
    }
}
