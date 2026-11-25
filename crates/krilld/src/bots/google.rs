// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! 100% local BYO Google Desktop OAuth 2.0 (PKCE S256 + loopback callback) and lazy 60s
//! Google Workspace poller (`Gmail`, `Calendar`, `Drive`).
//!
//! Zero cloud backend required: the user brings their own Google Desktop OAuth JSON saved in
//! [`SecretVault`]. Access & refresh tokens stay encrypted in [`SecretVault`] and auto-refresh.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krill_proto::bots::{
    BotConfig, BotConnectorStatus, BotConnectorTestResult, BotErrorCategory, BotErrorDetail, BotId,
    BotItemActor, BotItemLink, BotItemStatus, ConnectorHealthState, CreateBotItemParams,
    ExternalSource, GoogleServicesConfig,
};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::bots::http_client::{
    self, HttpRequest, SsrfPolicy, base64url_encode, parse_query_params, percent_encode,
};
use crate::bots::items::BotItemsStore;
use crate::bots::secrets::SecretVault;
use crate::bots::store::BotStore;
use crate::bots::tool_relay::constant_time_eq;
use crate::session::lock;

const STACK_SIZE: usize = 128 * 1024;
const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
const OAUTH_ACCEPT_POLL_SLEEP: Duration = Duration::from_millis(25);
const MAX_OAUTH_HEAD_BYTES: usize = 16 * 1024;
const TOKEN_REFRESH_MARGIN_MS: u64 = 60_000;
const DEFAULT_POLL_SECS: u64 = 60;

fn policy_for_url(url: &str) -> SsrfPolicy {
    if cfg!(test)
        && (url.starts_with("http://127.0.0.1")
            || url.starts_with("http://localhost")
            || url.starts_with("http://[::1]"))
    {
        SsrfPolicy::AllowLoopback
    } else {
        SsrfPolicy::StrictExternal
    }
}

fn bot_refresh_mutex(bot_id: &BotId) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<BotId, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut g = lock(map);
    Arc::clone(
        g.entry(bot_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

struct GoogleOAuthCancel {
    cancel: AtomicBool,
    active_socket: http_client::RequestCancelSlot,
}

impl GoogleOAuthCancel {
    fn new() -> Self {
        Self {
            cancel: AtomicBool::new(false),
            active_socket: Arc::new(Mutex::new(None)),
        }
    }

    fn trigger(&self) {
        self.cancel.store(true, Ordering::Release);
        http_client::cancel_active_request(&self.active_socket);
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

fn active_google_oauth_flows() -> &'static Mutex<BTreeMap<BotId, Arc<GoogleOAuthCancel>>> {
    static FLOWS: OnceLock<Mutex<BTreeMap<BotId, Arc<GoogleOAuthCancel>>>> = OnceLock::new();
    FLOWS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Cancels any active loopback Google OAuth listener thread for `bot_id`.
pub fn cancel_google_oauth(bot_id: &BotId) {
    if let Some(entry) = lock(active_google_oauth_flows()).remove(bot_id) {
        entry.trigger();
    }
}

/// Cancels all active loopback Google OAuth listener threads (e.g. on daemon shutdown).
pub fn cancel_all_google_oauth() {
    let drained: Vec<Arc<GoogleOAuthCancel>> = {
        let mut map = lock(active_google_oauth_flows());
        std::mem::take(&mut *map).into_values().collect()
    };
    for entry in drained {
        entry.trigger();
    }
}

/// Overridable Google OAuth & Workspace API endpoints (defaults to official Google endpoints).
#[expect(
    clippy::struct_field_names,
    reason = "explicit _url suffix distinguishes each Google OAuth/API endpoint"
)]
#[derive(Debug, Clone)]
pub struct GoogleEndpoints {
    pub auth_base_url: String,
    pub token_url: String,
    pub gmail_base_url: String,
    pub calendar_base_url: String,
    pub drive_base_url: String,
}

impl Default for GoogleEndpoints {
    fn default() -> Self {
        Self {
            auth_base_url: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
            token_url: "https://oauth2.googleapis.com/token".to_owned(),
            gmail_base_url: "https://gmail.googleapis.com".to_owned(),
            calendar_base_url: "https://www.googleapis.com".to_owned(),
            drive_base_url: "https://www.googleapis.com".to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
struct OAuthClientCredentials {
    client_id: String,
    client_secret: Option<Zeroizing<String>>,
}

#[derive(Clone, Serialize, Deserialize)]
struct GoogleTokenBundle {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at_ms: u64,
    #[serde(default)]
    token_type: Option<String>,
}

impl fmt::Debug for GoogleTokenBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleTokenBundle")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("has_refresh_token", &self.refresh_token.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    token_type: Option<String>,
}

/// Callback result emitted after the local loopback OAuth redirect finishes.
pub type OAuthCompleteCallback = Arc<dyn Fn(BotConnectorStatus) + Send + Sync>;

/// Starts a local loopback BYO Google Desktop OAuth 2.0 PKCE S256 flow.
///
/// Binds `127.0.0.1:0`, spawns a short-lived 128 KiB callback listener thread, and returns
/// `(auth_url, state)` for the user's browser.
pub fn start_google_oauth(
    bot: &BotConfig,
    secrets: Arc<SecretVault>,
    endpoints: GoogleEndpoints,
    on_complete: Option<OAuthCompleteCallback>,
) -> Result<(String, String), String> {
    let creds = load_oauth_client(bot, &secrets)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|err| format!("failed to bind OAuth loopback listener: {err}"))?;
    let port = listener
        .local_addr()
        .map_err(|err| format!("failed to inspect OAuth listener addr: {err}"))?
        .port();
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("failed to configure OAuth listener: {err}"))?;

    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let verifier = generate_pkce_verifier().map_err(|err| err.to_string())?;
    let challenge = pkce_s256_challenge(&verifier);
    let state = generate_random_hex(16).map_err(|err| err.to_string())?;

    let scopes = build_scopes(bot.connectors.google.services);
    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        endpoints.auth_base_url,
        percent_encode(&creds.client_id),
        percent_encode(&redirect_uri),
        percent_encode(&scopes.join(" ")),
        percent_encode(&state),
        percent_encode(&challenge),
    );

    let bot_id = bot.id.clone();
    let token_ref = bot
        .connectors
        .google
        .token_secret_ref
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(
            || SecretVault::scoped_ref(&bot.id, "google_oauth_tokens"),
            ToOwned::to_owned,
        );
    let expected_state = state.clone();

    let cancel = Arc::new(GoogleOAuthCancel::new());
    if let Some(prev) =
        lock(active_google_oauth_flows()).insert(bot_id.clone(), Arc::clone(&cancel))
    {
        prev.trigger();
    }
    let cancel_bg = Arc::clone(&cancel);
    let bot_id_bg = bot_id.clone();

    if let Err(err) = thread::Builder::new()
        .name(format!("bot-goauth-{}", bot.slug.as_str()))
        .stack_size(STACK_SIZE)
        .spawn(move || {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_oauth_callback_listener(
                    &listener,
                    &cancel_bg,
                    &bot_id_bg,
                    &creds,
                    &redirect_uri,
                    &verifier,
                    &expected_state,
                    &token_ref,
                    &endpoints.token_url,
                    &secrets,
                )
            }));
            {
                let mut flows = lock(active_google_oauth_flows());
                if flows
                    .get(&bot_id_bg)
                    .is_some_and(|cur| Arc::ptr_eq(cur, &cancel_bg))
                {
                    flows.remove(&bot_id_bg);
                }
            }
            if cancel_bg.is_cancelled() {
                return;
            }
            let status = match res {
                Ok(Ok(())) => BotConnectorStatus {
                    bot_id: bot_id_bg.clone(),
                    source: ExternalSource::Google,
                    enabled: true,
                    connected: true,
                    state: ConnectorHealthState::Connected,
                    account_label: Some("Google Workspace (OAuth)".to_owned()),
                    last_sync_ms: Some(now_ms()),
                    last_error: None,
                    error_category: None,
                },
                Ok(Err(msg)) => BotConnectorStatus {
                    bot_id: bot_id_bg.clone(),
                    source: ExternalSource::Google,
                    enabled: true,
                    connected: false,
                    state: ConnectorHealthState::Error,
                    account_label: None,
                    last_sync_ms: None,
                    last_error: Some(msg),
                    error_category: Some(BotErrorCategory::SignedOut),
                },
                Err(_) => BotConnectorStatus {
                    bot_id: bot_id_bg.clone(),
                    source: ExternalSource::Google,
                    enabled: true,
                    connected: false,
                    state: ConnectorHealthState::Error,
                    account_label: None,
                    last_sync_ms: None,
                    last_error: Some("Google OAuth listener panicked".to_owned()),
                    error_category: Some(BotErrorCategory::AgentError),
                },
            };
            if let Some(cb) = on_complete {
                cb(status);
            }
        })
    {
        let mut flows = lock(active_google_oauth_flows());
        if flows
            .get(&bot_id)
            .is_some_and(|cur| Arc::ptr_eq(cur, &cancel))
        {
            flows.remove(&bot_id);
        }
        return Err(format!(
            "failed to spawn Google OAuth listener thread: {err}"
        ));
    }

    Ok((auth_url, state))
}

