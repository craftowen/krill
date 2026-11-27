//! Loopback PKCE (S256) `BrowserOAuth` manager for remote Krill Bot MCP tools.
//!
//! Implements:
//! - RFC 9728 Protected Resource Metadata discovery (`/.well-known/oauth-protected-resource` and
//!   `WWW-Authenticate: Bearer resource_metadata=...` challenge probing)
//! - RFC 8414 Authorization Server Metadata discovery (`/.well-known/oauth-authorization-server`
//!   and `OpenID` Connect fallback)
//! - RFC 7591 Dynamic Client Registration (ephemeral `http://127.0.0.1:<port>/callback`)
//! - PKCE S256 loopback listener (accept loop ignoring `/favicon.ico` and reading full `\r\n\r\n`)
//! - Persisted access + refresh tokens in [`SecretVault`] with per-(bot, tool) refresh mutexes.

use std::collections::BTreeMap;
use std::io::{self, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::bots::{BotConfig, BotId, BotMcpAuthKind, BotMcpOAuthState, BotMcpToolConfig};
use ring::digest;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::bots::http_client::{self, HttpRequest, SsrfPolicy};
use crate::bots::secrets::SecretVault;
use crate::bots::tool_relay::constant_time_eq;
use crate::session::lock;

const STACK_SIZE: usize = 128 * 1024;
const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
const OAUTH_ACCEPT_POLL_SLEEP: Duration = Duration::from_millis(25);
const MAX_CALLBACK_HEAD_BYTES: usize = 16 * 1024;
const TOKEN_REFRESH_SKEW_MS: u64 = 60_000;
const TERMINAL_FLOW_TTL_MS: u64 = 300_000;

/// Persisted MCP `BrowserOAuth` token bundle stored encrypted/protected in [`SecretVault`].
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredMcpOAuthCredentials {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_endpoint: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
}

impl std::fmt::Debug for StoredMcpOAuthCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredMcpOAuthCredentials")
            .field("has_refresh_token", &self.refresh_token.is_some())
            .field("token_endpoint", &self.token_endpoint)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
struct DiscoveredOAuthMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    scopes: Vec<String>,
    resource: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AuthServerMetadata {
    #[serde(default)]
    authorization_endpoint: Option<String>,
    #[serde(default)]
    token_endpoint: Option<String>,
    #[serde(default)]
    registration_endpoint: Option<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DynamicClientRegistrationResp {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

struct ActiveOAuthFlow {
    state: Mutex<BotMcpOAuthState>,
    cancel: Arc<AtomicBool>,
    active_socket: http_client::RequestCancelSlot,
}

impl ActiveOAuthFlow {
    fn trigger_cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        http_client::cancel_active_request(&self.active_socket);
    }
}

type ActiveOAuthFlowMap = Arc<Mutex<BTreeMap<(BotId, String), Arc<ActiveOAuthFlow>>>>;

/// Manages active `BrowserOAuth` sign-in flows across bots and MCP tools.
pub struct McpOAuthManager {
    secrets: Arc<SecretVault>,
    flows: ActiveOAuthFlowMap,
    ssrf_policy: Mutex<SsrfPolicy>,
}

impl Drop for McpOAuthManager {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

impl std::fmt::Debug for McpOAuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.flows.lock().map_or(0, |m| m.len());
        f.debug_struct("McpOAuthManager")
            .field("active_flows", &count)
            .finish_non_exhaustive()
    }
}

impl McpOAuthManager {
    #[must_use]
    pub fn new(secrets: Arc<SecretVault>) -> Self {
        Self {
            secrets,
            flows: Arc::new(Mutex::new(BTreeMap::new())),
            ssrf_policy: Mutex::new(SsrfPolicy::StrictExternal),
        }
    }

    /// Configures outbound SSRF policy (for local loopback mock tests).
    pub fn set_ssrf_policy(&self, policy: SsrfPolicy) {
        *lock(&self.ssrf_policy) = policy;
    }

    /// Starts PKCE S256 `BrowserOAuth` discovery + loopback listener for `tool_id` on `bot`.
    pub fn start_oauth(&self, bot: &BotConfig, tool_id: &str) -> Result<BotMcpOAuthState, String> {
        let tool = find_oauth_tool(bot, tool_id)?;
        let endpoint_url = tool
            .endpoint_url
            .as_deref()
            .map(str::trim)
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
            .ok_or_else(|| {
                format!(
                    "MCP tool '{}' has no valid HTTP/HTTPS endpoint_url",
                    tool.id
                )
            })?
            .to_owned();

        let policy = *lock(&self.ssrf_policy);
        validate_oauth_endpoint_url(&endpoint_url, policy, "endpoint_url")?;
        let metadata = discover_oauth_metadata(&endpoint_url, policy)?;

        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|err| format!("failed to bind MCP OAuth callback listener: {err}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|err| format!("failed configuring MCP OAuth listener: {err}"))?;
        let local_port = listener
            .local_addr()
            .map_err(|err| format!("failed reading OAuth listener address: {err}"))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{local_port}/callback");

        let (client_id, client_secret) = if let Some(ref reg_url) = metadata.registration_endpoint {
            register_dynamic_client(reg_url, &redirect_uri, policy)?
        } else {
            ("krill-mcp-client".to_owned(), None)
        };

        let code_verifier = generate_random_b64url(32)?;
        let code_challenge = compute_pkce_s256(&code_verifier);
        let oauth_state = generate_random_b64url(24)?;

        let auth_url = build_authorize_url(
            &metadata.authorization_endpoint,
            &client_id,
            &redirect_uri,
            &code_challenge,
            &oauth_state,
            &metadata.scopes,
            metadata.resource.as_deref(),
            policy,
        )?;

        // Cancel any earlier flow for the same `(bot_id, tool_id)` key.
        let key = (bot.id.clone(), tool.id.clone());
        if let Some(prev) = lock(&self.flows).remove(&key) {
            prev.trigger_cancel();
        }

        let initial_state = BotMcpOAuthState {
            bot_id: bot.id.clone(),
            tool_id: tool.id.clone(),
            status: "pending".to_owned(),
            auth_url: Some(auth_url),
            error: None,
            updated_at_ms: now_ms(),
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let active_socket: http_client::RequestCancelSlot = Arc::new(Mutex::new(None));
        let flow = Arc::new(ActiveOAuthFlow {
            state: Mutex::new(initial_state.clone()),
            cancel: Arc::clone(&cancel),
            active_socket: Arc::clone(&active_socket),
        });
        lock(&self.flows).insert(key.clone(), Arc::clone(&flow));

        let flow_bg = Arc::clone(&flow);
        let cancel_bg = Arc::clone(&cancel);
        let socket_bg = Arc::clone(&active_socket);
        let secrets_bg = Arc::clone(&self.secrets);
        let bot_id_bg = bot.id.clone();
        let tool_bg = tool.clone();
        let resource_bg = metadata.resource.clone();

        let thread_token = sanitize_thread_name_token(&tool.id);
        if let Err(err) = thread::Builder::new()
            .name(format!("mcp-oauth-{thread_token}"))
            .stack_size(STACK_SIZE)
            .spawn(move || {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_pkce_callback_listener(
                        &listener,
                        &cancel_bg,
                        &socket_bg,
                        &bot_id_bg,
                        &tool_bg,
                        &redirect_uri,
                        &client_id,
                        client_secret.as_deref(),
                        &code_verifier,
                        &oauth_state,
                        &metadata.token_endpoint,
                        resource_bg.as_deref(),
                        &secrets_bg,
                        policy,
                    )
                }));
                let mut st = lock(&flow_bg.state);
                if st.status == "cancelled" || cancel_bg.load(Ordering::Acquire) {
                    "cancelled".clone_into(&mut st.status);
                    st.auth_url = None;
                    st.error = Some("OAuth sign-in cancelled".to_owned());
                    st.updated_at_ms = now_ms();
                    return;
                }
                match res {
                    Ok(Ok(())) => {
                        "authorized".clone_into(&mut st.status);
                        st.auth_url = None;
                        st.error = None;
                        st.updated_at_ms = now_ms();
                    }
                    Ok(Err(msg)) => {
                        "error".clone_into(&mut st.status);
                        st.auth_url = None;
                        st.error = Some(msg);
                        st.updated_at_ms = now_ms();
                    }
                    Err(_) => {
                        "error".clone_into(&mut st.status);
                        st.auth_url = None;
                        st.error = Some("MCP OAuth listener panicked".to_owned());
                        st.updated_at_ms = now_ms();
                    }
                }
            })
        {
            let mut flows = lock(&self.flows);
            if flows.get(&key).is_some_and(|cur| Arc::ptr_eq(cur, &flow)) {
                flows.remove(&key);
            }
            return Err(format!("failed to spawn MCP OAuth thread: {err}"));
        }

        Ok(initial_state)
    }

    /// Returns the live or persisted OAuth sign-in status for `(bot_id, tool_id)`.
    #[must_use]
    pub fn status(&self, bot: &BotConfig, tool_id: &str) -> BotMcpOAuthState {
        let key = (bot.id.clone(), tool_id.to_owned());
        let secret_ref = bot
            .mcp_tools
            .iter()
            .find(|t| t.id == tool_id)
            .and_then(|t| t.secret_ref.as_deref());
        let flow_opt = lock(&self.flows).get(&key).cloned();
        if let Some(flow) = flow_opt {
            let st = lock(&flow.state).clone();
            if st.status == "pending" {
                return st;
            }
            let has_token =
                has_stored_oauth_credentials(&self.secrets, &bot.id, tool_id, secret_ref);
            let stale_error_overridden = st.status != "authorized" && has_token;
            let stale_auth_deleted = st.status == "authorized" && !has_token;
            let expired = now_ms().saturating_sub(st.updated_at_ms) > TERMINAL_FLOW_TTL_MS;
            if stale_error_overridden || stale_auth_deleted || expired {
                let mut flows = lock(&self.flows);
                if flows.get(&key).is_some_and(|cur| Arc::ptr_eq(cur, &flow)) {
                    flows.remove(&key);
                }
            } else {
                return st;
            }
        }
        let has_token = has_stored_oauth_credentials(&self.secrets, &bot.id, tool_id, secret_ref);
        BotMcpOAuthState {
            bot_id: bot.id.clone(),
            tool_id: tool_id.to_owned(),
            status: if has_token {
                "authorized".to_owned()
            } else {
                "idle".to_owned()
            },
            auth_url: None,
            error: None,
            updated_at_ms: now_ms(),
        }
    }

    /// Cancels an active `BrowserOAuth` sign-in flow if running.
    #[must_use]
    pub fn cancel(&self, bot_id: &BotId, tool_id: &str) -> BotMcpOAuthState {
        let key = (bot_id.clone(), tool_id.to_owned());
        let flow_opt = lock(&self.flows).get(&key).cloned();
        if let Some(flow) = flow_opt {
            let mut st = lock(&flow.state);
            if st.status != "pending" {
                return st.clone();
            }
            flow.trigger_cancel();
            "cancelled".clone_into(&mut st.status);
            st.auth_url = None;
            st.error = Some("OAuth sign-in cancelled".to_owned());
            st.updated_at_ms = now_ms();
            return st.clone();
        }
        BotMcpOAuthState {
            bot_id: bot_id.clone(),
            tool_id: tool_id.to_owned(),
            status: "cancelled".to_owned(),
            auth_url: None,
            error: None,
            updated_at_ms: now_ms(),
        }
    }

    /// Cancels and removes all active `BrowserOAuth` sign-in flows belonging to `bot_id`.
    pub fn cancel_bot(&self, bot_id: &BotId) {
        let drained: Vec<Arc<ActiveOAuthFlow>> = {
            let mut flows = lock(&self.flows);
            let keys: Vec<(BotId, String)> = flows
                .keys()
                .filter(|(bid, _)| bid == bot_id)
                .cloned()
                .collect();
            keys.into_iter().filter_map(|k| flows.remove(&k)).collect()
        };
        for flow in drained {
            flow.trigger_cancel();
            let mut st = lock(&flow.state);
            if st.status == "pending" {
                "cancelled".clone_into(&mut st.status);
                st.auth_url = None;
                st.error = Some("OAuth sign-in cancelled".to_owned());
                st.updated_at_ms = now_ms();
            }
        }
    }

    /// Cancels and removes all active `BrowserOAuth` sign-in flows (used on daemon shutdown/drop).
    pub fn cancel_all(&self) {
        let drained: Vec<Arc<ActiveOAuthFlow>> = {
            let mut flows = lock(&self.flows);
            std::mem::take(&mut *flows).into_values().collect()
        };
        for flow in drained {
            flow.trigger_cancel();
            let mut st = lock(&flow.state);
            if st.status == "pending" {
                "cancelled".clone_into(&mut st.status);
                st.auth_url = None;
                st.error = Some("OAuth sign-in cancelled".to_owned());
                st.updated_at_ms = now_ms();
            }
        }
    }
}

