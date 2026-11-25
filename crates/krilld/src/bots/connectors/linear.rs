// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Outbound Linear issue comment [`PlatformSender`] (GraphQL) for Krill Bots external conversations.
//!
//! Enables immediate reply delivery while helper workers run (Orca #47532) and restart-cut
//! interruption notices (Orca #47898) on Linear-linked bot conversations.

use std::io;
use std::time::Duration;

use krill_proto::bots::ExternalSource;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::bots::connectors::policy_for_api_base;
use crate::bots::connectors::router::{DeliveryTarget, PlatformSender, sanitize_error_for_status};
use crate::bots::http_client::{self, HttpRequest, SsrfPolicy};

/// Production Linear GraphQL endpoint.
pub const DEFAULT_LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";
/// Safe single-comment length for Linear issue comments.
pub const LINEAR_MAX_COMMENT_CHARS: usize = 32_000;
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Linear GraphQL client implementing [`PlatformSender`] for issue comments.
pub struct LinearSender {
    graphql_url: String,
    api_key: Zeroizing<String>,
    ssrf_policy: SsrfPolicy,
}

impl std::fmt::Debug for LinearSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearSender")
            .field("graphql_url", &self.graphql_url)
            .finish_non_exhaustive()
    }
}

impl LinearSender {
    #[must_use]
    pub fn new(graphql_url: &str, api_key: Zeroizing<String>) -> Self {
        let url = graphql_url.trim().to_owned();
        let ssrf_policy = policy_for_api_base(&url);
        Self {
            graphql_url: url,
            api_key,
            ssrf_policy,
        }
    }

    fn post_graphql(&self, payload: &Value) -> io::Result<Value> {
        let body = serde_json::to_vec(payload).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "failed to encode Linear GraphQL payload",
            )
        })?;
        let req = HttpRequest::post_json(&self.graphql_url, body)
            .with_header("Authorization", self.api_key.trim())
            .with_timeout(API_REQUEST_TIMEOUT)
            .with_ssrf_policy(self.ssrf_policy);
        let resp = http_client::send(&req)
            .map_err(|err| io::Error::new(err.kind(), "Linear GraphQL transport error"))?;
        let parsed: Value = resp.json().unwrap_or(Value::Null);
        if !resp.is_success() {
            return Err(io::Error::other(format!(
                "HTTP {}: Linear GraphQL failed",
                resp.status
            )));
        }
        if let Some(err_msg) = parsed
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
        {
            return Err(io::Error::other(sanitize_error_for_status(err_msg)));
        }
        Ok(parsed)
    }
}

impl PlatformSender for LinearSender {
    fn source(&self) -> ExternalSource {
        ExternalSource::Linear
    }

    fn max_chunk_chars(&self) -> usize {
        LINEAR_MAX_COMMENT_CHARS
    }

    fn send_message(&self, target: &DeliveryTarget, text: &str) -> io::Result<String> {
        let issue_id = target.channel_id.trim();
        if issue_id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Linear target requires issue id in channel_id",
            ));
        }
        let mut input = json!({
            "issueId": issue_id,
            "body": text,
        });
        if let Some(parent_id) = target
            .thread_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            && let Some(obj) = input.as_object_mut()
        {
            obj.insert("parentId".to_owned(), json!(parent_id));
        }
        let payload = json!({
            "query": "mutation KrillCommentCreate($input: CommentCreateInput!) { commentCreate(input: $input) { success comment { id } } }",
            "variables": { "input": input },
        });
        let parsed = self.post_graphql(&payload)?;
        let id = parsed
            .get("data")
            .and_then(|d| d.get("commentCreate"))
            .and_then(|c| c.get("comment"))
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("0")
            .to_owned();
        Ok(id)
    }

    fn edit_message(
        &self,
        _target: &DeliveryTarget,
        message_id: &str,
        text: &str,
    ) -> io::Result<()> {
        let comment_id = message_id.trim();
        if comment_id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Linear comment id cannot be empty",
            ));
        }
        let payload = json!({
            "query": "mutation KrillCommentUpdate($id: String!, $input: CommentUpdateInput!) { commentUpdate(id: $id, input: $input) { success } }",
            "variables": {
                "id": comment_id,
                "input": { "body": text },
            },
        });
        self.post_graphql(&payload)?;
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
    fn linear_sender_creates_and_updates_issue_comments() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = thread::spawn(move || {
            let mut seen = Vec::new();
            for idx in 0..2 {
                let (mut sock, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let body = if idx == 0 {
                    r#"{"data":{"commentCreate":{"success":true,"comment":{"id":"lin-c-1"}}}}"#
                } else {
                    r#"{"data":{"commentUpdate":{"success":true}}}"#
                };
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
            }
            seen
        });

        let sender = LinearSender::new(
            &format!("http://{addr}/graphql"),
            Zeroizing::new("lin_api_test".to_owned()),
        );
        let target = DeliveryTarget {
            source: ExternalSource::Linear,
            workspace_id: None,
            channel_id: "issue-uuid-123".to_owned(),
            thread_id: Some("parent-comment-9".to_owned()),
            reply_to_message_id: None,
        };
        let cid = sender
            .send_message(&target, "Investigating with worker.")
            .unwrap();
        assert_eq!(cid, "lin-c-1");
        sender.edit_message(&target, &cid, "Resolved.").unwrap();

        let seen = srv.join().unwrap();
        assert!(seen[0].contains("commentCreate"));
        assert!(seen[0].contains("issue-uuid-123"));
        assert!(seen[0].contains("parent-comment-9"));
        assert!(seen[1].contains("commentUpdate"));
        assert!(seen[1].contains("lin-c-1"));
    }
}