fn read_oauth_head_target(stream: &mut std::net::TcpStream) -> io::Result<String> {
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = std::str::from_utf8(buf.get(..end).unwrap_or_default())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF8 OAuth head"))?;
            let first_line = head.lines().next().unwrap_or_default();
            let target = first_line.split_whitespace().nth(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "missing request target")
            })?;
            return Ok(target.to_owned());
        }
        if buf.len() >= MAX_OAUTH_HEAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "OAuth callback headers exceed limit",
            ));
        }
        match stream.read(&mut tmp) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF reading OAuth callback",
                ));
            }
            Ok(n) => buf.extend_from_slice(tmp.get(..n).unwrap_or_default()),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

#[expect(clippy::too_many_arguments, reason = "OAuth callback state bundle")]
fn run_oauth_callback_listener(
    listener: &TcpListener,
    cancel: &GoogleOAuthCancel,
    _bot_id: &BotId,
    creds: &OAuthClientCredentials,
    redirect_uri: &str,
    verifier: &Zeroizing<String>,
    expected_state: &str,
    token_ref: &str,
    token_url: &str,
    secrets: &SecretVault,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + OAUTH_CALLBACK_TIMEOUT;
    let code = loop {
        if cancel.is_cancelled() {
            return Err("Google OAuth sign-in cancelled".to_owned());
        }
        if std::time::Instant::now() >= deadline {
            return Err("Google OAuth callback timed out".to_owned());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let Ok(target) = read_oauth_head_target(&mut stream) else {
                    write_html_page(&mut stream, 400, "Malformed OAuth callback request.").ok();
                    continue;
                };
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                if path == "/favicon.ico" {
                    stream
                        .write_all(
                            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .ok();
                    stream.flush().ok();
                    continue;
                }
                if path != "/callback" {
                    write_html_page(&mut stream, 404, "Unknown OAuth callback route.").ok();
                    continue;
                }
                let params = parse_query_params(query);
                let state = params
                    .iter()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.as_str())
                    .unwrap_or_default();
                if !constant_time_eq(state.as_bytes(), expected_state.as_bytes()) {
                    write_html_page(
                        &mut stream,
                        400,
                        "Invalid OAuth state parameter. Please restart from Krill.",
                    )
                    .ok();
                    continue;
                }
                if let Some((_, err_val)) = params.iter().find(|(k, _)| k == "error") {
                    write_html_page(
                        &mut stream,
                        400,
                        "Google sign-in declined. You can close this tab.",
                    )
                    .ok();
                    return Err(format!("Google OAuth denied: {err_val}"));
                }

                let Some(code) = params
                    .iter()
                    .find(|(k, _)| k == "code")
                    .map(|(_, v)| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
                else {
                    write_html_page(&mut stream, 400, "Missing authorization code.").ok();
                    continue;
                };

                let finish_res: Result<(), String> = (|| {
                    if cancel.is_cancelled() {
                        return Err("Google OAuth sign-in cancelled".to_owned());
                    }
                    let bundle = exchange_code_for_tokens(
                        token_url,
                        creds,
                        &code,
                        redirect_uri,
                        verifier,
                        cancel,
                    )?;
                    if cancel.is_cancelled() {
                        return Err("Google OAuth sign-in cancelled".to_owned());
                    }
                    let encoded = serde_json::to_string(&bundle)
                        .map_err(|_| "failed to serialize Google OAuth token bundle".to_owned())?;
                    if cancel.is_cancelled() {
                        return Err("Google OAuth sign-in cancelled".to_owned());
                    }
                    secrets
                        .set_ref(token_ref, Zeroizing::new(encoded))
                        .map_err(|err| {
                            format!("failed to store Google OAuth tokens: {}", err.kind())
                        })?;
                    Ok(())
                })();
                if let Err(err) = finish_res {
                    let (status_code, msg) = if cancel.is_cancelled() {
                        (
                            409,
                            "Google sign-in was cancelled or superseded. You can close this tab.",
                        )
                    } else {
                        (
                            502,
                            "Google token exchange or storage failed. Please retry from Krill.",
                        )
                    };
                    write_html_page(&mut stream, status_code, msg).ok();
                    return Err(err);
                }

                write_html_page(
                    &mut stream,
                    200,
                    "Krill connected to Google Workspace. You can close this tab.",
                )
                .ok();
                break code;
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(OAUTH_ACCEPT_POLL_SLEEP);
            }
            Err(err) => return Err(format!("OAuth callback accept error: {}", err.kind())),
        }
    };
    let _ = code;
    Ok(())
}

fn exchange_code_for_tokens(
    token_url: &str,
    creds: &OAuthClientCredentials,
    code: &str,
    redirect_uri: &str,
    verifier: &Zeroizing<String>,
    cancel: &GoogleOAuthCancel,
) -> Result<GoogleTokenBundle, String> {
    let mut pairs: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", &creds.client_id),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier.as_str()),
    ];
    if let Some(ref secret) = creds.client_secret {
        pairs.push(("client_secret", secret.as_str()));
    }
    let resp = http_client::send_interruptible(
        &HttpRequest::post_form(token_url, &pairs).with_ssrf_policy(policy_for_url(token_url)),
        Some(&cancel.cancel),
        Some(&cancel.active_socket),
    )
    .map_err(|err| format!("Google token exchange transport error: {}", err.kind()))?;
    if !resp.is_success() {
        return Err(format!(
            "Google token exchange rejected with HTTP {}",
            resp.status
        ));
    }
    let parsed: OAuthTokenResponse = resp
        .json()
        .map_err(|_| "invalid Google token exchange JSON response".to_owned())?;
    let ttl_ms = parsed.expires_in.unwrap_or(3600).saturating_mul(1_000);
    Ok(GoogleTokenBundle {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_at_ms: now_ms().saturating_add(ttl_ms),
        token_type: parsed.token_type,
    })
}