/// Secret vault key holding full serialized [`StoredMcpOAuthCredentials`] metadata.
#[must_use]
pub fn oauth_bundle_secret_key(tool_id: &str) -> String {
    format!("mcp_oauth:{tool_id}")
}

/// Checks whether either the OAuth credential bundle or direct access token is present in [`SecretVault`].
#[must_use]
pub fn has_stored_oauth_credentials(
    secrets: &SecretVault,
    bot_id: &BotId,
    tool_id: &str,
    secret_ref: Option<&str>,
) -> bool {
    let bundle_key = oauth_bundle_secret_key(tool_id);
    if secrets
        .get_secret(bot_id, &bundle_key)
        .ok()
        .flatten()
        .is_some()
    {
        return true;
    }
    secrets
        .resolve_secret(bot_id, secret_ref, &format!("mcp:{tool_id}"))
        .ok()
        .flatten()
        .is_some()
}

/// Resolves an active Bearer access token for a `BrowserOAuth` MCP tool, refreshing via
/// `refresh_token` if expired or nearing expiry (serialized per `(bot_id, tool_id)`).
pub fn resolve_or_refresh_token(
    secrets: &SecretVault,
    bot_id: &BotId,
    tool_id: &str,
    secret_ref: Option<&str>,
    fallback_secret_key: &str,
    policy: SsrfPolicy,
) -> io::Result<Option<Zeroizing<String>>> {
    let mutex = refresh_mutex_for(bot_id, tool_id);
    let _guard = lock(&mutex);

    let bundle_key = oauth_bundle_secret_key(tool_id);
    if let Some(raw_bundle) = secrets.get_secret(bot_id, &bundle_key)?
        && let Ok(mut creds) = serde_json::from_str::<StoredMcpOAuthCredentials>(&raw_bundle)
    {
        let now = now_ms();
        let expired = creds.expires_at_ms.is_some_and(|exp| exp <= now);
        let needs_refresh = creds
            .expires_at_ms
            .is_some_and(|exp| exp <= now.saturating_add(TOKEN_REFRESH_SKEW_MS));
        if needs_refresh {
            if let (Some(refresh_tok), Some(token_url)) = (
                creds.refresh_token.as_deref().filter(|s| !s.is_empty()),
                creds.token_endpoint.as_deref().filter(|s| !s.is_empty()),
            ) {
                match exchange_refresh_token(
                    token_url,
                    refresh_tok,
                    creds.client_id.as_deref(),
                    creds.client_secret.as_deref(),
                    creds.resource.as_deref(),
                    policy,
                ) {
                    Ok(refreshed) => {
                        creds.access_token.clone_from(&refreshed.access_token);
                        let rotated_refresh = refreshed.refresh_token.is_some();
                        if let Some(next_rt) = refreshed.refresh_token {
                            creds.refresh_token = Some(next_rt);
                        }
                        creds.expires_at_ms = refreshed
                            .expires_in
                            .map(|exp_s| now.saturating_add(exp_s.saturating_mul(1_000)));
                        if let Some(sc) = refreshed.scope {
                            creds.scope = Some(sc);
                        }
                        if let Err(err) =
                            persist_oauth_credentials(secrets, bot_id, tool_id, secret_ref, &creds)
                        {
                            tracing::warn!(
                                bot_id = %bot_id.as_str(),
                                tool_id,
                                rotated_refresh,
                                kind = ?err.kind(),
                                "failed to persist refreshed MCP OAuth credentials"
                            );
                            if rotated_refresh {
                                return Err(io::Error::new(
                                    err.kind(),
                                    format!(
                                        "failed to persist rotated MCP OAuth refresh token: {err}"
                                    ),
                                ));
                            }
                        }
                        return Ok(Some(Zeroizing::new(creds.access_token)));
                    }
                    Err(err) => {
                        if expired {
                            return Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                format!("MCP OAuth access token expired and refresh failed: {err}"),
                            ));
                        }
                    }
                }
            } else if expired {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "MCP OAuth access token expired and no refresh token is available",
                ));
            }
        }
        return Ok(Some(Zeroizing::new(creds.access_token)));
    }

    secrets.resolve_secret(bot_id, secret_ref, fallback_secret_key)
}

