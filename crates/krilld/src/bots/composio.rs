// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! BYO Composio integration (`https://backend.composio.dev/api/v3.1`).
//!
//! Resolves a Composio MCP endpoint using the user's BYO `x-api-key` from [`SecretVault`] and
//! registers the upstream endpoint on [`BotToolRelay`] so `x-api-key` is injected locally on
//! loopback requests and NEVER exposed to agent CLI argv/env/config.

use krill_proto::bots::{
    BotConfig, BotConnectorStatus, BotErrorCategory, BotErrorDetail, BotMcpAuthKind,
    ConnectorHealthState, ExternalSource,
};

use crate::bots::http_client::{self, HttpRequest};
use crate::bots::secrets::SecretVault;
use crate::bots::tool_relay::{BotToolRelay, ToolRoute, make_cred_id};

/// Default Composio v3.1 API base URL.
pub const DEFAULT_COMPOSIO_BASE_URL: &str = "https://backend.composio.dev/api/v3.1";

/// Resolved Composio MCP session endpoint registered on [`BotToolRelay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposioMcpSession {
    pub cred_id: String,
    pub upstream_url: String,
    pub enabled_apps: Vec<String>,
}

/// Resolves the bot's Composio MCP endpoint via BYO `x-api-key` and registers it with [`BotToolRelay`].
pub fn resolve_and_register_composio(
    bot: &BotConfig,
    secrets: &SecretVault,
    relay: &BotToolRelay,
    base_url: &str,
) -> Result<ComposioMcpSession, BotErrorDetail> {
    let api_key = secrets
        .resolve_secret(
            &bot.id,
            bot.connectors.composio.api_key_secret_ref.as_deref(),
            "composio_api_key",
        )
        .map_err(|err| BotErrorDetail {
            category: BotErrorCategory::NotSetUp,
            agent_name: None,
            message: format!("failed to read Composio API key: {}", err.kind()),
            resets_at_ms: None,
        })?
        .ok_or_else(|| BotErrorDetail {
            category: BotErrorCategory::NotSetUp,
            agent_name: None,
            message: "Composio API key not configured in secret vault".to_owned(),
            resets_at_ms: None,
        })?;

    let base = base_url.trim_end_matches('/');
    let endpoint = format!("{base}/mcp/servers");
    let body = serde_json::json!({
        "name": format!("krill-{}", bot.slug.as_str()),
        "toolkits": bot.connectors.composio.enabled_apps,
    });
    let payload = serde_json::to_vec(&body).map_err(|_| BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: "failed to encode Composio request".to_owned(),
        resets_at_ms: None,
    })?;

    let resp = http_client::send(
        &HttpRequest::post_json(&endpoint, payload)
            .with_header("x-api-key", api_key.trim())
            .with_header("Accept", "application/json"),
    )
    .map_err(|err| BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: format!("Composio API network error: {}", err.kind()),
        resets_at_ms: None,
    })?;

    if resp.status == 401 || resp.status == 403 {
        return Err(BotErrorDetail {
            category: BotErrorCategory::SignedOut,
            agent_name: None,
            message: format!("Composio API key rejected (HTTP {})", resp.status),
            resets_at_ms: None,
        });
    }
    if !resp.is_success() {
        return Err(BotErrorDetail {
            category: BotErrorCategory::AgentError,
            agent_name: None,
            message: format!("Composio API returned HTTP {}", resp.status),
            resets_at_ms: None,
        });
    }

    let upstream_url = extract_mcp_url(&resp.body, base);
    let cred_id = make_cred_id(&bot.id, "composio");

    relay.register_route(ToolRoute {
        cred_id: cred_id.clone(),
        bot_id: bot.id.clone(),
        tool_id: "composio".to_owned(),
        name: "Composio MCP".to_owned(),
        upstream_url: upstream_url.clone(),
        auth_kind: BotMcpAuthKind::BearerHeader,
        header_names: vec!["x-api-key".to_owned()],
        secret_ref: bot.connectors.composio.api_key_secret_ref.clone(),
        fallback_secret_key: "composio_api_key".to_owned(),
    });

    Ok(ComposioMcpSession {
        cred_id,
        upstream_url,
        enabled_apps: bot.connectors.composio.enabled_apps.clone(),
    })
}

