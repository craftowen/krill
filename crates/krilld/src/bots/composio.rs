// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! BYO Composio v3.1 Tool Router integration (`https://backend.composio.dev/api/v3.1`).
//!
//! Implements Orca's Tool Router endpoints:
//! - `POST /tool_router/session` (`user_id`, `manage_connections.enable=false`,
//!   `workbench.enable=false`) with per-bot `session_id` reuse & `PATCH /tool_router/session/{id}`
//! - `GET /connected_accounts?user_ids=...` -> `Vec<ComposioConnectionInfo>`
//! - `POST /tool_router/session/{id}/link` -> `(connection_id, redirect_url)`
//! - `DELETE /connected_accounts/{id}`
//! - `GET /toolkits` and `GET /trigger_instances/active` (ingesting `BotItem`s on new triggers)
//!
//! Registers the resolved MCP endpoint onto [`BotToolRelay`] so `x-api-key` stays inside
//! [`SecretVault`] and is NEVER exposed to agent CLI argv, environment, or config.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use krill_proto::bots::{
    BotConfig, BotConnectorStatus, BotConnectorTestResult, BotErrorCategory, BotErrorDetail, BotId,
    BotItem, BotItemActor, BotItemLink, BotItemStatus, BotMcpAuthKind, ComposioConnectionInfo,
    ConnectorHealthState, CreateBotItemParams, ExternalSource,
};
use serde::{Deserialize, Serialize};

use crate::bots::http_client::{self, HttpMethod, HttpRequest, SsrfPolicy};
use crate::bots::items::BotItemsStore;
use crate::bots::secrets::SecretVault;
use crate::bots::tool_relay::{BotToolRelay, ToolRoute, make_cred_id};
use crate::session::lock;

/// Default Composio v3.1 API base URL.
pub const DEFAULT_COMPOSIO_BASE_URL: &str = "https://backend.composio.dev/api/v3.1";

const SESSIONS_FILE: &str = "composio-sessions.json";

/// Resolved Composio MCP session endpoint registered on [`BotToolRelay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposioMcpSession {
    pub cred_id: String,
    pub session_id: String,
    pub upstream_url: String,
    pub enabled_apps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ComposioSessionRecord {
    session_id: String,
    user_id: String,
    mcp_url: String,
    #[serde(default)]
    enabled_apps: Vec<String>,
    #[serde(default)]
    updated_at_ms: u64,
}

fn memory_sessions() -> &'static Mutex<BTreeMap<BotId, ComposioSessionRecord>> {
    static CACHE: OnceLock<Mutex<BTreeMap<BotId, ComposioSessionRecord>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn seen_triggers() -> &'static Mutex<BTreeSet<(BotId, String)>> {
    static SEEN: OnceLock<Mutex<BTreeSet<(BotId, String)>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn policy_for_base_url(base_url: &str) -> SsrfPolicy {
    if cfg!(test)
        && (base_url.starts_with("http://127.0.0.1")
            || base_url.starts_with("http://localhost")
            || base_url.starts_with("http://[::1]"))
    {
        SsrfPolicy::AllowLoopback
    } else {
        SsrfPolicy::StrictExternal
    }
}

fn composio_user_id(bot: &BotConfig) -> String {
    format!("krill-bot-{}", bot.id.as_str())
}

fn read_api_key(
    bot: &BotConfig,
    secrets: &SecretVault,
) -> Result<zeroize::Zeroizing<String>, BotErrorDetail> {
    secrets
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
        })
}

fn load_persisted_session(
    state_root: Option<&Path>,
    bot_id: &BotId,
) -> Option<ComposioSessionRecord> {
    if let Some(root) = state_root {
        let path = root.join(SESSIONS_FILE);
        if let Ok(meta) = fs::symlink_metadata(&path)
            && !meta.file_type().is_symlink()
            && let Ok(raw) = fs::read_to_string(&path)
            && let Ok(map) = serde_json::from_str::<BTreeMap<BotId, ComposioSessionRecord>>(&raw)
            && let Some(rec) = map.get(bot_id).cloned()
        {
            return Some(rec);
        }
    }
    lock(memory_sessions()).get(bot_id).cloned()
}