fn persist_oauth_credentials(
    secrets: &SecretVault,
    bot_id: &BotId,
    tool_id: &str,
    secret_ref: Option<&str>,
    creds: &StoredMcpOAuthCredentials,
) -> io::Result<()> {
    let json = serde_json::to_string(creds)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    secrets.set_secret(
        bot_id,
        &oauth_bundle_secret_key(tool_id),
        Zeroizing::new(json),
    )?;
    if let Some(custom_ref) = secret_ref.map(str::trim).filter(|r| !r.is_empty()) {
        secrets.set_ref(custom_ref, Zeroizing::new(creds.access_token.clone()))?;
    } else {
        secrets.set_secret(
            bot_id,
            &format!("mcp:{tool_id}"),
            Zeroizing::new(creds.access_token.clone()),
        )?;
    }
    Ok(())
}

fn sanitize_thread_name_token(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect();
    if clean.is_empty() {
        "tool".to_owned()
    } else {
        clean
    }
}

fn validate_mcp_tool_id(tool_id: &str) -> Result<(), String> {
    if tool_id.trim().is_empty()
        || tool_id.len() > 128
        || tool_id.chars().any(|c| c.is_control() || c == '\0')
    {
        return Err("MCP tool id must be 1..=128 non-control characters".to_owned());
    }
    Ok(())
}

fn find_oauth_tool<'a>(bot: &'a BotConfig, tool_id: &str) -> Result<&'a BotMcpToolConfig, String> {
    validate_mcp_tool_id(tool_id)?;
    let tool = bot
        .mcp_tools
        .iter()
        .find(|t| t.id == tool_id)
        .ok_or_else(|| {
            format!(
                "MCP tool '{tool_id}' is not configured on bot '{}'",
                bot.id.as_str()
            )
        })?;
    validate_mcp_tool_id(&tool.id)?;
    if tool.auth_kind != BotMcpAuthKind::BrowserOAuth {
        return Err(format!(
            "MCP tool '{}' uses {:?} auth, not BrowserOAuth",
            tool.id, tool.auth_kind
        ));
    }
    Ok(tool)
}