/// Loads the bot's Google access token, transparently refreshing it via `refresh_token` when expired
/// (serialized per `bot_id` to prevent concurrent single-use refresh-token races).
pub fn ensure_valid_access_token(
    bot: &BotConfig,
    secrets: &SecretVault,
    endpoints: &GoogleEndpoints,
    current_ms: u64,
) -> Result<Zeroizing<String>, BotErrorDetail> {
    let refresh_lock = bot_refresh_mutex(&bot.id);
    let _refresh_guard = lock(&refresh_lock);

    let custom_token_ref = bot
        .connectors
        .google
        .token_secret_ref
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let raw = secrets
        .resolve_secret(&bot.id, custom_token_ref, "google_oauth_tokens")
        .map_err(|err| BotErrorDetail {
            category: BotErrorCategory::NotSetUp,
            agent_name: None,
            message: format!("failed reading Google token vault: {}", err.kind()),
            resets_at_ms: None,
        })?
        .ok_or_else(|| BotErrorDetail {
            category: BotErrorCategory::SignedOut,
            agent_name: None,
            message: "Google OAuth not completed yet; run StartGoogleOAuth".to_owned(),
            resets_at_ms: None,
        })?;

    let mut bundle: GoogleTokenBundle = serde_json::from_str(&raw).map_err(|_| BotErrorDetail {
        category: BotErrorCategory::SignedOut,
        agent_name: None,
        message: "corrupt Google OAuth token bundle".to_owned(),
        resets_at_ms: None,
    })?;

    if bundle.expires_at_ms > current_ms.saturating_add(TOKEN_REFRESH_MARGIN_MS) {
        return Ok(Zeroizing::new(bundle.access_token));
    }

    let refresh_token = bundle
        .refresh_token
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| BotErrorDetail {
            category: BotErrorCategory::SignedOut,
            agent_name: None,
            message: "Google access token expired and no refresh token is stored".to_owned(),
            resets_at_ms: None,
        })?;

    let creds = load_oauth_client(bot, secrets).map_err(|msg| BotErrorDetail {
        category: BotErrorCategory::NotSetUp,
        agent_name: None,
        message: msg,
        resets_at_ms: None,
    })?;

    let mut pairs: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", &refresh_token),
        ("client_id", &creds.client_id),
    ];
    if let Some(ref secret) = creds.client_secret {
        pairs.push(("client_secret", secret.as_str()));
    }

    let resp = http_client::send(
        &HttpRequest::post_form(&endpoints.token_url, &pairs)
            .with_ssrf_policy(policy_for_url(&endpoints.token_url)),
    )
    .map_err(|err| BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: format!("Google token refresh failed: {}", err.kind()),
        resets_at_ms: None,
    })?;
    if !resp.is_success() {
        return Err(BotErrorDetail {
            category: BotErrorCategory::SignedOut,
            agent_name: None,
            message: format!("Google token refresh returned HTTP {}", resp.status),
            resets_at_ms: None,
        });
    }

    let refreshed: OAuthTokenResponse = resp.json().map_err(|_| BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: "invalid Google refresh JSON response".to_owned(),
        resets_at_ms: None,
    })?;
    let ttl_ms = refreshed.expires_in.unwrap_or(3600).saturating_mul(1_000);
    bundle.access_token = refreshed.access_token;
    if refreshed.refresh_token.is_some() {
        bundle.refresh_token = refreshed.refresh_token;
    }
    bundle.expires_at_ms = current_ms.saturating_add(ttl_ms);

    let save_ref = custom_token_ref.map_or_else(
        || SecretVault::scoped_ref(&bot.id, "google_oauth_tokens"),
        ToOwned::to_owned,
    );
    if let Ok(encoded) = serde_json::to_string(&bundle) {
        secrets.set_ref(&save_ref, Zeroizing::new(encoded)).ok();
    }

    Ok(Zeroizing::new(bundle.access_token))
}

/// Polls enabled Google Workspace services (`Gmail`, `Calendar`, `Drive`) for `bot` and creates
/// deduplicated [`BotItem`]s for newly discovered events. Returns the number of newly created items.
pub fn poll_bot_google(
    bot: &BotConfig,
    secrets: &SecretVault,
    items: &BotItemsStore,
    endpoints: &GoogleEndpoints,
    current_ms: u64,
) -> Result<usize, BotErrorDetail> {
    let token = ensure_valid_access_token(bot, secrets, endpoints, current_ms)?;
    let auth_header = format!("Bearer {}", *token);
    let actor = BotItemActor::System {
        reason: "google-poller".to_owned(),
    };
    let mut created = 0usize;

    if bot.connectors.google.services.gmail {
        let gmail_base = endpoints.gmail_base_url.trim_end_matches('/');
        let url = format!("{gmail_base}/gmail/v1/users/me/messages?q=is:unread&maxResults=5");
        let resp = http_client::send(
            &HttpRequest::get(&url)
                .with_header("Authorization", &auth_header)
                .with_ssrf_policy(policy_for_url(&url)),
        )
        .map_err(|err| google_transport_err("Gmail", &err))?;
        if !resp.is_success() {
            return Err(google_http_error("Gmail", resp.status));
        }
        if let Ok(val) = resp.json::<serde_json::Value>()
            && let Some(messages) = val.get("messages").and_then(serde_json::Value::as_array)
        {
            for msg in messages {
                let Some(id) = msg.get("id").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let link =
                    BotItemLink::Url(format!("https://mail.google.com/mail/u/0/#inbox/{id}"));
                if items
                    .find_item_by_link(&bot.id, &link)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    continue;
                }
                let meta_url = format!(
                    "{gmail_base}/gmail/v1/users/me/messages/{}?format=metadata&metadataHeaders=Subject&metadataHeaders=From",
                    percent_encode(id)
                );
                let meta_json = http_client::send(
                    &HttpRequest::get(&meta_url)
                        .with_header("Authorization", &auth_header)
                        .with_ssrf_policy(policy_for_url(&meta_url)),
                )
                .ok()
                .filter(http_client::HttpResponse::is_success)
                .and_then(|r| r.json::<serde_json::Value>().ok());

                let (hdr_subject, hdr_from, meta_snippet) =
                    meta_json.as_ref().map_or((None, None, None), |m| {
                        let headers = m
                            .get("payload")
                            .and_then(|p| p.get("headers"))
                            .and_then(serde_json::Value::as_array);
                        let subj = headers.and_then(|arr| extract_email_header(arr, "Subject"));
                        let from = headers.and_then(|arr| extract_email_header(arr, "From"));
                        let snip = m
                            .get("snippet")
                            .and_then(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(ToOwned::to_owned);
                        (subj, from, snip)
                    });

                let fallback_snippet = msg
                    .get("snippet")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
                let snippet = meta_snippet.or(fallback_snippet);

                let raw_title = match (&hdr_subject, &snippet) {
                    (Some(s), _) => format!("Gmail: {}", truncate_chars(s, 120)),
                    (None, Some(sn)) => format!("Gmail: {}", truncate_chars(sn, 120)),
                    (None, None) => format!("Unread Gmail message ({id})"),
                };
                let title = crate::bots::items::clamp_item_title(&raw_title);
                let raw_desc = match (hdr_from.as_deref(), snippet.as_deref()) {
                    (Some(f), Some(sn)) => Some(format!("From: {f}\n\n{sn}")),
                    (Some(f), None) => Some(format!("From: {f}")),
                    (None, Some(sn)) => Some(sn.to_owned()),
                    (None, None) => Some(format!("Detected unread Gmail message {id}.")),
                };
                let description = raw_desc
                    .as_deref()
                    .and_then(crate::bots::items::clamp_item_description);
                if items
                    .create_item(
                        &CreateBotItemParams {
                            bot_id: bot.id.clone(),
                            title,
                            description,
                            done_means: None,
                            status: Some(BotItemStatus::Planned),
                            links: vec![link],
                            waiting_reason: None,
                        },
                        &actor,
                        current_ms,
                    )
                    .is_ok()
                {
                    created = created.saturating_add(1);
                }
            }
        }
    }

    if bot.connectors.google.services.calendar {
        let time_min = format_rfc3339_utc(current_ms);
        let url = format!(
            "{}/calendar/v3/calendars/primary/events?maxResults=5&singleEvents=true&orderBy=startTime&timeMin={}",
            endpoints.calendar_base_url.trim_end_matches('/'),
            percent_encode(&time_min)
        );
        let resp = http_client::send(
            &HttpRequest::get(&url)
                .with_header("Authorization", &auth_header)
                .with_ssrf_policy(policy_for_url(&url)),
        )
        .map_err(|err| google_transport_err("Calendar", &err))?;
        if !resp.is_success() {
            return Err(google_http_error("Calendar", resp.status));
        }
        if let Ok(val) = resp.json::<serde_json::Value>()
            && let Some(events) = val.get("items").and_then(serde_json::Value::as_array)
        {
            for ev in events {
                let Some(id) = ev.get("id").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let link_url = ev
                    .get("htmlLink")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(
                        || format!("https://calendar.google.com/calendar/event?eid={id}"),
                        ToOwned::to_owned,
                    );
                let link = BotItemLink::Url(link_url);
                if items
                    .find_item_by_link(&bot.id, &link)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    continue;
                }
                let summary = ev
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("Upcoming calendar event");
                if items
                    .create_item(
                        &CreateBotItemParams {
                            bot_id: bot.id.clone(),
                            title: crate::bots::items::clamp_item_title(&format!(
                                "Calendar: {}",
                                truncate_chars(summary.trim(), 120)
                            )),
                            description: ev
                                .get("description")
                                .and_then(serde_json::Value::as_str)
                                .and_then(crate::bots::items::clamp_item_description),
                            done_means: None,
                            status: Some(BotItemStatus::Planned),
                            links: vec![link],
                            waiting_reason: None,
                        },
                        &actor,
                        current_ms,
                    )
                    .is_ok()
                {
                    created = created.saturating_add(1);
                }
            }
        }
    }

    if bot.connectors.google.services.drive
        || bot.connectors.google.services.docs
        || bot.connectors.google.services.sheets
    {
        let url = format!(
            "{}/drive/v3/files?pageSize=5&orderBy=modifiedTime%20desc&fields=files(id,name,webViewLink,modifiedTime)",
            endpoints.drive_base_url.trim_end_matches('/')
        );
        let resp = http_client::send(
            &HttpRequest::get(&url)
                .with_header("Authorization", &auth_header)
                .with_ssrf_policy(policy_for_url(&url)),
        )
        .map_err(|err| google_transport_err("Drive", &err))?;
        if !resp.is_success() {
            return Err(google_http_error("Drive", resp.status));
        }
        if let Ok(val) = resp.json::<serde_json::Value>()
            && let Some(files) = val.get("files").and_then(serde_json::Value::as_array)
        {
            for file in files {
                let Some(id) = file.get("id").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let link_url = file
                    .get("webViewLink")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(
                        || format!("https://drive.google.com/file/d/{id}/view"),
                        ToOwned::to_owned,
                    );
                let link = BotItemLink::Url(link_url);
                if items
                    .find_item_by_link(&bot.id, &link)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    continue;
                }
                let name = file
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Drive file");
                if items
                    .create_item(
                        &CreateBotItemParams {
                            bot_id: bot.id.clone(),
                            title: crate::bots::items::clamp_item_title(&format!(
                                "Drive: {}",
                                truncate_chars(name.trim(), 120)
                            )),
                            description: None,
                            done_means: None,
                            status: Some(BotItemStatus::Planned),
                            links: vec![link],
                            waiting_reason: None,
                        },
                        &actor,
                        current_ms,
                    )
                    .is_ok()
                {
                    created = created.saturating_add(1);
                }
            }
        }
    }

    Ok(created)
}