/// Syncs Composio for `bot` and returns a [`BotConnectorStatus`] snapshot.
pub fn sync_composio_bot(
    bot: &BotConfig,
    secrets: &SecretVault,
    relay: &BotToolRelay,
    base_url: &str,
    now_ms: u64,
) -> BotConnectorStatus {
    match resolve_and_register_composio(bot, secrets, relay, base_url) {
        Ok(session) => BotConnectorStatus {
            bot_id: bot.id.clone(),
            source: ExternalSource::Composio,
            enabled: bot.connectors.composio.enabled,
            connected: true,
            state: ConnectorHealthState::Connected,
            account_label: Some(format!("{} app(s)", session.enabled_apps.len())),
            last_sync_ms: Some(now_ms),
            last_error: None,
            error_category: None,
        },
        Err(detail) => BotConnectorStatus {
            bot_id: bot.id.clone(),
            source: ExternalSource::Composio,
            enabled: bot.connectors.composio.enabled,
            connected: false,
            state: ConnectorHealthState::Error,
            account_label: None,
            last_sync_ms: None,
            last_error: Some(detail.message),
            error_category: Some(detail.category),
        },
    }
}

fn extract_mcp_url(body: &[u8], base_url: &str) -> String {
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(body) {
        let candidates = [
            val.get("mcp_url"),
            val.get("endpoint_url"),
            val.get("url"),
            val.get("data").and_then(|d| d.get("mcp_url")),
            val.get("data").and_then(|d| d.get("endpoint_url")),
            val.get("data").and_then(|d| d.get("url")),
        ];
        for cand in candidates.into_iter().flatten() {
            if let Some(u) = cand.as_str()
                && (u.starts_with("http://") || u.starts_with("https://"))
            {
                return u.to_owned();
            }
        }
        if let Some(id) = val
            .get("id")
            .or_else(|| val.get("data").and_then(|d| d.get("id")))
            .and_then(serde_json::Value::as_str)
            && !id.is_empty()
        {
            return format!("{base_url}/mcp/{id}");
        }
    }
    format!("{base_url}/mcp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::BotId;
    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use zeroize::Zeroizing;

    use crate::session::lock;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-composio-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn composio_resolves_session_and_injects_api_key_through_tool_relay() {
        let dir = temp_dir("mcp");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x99; 32]));
        let relay = BotToolRelay::new(Arc::clone(&secrets));

        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = mock.local_addr().unwrap();
        let saw_mcp_key = Arc::new(Mutex::new(false));
        let saw_bg = Arc::clone(&saw_mcp_key);

        let server = thread::spawn(move || {
            for idx in 0..2 {
                let (mut sock, _) = mock.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(buf.get(..n).unwrap_or_default());
                let body = if idx == 0 {
                    assert!(req.contains("x-api-key: cmp_live_secret"));
                    format!(r#"{{"id":"srv-1","mcp_url":"http://{addr}/mcp/srv-1"}}"#)
                } else {
                    if req.contains("x-api-key: cmp_live_secret") {
                        *lock(&saw_bg) = true;
                    }
                    r#"{"jsonrpc":"2.0","result":{"ok":true}}"#.to_owned()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                sock.write_all(resp.as_bytes()).unwrap();
            }
        });

        let bot_id = BotId::new("bot-cmp").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "Composio Bot", "claude", 1_000).unwrap();
        bot.connectors.composio.enabled = true;
        bot.connectors.composio.enabled_apps = vec!["notion".to_owned(), "jira".to_owned()];

        secrets
            .set_secret(
                &bot_id,
                "composio_api_key",
                Zeroizing::new("cmp_live_secret".to_owned()),
            )
            .unwrap();

        let base_url = format!("http://{addr}/api/v3.1");
        let status = sync_composio_bot(&bot, &secrets, &relay, &base_url, 2_000);
        assert!(status.connected);

        let grant = relay.prepare_launch_for_bot(&bot).unwrap().unwrap();
        let cred_id = grant.cred_ids.first().unwrap();
        let relay_resp = http_client::send(
            &HttpRequest::post_json(
                format!("{}/bot-tools/{cred_id}/mcp", grant.base_url),
                b"{}".to_vec(),
            )
            .with_header("Authorization", format!("Bearer {}", *grant.token)),
        )
        .unwrap();
        assert_eq!(relay_resp.status, 200);

        server.join().unwrap();
        assert!(*lock(&saw_mcp_key));

        relay.shutdown();
        fs::remove_dir_all(&dir).ok();
    }
}