fn discover_oauth_metadata(
    endpoint_url: &str,
    policy: SsrfPolicy,
) -> Result<DiscoveredOAuthMetadata, String> {
    validate_oauth_endpoint_url(endpoint_url, policy, "endpoint_url")?;
    let parsed = http_client::parse_url(endpoint_url)
        .map_err(|err| format!("invalid MCP endpoint URL: {err}"))?;
    let scheme = if parsed.https { "https" } else { "http" };
    let origin = format!("{scheme}://{}", parsed.host_header);
    let path_only = parsed.target.split('?').next().unwrap_or("");
    let path_suffix = if path_only == "/" { "" } else { path_only };

    // 1. RFC 9728 Protected Resource Metadata discovery.
    let mut prm_candidates = Vec::new();
    if !path_suffix.is_empty() {
        prm_candidates.push(format!(
            "{origin}/.well-known/oauth-protected-resource{path_suffix}"
        ));
    }
    prm_candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));

    let mut prm: Option<ProtectedResourceMetadata> = None;
    for url in prm_candidates {
        if let Some(found) = fetch_json::<ProtectedResourceMetadata>(&url, policy) {
            prm = Some(found);
            break;
        }
    }

    if prm.is_none()
        && let Some(challenge_url) = probe_resource_metadata_challenge(endpoint_url, policy)
        && validate_oauth_endpoint_url(&challenge_url, policy, "resource_metadata").is_ok()
    {
        prm = fetch_json::<ProtectedResourceMetadata>(&challenge_url, policy);
    }

    let (issuer_url, prm_scopes, resource) = match prm {
        Some(m) => {
            let issuer = m
                .authorization_servers
                .into_iter()
                .find(|s| validate_oauth_endpoint_url(s, policy, "authorization_server").is_ok())
                .unwrap_or_else(|| origin.clone());
            (issuer, m.scopes_supported, m.resource)
        }
        None => (origin, Vec::new(), Some(endpoint_url.to_owned())),
    };
    validate_oauth_endpoint_url(&issuer_url, policy, "authorization_server")?;

    // 2. RFC 8414 Authorization Server Metadata (+ OpenID Connect fallback).
    let parsed_issuer = http_client::parse_url(&issuer_url)
        .map_err(|err| format!("invalid authorization server URL '{issuer_url}': {err}"))?;
    let i_scheme = if parsed_issuer.https { "https" } else { "http" };
    let i_origin = format!("{i_scheme}://{}", parsed_issuer.host_header);
    let i_path = parsed_issuer.target.split('?').next().unwrap_or("");
    let i_suffix = if i_path == "/" { "" } else { i_path };

    let mut as_candidates = Vec::new();
    if !i_suffix.is_empty() {
        as_candidates.push(format!(
            "{i_origin}/.well-known/oauth-authorization-server{i_suffix}"
        ));
        as_candidates.push(format!(
            "{i_origin}{i_suffix}/.well-known/openid-configuration"
        ));
    }
    as_candidates.push(format!("{i_origin}/.well-known/oauth-authorization-server"));
    as_candidates.push(format!("{i_origin}/.well-known/openid-configuration"));

    let mut auth_server: Option<AuthServerMetadata> = None;
    for url in as_candidates {
        if let Some(found) = fetch_json::<AuthServerMetadata>(&url, policy)
            && found.authorization_endpoint.is_some()
            && found.token_endpoint.is_some()
        {
            auth_server = Some(found);
            break;
        }
    }

    let as_meta = auth_server.ok_or_else(|| {
        format!("authorization server metadata discovery failed for '{issuer_url}'")
    })?;
    if !as_meta.code_challenge_methods_supported.is_empty()
        && !as_meta
            .code_challenge_methods_supported
            .iter()
            .any(|m| m.eq_ignore_ascii_case("S256"))
    {
        return Err("MCP OAuth server does not support required PKCE S256".to_owned());
    }
    let authorization_endpoint = as_meta
        .authorization_endpoint
        .ok_or_else(|| "OAuth metadata missing authorization_endpoint".to_owned())?;
    validate_oauth_endpoint_url(&authorization_endpoint, policy, "authorization_endpoint")?;
    let token_endpoint = as_meta
        .token_endpoint
        .ok_or_else(|| "OAuth metadata missing token_endpoint".to_owned())?;
    validate_oauth_endpoint_url(&token_endpoint, policy, "token_endpoint")?;
    if let Some(ref reg_url) = as_meta.registration_endpoint {
        validate_oauth_endpoint_url(reg_url, policy, "registration_endpoint")?;
    }

    let scopes = if prm_scopes.is_empty() {
        as_meta.scopes_supported
    } else {
        prm_scopes
    };

    Ok(DiscoveredOAuthMetadata {
        authorization_endpoint,
        token_endpoint,
        registration_endpoint: as_meta.registration_endpoint,
        scopes,
        resource,
    })
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn validate_oauth_endpoint_url(url: &str, policy: SsrfPolicy, label: &str) -> Result<(), String> {
    let parsed =
        http_client::parse_url(url).map_err(|err| format!("invalid OAuth {label} URL: {err}"))?;
    if parsed.https {
        return Ok(());
    }
    if policy == SsrfPolicy::AllowLoopback && is_loopback_host(&parsed.host) {
        return Ok(());
    }
    Err(format!("OAuth {label} must use https://"))
}

fn fetch_json<T: for<'de> Deserialize<'de>>(url: &str, policy: SsrfPolicy) -> Option<T> {
    let resp = http_client::send(
        &HttpRequest::get(url)
            .with_header("Accept", "application/json")
            .with_timeout(Duration::from_secs(10))
            .with_ssrf_policy(policy),
    )
    .ok()?;
    if !resp.is_success() {
        return None;
    }
    resp.json::<T>().ok()
}

fn probe_resource_metadata_challenge(endpoint_url: &str, policy: SsrfPolicy) -> Option<String> {
    let resp = http_client::send(
        &HttpRequest::get(endpoint_url)
            .with_timeout(Duration::from_secs(8))
            .with_ssrf_policy(policy),
    )
    .ok()?;
    if resp.status != 401 && resp.status != 403 {
        return None;
    }
    for (name, val) in &resp.headers {
        if !name.eq_ignore_ascii_case("www-authenticate") {
            continue;
        }
        if let Some(url) = extract_quoted_param(val, "resource_metadata")
            && validate_oauth_endpoint_url(&url, policy, "resource_metadata").is_ok()
        {
            return Some(url);
        }
    }
    None
}

fn extract_quoted_param(header: &str, key: &str) -> Option<String> {
    let lower = header.to_ascii_lowercase();
    let needle = format!("{key}=");
    let pos = lower.find(&needle)?;
    let rest = header.get(pos.saturating_add(needle.len())..)?.trim_start();
    if let Some(after_quote) = rest.strip_prefix('"') {
        let end = after_quote.find('"')?;
        return Some(after_quote.get(..end)?.to_owned());
    }
    let end = rest.find([',', ' ', '\t']).unwrap_or(rest.len());
    Some(rest.get(..end)?.to_owned())
}

fn register_dynamic_client(
    registration_url: &str,
    redirect_uri: &str,
    policy: SsrfPolicy,
) -> Result<(String, Option<String>), String> {
    let body = serde_json::json!({
        "client_name": "Krill",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let payload = serde_json::to_vec(&body)
        .map_err(|err| format!("failed encoding dynamic client registration: {err}"))?;
    let resp = http_client::send(
        &HttpRequest::post_json(registration_url, payload)
            .with_header("Accept", "application/json")
            .with_timeout(Duration::from_secs(12))
            .with_ssrf_policy(policy),
    )
    .map_err(|err| format!("dynamic client registration failed: {}", err.kind()))?;
    if !resp.is_success() {
        return Err(format!(
            "dynamic client registration rejected with HTTP {}",
            resp.status
        ));
    }
    let parsed: DynamicClientRegistrationResp = resp
        .json()
        .map_err(|err| format!("invalid dynamic client registration response: {err}"))?;
    if parsed.client_id.trim().is_empty() {
        return Err("dynamic client registration returned empty client_id".to_owned());
    }
    Ok((parsed.client_id, parsed.client_secret))
}

fn generate_random_b64url(bytes_len: usize) -> Result<Zeroizing<String>, String> {
    let mut raw = Zeroizing::new(vec![0u8; bytes_len]);
    SystemRandom::new()
        .fill(raw.as_mut_slice())
        .map_err(|_| "OS RNG failed during OAuth PKCE generation".to_owned())?;
    Ok(Zeroizing::new(http_client::base64url_encode(
        raw.as_slice(),
    )))
}

fn compute_pkce_s256(verifier: &str) -> String {
    let sum = digest::digest(&digest::SHA256, verifier.as_bytes());
    http_client::base64url_encode(sum.as_ref())
}

#[expect(
    clippy::too_many_arguments,
    reason = "OAuth authorize URL builder parameters"
)]
fn build_authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
    scopes: &[String],
    resource: Option<&str>,
    policy: SsrfPolicy,
) -> Result<String, String> {
    validate_oauth_endpoint_url(authorization_endpoint, policy, "authorization_endpoint")?;
    let sep = if authorization_endpoint.contains('?') {
        "&"
    } else {
        "?"
    };
    let mut params = vec![
        ("response_type", "code".to_owned()),
        ("client_id", client_id.to_owned()),
        ("redirect_uri", redirect_uri.to_owned()),
        ("code_challenge", code_challenge.to_owned()),
        ("code_challenge_method", "S256".to_owned()),
        ("state", state.to_owned()),
    ];
    if !scopes.is_empty() {
        params.push(("scope", scopes.join(" ")));
    }
    if let Some(res) = resource.filter(|r| !r.is_empty()) {
        params.push(("resource", res.to_owned()));
    }
    let refs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let encoded = http_client::encode_form_pairs(&refs);
    if encoded.is_empty() {
        return Err("failed encoding authorization query".to_owned());
    }
    Ok(format!("{authorization_endpoint}{sep}{encoded}"))
}