/// Tests Google OAuth token / Workspace API connectivity for `TestConnector`.
#[must_use]
pub fn test_google(
    bot: &BotConfig,
    secrets: &SecretVault,
    endpoints: &GoogleEndpoints,
    current_ms: u64,
) -> BotConnectorTestResult {
    let token = match ensure_valid_access_token(bot, secrets, endpoints, current_ms) {
        Ok(t) => t,
        Err(detail) => {
            return BotConnectorTestResult {
                source: ExternalSource::Google,
                ok: false,
                account_label: None,
                detail: detail.message,
            };
        }
    };
    let services = bot.connectors.google.services;
    let (probe_service, probe_url) = if services.gmail {
        (
            "Gmail",
            format!(
                "{}/gmail/v1/users/me/profile",
                endpoints.gmail_base_url.trim_end_matches('/')
            ),
        )
    } else if services.calendar {
        (
            "Calendar",
            format!(
                "{}/calendar/v3/calendars/primary",
                endpoints.calendar_base_url.trim_end_matches('/')
            ),
        )
    } else if services.drive || services.docs || services.sheets {
        (
            "Drive",
            format!(
                "{}/drive/v3/about?fields=user(emailAddress,displayName)",
                endpoints.drive_base_url.trim_end_matches('/')
            ),
        )
    } else {
        (
            "Gmail",
            format!(
                "{}/gmail/v1/users/me/profile",
                endpoints.gmail_base_url.trim_end_matches('/')
            ),
        )
    };
    let auth = format!("Bearer {}", *token);
    let resp = match http_client::send(
        &HttpRequest::get(&probe_url)
            .with_header("Authorization", auth)
            .with_ssrf_policy(policy_for_url(&probe_url)),
    ) {
        Ok(r) => r,
        Err(err) => {
            return BotConnectorTestResult {
                source: ExternalSource::Google,
                ok: false,
                account_label: None,
                detail: format!("Google {probe_service} transport error: {}", err.kind()),
            };
        }
    };
    if !resp.is_success() {
        let err = google_http_error(probe_service, resp.status);
        return BotConnectorTestResult {
            source: ExternalSource::Google,
            ok: false,
            account_label: None,
            detail: err.message,
        };
    }
    let mut label = Some("Google Workspace".to_owned());
    if let Ok(val) = resp.json::<serde_json::Value>()
        && let Some(extracted) = val
            .get("emailAddress")
            .or_else(|| val.get("user").and_then(|u| u.get("emailAddress")))
            .or_else(|| val.get("summary"))
            .or_else(|| val.get("id"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        label = Some(extracted.to_owned());
    }
    BotConnectorTestResult {
        source: ExternalSource::Google,
        ok: true,
        account_label: label,
        detail: "Google Workspace OAuth token valid".to_owned(),
    }
}

/// Callback invoked whenever background Google polling creates `> 0` new items.
pub type GoogleNewItemsCallback = Arc<dyn Fn(&BotConfig, usize) + Send + Sync>;

#[derive(Default)]
struct GoogleWakeState {
    stop: bool,
    wake_now: bool,
}

struct RunningGooglePoller {
    stop: Arc<(Mutex<GoogleWakeState>, Condvar)>,
    handle: JoinHandle<()>,
}

/// Strictly-lazy 60s local Google Workspace trigger checker thread.
pub struct GoogleTriggerChecker {
    store: Arc<BotStore>,
    secrets: Arc<SecretVault>,
    items: Arc<BotItemsStore>,
    endpoints: GoogleEndpoints,
    statuses: Arc<Mutex<BTreeMap<BotId, BotConnectorStatus>>>,
    on_new_items: Arc<Mutex<Option<GoogleNewItemsCallback>>>,
    running: AtomicBool,
    reconcile_lock: Mutex<()>,
    lifecycle: Mutex<Option<RunningGooglePoller>>,
}

impl fmt::Debug for GoogleTriggerChecker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleTriggerChecker")
            .field("running", &self.running.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl Drop for GoogleTriggerChecker {
    fn drop(&mut self) {
        self.stop();
    }
}

impl GoogleTriggerChecker {
    /// Creates a lazy checker with ZERO background threads until `reconcile` sees an enabled bot.
    #[must_use]
    pub fn new(
        store: Arc<BotStore>,
        secrets: Arc<SecretVault>,
        items: Arc<BotItemsStore>,
        endpoints: GoogleEndpoints,
    ) -> Self {
        Self {
            store,
            secrets,
            items,
            endpoints,
            statuses: Arc::new(Mutex::new(BTreeMap::new())),
            on_new_items: Arc::new(Mutex::new(None)),
            running: AtomicBool::new(false),
            reconcile_lock: Mutex::new(()),
            lifecycle: Mutex::new(None),
        }
    }

    /// Registers a callback invoked whenever background Google polling creates new [`BotItem`]s.
    pub fn set_on_new_items(&self, cb: GoogleNewItemsCallback) {
        *lock(&self.on_new_items) = Some(cb);
    }

    /// Starts the poller thread if ≥1 bot has `google.enabled`, wakes immediately to check
    /// enabled bots (honoring each bot's `min_interval`), or stops only the poller when none do
    /// (without cancelling any in-flight OAuth sign-in flow: MB-11).
    pub fn reconcile(&self) {
        let _reconcile = lock(&self.reconcile_lock);
        let bots = self.store.list_bots().unwrap_or_default();
        let active_ids: std::collections::BTreeSet<BotId> = bots
            .iter()
            .filter(|b| !b.paused && b.connectors.google.enabled)
            .map(|b| b.id.clone())
            .collect();
        if active_ids.is_empty() {
            self.stop_poller_locked();
            // Prune AFTER joining the poller thread so an in-flight poll cannot resurrect stale entries.
            lock(&self.statuses).retain(|bid, _| active_ids.contains(bid));
        } else {
            lock(&self.statuses).retain(|bid, _| active_ids.contains(bid));
            let mut guard = lock(&self.lifecycle);
            if let Some(ref running) = *guard {
                let mut st = lock(&running.stop.0);
                st.wake_now = true;
                running.stop.1.notify_all();
            } else {
                self.spawn_locked(&mut guard);
            }
        }
    }

    /// Runs an immediate sync for `bot` and records its updated [`BotConnectorStatus`].
    pub fn sync_bot_now(&self, bot: &BotConfig, current_ms: u64) -> BotConnectorStatus {
        let status =
            match poll_bot_google(bot, &self.secrets, &self.items, &self.endpoints, current_ms) {
                Ok(_) => BotConnectorStatus {
                    bot_id: bot.id.clone(),
                    source: ExternalSource::Google,
                    enabled: bot.connectors.google.enabled,
                    connected: true,
                    state: ConnectorHealthState::Connected,
                    account_label: Some("Google Workspace".to_owned()),
                    last_sync_ms: Some(current_ms),
                    last_error: None,
                    error_category: None,
                },
                Err(detail) => BotConnectorStatus {
                    bot_id: bot.id.clone(),
                    source: ExternalSource::Google,
                    enabled: bot.connectors.google.enabled,
                    connected: false,
                    state: ConnectorHealthState::Error,
                    account_label: None,
                    last_sync_ms: None,
                    last_error: Some(detail.message),
                    error_category: Some(detail.category),
                },
            };
        self.record_status(status.clone());
        status
    }

    /// Updates a stored status (e.g., on OAuth callback completion).
    pub fn record_status(&self, status: BotConnectorStatus) {
        lock(&self.statuses).insert(status.bot_id.clone(), status);
    }

    /// Returns tracked Google connector statuses, optionally filtered by `bot_id`.
    #[must_use]
    pub fn statuses(&self, bot_id: Option<&BotId>) -> Vec<BotConnectorStatus> {
        lock(&self.statuses)
            .values()
            .filter(|s| bot_id.is_none_or(|id| &s.bot_id == id))
            .cloned()
            .collect()
    }

    /// Returns a cloned copy of the configured endpoints.
    #[must_use]
    pub fn endpoints(&self) -> GoogleEndpoints {
        self.endpoints.clone()
    }

    /// Returns whether the background poller thread is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn spawn_locked(&self, lifecycle_guard: &mut Option<RunningGooglePoller>) {
        if lifecycle_guard.is_some() {
            return;
        }
        // Fresh per-generation stop/condvar state prevents stop/restart races (MB-17).
        let stop = Arc::new((
            Mutex::new(GoogleWakeState {
                stop: false,
                wake_now: true,
            }),
            Condvar::new(),
        ));
        let store = Arc::clone(&self.store);
        let secrets = Arc::clone(&self.secrets);
        let items = Arc::clone(&self.items);
        let endpoints = self.endpoints.clone();
        let statuses = Arc::clone(&self.statuses);
        let on_new_items = Arc::clone(&self.on_new_items);
        let stop_bg = Arc::clone(&stop);

        if let Ok(handle) = thread::Builder::new()
            .name("bot-google-poll".to_owned())
            .stack_size(STACK_SIZE)
            .spawn(move || {
                let mut last_polled: BTreeMap<BotId, u64> = BTreeMap::new();
                loop {
                    let min_sleep_secs = store
                        .list_bots()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|b| !b.paused && b.connectors.google.enabled)
                        .map(|b| b.connectors.google.poll_interval_secs.max(15))
                        .min()
                        .unwrap_or(DEFAULT_POLL_SECS);
                    let wait_res = {
                        let mut guard = lock(&stop_bg.0);
                        if guard.stop {
                            break;
                        }
                        if guard.wake_now {
                            guard.wake_now = false;
                            false
                        } else {
                            let (mut guard, _) = stop_bg
                                .1
                                .wait_timeout_while(
                                    guard,
                                    Duration::from_secs(min_sleep_secs),
                                    |s| !s.stop && !s.wake_now,
                                )
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            guard.wake_now = false;
                            guard.stop
                        }
                    };
                    if wait_res {
                        break;
                    }
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let ts = now_ms();
                        let bots = store.list_bots().unwrap_or_default();
                        for bot in bots {
                            if lock(&stop_bg.0).stop {
                                break;
                            }
                            if bot.paused || !bot.connectors.google.enabled {
                                continue;
                            }
                            let interval_ms = bot
                                .connectors
                                .google
                                .poll_interval_secs
                                .max(15)
                                .saturating_mul(1_000);
                            if last_polled
                                .get(&bot.id)
                                .is_some_and(|&prev| ts.saturating_sub(prev) < interval_ms)
                            {
                                continue;
                            }
                            last_polled.insert(bot.id.clone(), ts);
                            let res = poll_bot_google(&bot, &secrets, &items, &endpoints, ts);
                            let still_active = !lock(&stop_bg.0).stop
                                && store.get_bot(&bot.id).ok().flatten().is_some_and(|live| {
                                    !live.paused && live.connectors.google.enabled
                                });
                            if !still_active {
                                lock(&statuses).remove(&bot.id);
                                continue;
                            }
                            let st = match res {
                                Ok(new_count) => {
                                    if new_count > 0
                                        && let Some(cb) = lock(&on_new_items).as_ref().cloned()
                                    {
                                        cb(&bot, new_count);
                                    }
                                    BotConnectorStatus {
                                        bot_id: bot.id.clone(),
                                        source: ExternalSource::Google,
                                        enabled: true,
                                        connected: true,
                                        state: ConnectorHealthState::Connected,
                                        account_label: Some("Google Workspace".to_owned()),
                                        last_sync_ms: Some(ts),
                                        last_error: None,
                                        error_category: None,
                                    }
                                }
                                Err(detail) => BotConnectorStatus {
                                    bot_id: bot.id.clone(),
                                    source: ExternalSource::Google,
                                    enabled: true,
                                    connected: false,
                                    state: ConnectorHealthState::Error,
                                    account_label: None,
                                    last_sync_ms: None,
                                    last_error: Some(detail.message),
                                    error_category: Some(detail.category),
                                },
                            };
                            lock(&statuses).insert(bot.id.clone(), st);
                        }
                    }))
                    .ok();
                }
            })
        {
            self.running.store(true, Ordering::Release);
            *lifecycle_guard = Some(RunningGooglePoller { stop, handle });
        }
    }

    /// Stops ONLY the background poller thread without cancelling active OAuth sign-in flows.
    pub fn stop_poller(&self) {
        let _reconcile = lock(&self.reconcile_lock);
        self.stop_poller_locked();
    }

    fn stop_poller_locked(&self) {
        let running = {
            let mut guard = lock(&self.lifecycle);
            let taken = guard.take();
            if let Some(ref r) = taken {
                lock(&r.stop.0).stop = true;
                r.stop.1.notify_all();
            }
            self.running.store(false, Ordering::Release);
            taken
        };
        if let Some(r) = running {
            r.handle.join().ok();
        }
    }

    /// Stops both active OAuth sign-in listeners and the background poller thread cleanly.
    pub fn stop(&self) {
        cancel_all_google_oauth();
        self.stop_poller();
    }
}