fn save_persisted_session(state_root: Option<&Path>, bot_id: &BotId, rec: ComposioSessionRecord) {
    lock(memory_sessions()).insert(bot_id.clone(), rec.clone());
    let Some(root) = state_root else {
        return;
    };
    let path = root.join(SESSIONS_FILE);
    let mut map: BTreeMap<BotId, ComposioSessionRecord> = fs::symlink_metadata(&path)
        .ok()
        .filter(|m| !m.file_type().is_symlink())
        .and_then(|_| fs::read_to_string(&path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    map.insert(bot_id.clone(), rec);
    if let Ok(bytes) = serde_json::to_vec_pretty(&map) {
        krill_proto::os::write_atomic_0600(&path, &bytes).ok();
    }
}

/// Removes persisted Composio session metadata and unregisters the relay route for `bot_id`.
pub fn cleanup_composio_bot(bot_id: &BotId, state_root: Option<&Path>, relay: &BotToolRelay) {
    lock(memory_sessions()).remove(bot_id);
    if let Some(root) = state_root {
        let path = root.join(SESSIONS_FILE);
        if let Ok(meta) = fs::symlink_metadata(&path)
            && !meta.file_type().is_symlink()
            && let Ok(raw) = fs::read_to_string(&path)
            && let Ok(mut map) =
                serde_json::from_str::<BTreeMap<BotId, ComposioSessionRecord>>(&raw)
            && map.remove(bot_id).is_some()
            && let Ok(bytes) = serde_json::to_vec_pretty(&map)
        {
            krill_proto::os::write_atomic_0600(&path, &bytes).ok();
        }
    }
    let cred = make_cred_id(bot_id, "composio");
    relay.unregister_route(&cred);
}

/// Resolves (or reuses + `PATCH`es) the bot's Composio Tool Router session and registers it on [`BotToolRelay`].
pub fn resolve_and_register_composio(
    bot: &BotConfig,
    secrets: &SecretVault,
    relay: &BotToolRelay,
    base_url: &str,
) -> Result<ComposioMcpSession, BotErrorDetail> {
    resolve_and_register_composio_with_root(bot, secrets, relay, None, base_url, 0)
}

/// Resolves or creates a Composio v3.1 Tool Router session with optional persistent state storage.
pub fn resolve_and_register_composio_with_root(
    bot: &BotConfig,
    secrets: &SecretVault,
    relay: &BotToolRelay,
    state_root: Option<&Path>,
    base_url: &str,
    now_ms: u64,
) -> Result<ComposioMcpSession, BotErrorDetail> {
    let api_key = read_api_key(bot, secrets)?;
    let base = base_url.trim_end_matches('/');
    let policy = policy_for_base_url(base);
    let user_id = composio_user_id(bot);
    let enabled_apps = bot.connectors.composio.enabled_apps.clone();

    let existing = load_persisted_session(state_root, &bot.id);
    let session_rec = if let Some(prev) = existing {
        // Re-sync existing Tool Router session via PATCH /tool_router/session/{id}.
        match patch_tool_router_session(base, &api_key, &prev.session_id, &enabled_apps, policy) {
            Ok(updated_opt) => {
                let next_url = updated_opt.unwrap_or_else(|| prev.mcp_url.clone());
                ComposioSessionRecord {
                    session_id: prev.session_id,
                    user_id,
                    mcp_url: next_url,
                    enabled_apps: enabled_apps.clone(),
                    updated_at_ms: now_ms,
                }
            }
            Err(err) if err.message.contains("HTTP 404") => {
                create_tool_router_session(base, &api_key, &user_id, &enabled_apps, policy, now_ms)?
            }
            Err(err) => return Err(err),
        }
    } else {
        create_tool_router_session(base, &api_key, &user_id, &enabled_apps, policy, now_ms)?
    };

    save_persisted_session(state_root, &bot.id, session_rec.clone());
    let cred_id = make_cred_id(&bot.id, "composio");
    relay.register_route(ToolRoute {
        cred_id: cred_id.clone(),
        bot_id: bot.id.clone(),
        tool_id: "composio".to_owned(),
        name: "Composio MCP".to_owned(),
        upstream_url: session_rec.mcp_url.clone(),
        auth_kind: BotMcpAuthKind::BearerHeader,
        header_names: vec!["x-api-key".to_owned()],
        secret_ref: bot.connectors.composio.api_key_secret_ref.clone(),
        fallback_secret_key: "composio_api_key".to_owned(),
    });

    Ok(ComposioMcpSession {
        cred_id,
        session_id: session_rec.session_id,
        upstream_url: session_rec.mcp_url,
        enabled_apps,
    })
}

fn create_tool_router_session(
    base: &str,
    api_key: &str,
    user_id: &str,
    enabled_apps: &[String],
    policy: SsrfPolicy,
    now_ms: u64,
) -> Result<ComposioSessionRecord, BotErrorDetail> {
    let endpoint = format!("{base}/tool_router/session");
    let mut body = serde_json::json!({
        "user_id": user_id,
        "manage_connections": { "enable": false },
        "workbench": { "enable": false },
    });
    if !enabled_apps.is_empty()
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert(
            "toolkits".to_owned(),
            serde_json::json!({ "enable": enabled_apps }),
        );
    }
    let payload = serde_json::to_vec(&body)
        .map_err(|_| agent_err("failed to encode Composio session request"))?;
    let resp = http_client::send(
        &HttpRequest::post_json(&endpoint, payload)
            .with_header("x-api-key", api_key.trim())
            .with_header("Accept", "application/json")
            .with_ssrf_policy(policy),
    )
    .map_err(|err| agent_err(format!("Composio API network error: {}", err.kind())))?;

    check_composio_status(&resp)?;
    let session_id = extract_session_id(&resp.body)
        .ok_or_else(|| agent_err("Composio /tool_router/session response missing session_id"))?;
    let mcp_url = extract_mcp_url(&resp.body, base, &session_id);
    Ok(ComposioSessionRecord {
        session_id,
        user_id: user_id.to_owned(),
        mcp_url,
        enabled_apps: enabled_apps.to_vec(),
        updated_at_ms: now_ms,
    })
}

fn patch_tool_router_session(
    base: &str,
    api_key: &str,
    session_id: &str,
    enabled_apps: &[String],
    policy: SsrfPolicy,
) -> Result<Option<String>, BotErrorDetail> {
    let endpoint = format!(
        "{base}/tool_router/session/{}",
        http_client::percent_encode(session_id)
    );
    let body = serde_json::json!({
        "manage_connections": { "enable": false },
        "workbench": { "enable": false },
        "toolkits": { "enable": enabled_apps },
    });
    let payload =
        serde_json::to_vec(&body).map_err(|_| agent_err("failed to encode Composio patch"))?;
    let resp = http_client::send(
        &HttpRequest::new(HttpMethod::Patch, &endpoint)
            .with_header("Content-Type", "application/json")
            .with_header("x-api-key", api_key.trim())
            .with_header("Accept", "application/json")
            .with_body(payload)
            .with_ssrf_policy(policy),
    )
    .map_err(|err| agent_err(format!("Composio PATCH network error: {}", err.kind())))?;

    check_composio_status(&resp)?;
    Ok(extract_explicit_mcp_url(&resp.body))
}

/// Syncs Composio for `bot` and returns a [`BotConnectorStatus`] snapshot.
pub fn sync_composio_bot(
    bot: &BotConfig,
    secrets: &SecretVault,
    relay: &BotToolRelay,
    base_url: &str,
    now_ms: u64,
) -> BotConnectorStatus {
    sync_composio_bot_with_root(bot, secrets, relay, None, base_url, now_ms)
}

/// Syncs Composio for `bot` using persistent session storage under `state_root`.
pub fn sync_composio_bot_with_root(
    bot: &BotConfig,
    secrets: &SecretVault,
    relay: &BotToolRelay,
    state_root: Option<&Path>,
    base_url: &str,
    now_ms: u64,
) -> BotConnectorStatus {
    match resolve_and_register_composio_with_root(bot, secrets, relay, state_root, base_url, now_ms)
    {
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

/// Lists Composio connected accounts for `bot` via `GET /connected_accounts?user_ids=...`.
pub fn list_connected_accounts(
    bot: &BotConfig,
    secrets: &SecretVault,
    base_url: &str,
) -> Result<Vec<ComposioConnectionInfo>, BotErrorDetail> {
    let api_key = read_api_key(bot, secrets)?;
    let base = base_url.trim_end_matches('/');
    let policy = policy_for_base_url(base);
    let user_id = composio_user_id(bot);
    let endpoint = format!(
        "{base}/connected_accounts?user_ids={}",
        http_client::percent_encode(&user_id)
    );
    let resp = http_client::send(
        &HttpRequest::get(&endpoint)
            .with_header("x-api-key", api_key.trim())
            .with_header("Accept", "application/json")
            .with_ssrf_policy(policy),
    )
    .map_err(|err| {
        agent_err(format!(
            "Composio list connections network error: {}",
            err.kind()
        ))
    })?;
    check_composio_status(&resp)?;

    let val: serde_json::Value = resp
        .json()
        .map_err(|err| agent_err(format!("invalid Composio connected_accounts JSON: {err}")))?;
    let items = extract_array(&val);
    let mut out = Vec::new();
    for entry in items {
        let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let app_id = entry
            .get("toolkit")
            .and_then(|t| t.get("slug").or_else(|| t.get("name")))
            .or_else(|| entry.get("app_id"))
            .or_else(|| entry.get("appName"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let status = entry
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("ACTIVE")
            .to_owned();
        let account_label = entry
            .get("account_label")
            .or_else(|| entry.get("member").and_then(|m| m.get("email")))
            .or_else(|| {
                entry
                    .get("state")
                    .and_then(|s| s.get("val"))
                    .and_then(|v| v.get("email"))
            })
            .or_else(|| entry.get("toolkit").and_then(|t| t.get("name")))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        let created_at_ms = entry
            .get("created_at_ms")
            .and_then(serde_json::Value::as_u64);
        out.push(ComposioConnectionInfo {
            id: id.to_owned(),
            app_id,
            status,
            account_label,
            created_at_ms,
        });
    }
    Ok(out)
}

/// Starts a Composio v3.1 connect link flow via `POST /tool_router/session/{id}/link`.
pub fn start_composio_connect(
    bot: &BotConfig,
    secrets: &SecretVault,
    relay: &BotToolRelay,
    state_root: Option<&Path>,
    base_url: &str,
    app_id: &str,
    now_ms: u64,
) -> Result<(String, String), BotErrorDetail> {
    let app_trimmed = app_id.trim();
    if app_trimmed.is_empty() {
        return Err(agent_err("Composio app_id cannot be empty"));
    }
    let session =
        resolve_and_register_composio_with_root(bot, secrets, relay, state_root, base_url, now_ms)?;
    let api_key = read_api_key(bot, secrets)?;
    let base = base_url.trim_end_matches('/');
    let policy = policy_for_base_url(base);
    let endpoint = format!(
        "{base}/tool_router/session/{}/link",
        http_client::percent_encode(&session.session_id)
    );
    let payload = serde_json::to_vec(&serde_json::json!({
        "toolkit": app_trimmed,
    }))
    .map_err(|_| agent_err("failed encoding Composio link request"))?;

    let resp = http_client::send(
        &HttpRequest::post_json(&endpoint, payload)
            .with_header("x-api-key", api_key.trim())
            .with_header("Accept", "application/json")
            .with_ssrf_policy(policy),
    )
    .map_err(|err| {
        agent_err(format!(
            "Composio link request network error: {}",
            err.kind()
        ))
    })?;
    check_composio_status(&resp)?;

    let val: serde_json::Value = resp
        .json()
        .map_err(|err| agent_err(format!("invalid Composio link JSON: {err}")))?;
    let redirect_url = [
        val.get("redirect_url"),
        val.get("redirectUrl"),
        val.get("link_url"),
        val.get("url"),
        val.get("data").and_then(|d| d.get("redirect_url")),
    ]
    .into_iter()
    .flatten()
    .find_map(serde_json::Value::as_str)
    .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
    .ok_or_else(|| agent_err("Composio /link response missing redirect_url"))?
    .to_owned();
    let connection_id = [
        val.get("connected_account_id"),
        val.get("connection_id"),
        val.get("id"),
        val.get("data").and_then(|d| d.get("id")),
    ]
    .into_iter()
    .flatten()
    .find_map(serde_json::Value::as_str)
    .map_or_else(|| format!("link-{}", session.session_id), ToOwned::to_owned);

    Ok((connection_id, redirect_url))
}

/// Disconnects a Composio connected account via `DELETE /connected_accounts/{id}`.
pub fn disconnect_composio_account(
    bot: &BotConfig,
    secrets: &SecretVault,
    base_url: &str,
    connection_id: &str,
) -> Result<(), BotErrorDetail> {
    let cid = connection_id.trim();
    if cid.is_empty() || cid.contains(['/', '?', '#', ' ']) {
        return Err(agent_err("invalid Composio connection_id"));
    }
    let api_key = read_api_key(bot, secrets)?;
    let base = base_url.trim_end_matches('/');
    let policy = policy_for_base_url(base);
    let endpoint = format!(
        "{base}/connected_accounts/{}",
        http_client::percent_encode(cid)
    );
    let resp = http_client::send(
        &HttpRequest::new(HttpMethod::Delete, &endpoint)
            .with_header("x-api-key", api_key.trim())
            .with_header("Accept", "application/json")
            .with_ssrf_policy(policy),
    )
    .map_err(|err| agent_err(format!("Composio disconnect network error: {}", err.kind())))?;
    check_composio_status(&resp)?;
    Ok(())
}

/// Polls `GET /trigger_instances/active` and creates deduplicated [`BotItem`]s for new triggers.
pub fn poll_composio_triggers(
    bot: &BotConfig,
    secrets: &SecretVault,
    items: &BotItemsStore,
    base_url: &str,
    now_ms: u64,
) -> Result<Vec<BotItem>, BotErrorDetail> {
    let api_key = read_api_key(bot, secrets)?;
    let base = base_url.trim_end_matches('/');
    let policy = policy_for_base_url(base);
    let user_id = composio_user_id(bot);
    let endpoint = format!(
        "{base}/trigger_instances/active?user_ids={}",
        http_client::percent_encode(&user_id)
    );
    let resp = http_client::send(
        &HttpRequest::get(&endpoint)
            .with_header("x-api-key", api_key.trim())
            .with_header("Accept", "application/json")
            .with_ssrf_policy(policy),
    )
    .map_err(|err| agent_err(format!("Composio triggers poll error: {}", err.kind())))?;
    check_composio_status(&resp)?;

    let val: serde_json::Value = resp
        .json()
        .map_err(|err| agent_err(format!("invalid Composio triggers JSON: {err}")))?;
    let mut created = Vec::new();
    for entry in extract_array(&val) {
        let Some(trig_id) = entry
            .get("id")
            .or_else(|| entry.get("nano_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let key = (bot.id.clone(), trig_id.to_owned());
        if lock(seen_triggers()).contains(&key) {
            continue;
        }
        let link = BotItemLink::Url(format!("composio:trigger:{trig_id}"));
        if let Ok(Some(_)) = items.find_item_by_link(&bot.id, &link) {
            lock(seen_triggers()).insert(key);
            continue;
        }
        let trigger_name = entry
            .get("trigger_name")
            .or_else(|| entry.get("triggerName"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Trigger event");
        let app_name = entry
            .get("toolkit")
            .and_then(|t| t.get("slug").or_else(|| t.get("name")))
            .or_else(|| entry.get("appName"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("composio");
        let payload_summary = entry
            .get("payload")
            .or_else(|| entry.get("data"))
            .map(std::string::ToString::to_string);

        if let Ok(item) = items.create_item(
            &CreateBotItemParams {
                bot_id: bot.id.clone(),
                title: format!("[Composio/{app_name}] {trigger_name}"),
                description: payload_summary,
                done_means: None,
                status: Some(BotItemStatus::Planned),
                links: vec![link],
            },
            &BotItemActor::System {
                reason: "composio_trigger".to_owned(),
            },
            now_ms,
        ) {
            lock(seen_triggers()).insert(key);
            created.push(item);
        }
    }
    Ok(created)
}

/// Tests Composio API key & `/toolkits` reachability for `TestConnector`.
#[must_use]
pub fn test_composio(
    bot: &BotConfig,
    secrets: &SecretVault,
    base_url: &str,
) -> BotConnectorTestResult {
    let api_key = match read_api_key(bot, secrets) {
        Ok(k) => k,
        Err(detail) => {
            return BotConnectorTestResult {
                source: ExternalSource::Composio,
                ok: false,
                account_label: None,
                detail: detail.message,
            };
        }
    };
    let base = base_url.trim_end_matches('/');
    let policy = policy_for_base_url(base);
    let endpoint = format!("{base}/toolkits");
    let resp = match http_client::send(
        &HttpRequest::get(&endpoint)
            .with_header("x-api-key", api_key.trim())
            .with_header("Accept", "application/json")
            .with_ssrf_policy(policy),
    ) {
        Ok(r) => r,
        Err(err) => {
            return BotConnectorTestResult {
                source: ExternalSource::Composio,
                ok: false,
                account_label: None,
                detail: format!("Composio network error: {}", err.kind()),
            };
        }
    };
    if !resp.is_success() {
        return BotConnectorTestResult {
            source: ExternalSource::Composio,
            ok: false,
            account_label: None,
            detail: format!("Composio API returned HTTP {}", resp.status),
        };
    }
    let toolkit_count = resp
        .json::<serde_json::Value>()
        .ok()
        .map_or(0, |v| extract_array(&v).len());
    BotConnectorTestResult {
        source: ExternalSource::Composio,
        ok: true,
        account_label: Some(format!(
            "{} app(s) enabled",
            bot.connectors.composio.enabled_apps.len()
        )),
        detail: format!("Composio v3.1 connected ({toolkit_count} toolkit(s) visible)"),
    }
}

fn check_composio_status(resp: &http_client::HttpResponse) -> Result<(), BotErrorDetail> {
    if resp.status == 401 || resp.status == 403 {
        return Err(BotErrorDetail {
            category: BotErrorCategory::SignedOut,
            agent_name: None,
            message: format!("Composio API key rejected (HTTP {})", resp.status),
            resets_at_ms: None,
        });
    }
    if !resp.is_success() {
        return Err(agent_err(format!(
            "Composio API returned HTTP {}",
            resp.status
        )));
    }
    Ok(())
}

fn agent_err(message: impl Into<String>) -> BotErrorDetail {
    BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: message.into(),
        resets_at_ms: None,
    }
}

fn extract_array(val: &serde_json::Value) -> Vec<serde_json::Value> {
    if let Some(arr) = val.as_array() {
        return arr.clone();
    }
    for key in ["items", "data", "toolkits"] {
        if let Some(arr) = val.get(key).and_then(serde_json::Value::as_array) {
            return arr.clone();
        }
    }
    Vec::new()
}

fn extract_session_id(body: &[u8]) -> Option<String> {
    let val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    for cand in [
        val.get("session_id"),
        val.get("id"),
        val.get("data").and_then(|d| d.get("session_id")),
        val.get("data").and_then(|d| d.get("id")),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(s) = cand.as_str()
            && !s.trim().is_empty()
        {
            return Some(s.to_owned());
        }
    }
    None
}

fn extract_explicit_mcp_url(body: &[u8]) -> Option<String> {
    let val = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let candidates = [
        val.get("mcp").and_then(|m| m.get("url")),
        val.get("mcp_url"),
        val.get("endpoint_url"),
        val.get("url"),
        val.get("data")
            .and_then(|d| d.get("mcp"))
            .and_then(|m| m.get("url")),
        val.get("data").and_then(|d| d.get("mcp_url")),
        val.get("data").and_then(|d| d.get("endpoint_url")),
        val.get("data").and_then(|d| d.get("url")),
    ];
    for cand in candidates.into_iter().flatten() {
        if let Some(u) = cand.as_str()
            && (u.starts_with("http://") || u.starts_with("https://"))
        {
            return Some(u.to_owned());
        }
    }
    None
}

fn extract_mcp_url(body: &[u8], base_url: &str, session_id: &str) -> String {
    extract_explicit_mcp_url(body)
        .unwrap_or_else(|| format!("{base_url}/tool_router/session/{session_id}/mcp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::BotId;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::thread;
    use zeroize::Zeroizing;

    fn temp_dir(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-composio-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn composio_v31_session_reuse_patch_link_accounts_triggers_and_relay() {
        let dir = temp_dir("v31");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x99; 32]));
        let relay = BotToolRelay::new(Arc::clone(&secrets));
        relay.set_upstream_ssrf_policy(SsrfPolicy::AllowLoopback);
        let items = BotItemsStore::new(dir.join("items.db"));

        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        mock.set_nonblocking(true).unwrap();
        let addr = mock.local_addr().unwrap();
        let saw_methods = Arc::new(Mutex::new(Vec::<String>::new()));
        let methods_bg = Arc::clone(&saw_methods);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);

        let server = thread::spawn(move || {
            while !stop_bg.load(std::sync::atomic::Ordering::Acquire) {
                let (mut sock, _) = match mock.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                sock.set_nonblocking(false).ok();
                let mut buf = [0u8; 8192];
                let n = sock.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    continue;
                }
                let req = String::from_utf8_lossy(buf.get(..n).unwrap_or_default()).into_owned();
                let req_line = req.lines().next().unwrap_or_default().to_owned();
                assert!(req.contains("x-api-key: cmp_live_secret"));
                lock(&methods_bg).push(req_line.clone());

                let target = req_line.split_whitespace().nth(1).unwrap_or("/");
                let body = if req_line.starts_with("POST /api/v3.1/tool_router/session ") {
                    assert!(req.contains("\"manage_connections\":{\"enable\":false}"));
                    assert!(req.contains("\"workbench\":{\"enable\":false}"));
                    format!(
                        r#"{{"session_id":"tr-sess-1","mcp":{{"url":"http://{addr}/mcp/tr-sess-1"}}}}"#
                    )
                } else if req_line.starts_with("PATCH /api/v3.1/tool_router/session/tr-sess-1") {
                    format!(
                        r#"{{"session_id":"tr-sess-1","mcp":{{"url":"http://{addr}/mcp/tr-sess-1"}}}}"#
                    )
                } else if target.contains("/tool_router/session/tr-sess-1/link") {
                    r#"{"connected_account_id":"ca-new-1","redirect_url":"https://connect.composio.dev/link/abc"}"#.to_owned()
                } else if target.starts_with("/api/v3.1/connected_accounts?") {
                    r#"{"items":[{"id":"ca-1","toolkit":{"slug":"notion","name":"Notion"},"status":"ACTIVE","member":{"email":"bot@krill.local"}}]}"#.to_owned()
                } else if req_line.starts_with("DELETE /api/v3.1/connected_accounts/ca-1") {
                    r#"{"deleted":true}"#.to_owned()
                } else if target == "/api/v3.1/toolkits" {
                    r#"{"items":[{"slug":"notion"},{"slug":"github"}]}"#.to_owned()
                } else if target.starts_with("/api/v3.1/trigger_instances/active") {
                    r#"{"items":[{"id":"trig-42","trigger_name":"GITHUB_ISSUE_CREATED","toolkit":{"slug":"github"},"payload":{"issue":42}}]}"#.to_owned()
                } else {
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

        let bot_id = BotId::new("bot-cmp-v31").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "Composio Bot", "claude", 1_000).unwrap();
        bot.connectors.composio.enabled = true;
        bot.connectors.composio.enabled_apps = vec!["notion".to_owned(), "github".to_owned()];

        secrets
            .set_secret(
                &bot_id,
                "composio_api_key",
                Zeroizing::new("cmp_live_secret".to_owned()),
            )
            .unwrap();

        let base_url = format!("http://{addr}/api/v3.1");
        // 1. Initial sync -> POST /tool_router/session
        let status =
            sync_composio_bot_with_root(&bot, &secrets, &relay, Some(&dir), &base_url, 2_000);
        assert!(status.connected);

        // 2. Second sync reuses session -> PATCH /tool_router/session/tr-sess-1
        let status2 =
            sync_composio_bot_with_root(&bot, &secrets, &relay, Some(&dir), &base_url, 3_000);
        assert!(status2.connected);

        // 3. Connect link flow -> POST /tool_router/session/tr-sess-1/link
        let (cid, redir) = start_composio_connect(
            &bot,
            &secrets,
            &relay,
            Some(&dir),
            &base_url,
            "notion",
            4_000,
        )
        .unwrap();
        assert_eq!(cid, "ca-new-1");
        assert!(redir.contains("connect.composio.dev"));

        // 4. Connected accounts list + disconnect
        let conns = list_connected_accounts(&bot, &secrets, &base_url).unwrap();
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].app_id, "notion");
        assert_eq!(conns[0].account_label.as_deref(), Some("bot@krill.local"));
        disconnect_composio_account(&bot, &secrets, &base_url, "ca-1").unwrap();

        // 5. Toolkits test + trigger ingestion into BotItems (deduplicated)
        let test_res = test_composio(&bot, &secrets, &base_url);
        assert!(test_res.ok);
        let created = poll_composio_triggers(&bot, &secrets, &items, &base_url, 5_000).unwrap();
        assert_eq!(created.len(), 1);
        assert!(created[0].title.contains("GITHUB_ISSUE_CREATED"));

        // 6. Verify relay forwards MCP with injected x-api-key (last call)
        let grant = relay.prepare_launch_for_bot(&bot).unwrap().unwrap();
        let cred_id = grant.cred_ids.first().unwrap();
        let relay_resp = http_client::send(
            &HttpRequest::post_json(
                format!("{}/bot-tools/{cred_id}/mcp", grant.base_url),
                b"{}".to_vec(),
            )
            .with_header("Authorization", format!("Bearer {}", *grant.token))
            .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(relay_resp.status, 200);

        stop.store(true, std::sync::atomic::Ordering::Release);
        server.join().unwrap();
        let recorded = lock(&saw_methods).clone();
        assert!(
            recorded
                .iter()
                .any(|l| l.starts_with("POST /api/v3.1/tool_router/session "))
        );
        assert!(
            recorded
                .iter()
                .any(|l| l.starts_with("PATCH /api/v3.1/tool_router/session/tr-sess-1"))
        );

        cleanup_composio_bot(&bot_id, Some(&dir), &relay);
        relay.unregister_bot(&bot_id);
        fs::remove_dir_all(&dir).ok();
    }
}