#[expect(clippy::too_many_arguments, reason = "loopback PKCE callback state")]
fn run_pkce_callback_listener(
    listener: &TcpListener,
    cancel: &AtomicBool,
    active_socket: &http_client::RequestCancelSlot,
    bot_id: &BotId,
    tool: &BotMcpToolConfig,
    redirect_uri: &str,
    client_id: &str,
    client_secret: Option<&str>,
    code_verifier: &Zeroizing<String>,
    expected_state: &str,
    token_endpoint: &str,
    resource: Option<&str>,
    secrets: &SecretVault,
    policy: SsrfPolicy,
) -> Result<(), String> {
    let deadline = Instant::now() + OAUTH_CALLBACK_TIMEOUT;
    let (mut callback_stream, code) = loop {
        if cancel.load(Ordering::Acquire) {
            return Err("OAuth sign-in cancelled".to_owned());
        }
        if Instant::now() >= deadline {
            return Err("MCP OAuth sign-in timed out awaiting callback".to_owned());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let Ok(target) = read_callback_request_target(&mut stream) else {
                    write_html_response(&mut stream, 400, "Bad Request", "Malformed request.");
                    continue;
                };
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                if path == "/favicon.ico" {
                    write_empty_response(&mut stream, 204, "No Content");
                    continue;
                }
                if path != "/callback" {
                    write_html_response(&mut stream, 404, "Not Found", "Unknown callback route.");
                    continue;
                }
                let params = http_client::parse_query_params(query);
                let got_state = params
                    .iter()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or_default();
                if !constant_time_eq(got_state.as_bytes(), expected_state.as_bytes()) {
                    write_html_response(&mut stream, 400, "Invalid State", "State mismatch.");
                    continue;
                }
                if let Some((_, err)) = params.iter().find(|(k, _)| k == "error") {
                    write_html_response(
                        &mut stream,
                        400,
                        "Authorization Failed",
                        "Authorization denied. You may close this tab.",
                    );
                    return Err(format!("MCP OAuth authorization error: {err}"));
                }
                let Some((_, code)) = params
                    .iter()
                    .find(|(k, v)| k == "code" && !v.trim().is_empty())
                else {
                    write_html_response(
                        &mut stream,
                        400,
                        "Missing Code",
                        "Missing authorization code.",
                    );
                    continue;
                };
                break (stream, code.clone());
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(OAUTH_ACCEPT_POLL_SLEEP);
            }
            Err(err) => {
                return Err(format!("MCP OAuth callback listener error: {}", err.kind()));
            }
        }
    };

    let finish_result: Result<(), String> = (|| {
        if cancel.load(Ordering::Acquire) {
            return Err("OAuth sign-in cancelled".to_owned());
        }
        let mut form = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code),
            ("redirect_uri", redirect_uri.to_owned()),
            ("client_id", client_id.to_owned()),
            ("code_verifier", (**code_verifier).clone()),
        ];
        if let Some(secret) = client_secret.filter(|s| !s.is_empty()) {
            form.push(("client_secret", secret.to_owned()));
        }
        if let Some(res) = resource.filter(|r| !r.is_empty()) {
            form.push(("resource", res.to_owned()));
        }
        let refs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let resp = http_client::send_interruptible(
            &HttpRequest::post_form(token_endpoint, &refs)
                .with_header("Accept", "application/json")
                .with_timeout(Duration::from_secs(15))
                .with_ssrf_policy(policy),
            Some(cancel),
            Some(active_socket),
        )
        .map_err(|err| format!("MCP OAuth token exchange failed: {}", err.kind()))?;
        if cancel.load(Ordering::Acquire) {
            return Err("OAuth sign-in cancelled".to_owned());
        }
        if !resp.is_success() {
            return Err(format!(
                "MCP OAuth token endpoint returned HTTP {}",
                resp.status
            ));
        }
        let tok: OAuthTokenResponse = resp
            .json()
            .map_err(|err| format!("invalid MCP OAuth token payload: {err}"))?;
        if tok.access_token.trim().is_empty() {
            return Err("MCP OAuth token response missing access_token".to_owned());
        }

        let now = now_ms();
        let creds = StoredMcpOAuthCredentials {
            access_token: tok.access_token,
            refresh_token: tok.refresh_token,
            token_endpoint: Some(token_endpoint.to_owned()),
            client_id: Some(client_id.to_owned()),
            client_secret: client_secret.map(ToOwned::to_owned),
            expires_at_ms: tok
                .expires_in
                .map(|s| now.saturating_add(s.saturating_mul(1_000))),
            scope: tok.scope,
            resource: resource.map(ToOwned::to_owned),
        };
        // Serialize callback token persistence with concurrent background token refreshes (CORE-18).
        let refresh_lock = refresh_mutex_for(bot_id, &tool.id);
        let _refresh_guard = lock(&refresh_lock);
        if cancel.load(Ordering::Acquire) {
            return Err("OAuth sign-in cancelled".to_owned());
        }
        persist_oauth_credentials(
            secrets,
            bot_id,
            &tool.id,
            tool.secret_ref.as_deref(),
            &creds,
        )
        .map_err(|err| format!("failed persisting MCP OAuth tokens: {err}"))?;
        Ok(())
    })();

    match finish_result {
        Ok(()) => {
            write_html_response(
                &mut callback_stream,
                200,
                "Authorized",
                "MCP authorization complete. You may close this tab and return to Krill.",
            );
            Ok(())
        }
        Err(err) => {
            let (code, title, msg) = if cancel.load(Ordering::Acquire) {
                (
                    409,
                    "Authorization Cancelled",
                    "MCP authorization was cancelled or superseded. You may close this tab.",
                )
            } else {
                (
                    502,
                    "Authorization Failed",
                    "MCP token exchange or storage failed. Please close this tab and retry in Krill.",
                )
            };
            write_html_response(&mut callback_stream, code, title, msg);
            Err(err)
        }
    }
}

fn exchange_refresh_token(
    token_endpoint: &str,
    refresh_token: &str,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    resource: Option<&str>,
    policy: SsrfPolicy,
) -> Result<OAuthTokenResponse, String> {
    validate_oauth_endpoint_url(token_endpoint, policy, "token_endpoint")?;
    let mut form = vec![
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", refresh_token.to_owned()),
    ];
    if let Some(cid) = client_id.filter(|s| !s.is_empty()) {
        form.push(("client_id", cid.to_owned()));
    }
    if let Some(csec) = client_secret.filter(|s| !s.is_empty()) {
        form.push(("client_secret", csec.to_owned()));
    }
    if let Some(res) = resource.filter(|s| !s.is_empty()) {
        form.push(("resource", res.to_owned()));
    }
    let refs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let resp = http_client::send(
        &HttpRequest::post_form(token_endpoint, &refs)
            .with_header("Accept", "application/json")
            .with_timeout(Duration::from_secs(15))
            .with_ssrf_policy(policy),
    )
    .map_err(|err| format!("refresh request failed: {}", err.kind()))?;
    if !resp.is_success() {
        return Err(format!("refresh endpoint HTTP {}", resp.status));
    }
    let parsed: OAuthTokenResponse = resp
        .json()
        .map_err(|err| format!("invalid refresh token payload: {err}"))?;
    if parsed.access_token.trim().is_empty() {
        return Err("empty access_token in refresh response".to_owned());
    }
    Ok(parsed)
}