fn extract_email_header(headers: &[serde_json::Value], wanted: &str) -> Option<String> {
    for h in headers {
        if let Some(name) = h.get("name").and_then(serde_json::Value::as_str)
            && name.eq_ignore_ascii_case(wanted)
            && let Some(val) = h.get("value").and_then(serde_json::Value::as_str)
        {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

fn format_rfc3339_utc(epoch_ms: u64) -> String {
    let total_secs = epoch_ms / 1_000;
    let secs_of_day = total_secs % 86_400;
    let days_since_epoch = i64::try_from(total_secs / 86_400).unwrap_or(0);
    // Howard Hinnant civil-from-days conversion (1970-01-01 = day 0).
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    let hour = secs_of_day / 3_600;
    let min = (secs_of_day % 3_600) / 60;
    let sec = secs_of_day % 60;
    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}

fn load_oauth_client(
    bot: &BotConfig,
    secrets: &SecretVault,
) -> Result<OAuthClientCredentials, String> {
    let raw = secrets
        .resolve_secret(
            &bot.id,
            bot.connectors.google.oauth_client_secret_ref.as_deref(),
            "google_oauth_client",
        )
        .map_err(|err| format!("failed to read Google OAuth JSON: {}", err.kind()))?
        .ok_or_else(|| {
            "missing Google Desktop OAuth JSON in secret vault (set secret before starting OAuth)"
                .to_owned()
        })?;
    let val: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|_| "invalid Google Desktop OAuth JSON format".to_owned())?;
    let obj = val
        .get("installed")
        .or_else(|| val.get("web"))
        .unwrap_or(&val);
    let client_id = obj
        .get("client_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Google OAuth JSON missing client_id".to_owned())?
        .to_owned();
    let client_secret = obj
        .get("client_secret")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Zeroizing::new(s.to_owned()));
    Ok(OAuthClientCredentials {
        client_id,
        client_secret,
    })
}

fn build_scopes(services: GoogleServicesConfig) -> Vec<&'static str> {
    let mut scopes = Vec::new();
    if services.gmail {
        scopes.push("https://www.googleapis.com/auth/gmail.readonly");
    }
    if services.calendar {
        scopes.push("https://www.googleapis.com/auth/calendar.readonly");
    }
    if services.drive || services.docs || services.sheets {
        scopes.push("https://www.googleapis.com/auth/drive.readonly");
    }
    if services.contacts {
        scopes.push("https://www.googleapis.com/auth/contacts.readonly");
    }
    if scopes.is_empty() {
        scopes.push("https://www.googleapis.com/auth/gmail.readonly");
        scopes.push("https://www.googleapis.com/auth/calendar.readonly");
    }
    scopes
}

fn generate_pkce_verifier() -> io::Result<Zeroizing<String>> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    SystemRandom::new()
        .fill(bytes.as_mut())
        .map_err(|_| io::Error::other("PKCE RNG failure"))?;
    Ok(Zeroizing::new(base64url_encode(bytes.as_ref())))
}

fn pkce_s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url_encode(&digest)
}

fn generate_random_hex(len: usize) -> io::Result<String> {
    let mut buf = vec![0u8; len];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| io::Error::other("RNG failure"))?;
    Ok(crate::mobile::devices::hex(&buf))
}

fn write_html_page(stream: &mut std::net::TcpStream, status: u16, msg: &str) -> io::Result<()> {
    let body = format!(
        "<!doctype html><html><body style=\"font-family:sans-serif;padding:24px\"><h3>{msg}</h3></body></html>"
    );
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        502 => "Bad Gateway",
        _ => "Bad Request",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()
}

fn google_http_error(service: &str, status: u16) -> BotErrorDetail {
    if status == 401 || status == 403 {
        return BotErrorDetail {
            category: BotErrorCategory::SignedOut,
            agent_name: None,
            message: format!("{service} rejected credentials (HTTP {status})"),
            resets_at_ms: None,
        };
    }
    if status == 429 {
        return BotErrorDetail {
            category: BotErrorCategory::UsageLimit,
            agent_name: None,
            message: format!("{service} rate limited (HTTP 429)"),
            resets_at_ms: None,
        };
    }
    BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: format!("{service} API returned HTTP {status}"),
        resets_at_ms: None,
    }
}