fn read_callback_request_target(stream: &mut TcpStream) -> io::Result<String> {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = std::str::from_utf8(buf.get(..end).unwrap_or_default())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF8 head"))?;
            let req_line = head.lines().next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "missing request line")
            })?;
            let target = req_line
                .split_whitespace()
                .nth(1)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing target"))?;
            return Ok(target.to_owned());
        }
        if buf.len() >= MAX_CALLBACK_HEAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "callback HTTP header too large",
            ));
        }
        let n = match stream.read(&mut tmp) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF before OAuth callback headers",
                ));
            }
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        buf.extend_from_slice(tmp.get(..n).unwrap_or_default());
    }
}

fn write_empty_response(stream: &mut TcpStream, status: u16, reason: &str) {
    let resp =
        format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    stream.write_all(resp.as_bytes()).ok();
    stream.flush().ok();
}

fn write_html_response(stream: &mut TcpStream, status: u16, reason: &str, message: &str) {
    let body = format!(
        "<!doctype html><html><body style=\"font-family:sans-serif;padding:24px\"><h2>{message}</h2></body></html>"
    );
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).ok();
    stream.flush().ok();
}

fn refresh_mutex_for(bot_id: &BotId, tool_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let key = format!("{}:{tool_id}", bot_id.as_str());
    let mut guard = lock(map);
    Arc::clone(guard.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))))
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-mcp-oauth-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discovery_dcr_pkce_callback_with_favicon_and_token_refresh_work() {
        let dir = temp_dir("flow");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x44; 32]));
        let mgr = McpOAuthManager::new(Arc::clone(&secrets));
        mgr.set_ssrf_policy(SsrfPolicy::AllowLoopback);

        // Mock MCP Authorization Server serving PRM, AS metadata, DCR, and Token endpoints.
        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        mock.set_nonblocking(true).unwrap();
        let addr = mock.local_addr().unwrap();
        let captured_verifier = Arc::new(Mutex::new(String::new()));
        let verifier_bg = Arc::clone(&captured_verifier);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);

        let srv = thread::spawn(move || {
            while !stop_bg.load(Ordering::Acquire) {
                let (mut sock, _) = match mock.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                sock.set_nonblocking(false).ok();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    continue;
                }
                let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
                let target = raw
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let body_part = raw.split("\r\n\r\n").nth(1).unwrap_or("");

                let (status, json) = if target.starts_with("/.well-known/oauth-protected-resource")
                {
                    (
                        "200 OK",
                        format!(
                            r#"{{"resource":"http://{addr}/mcp","authorization_servers":["http://{addr}"],"scopes_supported":["read","write"]}}"#
                        ),
                    )
                } else if target == "/.well-known/oauth-authorization-server" {
                    (
                        "200 OK",
                        format!(
                            r#"{{"authorization_endpoint":"http://{addr}/authorize","token_endpoint":"http://{addr}/token","registration_endpoint":"http://{addr}/register","code_challenge_methods_supported":["S256"]}}"#
                        ),
                    )
                } else if target == "/register" {
                    (
                        "201 Created",
                        r#"{"client_id":"dyn-client-123"}"#.to_owned(),
                    )
                } else if target == "/token" {
                    let form = http_client::parse_query_params(body_part);
                    let grant_type = form
                        .iter()
                        .find(|(k, _)| k == "grant_type")
                        .map_or("", |(_, v)| v.as_str());
                    if grant_type == "authorization_code" {
                        let v = form
                            .iter()
                            .find(|(k, _)| k == "code_verifier")
                            .map(|(_, v)| v.clone())
                            .unwrap_or_default();
                        *lock(&verifier_bg) = v;
                        (
                            "200 OK",
                            r#"{"access_token":"mcp-tok-initial","refresh_token":"mcp-ref-1","expires_in":1}"#
                                .to_owned(),
                        )
                    } else {
                        assert_eq!(grant_type, "refresh_token");
                        (
                            "200 OK",
                            r#"{"access_token":"mcp-tok-refreshed","refresh_token":"mcp-ref-2","expires_in":3600}"#
                                .to_owned(),
                        )
                    }
                } else {
                    ("404 Not Found", "{}".to_owned())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json}",
                    json.len()
                );
                sock.write_all(resp.as_bytes()).unwrap();
            }
        });

        let bot_id = BotId::new("bot-oauth").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "OAuth Bot", "claude", 1_000).unwrap();
        bot.mcp_tools.push(BotMcpToolConfig {
            id: "notion".to_owned(),
            name: "Notion".to_owned(),
            endpoint_url: Some(format!("http://{addr}/mcp")),
            command: Vec::new(),
            auth_kind: BotMcpAuthKind::BrowserOAuth,
            header_names: vec!["Authorization".to_owned()],
            secret_ref: None,
            enabled: true,
        });

        let started = mgr.start_oauth(&bot, "notion").unwrap();
        assert_eq!(started.status, "pending");
        let auth_url = started.auth_url.clone().unwrap();
        assert!(auth_url.contains("code_challenge_method=S256"));
        assert!(auth_url.contains("client_id=dyn-client-123"));

        let query = auth_url.split('?').nth(1).unwrap();
        let q_params = http_client::parse_query_params(query);
        let redirect_uri = q_params
            .iter()
            .find(|(k, _)| k == "redirect_uri")
            .unwrap()
            .1
            .clone();
        let state = q_params
            .iter()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .clone();
        let challenge = q_params
            .iter()
            .find(|(k, _)| k == "code_challenge")
            .unwrap()
            .1
            .clone();

        // Hit /favicon.ico first (must be ignored by loop!), then send split /callback headers.
        let cb_parsed = http_client::parse_url(&redirect_uri).unwrap();
        let fav = http_client::send(
            &HttpRequest::get(format!("http://{}/favicon.ico", cb_parsed.host_header))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(fav.status, 204);

        let cb_sock = std::net::TcpStream::connect(&cb_parsed.host_header).unwrap();
        let mut cb_writer = cb_sock;
        let head = format!(
            "GET /callback?code=auth-code-9&state={state} HTTP/1.1\r\nHost: {}\r\n",
            cb_parsed.host_header
        );
        cb_writer.write_all(head.as_bytes()).unwrap();
        cb_writer.flush().unwrap();
        thread::sleep(Duration::from_millis(20));
        cb_writer.write_all(b"\r\n").unwrap();
        cb_writer.flush().unwrap();

        let mut final_state = mgr.status(&bot, "notion");
        for _ in 0..40 {
            if final_state.status == "authorized" {
                break;
            }
            thread::sleep(Duration::from_millis(25));
            final_state = mgr.status(&bot, "notion");
        }
        assert_eq!(
            final_state.status, "authorized",
            "err: {:?}",
            final_state.error
        );

        let seen_verifier = lock(&captured_verifier).clone();
        assert_eq!(compute_pkce_s256(&seen_verifier), challenge);

        // Token expires_in=1s -> within 60s skew -> resolves via refresh_token!
        let tok = resolve_or_refresh_token(
            &secrets,
            &bot_id,
            "notion",
            None,
            "mcp:notion",
            SsrfPolicy::AllowLoopback,
        )
        .unwrap()
        .unwrap();
        assert_eq!(&*tok, "mcp-tok-refreshed");

        stop.store(true, Ordering::Release);
        srv.join().ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancel_marks_pending_flow_cancelled() {
        let dir = temp_dir("cancel");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x55; 32]));
        let mgr = McpOAuthManager::new(secrets);
        let bot_id = BotId::new("bot-c").unwrap();
        let st = mgr.cancel(&bot_id, "slack-mcp");
        assert_eq!(st.status, "cancelled");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oauth_validates_endpoint_schemes_state_order_exchange_status_refresh_and_cancel() {
        // 1. Authorization endpoint scheme validation.
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://evil.example/authorize",
        ] {
            assert!(
                validate_oauth_endpoint_url(
                    bad,
                    SsrfPolicy::StrictExternal,
                    "authorization_endpoint"
                )
                .is_err(),
                "must reject {bad}"
            );
        }
        assert!(
            validate_oauth_endpoint_url(
                "https://auth.example.com/authorize",
                SsrfPolicy::StrictExternal,
                "authorization_endpoint"
            )
            .is_ok()
        );
        assert!(
            validate_oauth_endpoint_url(
                "http://127.0.0.1:9999/authorize",
                SsrfPolicy::AllowLoopback,
                "authorization_endpoint"
            )
            .is_ok()
        );
        assert!(
            validate_oauth_endpoint_url(
                "http://127.0.0.1:9999/authorize",
                SsrfPolicy::StrictExternal,
                "authorization_endpoint"
            )
            .is_err()
        );

        // 2. Callback ignores bogus `?error=` with wrong state, returns 502 on token exchange failure,
        //    and manual vault token overrides failed flow in `status()`.
        let dir = temp_dir("hardening");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x61; 32]));
        let mgr = McpOAuthManager::new(Arc::clone(&secrets));
        mgr.set_ssrf_policy(SsrfPolicy::AllowLoopback);

        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        mock.set_nonblocking(true).unwrap();
        let addr = mock.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let refresh_mode = Arc::new(Mutex::new("fail"));
        let refresh_mode_bg = Arc::clone(&refresh_mode);
        let srv = thread::spawn(move || {
            while !stop_bg.load(Ordering::Acquire) {
                let (mut sock, _) = match mock.accept() {
                    Ok(p) => p,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                sock.set_nonblocking(false).ok();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    continue;
                }
                let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
                let target = raw
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (status, json) = if target == "/.well-known/oauth-authorization-server" {
                    (
                        "200 OK",
                        format!(
                            r#"{{"authorization_endpoint":"http://{addr}/authorize","token_endpoint":"http://{addr}/token","code_challenge_methods_supported":["S256"]}}"#
                        ),
                    )
                } else if target == "/token" {
                    match *lock(&refresh_mode_bg) {
                        "fail" => ("400 Bad Request", r#"{"error":"invalid_grant"}"#.to_owned()),
                        _ => ("200 OK", r#"{"access_token":"tok-no-exp"}"#.to_owned()),
                    }
                } else {
                    ("404 Not Found", "{}".to_owned())
                };
                let wire = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json}",
                    json.len()
                );
                sock.write_all(wire.as_bytes()).ok();
            }
        });

        let bot_id = BotId::new("bot-oauth-hard").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "OAuthHard", "claude", 1_000).unwrap();
        bot.mcp_tools.push(BotMcpToolConfig {
            id: "linear".to_owned(),
            name: "Linear".to_owned(),
            endpoint_url: Some(format!("http://{addr}/mcp")),
            command: Vec::new(),
            auth_kind: BotMcpAuthKind::BrowserOAuth,
            header_names: vec!["Authorization".to_owned()],
            secret_ref: None,
            enabled: true,
        });

        let started = mgr.start_oauth(&bot, "linear").unwrap();
        let auth_url = started.auth_url.unwrap();
        let q_params = http_client::parse_query_params(auth_url.split('?').nth(1).unwrap());
        let redirect_uri = q_params
            .iter()
            .find(|(k, _)| k == "redirect_uri")
            .unwrap()
            .1
            .clone();
        let state = q_params
            .iter()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .clone();

        // Bogus error with mismatched state must NOT abort the listener.
        let bogus = http_client::send(
            &HttpRequest::get(format!("{redirect_uri}?error=access_denied&state=wrong"))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(bogus.status, 400);
        assert_eq!(mgr.status(&bot, "linear").status, "pending");

        // Valid state+code triggers token exchange (which returns 400) -> callback HTTP 502 (not 200!).
        let cb = http_client::send(
            &HttpRequest::get(format!("{redirect_uri}?code=abc&state={state}"))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(cb.status, 502);

        let mut st = mgr.status(&bot, "linear");
        for _ in 0..40 {
            if st.status == "error" {
                break;
            }
            thread::sleep(Duration::from_millis(20));
            st = mgr.status(&bot, "linear");
        }
        assert_eq!(st.status, "error");

        // Storing a token manually in the vault must clear the stale error and report authorized.
        secrets
            .set_secret(
                &bot_id,
                "mcp:linear",
                Zeroizing::new("manual-tok".to_owned()),
            )
            .unwrap();
        assert_eq!(mgr.status(&bot, "linear").status, "authorized");

        // `cancel()` must NOT overwrite an already-authorized flow.
        let key = (bot_id.clone(), "linear".to_owned());
        lock(&mgr.flows).insert(
            key,
            Arc::new(ActiveOAuthFlow {
                state: Mutex::new(BotMcpOAuthState {
                    bot_id: bot_id.clone(),
                    tool_id: "linear".to_owned(),
                    status: "authorized".to_owned(),
                    auth_url: None,
                    error: None,
                    updated_at_ms: now_ms(),
                }),
                cancel: Arc::new(AtomicBool::new(false)),
                active_socket: Arc::new(Mutex::new(None)),
            }),
        );
        assert_eq!(mgr.cancel(&bot_id, "linear").status, "authorized");

        // 3. Expired token with failing refresh must return Err (NOT expired access_token).
        let expired_creds = StoredMcpOAuthCredentials {
            access_token: "stale-tok".to_owned(),
            refresh_token: Some("bad-ref".to_owned()),
            token_endpoint: Some(format!("http://{addr}/token")),
            client_id: Some("cid".to_owned()),
            client_secret: None,
            expires_at_ms: Some(1),
            scope: None,
            resource: None,
        };
        persist_oauth_credentials(&secrets, &bot_id, "linear", None, &expired_creds).unwrap();
        let err = resolve_or_refresh_token(
            &secrets,
            &bot_id,
            "linear",
            None,
            "mcp:linear",
            SsrfPolicy::AllowLoopback,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        // 4. Successful refresh omitting `expires_in` clears stale past `expires_at_ms`.
        *lock(&refresh_mode) = "no_exp";
        let refreshed = resolve_or_refresh_token(
            &secrets,
            &bot_id,
            "linear",
            None,
            "mcp:linear",
            SsrfPolicy::AllowLoopback,
        )
        .unwrap()
        .unwrap();
        assert_eq!(&*refreshed, "tok-no-exp");
        let saved_raw = secrets
            .get_secret(&bot_id, &oauth_bundle_secret_key("linear"))
            .unwrap()
            .unwrap();
        let saved: StoredMcpOAuthCredentials = serde_json::from_str(&saved_raw).unwrap();
        assert_eq!(saved.expires_at_ms, None);

        stop.store(true, Ordering::Release);
        srv.join().ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mcp_oauth_rejects_nul_tool_id_and_sanitizes_thread_names() {
        let dir = temp_dir("nul-id");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x51; 32]));
        let mgr = McpOAuthManager::new(secrets);
        let bot_id = BotId::new("bot-nul").unwrap();
        let mut bot = BotConfig::new(bot_id, "Nul Bot", "claude", 1_000).unwrap();
        bot.mcp_tools.push(BotMcpToolConfig {
            id: "evil\0tool".to_owned(),
            name: "Evil".to_owned(),
            endpoint_url: Some("https://mcp.example.com".to_owned()),
            command: Vec::new(),
            auth_kind: BotMcpAuthKind::BrowserOAuth,
            header_names: Vec::new(),
            secret_ref: None,
            enabled: true,
        });

        let err = mgr.start_oauth(&bot, "evil\0tool").unwrap_err();
        assert!(err.contains("non-control"), "{err}");

        let clean = sanitize_thread_name_token("my\0weird/tool name!!");
        assert_eq!(clean, "myweirdtoolname");
        assert!(!clean.contains('\0'));
        assert_eq!(sanitize_thread_name_token("\0\n"), "tool");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn r2m06_r2m11_strict_https_validation_and_cancel_prevents_token_persistence() {
        // R2M-06: StrictExternal rejects cleartext http:// on discovery endpoint & refresh token_endpoint.
        let disc_err =
            discover_oauth_metadata("http://mcp.example.com/sse", SsrfPolicy::StrictExternal)
                .unwrap_err();
        assert!(disc_err.contains("https://"), "{disc_err}");
        let Err(ref_err) = exchange_refresh_token(
            "http://auth.example.com/token",
            "ref-1",
            Some("cid"),
            None,
            None,
            SsrfPolicy::StrictExternal,
        ) else {
            panic!("expected https validation error");
        };
        assert!(ref_err.contains("https://"), "{ref_err}");

        // R2M-11: Cancelling an in-flight MCP OAuth flow during token exchange prevents SecretVault write.
        let dir = temp_dir("r2m11-cancel");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x52; 32]));
        let mgr = Arc::new(McpOAuthManager::new(Arc::clone(&secrets)));
        mgr.set_ssrf_policy(SsrfPolicy::AllowLoopback);

        let token_hit = Arc::new(AtomicBool::new(false));
        let token_hit_bg = Arc::clone(&token_hit);
        let release_token = Arc::new(AtomicBool::new(false));
        let release_token_bg = Arc::clone(&release_token);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop_srv = Arc::new(AtomicBool::new(false));
        let stop_srv_bg = Arc::clone(&stop_srv);
        listener.set_nonblocking(true).unwrap();

        let srv = thread::spawn(move || {
            while !stop_srv_bg.load(Ordering::Acquire) {
                let Ok((mut sock, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                };
                sock.set_nonblocking(false).ok();
                sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(buf.get(..n).unwrap_or_default());
                let first = req.lines().next().unwrap_or_default();
                let (status, body) = if first.contains("/.well-known/oauth-authorization-server") {
                    (
                        "200 OK",
                        format!(
                            r#"{{"authorization_endpoint":"http://{addr}/authorize","token_endpoint":"http://{addr}/token","code_challenge_methods_supported":["S256"]}}"#
                        ),
                    )
                } else if first.contains("POST /token") {
                    token_hit_bg.store(true, Ordering::Release);
                    let wait_until = Instant::now() + Duration::from_secs(3);
                    while !release_token_bg.load(Ordering::Acquire) && Instant::now() < wait_until {
                        thread::sleep(Duration::from_millis(10));
                    }
                    (
                        "200 OK",
                        r#"{"access_token":"leaked-after-cancel","expires_in":3600}"#.to_owned(),
                    )
                } else {
                    ("404 Not Found", "{}".to_owned())
                };
                let wire = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).ok();
            }
        });

        let bot_id = BotId::new("bot-r2m11").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "OAuthCancel", "claude", 1_000).unwrap();
        bot.mcp_tools.push(BotMcpToolConfig {
            id: "slow".to_owned(),
            name: "Slow".to_owned(),
            endpoint_url: Some(format!("http://{addr}/mcp")),
            command: Vec::new(),
            auth_kind: BotMcpAuthKind::BrowserOAuth,
            header_names: Vec::new(),
            secret_ref: None,
            enabled: true,
        });
        let started = mgr.start_oauth(&bot, "slow").unwrap();
        let auth_url = started.auth_url.unwrap();
        let q = http_client::parse_query_params(auth_url.split_once('?').unwrap().1);
        let redirect_uri = q
            .iter()
            .find(|(k, _)| k == "redirect_uri")
            .unwrap()
            .1
            .clone();
        let state = q.iter().find(|(k, _)| k == "state").unwrap().1.clone();

        let cb_thread = thread::spawn(move || {
            http_client::send(
                &HttpRequest::get(format!("{redirect_uri}?code=c1&state={state}"))
                    .with_ssrf_policy(SsrfPolicy::AllowLoopback),
            )
            .ok();
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        while !token_hit.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(token_hit.load(Ordering::Acquire));
        let cancelled = mgr.cancel(&bot_id, "slow");
        assert_eq!(cancelled.status, "cancelled");
        release_token.store(true, Ordering::Release);
        cb_thread.join().ok();
        thread::sleep(Duration::from_millis(80));

        assert!(
            !has_stored_oauth_credentials(&secrets, &bot_id, "slow", None),
            "cancelled MCP OAuth flow must NEVER persist tokens into SecretVault"
        );
        assert_eq!(mgr.status(&bot, "slow").status, "cancelled");

        stop_srv.store(true, Ordering::Release);
        srv.join().ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn mcp_rotated_refresh_token_persistence_failure_returns_error() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!("krill-mcp-rot-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _n = stream.read(&mut buf).unwrap_or(0);
                let body = r#"{"access_token":"new-mcp","refresh_token":"rotated-mcp-rt","expires_in":3600}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).ok();
            }
        });

        let secrets = SecretVault::with_static_key(dir.clone(), [0x88; 32]);
        let bot_id = BotId::new("bot-mcp-rot").unwrap();
        let creds = StoredMcpOAuthCredentials {
            access_token: "expired-tok".to_owned(),
            refresh_token: Some("old-rt".to_owned()),
            token_endpoint: Some(format!("http://{addr}/token")),
            client_id: Some("cid".to_owned()),
            client_secret: None,
            expires_at_ms: Some(1),
            scope: None,
            resource: None,
        };
        persist_oauth_credentials(&secrets, &bot_id, "sentry", None, &creds).unwrap();

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();

        let err = resolve_or_refresh_token(
            &secrets,
            &bot_id,
            "sentry",
            None,
            "mcp:sentry",
            SsrfPolicy::AllowLoopback,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("rotated MCP OAuth refresh token"),
            "unexpected error: {err}"
        );

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).ok();
        srv.join().ok();
        fs::remove_dir_all(&dir).ok();
    }
}