fn google_transport_err(service: &str, err: &io::Error) -> BotErrorDetail {
    BotErrorDetail {
        category: BotErrorCategory::AgentError,
        agent_name: None,
        message: format!("Google {service} poll failed: {}", err.kind()),
        resets_at_ms: None,
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push_str("...");
    }
    out
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
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
            "krilld-bot-google-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn google_oauth_pkce_callback_token_refresh_and_poller_work_on_loopback() {
        let dir = temp_dir("oauth");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x77; 32]));
        let items = BotItemsStore::new(dir.join("bot-items.db"));

        // Mock Google token + Gmail/Calendar/Drive API server.
        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        mock.set_nonblocking(true).unwrap();
        let mock_addr = mock.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let mock_thread = thread::spawn(move || {
            while !stop_bg.load(Ordering::Acquire) {
                let (mut sock, _) = match mock.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
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
                let req = String::from_utf8_lossy(buf.get(..n).unwrap_or_default());
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let body = if target.starts_with("/token") {
                    r#"{"access_token":"ya29.fresh","refresh_token":"1//ref","expires_in":3600,"token_type":"Bearer"}"#
                } else if target.starts_with("/gmail/") {
                    r#"{"messages":[{"id":"m1","snippet":"Deploy signoff needed"}]}"#
                } else if target.starts_with("/calendar/") {
                    r#"{"items":[{"id":"ev1","summary":"Release sync","htmlLink":"https://calendar.google.com/ev1"}]}"#
                } else {
                    r#"{"files":[{"id":"f1","name":"Runbook.md","webViewLink":"https://drive.google.com/f1"}]}"#
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                sock.write_all(resp.as_bytes()).unwrap();
            }
        });

        let bot_id = BotId::new("bot-google").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "Google Bot", "claude", 1_000).unwrap();
        bot.connectors.google.enabled = true;
        bot.connectors.google.services = GoogleServicesConfig {
            gmail: true,
            calendar: true,
            drive: true,
            docs: false,
            sheets: false,
            contacts: false,
        };

        secrets
            .set_secret(
                &bot_id,
                "google_oauth_client",
                Zeroizing::new(
                    r#"{"installed":{"client_id":"cid-123","client_secret":"csec-456"}}"#
                        .to_owned(),
                ),
            )
            .unwrap();

        let endpoints = GoogleEndpoints {
            auth_base_url: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
            token_url: format!("http://{mock_addr}/token"),
            gmail_base_url: format!("http://{mock_addr}"),
            calendar_base_url: format!("http://{mock_addr}"),
            drive_base_url: format!("http://{mock_addr}"),
        };

        let done_flag = Arc::new(AtomicBool::new(false));
        let done_cb = Arc::clone(&done_flag);
        let (auth_url, state) = start_google_oauth(
            &bot,
            Arc::clone(&secrets),
            endpoints.clone(),
            Some(Arc::new(move |st| {
                if st.connected {
                    done_cb.store(true, Ordering::Release);
                }
            })),
        )
        .unwrap();
        assert!(auth_url.contains("code_challenge_method=S256"));

        // Extract loopback redirect_uri from auth_url and simulate browser callback.
        let query = auth_url.split_once('?').unwrap().1;
        let params = parse_query_params(query);
        let redirect_uri = params
            .iter()
            .find(|(k, _)| k == "redirect_uri")
            .unwrap()
            .1
            .clone();
        let cb_resp = http_client::send(
            &HttpRequest::get(format!("{redirect_uri}?code=auth-code-99&state={state}"))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(cb_resp.status, 200);

        let wait_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done_flag.load(Ordering::Acquire) && std::time::Instant::now() < wait_deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(done_flag.load(Ordering::Acquire));

        // Poll Gmail + Calendar + Drive using stored OAuth token.
        let created = poll_bot_google(&bot, &secrets, &items, &endpoints, now_ms()).unwrap();
        assert_eq!(created, 3);
        assert_eq!(
            items
                .list_items(Some(&bot_id), None, true, true, now_ms())
                .unwrap()
                .len(),
            3
        );

        let test_res = test_google(&bot, &secrets, &endpoints, now_ms());
        assert!(test_res.ok);

        stop.store(true, Ordering::Release);
        mock_thread.join().unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn google_gmail_metadata_time_min_signed_out_calendar_and_favicon_skip_work() {
        let dir = temp_dir("meta");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x66; 32]));
        let items = BotItemsStore::new(dir.join("items.db"));

        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = mock.local_addr().unwrap();
        let cal_target = Arc::new(Mutex::new(String::new()));
        let cal_bg = Arc::clone(&cal_target);

        let srv = thread::spawn(move || {
            for idx in 0..4 {
                let (mut sock, _) = mock.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(buf.get(..n).unwrap_or_default());
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (status, body) = if target.contains("/messages?q=") {
                    ("200 OK", r#"{"messages":[{"id":"m-99"}]}"#.to_owned())
                } else if target.contains("/messages/m-99?format=metadata") {
                    (
                        "200 OK",
                        r#"{"id":"m-99","snippet":"Please review Q4","payload":{"headers":[{"name":"Subject","value":"Q4 Budget"},{"name":"From","value":"ceo@example.com"}]}}"#
                            .to_owned(),
                    )
                } else if target.contains("/calendar/") {
                    *lock(&cal_bg) = target.to_owned();
                    if idx >= 3 {
                        (
                            "401 Unauthorized",
                            r#"{"error":"invalid_token"}"#.to_owned(),
                        )
                    } else {
                        ("200 OK", r#"{"items":[]}"#.to_owned())
                    }
                } else {
                    (
                        "401 Unauthorized",
                        r#"{"error":"invalid_token"}"#.to_owned(),
                    )
                };
                let wire = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
            }
        });

        let bot_id = BotId::new("g-meta").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "GMeta", "claude", 1_000).unwrap();
        bot.connectors.google.enabled = true;
        bot.connectors.google.services.gmail = true;
        bot.connectors.google.services.calendar = true;
        secrets
            .set_secret(
                &bot_id,
                "google_oauth_tokens",
                Zeroizing::new(
                    serde_json::json!({
                        "access_token": "ya29.valid",
                        "expires_at_ms": now_ms().saturating_add(3_600_000),
                    })
                    .to_string(),
                ),
            )
            .unwrap();
        let endpoints = GoogleEndpoints {
            auth_base_url: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
            token_url: format!("http://{addr}/token"),
            gmail_base_url: format!("http://{addr}"),
            calendar_base_url: format!("http://{addr}"),
            drive_base_url: format!("http://{addr}"),
        };

        // Call #1-3: Gmail list, Gmail metadata, Calendar 200 (with timeMin).
        assert_eq!(
            poll_bot_google(&bot, &secrets, &items, &endpoints, 1_700_000_000_000).unwrap(),
            1
        );
        let listed = items
            .list_items(Some(&bot_id), None, true, true, now_ms())
            .unwrap();
        assert_eq!(listed[0].title, "Gmail: Q4 Budget");
        assert!(
            listed[0]
                .description
                .as_deref()
                .unwrap()
                .contains("ceo@example.com")
        );
        assert!(lock(&cal_target).contains("timeMin="));

        // Calendar 401 -> BotErrorCategory::SignedOut!
        bot.connectors.google.services.gmail = false;
        let err = poll_bot_google(&bot, &secrets, &items, &endpoints, now_ms()).unwrap_err();
        assert_eq!(err.category, BotErrorCategory::SignedOut);

        srv.join().unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn google_test_and_poll_fail_on_transport_429_5xx_and_oauth_ignores_bad_state_and_cancels_prior()
     {
        let dir = temp_dir("hardening");
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x88; 32]));
        let items = BotItemsStore::new(dir.join("items.db"));

        let bot_id = BotId::new("g-hard").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "GHard", "claude", 1_000).unwrap();
        bot.connectors.google.enabled = true;
        bot.connectors.google.services = GoogleServicesConfig {
            gmail: true,
            calendar: false,
            drive: false,
            docs: false,
            sheets: false,
            contacts: false,
        };
        secrets
            .set_secret(
                &bot_id,
                "google_oauth_tokens",
                Zeroizing::new(
                    serde_json::json!({
                        "access_token": "ya29.ok",
                        "expires_at_ms": now_ms().saturating_add(3_600_000),
                    })
                    .to_string(),
                ),
            )
            .unwrap();
        secrets
            .set_secret(
                &bot_id,
                "google_oauth_client",
                Zeroizing::new(
                    r#"{"installed":{"client_id":"cid","client_secret":"csec"}}"#.to_owned(),
                ),
            )
            .unwrap();

        // 1. Transport error in test_google must return ok: false.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_addr = closed.local_addr().unwrap();
        drop(closed);
        let dead_ep = GoogleEndpoints {
            auth_base_url: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
            token_url: format!("http://{dead_addr}/token"),
            gmail_base_url: format!("http://{dead_addr}"),
            calendar_base_url: format!("http://{dead_addr}"),
            drive_base_url: format!("http://{dead_addr}"),
        };
        assert!(!test_google(&bot, &secrets, &dead_ep, now_ms()).ok);

        // 2. HTTP 429 and 500 must fail in both test_google and poll_bot_google.
        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = mock.local_addr().unwrap();
        let srv = thread::spawn(move || {
            let responses = [
                ("429 Too Many Requests", r#"{"error":"rate_limit"}"#),
                ("500 Internal Server Error", r#"{"error":"boom"}"#),
                ("429 Too Many Requests", r#"{"error":"rate_limit"}"#),
                ("503 Service Unavailable", r#"{"error":"down"}"#),
                (
                    "200 OK",
                    r#"{"access_token":"ya29.new","refresh_token":"1//r","expires_in":3600}"#,
                ),
            ];
            for (status, body) in responses {
                let (mut sock, _) = mock.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                assert!(n > 0);
                let wire = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
            }
        });
        let ep = GoogleEndpoints {
            auth_base_url: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
            token_url: format!("http://{addr}/token"),
            gmail_base_url: format!("http://{addr}"),
            calendar_base_url: format!("http://{addr}"),
            drive_base_url: format!("http://{addr}"),
        };

        assert!(!test_google(&bot, &secrets, &ep, now_ms()).ok);
        assert!(!test_google(&bot, &secrets, &ep, now_ms()).ok);
        assert_eq!(
            poll_bot_google(&bot, &secrets, &items, &ep, now_ms())
                .unwrap_err()
                .category,
            BotErrorCategory::UsageLimit
        );
        assert_eq!(
            poll_bot_google(&bot, &secrets, &items, &ep, now_ms())
                .unwrap_err()
                .category,
            BotErrorCategory::AgentError
        );

        // 3. Starting OAuth twice cancels the first listener, and the active listener ignores
        //    spurious `?error=` / bad `state` / missing `code` before succeeding on valid callback.
        let first_cb_called = Arc::new(AtomicBool::new(false));
        let first_cb_flag = Arc::clone(&first_cb_called);
        let (first_url, _) = start_google_oauth(
            &bot,
            Arc::clone(&secrets),
            ep.clone(),
            Some(Arc::new(move |_| {
                first_cb_flag.store(true, Ordering::Release);
            })),
        )
        .unwrap();
        let second_done = Arc::new(AtomicBool::new(false));
        let second_done_cb = Arc::clone(&second_done);
        let (second_url, state2) = start_google_oauth(
            &bot,
            Arc::clone(&secrets),
            ep.clone(),
            Some(Arc::new(move |st| {
                if st.connected {
                    second_done_cb.store(true, Ordering::Release);
                }
            })),
        )
        .unwrap();
        assert_ne!(first_url, second_url);

        let redirect2 = parse_query_params(second_url.split_once('?').unwrap().1)
            .into_iter()
            .find(|(k, _)| k == "redirect_uri")
            .unwrap()
            .1;
        // Spurious error with wrong state -> 400, listener stays alive.
        let r_err = http_client::send(
            &HttpRequest::get(format!("{redirect2}?error=access_denied&state=wrong"))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(r_err.status, 400);
        // Missing code with valid state -> 400, listener stays alive.
        let r_nocode = http_client::send(
            &HttpRequest::get(format!("{redirect2}?state={state2}"))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(r_nocode.status, 400);
        // Legitimate callback -> 200 and connects!
        let r_ok = http_client::send(
            &HttpRequest::get(format!("{redirect2}?code=good&state={state2}"))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(r_ok.status, 200);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !second_done.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(second_done.load(Ordering::Acquire));
        assert!(!first_cb_called.load(Ordering::Acquire));

        srv.join().unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn google_reconcile_preserves_oauth_prunes_statuses_and_handles_empty_token_ref() {
        let dir = temp_dir("mb11-17-39-41-43");
        let store = Arc::new(BotStore::new(dir.clone()));
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x88; 32]));
        let items = Arc::new(BotItemsStore::new(dir.join("items.db")));

        let mut bot = store
            .create_bot(
                krill_proto::bots::CreateBotParams {
                    name: "Google OAuth Bot".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: None,
                },
                1_000,
            )
            .unwrap();
        // Empty/whitespace token_secret_ref must normalize to default scoped secret ref (MB-43).
        bot.connectors.google.token_secret_ref = Some("   ".to_owned());
        bot.connectors.google.enabled = false;
        bot.connectors.google.services.gmail = true;
        store
            .set_connector_config(&bot.id, bot.connectors.clone(), 1_100)
            .unwrap();
        secrets
            .set_secret(
                &bot.id,
                "google_oauth_client",
                Zeroizing::new(
                    r#"{"installed":{"client_id":"cid-mb11","client_secret":"csec-mb11"}}"#
                        .to_owned(),
                ),
            )
            .unwrap();

        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = mock.local_addr().unwrap();
        let long_cjk_subj = "긴한글이메일제목테스트".repeat(15); // 495 bytes
        let srv = thread::spawn(move || {
            for idx in 0..3 {
                let (mut sock, _) = mock.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                assert!(n > 0);
                let body = match idx {
                    0 => r#"{"access_token":"tok-mb11","refresh_token":"ref-mb11","expires_in":3600}"#
                        .to_owned(),
                    1 => r#"{"messages":[{"id":"m-cjk-1"}]}"#.to_owned(),
                    _ => serde_json::json!({
                        "snippet": "스니펫",
                        "payload": {
                            "headers": [
                                {"name": "Subject", "value": long_cjk_subj},
                                {"name": "From", "value": "alice@example.com"}
                            ]
                        }
                    })
                    .to_string(),
                };
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
            }
        });
        let ep = GoogleEndpoints {
            auth_base_url: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
            token_url: format!("http://{addr}/token"),
            gmail_base_url: format!("http://{addr}"),
            calendar_base_url: format!("http://{addr}"),
            drive_base_url: format!("http://{addr}"),
        };

        let checker = GoogleTriggerChecker::new(
            Arc::clone(&store),
            Arc::clone(&secrets),
            Arc::clone(&items),
            ep.clone(),
        );
        // Seed a stale status entry for a disabled bot -> reconcile must prune it (MB-41).
        checker.record_status(BotConnectorStatus {
            bot_id: bot.id.clone(),
            source: ExternalSource::Google,
            enabled: true,
            connected: true,
            state: ConnectorHealthState::Connected,
            account_label: None,
            last_sync_ms: Some(1_000),
            last_error: None,
            error_category: None,
        });

        let connected = Arc::new(AtomicBool::new(false));
        let connected_bg = Arc::clone(&connected);
        let (auth_url, state) = start_google_oauth(
            &bot,
            Arc::clone(&secrets),
            ep.clone(),
            Some(Arc::new(move |st| {
                if st.connected {
                    connected_bg.store(true, Ordering::Release);
                }
            })),
        )
        .unwrap();

        // MB-11: calling `reconcile()` while zero bots have `google.enabled` must NOT cancel the
        // in-flight OAuth listener!
        checker.reconcile();
        assert!(
            checker.statuses(None).is_empty(),
            "stale disabled status must be pruned"
        );
        assert!(!checker.is_running());

        let redirect_uri = parse_query_params(auth_url.split_once('?').unwrap().1)
            .into_iter()
            .find(|(k, _)| k == "redirect_uri")
            .unwrap()
            .1;
        let cb = http_client::send(
            &HttpRequest::get(format!("{redirect_uri}?code=ok-code&state={state}"))
                .with_ssrf_policy(SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(cb.status, 200);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !connected.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            connected.load(Ordering::Acquire),
            "reconcile() must not cancel in-flight Google OAuth listener"
        );

        // MB-43 + MB-39: token was saved under default scoped ref despite whitespace `token_secret_ref`,
        // AND Gmail item with >200B CJK subject clamps cleanly instead of failing validation.
        let created = poll_bot_google(&bot, &secrets, &items, &ep, now_ms()).unwrap();
        assert_eq!(created, 1);

        srv.join().unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn r2m10_r2m11_google_poller_no_resurrect_and_oauth_cancel_blocks_persist() {
        let dir = temp_dir("r2m10-11-google");
        let store = Arc::new(BotStore::new(dir.clone()));
        let secrets = Arc::new(SecretVault::with_static_key(dir.clone(), [0x47; 32]));
        let items = Arc::new(BotItemsStore::new(dir.join("items.db")));
        let bot = store
            .create_bot(
                krill_proto::bots::CreateBotParams {
                    name: "Google R2".to_owned(),
                    agent_id: "claude".to_owned(),
                    description: String::new(),
                    emoji: None,
                    avatar: None,
                    model: None,
                    effort: None,
                    permission_mode: None,
                    workspace: None,
                    template_id: None,
                    initial_soul: None,
                },
                1_000,
            )
            .unwrap();
        secrets
            .set_secret(
                &bot.id,
                "google_oauth_client",
                Zeroizing::new(
                    r#"{"installed":{"client_id":"cid-r2","client_secret":"csec-r2"}}"#.to_owned(),
                ),
            )
            .unwrap();

        // R2M-11: Cancelling Google OAuth during in-flight token exchange must NOT persist tokens.
        let tok_hit = Arc::new(AtomicBool::new(false));
        let tok_hit_bg = Arc::clone(&tok_hit);
        let release_tok = Arc::new(AtomicBool::new(false));
        let release_tok_bg = Arc::clone(&release_tok);
        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = mock.local_addr().unwrap();
        let srv = thread::spawn(move || {
            let (mut sock, _) = mock.accept().unwrap();
            let mut buf = [0u8; 4096];
            assert!(sock.read(&mut buf).unwrap_or(0) > 0);
            tok_hit_bg.store(true, Ordering::Release);
            let until = std::time::Instant::now() + Duration::from_secs(3);
            while !release_tok_bg.load(Ordering::Acquire) && std::time::Instant::now() < until {
                thread::sleep(Duration::from_millis(10));
            }
            let body =
                r#"{"access_token":"leaked-google","refresh_token":"ref","expires_in":3600}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(wire.as_bytes()).ok();
        });
        let ep = GoogleEndpoints {
            auth_base_url: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
            token_url: format!("http://{addr}/token"),
            gmail_base_url: format!("http://{addr}"),
            calendar_base_url: format!("http://{addr}"),
            drive_base_url: format!("http://{addr}"),
        };
        let (auth_url, state) =
            start_google_oauth(&bot, Arc::clone(&secrets), ep.clone(), None).unwrap();
        let redirect_uri = parse_query_params(auth_url.split_once('?').unwrap().1)
            .into_iter()
            .find(|(k, _)| k == "redirect_uri")
            .unwrap()
            .1;
        let cb_handle = thread::spawn(move || {
            http_client::send(
                &HttpRequest::get(format!("{redirect_uri}?code=c1&state={state}"))
                    .with_ssrf_policy(SsrfPolicy::AllowLoopback),
            )
            .ok();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !tok_hit.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(tok_hit.load(Ordering::Acquire));
        cancel_google_oauth(&bot.id);
        release_tok.store(true, Ordering::Release);
        cb_handle.join().ok();
        thread::sleep(Duration::from_millis(60));
        assert!(
            secrets
                .get_secret(&bot.id, "google_oauth_tokens")
                .unwrap()
                .is_none(),
            "cancelled Google OAuth flow must NOT persist tokens"
        );
        srv.join().ok();

        // R2M-10: Disabling Google connector during an in-flight poll must NOT resurrect status.
        let poll_hit = Arc::new(AtomicBool::new(false));
        let poll_hit_bg = Arc::clone(&poll_hit);
        let release_poll = Arc::new(AtomicBool::new(false));
        let release_poll_bg = Arc::clone(&release_poll);
        let mock2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr2 = mock2.local_addr().unwrap();
        let srv2 = thread::spawn(move || {
            let (mut sock, _) = mock2.accept().unwrap();
            let mut buf = [0u8; 4096];
            assert!(sock.read(&mut buf).unwrap_or(0) > 0);
            poll_hit_bg.store(true, Ordering::Release);
            let until = std::time::Instant::now() + Duration::from_secs(3);
            while !release_poll_bg.load(Ordering::Acquire) && std::time::Instant::now() < until {
                thread::sleep(Duration::from_millis(10));
            }
            let body = r#"{"messages":[]}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(wire.as_bytes()).ok();
        });
        let valid_bundle = serde_json::json!({
            "access_token": "tok-live",
            "expires_at_ms": now_ms() + 3_600_000,
        });
        secrets
            .set_secret(
                &bot.id,
                "google_oauth_tokens",
                Zeroizing::new(valid_bundle.to_string()),
            )
            .unwrap();
        let mut conn = bot.connectors.clone();
        conn.google.enabled = true;
        conn.google.services.gmail = true;
        conn.google.services.calendar = false;
        conn.google.services.drive = false;
        store
            .set_connector_config(&bot.id, conn.clone(), 2_000)
            .unwrap();

        let ep2 = GoogleEndpoints {
            auth_base_url: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
            token_url: format!("http://{addr2}/token"),
            gmail_base_url: format!("http://{addr2}"),
            calendar_base_url: format!("http://{addr2}"),
            drive_base_url: format!("http://{addr2}"),
        };
        let checker = GoogleTriggerChecker::new(Arc::clone(&store), secrets, items, ep2);
        checker.reconcile();
        let dl2 = std::time::Instant::now() + Duration::from_secs(3);
        while !poll_hit.load(Ordering::Acquire) && std::time::Instant::now() < dl2 {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(poll_hit.load(Ordering::Acquire));
        conn.google.enabled = false;
        store.set_connector_config(&bot.id, conn, 2_100).unwrap();
        release_poll.store(true, Ordering::Release);
        checker.reconcile();
        assert!(
            checker.statuses(None).is_empty(),
            "disabled Google bot status must NOT resurrect after in-flight poll finishes"
        );

        srv2.join().ok();
        fs::remove_dir_all(&dir).ok();
    }
}
