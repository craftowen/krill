// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! 100% local Slack Socket Mode (`wss://`) + Web API connector.
//!
//! Opens an outbound WebSocket via `POST /apps.connections.open` using the app-level token
//! (`xapp-`), immediately ACKs every Socket Mode envelope (`{"envelope_id": ...}`), suppresses
//! bot self-echoes, routes `events_api` (`app_mention`, `message` DMs/threads) and `interactive`
//! (`block_actions` gate approvals), and sends outbound Web API calls (`chat.postMessage`,
//! `chat.update`, `reactions.add`) with the bot token (`xoxb-`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krill_proto::bots::{
    BotConfig, BotConnectorStatus, BotConnectorTestResult, BotErrorCategory, BotId,
    ConnectorHealthState, ExternalSource,
};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::bots::connectors::policy_for_api_base;
use crate::bots::connectors::router::{
    ConversationRouter, DeliveryTarget, InboundRouteOutcome, PlatformSender, classify_error,
    now_ms, sanitize_error_for_status,
};
use crate::bots::connectors::ws_client::{WsClient, WsEvent};
use crate::bots::connectors::{ConnectorBridge, InboundExternalMessage, StopFlag};
use crate::bots::http_client::{self, HttpRequest, SsrfPolicy};
use crate::session::lock;

/// Production Slack Web API base URL.
pub const DEFAULT_SLACK_API_BASE: &str = "https://slack.com/api";
/// Safe single-message length for Slack `chat.postMessage` / `chat.update`.
pub const SLACK_MAX_MESSAGE_CHARS: usize = 3_900;
const WORKER_STACK_SIZE: usize = 128 * 1024;
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WS_POLL_TIMEOUT: Duration = Duration::from_millis(100);
const DEDUP_RING_CAPACITY: usize = 256;
const MAX_SLACK_USER_CACHE: usize = 256;
const MAX_OUTBOUND_SLACK_FILE_BYTES: usize = 25 * 1024 * 1024;

/// Slack Web API HTTP client & [`PlatformSender`] implementation.
pub struct SlackSender {
    api_base: String,
    bot_token: Zeroizing<String>,
    ssrf_policy: SsrfPolicy,
    user_cache: Mutex<(VecDeque<String>, BTreeMap<String, String>)>,
}

impl std::fmt::Debug for SlackSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackSender")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl SlackSender {
    #[must_use]
    pub fn new(api_base: &str, bot_token: Zeroizing<String>) -> Self {
        let base = api_base.trim_end_matches('/').to_owned();
        let ssrf_policy = policy_for_api_base(&base);
        Self {
            api_base: base,
            bot_token,
            ssrf_policy,
            user_cache: Mutex::new((VecDeque::new(), BTreeMap::new())),
        }
    }

    fn post_api(&self, method: &str, token: &str, payload: &Value) -> io::Result<Value> {
        let body = serde_json::to_vec(payload).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "failed to encode Slack API payload",
            )
        })?;
        let url = format!("{}/{method}", self.api_base);
        let req = HttpRequest::post_json(url, body)
            .with_header("Authorization", format!("Bearer {token}"))
            .with_timeout(API_REQUEST_TIMEOUT)
            .with_ssrf_policy(self.ssrf_policy);
        let resp = http_client::send(&req)
            .map_err(|err| io::Error::new(err.kind(), format!("Slack {method} transport error")))?;
        let parsed: Value = resp
            .json()
            .unwrap_or_else(|_| json!({"ok": false, "error": format!("HTTP {}", resp.status)}));
        if resp.is_success() && parsed.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(parsed)
        } else {
            let err_code = parsed
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("slack_api_error");
            let status = resp.status;
            Err(io::Error::other(format!(
                "HTTP {status}: {}",
                sanitize_error_for_status(err_code)
            )))
        }
    }

    fn get_api(&self, method_and_query: &str, token: &str) -> io::Result<Value> {
        let url = format!("{}/{method_and_query}", self.api_base);
        let req = HttpRequest::get(url)
            .with_header("Authorization", format!("Bearer {token}"))
            .with_timeout(API_REQUEST_TIMEOUT)
            .with_ssrf_policy(self.ssrf_policy);
        let resp = http_client::send(&req)
            .map_err(|err| io::Error::new(err.kind(), "Slack API transport error"))?;
        let parsed: Value = resp
            .json()
            .unwrap_or_else(|_| json!({"ok": false, "error": format!("HTTP {}", resp.status)}));
        if resp.is_success() && parsed.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(parsed)
        } else {
            let err_code = parsed
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("slack_api_error");
            Err(io::Error::other(format!(
                "HTTP {}: {}",
                resp.status,
                sanitize_error_for_status(err_code)
            )))
        }
    }

    /// Resolves a human-readable Slack sender display name via bounded LRU cache over `users.info`.
    #[must_use]
    pub fn resolve_sender_name(&self, user_id: &str) -> String {
        {
            let mut cache = lock(&self.user_cache);
            if let Some(cached) = cache.1.get(user_id).cloned() {
                cache.0.retain(|u| u != user_id);
                cache.0.push_back(user_id.to_owned());
                return cached;
            }
        }
        let query = format!("users.info?user={}", http_client::percent_encode(user_id));
        let resolved = self
            .get_api(&query, self.bot_token.as_str())
            .ok()
            .and_then(|v| {
                let u = v.get("user")?;
                let display = u
                    .get("profile")
                    .and_then(|p| p.get("display_name"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let real = u
                    .get("real_name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let name = u
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                display.or(real).or(name).map(ToOwned::to_owned)
            });
        if let Some(ref name) = resolved {
            let mut cache = lock(&self.user_cache);
            if !cache.1.contains_key(user_id) && cache.0.len() >= MAX_SLACK_USER_CACHE {
                if let Some(evicted) = cache.0.pop_front() {
                    cache.1.remove(&evicted);
                }
            } else {
                cache.0.retain(|u| u != user_id);
            }
            cache.0.push_back(user_id.to_owned());
            cache.1.insert(user_id.to_owned(), name.clone());
            return name.clone();
        }
        user_id.to_owned()
    }

    /// Calls `POST /apps.connections.open` using the app-level `xapp-` token to obtain a Socket Mode URL.
    pub fn open_socket_mode_url(&self, app_token: &str) -> io::Result<String> {
        let parsed = self.post_api("apps.connections.open", app_token, &json!({}))?;
        parsed
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Slack apps.connections.open omitted websocket url",
                )
            })
    }

    /// Calls `POST /auth.test` with the bot token to discover our own `user_id` and team label.
    pub fn auth_test(&self) -> io::Result<(Option<String>, Option<String>)> {
        let parsed = self.post_api("auth.test", self.bot_token.as_str(), &json!({}))?;
        let bot_user_id = parsed
            .get("user_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let team = parsed
            .get("team")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        Ok((bot_user_id, team))
    }

    /// Runs an on-demand connectivity check for `TestConnector` validating both bot (`xoxb-`) and
    /// Socket Mode app (`xapp-`) tokens.
    #[must_use]
    pub fn test_connection_with_app_token(&self, app_token: &str) -> BotConnectorTestResult {
        let team = match self.auth_test() {
            Ok((_uid, team)) => team,
            Err(err) => {
                return BotConnectorTestResult {
                    source: ExternalSource::Slack,
                    ok: false,
                    account_label: None,
                    detail: sanitize_error_for_status(&err.to_string()),
                };
            }
        };
        if let Err(err) = self.open_socket_mode_url(app_token) {
            return BotConnectorTestResult {
                source: ExternalSource::Slack,
                ok: false,
                account_label: None,
                detail: format!(
                    "Slack Socket Mode app token (xapp-) check failed: {}",
                    sanitize_error_for_status(&err.to_string())
                ),
            };
        }
        BotConnectorTestResult {
            source: ExternalSource::Slack,
            ok: true,
            account_label: team.clone(),
            detail: format!(
                "Connected to Slack ({})",
                team.unwrap_or_else(|| "auth.test ok".to_owned())
            ),
        }
    }

    /// Runs an on-demand connectivity check for `TestConnector` (bot token only).
    #[must_use]
    pub fn test_connection(&self) -> BotConnectorTestResult {
        match self.auth_test() {
            Ok((_uid, team)) => BotConnectorTestResult {
                source: ExternalSource::Slack,
                ok: true,
                account_label: team.clone(),
                detail: format!(
                    "Connected to Slack ({})",
                    team.unwrap_or_else(|| "auth.test ok".to_owned())
                ),
            },
            Err(err) => BotConnectorTestResult {
                source: ExternalSource::Slack,
                ok: false,
                account_label: None,
                detail: sanitize_error_for_status(&err.to_string()),
            },
        }
    }
}

impl PlatformSender for SlackSender {
    fn source(&self) -> ExternalSource {
        ExternalSource::Slack
    }

    fn max_chunk_chars(&self) -> usize {
        SLACK_MAX_MESSAGE_CHARS
    }

    fn send_message(&self, target: &DeliveryTarget, text: &str) -> io::Result<String> {
        let mut payload = json!({
            "channel": target.channel_id,
            "text": text,
        });
        if let Some(ref ts) = target.thread_id
            && !ts.trim().is_empty()
            && let Some(obj) = payload.as_object_mut()
        {
            obj.insert("thread_ts".to_owned(), json!(ts));
        }
        let parsed = self.post_api("chat.postMessage", self.bot_token.as_str(), &payload)?;
        let ts = parsed
            .get("ts")
            .and_then(Value::as_str)
            .unwrap_or("0")
            .to_owned();
        Ok(ts)
    }

    fn edit_message(
        &self,
        target: &DeliveryTarget,
        message_id: &str,
        text: &str,
    ) -> io::Result<()> {
        let payload = json!({
            "channel": target.channel_id,
            "ts": message_id,
            "text": text,
        });
        self.post_api("chat.update", self.bot_token.as_str(), &payload)?;
        Ok(())
    }

    fn add_reaction(
        &self,
        target: &DeliveryTarget,
        message_id: &str,
        reaction: &str,
    ) -> io::Result<()> {
        let clean = reaction.trim().trim_matches(':');
        if clean.is_empty() {
            return Ok(());
        }
        let payload = json!({
            "channel": target.channel_id,
            "timestamp": message_id,
            "name": clean,
        });
        match self.post_api("reactions.add", self.bot_token.as_str(), &payload) {
            Ok(_) => Ok(()),
            Err(err) if err.to_string().contains("already_reacted") => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn send_files(
        &self,
        target: &DeliveryTarget,
        files: &[String],
        caption: Option<&str>,
    ) -> io::Result<Option<String>> {
        let mut uploaded: Vec<Value> = Vec::with_capacity(files.len());
        for raw_path in files {
            let path = Path::new(raw_path);
            let meta = fs::symlink_metadata(path)?;
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("refusing non-regular attachment: {raw_path}"),
                ));
            }
            if usize::try_from(meta.len()).unwrap_or(usize::MAX) > MAX_OUTBOUND_SLACK_FILE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "attachment exceeds 25 MiB Slack limit",
                ));
            }
            let bytes = fs::read(path)?;
            let filename = path.file_name().map_or_else(
                || "attachment.bin".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            );
            let len_str = bytes.len().to_string();
            let get_url_req = HttpRequest::post_form(
                format!("{}/files.getUploadURLExternal", self.api_base),
                &[
                    ("filename", filename.as_str()),
                    ("length", len_str.as_str()),
                ],
            )
            .with_header(
                "Authorization",
                format!("Bearer {}", self.bot_token.as_str()),
            )
            .with_timeout(API_REQUEST_TIMEOUT)
            .with_ssrf_policy(self.ssrf_policy);
            let url_resp = http_client::send(&get_url_req).map_err(|err| {
                io::Error::new(err.kind(), "Slack files.getUploadURLExternal failed")
            })?;
            let url_json: Value = url_resp.json().unwrap_or(Value::Null);
            if !(url_resp.is_success() && url_json.get("ok").and_then(Value::as_bool) == Some(true))
            {
                return Err(io::Error::other(
                    url_json.get("error").and_then(Value::as_str).map_or_else(
                        || format!("HTTP {}", url_resp.status),
                        sanitize_error_for_status,
                    ),
                ));
            }
            let upload_url = url_json
                .get("upload_url")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "missing Slack upload_url")
                })?;
            let file_id = url_json
                .get("file_id")
                .and_then(Value::as_str)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Slack file_id"))?
                .to_owned();

            let up_req = HttpRequest::new(http_client::HttpMethod::Post, upload_url)
                .with_header("Content-Type", "application/octet-stream")
                .with_body(bytes)
                .with_timeout(API_REQUEST_TIMEOUT)
                .with_ssrf_policy(self.ssrf_policy);
            let up_resp = http_client::send(&up_req)
                .map_err(|err| io::Error::new(err.kind(), "Slack external file upload failed"))?;
            if !up_resp.is_success() {
                return Err(io::Error::other(format!(
                    "Slack upload returned HTTP {}",
                    up_resp.status
                )));
            }
            uploaded.push(json!({ "id": file_id, "title": filename }));
        }
        if uploaded.is_empty() {
            return Ok(None);
        }
        let last_id = uploaded
            .last()
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let mut complete_payload = json!({
            "files": uploaded,
            "channel_id": target.channel_id,
        });
        if let Some(ref ts) = target.thread_id
            && !ts.trim().is_empty()
            && let Some(obj) = complete_payload.as_object_mut()
        {
            obj.insert("thread_ts".to_owned(), json!(ts));
        }
        if let Some(cap) = caption.filter(|c| !c.trim().is_empty())
            && let Some(obj) = complete_payload.as_object_mut()
        {
            obj.insert("initial_comment".to_owned(), json!(cap));
        }
        self.post_api(
            "files.completeUploadExternal",
            self.bot_token.as_str(),
            &complete_payload,
        )?;
        Ok(last_id)
    }
}

/// Spawns a 128 KiB Slack Socket Mode worker thread for `bot`.
pub(crate) fn spawn_slack_worker(
    bot: BotConfig,
    app_token: Zeroizing<String>,
    sender: Arc<SlackSender>,
    bridge: Arc<dyn ConnectorBridge>,
    router: Arc<ConversationRouter>,
    stop: Arc<StopFlag>,
) -> io::Result<JoinHandle<()>> {
    let thread_name = format!("bot-slack-{}", bot.slug.as_str());
    thread::Builder::new()
        .name(thread_name)
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || {
            let bot_id = bot.id.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_slack_loop(&bot, &app_token, &sender, &bridge, &router, &stop);
            }));
            if outcome.is_err() {
                tracing::warn!(
                    bot_id = bot_id.as_str(),
                    source = "slack",
                    "slack socket mode worker panicked; isolated"
                );
                bridge.update_connector_status(BotConnectorStatus {
                    bot_id,
                    source: ExternalSource::Slack,
                    enabled: true,
                    connected: false,
                    state: ConnectorHealthState::Error,
                    account_label: None,
                    last_sync_ms: None,
                    last_error: Some("connector worker panicked".to_owned()),
                    error_category: Some(BotErrorCategory::AgentError),
                });
            }
        })
}

fn run_slack_loop(
    bot: &BotConfig,
    app_token: &Zeroizing<String>,
    sender: &Arc<SlackSender>,
    bridge: &Arc<dyn ConnectorBridge>,
    router: &Arc<ConversationRouter>,
    stop: &Arc<StopFlag>,
) {
    let (mut bot_user_id, mut account_label) = sender.auth_test().unwrap_or((None, None));
    let mut seen_events: VecDeque<String> = VecDeque::with_capacity(DEDUP_RING_CAPACITY);
    let mut participated_threads: BTreeSet<(String, String)> = BTreeSet::new();
    let mut backoff_ms: u64 = 1_000;
    let mut consecutive_disconnects: u32 = 0;

    while !stop.is_stopped() {
        report_status(
            bridge,
            &bot.id,
            false,
            ConnectorHealthState::Connecting,
            account_label.clone(),
            None,
            None,
            None,
        );

        let ws_url = match sender.open_socket_mode_url(app_token.as_str()) {
            Ok(url) => url,
            Err(err) => {
                if stop.is_stopped() {
                    break;
                }
                let raw = err.to_string();
                let cat = classify_error(&raw);
                report_status(
                    bridge,
                    &bot.id,
                    false,
                    ConnectorHealthState::Error,
                    account_label.clone(),
                    None,
                    Some(sanitize_error_for_status(&raw)),
                    Some(cat),
                );
                if stop.wait(Duration::from_millis(backoff_ms)) {
                    break;
                }
                backoff_ms = backoff_ms.saturating_mul(2).min(30_000);
                continue;
            }
        };

        let mut ws = match WsClient::connect_with_policy(
            &ws_url,
            &[],
            WS_CONNECT_TIMEOUT,
            sender.ssrf_policy,
        ) {
            Ok(client) => client,
            Err(err) => {
                if stop.is_stopped() {
                    break;
                }
                let raw = err.to_string();
                let cat = classify_error(&raw);
                report_status(
                    bridge,
                    &bot.id,
                    false,
                    ConnectorHealthState::Error,
                    account_label.clone(),
                    None,
                    Some(sanitize_error_for_status(&raw)),
                    Some(cat),
                );
                if stop.wait(Duration::from_millis(backoff_ms)) {
                    break;
                }
                backoff_ms = backoff_ms.saturating_mul(2).min(30_000);
                continue;
            }
        };

        backoff_ms = 1_000;
        if bot_user_id.is_none()
            && let Ok((uid, team)) = sender.auth_test()
        {
            bot_user_id = uid;
            if account_label.is_none() {
                account_label = team;
            }
        }
        report_status(
            bridge,
            &bot.id,
            true,
            ConnectorHealthState::Connected,
            account_label.clone(),
            Some(now_ms()),
            None,
            None,
        );

        let mut clean_reconnect = false;
        let mut saw_activity = false;
        while !stop.is_stopped() {
            match ws.poll_event(WS_POLL_TIMEOUT) {
                Ok(WsEvent::Timeout | WsEvent::Pong(_) | WsEvent::Binary(_)) => {}
                Ok(WsEvent::Text(raw)) => {
                    let Ok(envelope) = serde_json::from_str::<Value>(&raw) else {
                        continue;
                    };
                    // Slack Socket Mode requires immediate ACK of every envelope carrying `envelope_id`.
                    if let Some(envelope_id) = envelope.get("envelope_id").and_then(Value::as_str) {
                        let ack = json!({"envelope_id": envelope_id}).to_string();
                        if ws.send_text(&ack).is_err() {
                            break;
                        }
                    }

                    let env_type = envelope
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    match env_type {
                        "hello" => {
                            report_status(
                                bridge,
                                &bot.id,
                                true,
                                ConnectorHealthState::Connected,
                                account_label.clone(),
                                Some(now_ms()),
                                None,
                                None,
                            );
                        }
                        "disconnect" => {
                            clean_reconnect = true;
                            consecutive_disconnects = consecutive_disconnects.saturating_add(1);
                            break;
                        }
                        "events_api" => {
                            saw_activity = true;
                            consecutive_disconnects = 0;
                            if let Some(payload) = envelope.get("payload") {
                                handle_events_api(
                                    &bot.id,
                                    bot_user_id.as_deref(),
                                    sender,
                                    bridge,
                                    router,
                                    payload,
                                    &mut seen_events,
                                    &mut participated_threads,
                                );
                            }
                        }
                        "interactive" => {
                            saw_activity = true;
                            consecutive_disconnects = 0;
                            if let Some(payload) = envelope.get("payload") {
                                handle_interactive(&bot.id, sender, router, payload);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(WsEvent::Closed { .. }) => break,
                Err(err) => {
                    if !stop.is_stopped() {
                        let raw = err.to_string();
                        report_status(
                            bridge,
                            &bot.id,
                            false,
                            ConnectorHealthState::Error,
                            account_label.clone(),
                            None,
                            Some(sanitize_error_for_status(&raw)),
                            Some(classify_error(&raw)),
                        );
                    }
                    break;
                }
            }
        }

        ws.close();
        if stop.is_stopped() {
            break;
        }
        if clean_reconnect {
            if !saw_activity && consecutive_disconnects >= 2 {
                let exp = 1_000_u64
                    .saturating_mul(1_u64 << (consecutive_disconnects.saturating_sub(2)).min(4))
                    .min(30_000);
                let jitter = slack_jitter_ms(exp / 2);
                if stop.wait(Duration::from_millis(exp.saturating_add(jitter))) {
                    break;
                }
            }
        } else {
            consecutive_disconnects = 0;
            if stop.wait(Duration::from_millis(backoff_ms)) {
                break;
            }
            backoff_ms = backoff_ms.saturating_mul(2).min(30_000);
        }
    }

    report_status(
        bridge,
        &bot.id,
        false,
        ConnectorHealthState::Disconnected,
        account_label,
        None,
        None,
        None,
    );
}

#[expect(clippy::too_many_arguments, reason = "Slack event dispatch context")]
fn handle_events_api(
    bot_id: &BotId,
    bot_user_id: Option<&str>,
    sender: &Arc<SlackSender>,
    bridge: &Arc<dyn ConnectorBridge>,
    router: &Arc<ConversationRouter>,
    payload: &Value,
    seen_events: &mut VecDeque<String>,
    participated_threads: &mut BTreeSet<(String, String)>,
) {
    let Some(event) = payload.get("event") else {
        return;
    };
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if event_type != "app_mention" && event_type != "message" {
        return;
    }

    // Suppress bot self-echoes and edit/delete subtypes.
    if event.get("bot_id").and_then(Value::as_str).is_some()
        || event.get("bot_profile").is_some_and(|v| !v.is_null())
    {
        return;
    }
    if let Some(subtype) = event.get("subtype").and_then(Value::as_str)
        && matches!(
            subtype,
            "bot_message" | "message_changed" | "message_deleted" | "channel_join"
        )
    {
        return;
    }

    let Some(user_id) = event.get("user").and_then(Value::as_str) else {
        return;
    };
    if bot_user_id.is_some_and(|self_id| self_id == user_id) {
        return;
    }
    let Some(channel_id) = event.get("channel").and_then(Value::as_str) else {
        return;
    };
    let channel_type = event
        .get("channel_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let is_dm = channel_type == "im" || channel_id.starts_with('D');
    let explicit_thread_ts = event.get("thread_ts").and_then(Value::as_str);

    let raw_text = event
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mentions_bot = bot_user_id.is_some_and(|uid| raw_text.contains(&format!("<@{uid}>")));

    // Gate non-DM `message` events: only respond inside threads the bot participates in or @mentions.
    if event_type == "message" && !is_dm {
        let in_participated_thread = explicit_thread_ts.is_some_and(|tts| {
            participated_threads.contains(&(channel_id.to_owned(), tts.to_owned()))
        });
        if !in_participated_thread && !mentions_bot {
            return;
        }
    }

    let bot_cfg = bridge.get_bot_config(bot_id);
    if let Some(ref cfg) = bot_cfg
        && !is_dm
        && !cfg.connectors.slack.allowed_channel_ids.is_empty()
        && !cfg
            .connectors
            .slack
            .allowed_channel_ids
            .iter()
            .any(|c| c == channel_id)
    {
        return;
    }

    let ts = event.get("ts").and_then(Value::as_str);

    // Deduplicate dual delivery of `app_mention` + `message` for the same `(channel, ts)`.
    if let Some(msg_ts) = ts {
        let dedup_key = format!("{channel_id}:{msg_ts}");
        if seen_events.iter().any(|k| k == &dedup_key) {
            return;
        }
        if seen_events.len() >= DEDUP_RING_CAPACITY {
            seen_events.pop_front();
        }
        seen_events.push_back(dedup_key);
    }

    let cleaned = strip_slack_bot_mention(raw_text, bot_user_id);
    if cleaned.is_empty() {
        return;
    }

    let thread_id = explicit_thread_ts.map(ToOwned::to_owned).or_else(|| {
        if event_type == "app_mention" || !is_dm {
            ts.map(ToOwned::to_owned)
        } else {
            None
        }
    });

    let workspace_id = payload
        .get("team_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let acts_for = bot_cfg.map_or_else(Default::default, |b| b.connectors.slack.acts_for);
    let sender_name = sender.resolve_sender_name(user_id);

    let target = DeliveryTarget {
        source: ExternalSource::Slack,
        workspace_id: workspace_id.clone(),
        channel_id: channel_id.to_owned(),
        thread_id: thread_id.clone(),
        reply_to_message_id: ts.map(ToOwned::to_owned),
    };
    let inbound = InboundExternalMessage {
        bot_id: bot_id.clone(),
        source: ExternalSource::Slack,
        workspace_id,
        channel_id: channel_id.to_owned(),
        thread_id: thread_id.clone(),
        sender_id: user_id.to_owned(),
        sender_name,
        text: cleaned,
        attachments: Vec::new(),
        external_message_id: ts.map(ToOwned::to_owned),
        acts_for,
    };
    let outcome = router.handle_inbound_message(inbound, target, sender.as_ref());
    if matches!(outcome, InboundRouteOutcome::Dispatched(_))
        && let Some(tts) = thread_id
    {
        if participated_threads.len() >= 1_024
            && let Some(first) = participated_threads.iter().next().cloned()
        {
            participated_threads.remove(&first);
        }
        participated_threads.insert((channel_id.to_owned(), tts));
    }
}

fn handle_interactive(
    bot_id: &BotId,
    sender: &Arc<SlackSender>,
    router: &Arc<ConversationRouter>,
    payload: &Value,
) {
    if payload.get("type").and_then(Value::as_str) != Some("block_actions") {
        return;
    }
    let Some(user) = payload.get("user") else {
        return;
    };
    let Some(sender_id) = user.get("id").and_then(Value::as_str) else {
        return;
    };
    let sender_name = user
        .get("username")
        .or_else(|| user.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(sender_id);
    let channel_id = payload
        .get("channel")
        .and_then(|c| c.get("id"))
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("container")
                .and_then(|c| c.get("channel_id"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    let thread_ts = payload
        .get("message")
        .and_then(|m| m.get("thread_ts").or_else(|| m.get("ts")))
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("container")
                .and_then(|c| c.get("message_ts"))
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned);
    let workspace_id = payload
        .get("team")
        .and_then(|t| t.get("id"))
        .or_else(|| payload.get("team_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    let Some(actions) = payload.get("actions").and_then(Value::as_array) else {
        return;
    };
    for action in actions {
        let candidate = action
            .get("value")
            .and_then(Value::as_str)
            .filter(|v| v.starts_with("gate:"))
            .or_else(|| {
                action
                    .get("action_id")
                    .and_then(Value::as_str)
                    .filter(|v| v.starts_with("gate:"))
            });
        let Some(gate_payload) = candidate else {
            continue;
        };
        let outcome = router.handle_gate_callback_with_context(
            bot_id,
            ExternalSource::Slack,
            workspace_id.as_deref(),
            sender_id,
            sender_name,
            channel_id,
            thread_ts.as_deref(),
            gate_payload,
        );
        if !channel_id.is_empty() {
            let target = DeliveryTarget {
                source: ExternalSource::Slack,
                workspace_id: workspace_id.clone(),
                channel_id: channel_id.to_owned(),
                thread_id: thread_ts.clone(),
                reply_to_message_id: None,
            };
            sender.send_message(&target, &outcome.user_notice()).ok();
        }
    }
}

fn strip_slack_bot_mention(text: &str, bot_user_id: Option<&str>) -> String {
    let mut s = text.trim().to_owned();
    if let Some(uid) = bot_user_id {
        let token = format!("<@{uid}>");
        if s.contains(&token) {
            s = s.replace(&token, " ");
        }
    }
    let normalized = s.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized
        .trim_start_matches([':', ',', '-'])
        .trim()
        .to_owned()
}

fn slack_jitter_ms(max_jitter: u64) -> u64 {
    if max_jitter == 0 {
        return 0;
    }
    let nanos = u64::from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos(),
    );
    nanos % max_jitter.saturating_add(1)
}

#[expect(
    clippy::too_many_arguments,
    reason = "maps directly onto BotConnectorStatus fields"
)]
fn report_status(
    bridge: &Arc<dyn ConnectorBridge>,
    bot_id: &BotId,
    connected: bool,
    state: ConnectorHealthState,
    account_label: Option<String>,
    last_sync_ms: Option<u64>,
    last_error: Option<String>,
    error_category: Option<BotErrorCategory>,
) {
    bridge.update_connector_status(BotConnectorStatus {
        bot_id: bot_id.clone(),
        source: ExternalSource::Slack,
        enabled: true,
        connected,
        state,
        account_label,
        last_sync_ms,
        last_error,
        error_category,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::time::Instant;

    use krill_proto::SessionId;
    use krill_proto::bots::{ActsFor, CreateBotParams};
    use krill_proto::orchestration::OrchestrationStore;

    use crate::bots::connectors::ws_client::tests::{
        accept_ws_test_handshake, read_client_frame, write_server_frame,
    };
    use crate::bots::connectors::{
        AccessDecision, ConnectorEndpoints, ConnectorManager, DispatchOutcome,
    };
    use crate::bots::secrets::SecretVault;
    use crate::bots::store::BotStore;
    use crate::session::lock;

    struct SlackMockState {
        posted: Vec<Value>,
        updated: Vec<Value>,
        reactions: Vec<Value>,
        acks: Vec<Value>,
        connections_opened: usize,
    }

    fn read_http(stream: &mut std::net::TcpStream) -> (String, String, Vec<u8>) {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let end = loop {
            if let Some(p) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break p;
            }
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => return (String::new(), String::new(), Vec::new()),
                Ok(n) => raw.extend_from_slice(&buf[..n]),
            }
        };
        let head = String::from_utf8_lossy(&raw[..end]).into_owned();
        let path = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or_default()
            .to_owned();
        let auth = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("authorization")
                    .then(|| v.trim().to_owned())
            })
            .unwrap_or_default();
        let content_len = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        let mut body = raw[end + 4..].to_vec();
        while body.len() < content_len {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => body.extend_from_slice(&buf[..n]),
            }
        }
        body.truncate(content_len);
        (path, auth, body)
    }

    struct BridgeForSlack {
        state_dir: std::path::PathBuf,
        store: Arc<BotStore>,
        secrets: Arc<SecretVault>,
        dispatched: Mutex<Vec<InboundExternalMessage>>,
    }

    impl ConnectorBridge for BridgeForSlack {
        fn dispatch_external_message(
            &self,
            msg: InboundExternalMessage,
        ) -> io::Result<DispatchOutcome> {
            let link = self.store.lookup_or_create_link(
                &msg.bot_id,
                msg.source,
                msg.workspace_id.as_deref(),
                &msg.channel_id,
                msg.thread_id.as_deref(),
                msg.acts_for.clone(),
                now_ms(),
            )?;
            let link = self
                .store
                .bind_link_session(&link.id, SessionId(88), now_ms())?
                .unwrap_or(link);
            lock(&self.dispatched).push(msg);
            Ok(DispatchOutcome {
                link,
                session_id: SessionId(88),
                started_new_session: true,
            })
        }
        fn check_or_request_access(
            &self,
            bot_id: &BotId,
            source: ExternalSource,
            sender_id: &str,
            sender_name: &str,
            channel_id: &str,
        ) -> io::Result<AccessDecision> {
            self.store.check_or_request_access(
                bot_id,
                source,
                sender_id,
                sender_name,
                channel_id,
                now_ms(),
            )
        }
        fn resolve_secret(
            &self,
            bot_id: &BotId,
            explicit_ref: Option<&str>,
            fallback_key: &str,
        ) -> io::Result<Option<Zeroizing<String>>> {
            self.secrets
                .resolve_secret(bot_id, explicit_ref, fallback_key)
        }
        fn lookup_or_create_link(
            &self,
            bot_id: &BotId,
            source: ExternalSource,
            workspace_id: Option<&str>,
            channel_id: &str,
            thread_id: Option<&str>,
            acts_for: ActsFor,
        ) -> io::Result<krill_proto::bots::ExternalConversationLink> {
            self.store.lookup_or_create_link(
                bot_id,
                source,
                workspace_id,
                channel_id,
                thread_id,
                acts_for,
                now_ms(),
            )
        }
        fn reply_orchestration_gate(
            &self,
            gate_id: &str,
            decision: &str,
            rationale: Option<String>,
            resolved_by: &str,
        ) -> io::Result<()> {
            OrchestrationStore::mutate(&self.state_dir, |s| {
                s.resolve_gate(gate_id, decision, rationale, resolved_by)?;
                Ok(())
            })
        }
        fn update_connector_status(&self, _status: BotConnectorStatus) {}
        fn get_bot_config(&self, bot_id: &BotId) -> Option<BotConfig> {
            self.store.get_bot(bot_id).ok().flatten()
        }
        fn list_bots(&self) -> Vec<BotConfig> {
            self.store.list_bots().unwrap_or_default()
        }
    }

    #[test]
    fn slack_socket_mode_e2e_ack_echo_suppression_mention_activity_gate_and_mirror() {
        let dir = std::env::temp_dir().join(format!("krill-slack-e2e-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let ws_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let http_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        http_listener.set_nonblocking(true).unwrap();
        let http_addr = http_listener.local_addr().unwrap();

        let state = Arc::new(Mutex::new(SlackMockState {
            posted: Vec::new(),
            updated: Vec::new(),
            reactions: Vec::new(),
            acks: Vec::new(),
            connections_opened: 0,
        }));
        let stop = Arc::new(StopFlag::default());

        let gate = OrchestrationStore::mutate(&dir, |s| {
            s.create_gate(
                "worker-slack",
                "Merge PR?",
                vec!["yes".to_owned(), "no".to_owned()],
                None,
                None,
            )
        })
        .unwrap();

        // Loopback HTTP server for Slack Web API.
        let http_state = Arc::clone(&state);
        let http_stop = Arc::clone(&stop);
        let ws_url_str = format!("ws://{ws_addr}/link?ticket=t1");
        let http_thread = thread::spawn(move || {
            let mut next_ts = 1000_u64;
            while !http_stop.is_stopped() {
                match http_listener.accept() {
                    Ok((mut sock, _)) => {
                        sock.set_nonblocking(false).ok();
                        sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        let (path, auth, body) = read_http(&mut sock);
                        let req_json: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
                        let resp = if path.ends_with("/apps.connections.open") {
                            assert_eq!(auth, "Bearer xapp-test-token");
                            lock(&http_state).connections_opened += 1;
                            json!({"ok": true, "url": ws_url_str})
                        } else if path.ends_with("/auth.test") {
                            assert_eq!(auth, "Bearer xoxb-test-token");
                            json!({"ok": true, "user_id": "U_BOT", "team": "KrillHQ"})
                        } else if path.ends_with("/chat.postMessage") {
                            assert_eq!(auth, "Bearer xoxb-test-token");
                            next_ts += 1;
                            lock(&http_state).posted.push(req_json);
                            json!({"ok": true, "ts": format!("{next_ts}.0001")})
                        } else if path.ends_with("/chat.update") {
                            assert_eq!(auth, "Bearer xoxb-test-token");
                            lock(&http_state).updated.push(req_json);
                            json!({"ok": true})
                        } else if path.ends_with("/reactions.add") {
                            lock(&http_state).reactions.push(req_json);
                            json!({"ok": true})
                        } else {
                            json!({"ok": false, "error": "unknown"})
                        };
                        let payload = serde_json::to_vec(&resp).unwrap();
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            payload.len()
                        );
                        sock.write_all(header.as_bytes()).ok();
                        sock.write_all(&payload).ok();
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        // Loopback Socket Mode WebSocket server.
        let ws_state = Arc::clone(&state);
        let gate_id_clone = gate.id.clone();
        let ws_thread = thread::spawn(move || {
            let (mut sock, _) = ws_listener.accept().unwrap();
            accept_ws_test_handshake(&mut sock).unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(5))).ok();

            // 1. Hello envelope.
            write_server_frame(
                &mut sock,
                true,
                1,
                br#"{"type":"hello","num_connections":1}"#,
            )
            .unwrap();

            // 2. Bot self-echo event (MUST be ACKed but NOT dispatched).
            let echo_env = json!({
                "envelope_id": "env-echo",
                "type": "events_api",
                "payload": {
                    "team_id": "T1",
                    "event": {
                        "type": "message",
                        "subtype": "bot_message",
                        "bot_id": "B999",
                        "user": "U_BOT",
                        "channel": "C_DEV",
                        "text": "my own echo",
                        "ts": "100.1"
                    }
                }
            })
            .to_string();
            write_server_frame(&mut sock, true, 1, echo_env.as_bytes()).unwrap();
            let (_, ack1) = read_client_frame(&mut sock).unwrap();
            lock(&ws_state)
                .acks
                .push(serde_json::from_slice(&ack1).unwrap());

            // 3. Real app_mention event from allowed user U_OWEN.
            let mention_env = json!({
                "envelope_id": "env-mention",
                "type": "events_api",
                "payload": {
                    "team_id": "T1",
                    "event": {
                        "type": "app_mention",
                        "user": "U_OWEN",
                        "channel": "C_DEV",
                        "text": "<@U_BOT> check failing tests",
                        "ts": "200.1"
                    }
                }
            })
            .to_string();
            write_server_frame(&mut sock, true, 1, mention_env.as_bytes()).unwrap();
            let (_, ack2) = read_client_frame(&mut sock).unwrap();
            lock(&ws_state)
                .acks
                .push(serde_json::from_slice(&ack2).unwrap());

            // 4. Interactive block_actions gate approval from allowed user U_OWEN.
            let gate_env = json!({
                "envelope_id": "env-gate",
                "type": "interactive",
                "payload": {
                    "type": "block_actions",
                    "team": {"id": "T1"},
                    "user": {"id": "U_OWEN", "username": "owen"},
                    "channel": {"id": "C_DEV"},
                    "message": {"ts": "200.1"},
                    "actions": [{"action_id": "approve_btn", "value": format!("gate:{gate_id_clone}:yes")}]
                }
            })
            .to_string();
            write_server_frame(&mut sock, true, 1, gate_env.as_bytes()).unwrap();
            let (_, ack3) = read_client_frame(&mut sock).unwrap();
            lock(&ws_state)
                .acks
                .push(serde_json::from_slice(&ack3).unwrap());

            // Keep socket open briefly until client closes cleanly on shutdown.
            read_client_frame(&mut sock).ok();
        });

        let bots_root = dir.join("bots");
        let store = Arc::new(BotStore::new(bots_root.clone()));
        let secrets = Arc::new(SecretVault::with_static_key(bots_root, [0x33; 32]));
        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Slack Bot".to_owned(),
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
        let mut connectors = bot.connectors.clone();
        connectors.slack.enabled = true;
        connectors.slack.allowed_user_ids = vec!["U_OWEN".to_owned()];
        connectors.slack.allowed_channel_ids = vec!["C_DEV".to_owned()];
        connectors.slack.mirror_desktop = true;
        store
            .set_connector_config(&bot.id, connectors, 1_100)
            .unwrap();
        secrets
            .set_secret(
                &bot.id,
                "slack-app",
                Zeroizing::new("xapp-test-token".to_owned()),
            )
            .unwrap();
        secrets
            .set_secret(
                &bot.id,
                "slack-bot",
                Zeroizing::new("xoxb-test-token".to_owned()),
            )
            .unwrap();

        let bridge = Arc::new(BridgeForSlack {
            state_dir: dir.clone(),
            store: Arc::clone(&store),
            secrets: Arc::clone(&secrets),
            dispatched: Mutex::new(Vec::new()),
        });
        let mgr = ConnectorManager::with_endpoints(
            Arc::clone(&bridge) as Arc<dyn ConnectorBridge>,
            ConnectorEndpoints {
                slack: format!("http://{http_addr}"),
                ..ConnectorEndpoints::default()
            },
        );
        mgr.reload(None).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while (lock(&state).acks.len() < 3
            || lock(&bridge.dispatched).is_empty()
            || lock(&state).posted.is_empty())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(20));
        }

        // Verify all 3 envelopes ACKed immediately, self-echo suppressed, and mention stripped.
        assert_eq!(lock(&state).acks.len(), 3);
        assert_eq!(lock(&state).acks[0]["envelope_id"], "env-echo");
        assert_eq!(lock(&state).acks[1]["envelope_id"], "env-mention");
        assert_eq!(lock(&state).acks[2]["envelope_id"], "env-gate");
        assert_eq!(lock(&bridge.dispatched).len(), 1);
        assert_eq!(lock(&bridge.dispatched)[0].text, "check failing tests");
        assert_eq!(
            lock(&bridge.dispatched)[0].thread_id.as_deref(),
            Some("200.1")
        );
        assert!(
            lock(&state).posted[0]["text"]
                .as_str()
                .unwrap()
                .contains("yes")
        );

        // Verify block_actions gate resolved in OrchestrationStore.
        let orch = OrchestrationStore::load(&dir).unwrap();
        assert_eq!(orch.gates[0].decision.as_deref(), Some("yes"));

        // Verify 3s-throttled activity -> chat.postMessage + chat.update.
        lock(&state).posted.clear();
        mgr.router()
            .on_turn_activity_at(&bot.id, SessionId(88), "Using Cargo...", 20_000);
        mgr.router()
            .on_turn_activity_at(&bot.id, SessionId(88), "Thinking...", 23_500);
        assert_eq!(lock(&state).posted.len(), 1);
        assert_eq!(lock(&state).posted[0]["thread_ts"], "200.1");
        assert_eq!(lock(&state).updated.len(), 1);
        assert_eq!(lock(&state).updated[0]["text"], "Thinking...");

        // Final turn reply + desktop mirror check.
        lock(&state).posted.clear();
        mgr.on_turn_complete(&bot.id, SessionId(88), Some("All tests green."));
        assert_eq!(lock(&state).posted.len(), 1);
        assert_eq!(lock(&state).posted[0]["text"], "All tests green.");

        // Desktop-only session (SessionId(999), no active external turn) mirrors because mirror_desktop=true!
        mgr.on_turn_complete(&bot.id, SessionId(999), Some("Desktop turn mirrored."));
        assert_eq!(lock(&state).posted.len(), 2);
        assert_eq!(lock(&state).posted[1]["channel"], "C_DEV");
        assert_eq!(lock(&state).posted[1]["text"], "Desktop turn mirrored.");

        mgr.shutdown();
        stop.signal();
        ws_thread.join().unwrap();
        http_thread.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn slack_inline_mention_users_info_cache_gating_and_file_upload_work() {
        assert_eq!(
            strip_slack_bot_mention("hey <@U_BOT> please deploy", Some("U_BOT")),
            "hey please deploy"
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = std::env::temp_dir().join(format!("krill-slack-w3-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let fpath = dir.join("report.txt");
        std::fs::write(&fpath, b"slack file content").unwrap();

        let srv = thread::spawn(move || {
            for _ in 0..4 {
                let (mut sock, _) = listener.accept().unwrap();
                let (path, _auth, _body) = read_http(&mut sock);
                let resp = if path.starts_with("/users.info") {
                    json!({"ok": true, "user": {"name": "owen", "profile": {"display_name": "Owen"}}})
                } else if path.ends_with("/files.getUploadURLExternal") {
                    json!({"ok": true, "upload_url": format!("http://{addr}/upload-bin"), "file_id": "F123"})
                } else if path.ends_with("/upload-bin")
                    || path.ends_with("/files.completeUploadExternal")
                {
                    json!({"ok": true})
                } else {
                    json!({"ok": false, "error": "unexpected"})
                };
                let body = serde_json::to_vec(&resp).unwrap();
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
                sock.write_all(&body).unwrap();
            }
        });

        let sender = SlackSender::new(
            &format!("http://{addr}"),
            Zeroizing::new("xoxb-t".to_owned()),
        );
        // Cached after first users.info hit: second call must not hit mock server.
        assert_eq!(sender.resolve_sender_name("U_1"), "Owen");
        assert_eq!(sender.resolve_sender_name("U_1"), "Owen");

        let target = DeliveryTarget {
            source: ExternalSource::Slack,
            workspace_id: Some("T1".to_owned()),
            channel_id: "C_DEV".to_owned(),
            thread_id: Some("100.1".to_owned()),
            reply_to_message_id: None,
        };
        let fid = sender
            .send_files(
                &target,
                &[fpath.to_string_lossy().into_owned()],
                Some("cap"),
            )
            .unwrap();
        assert_eq!(fid.as_deref(), Some("F123"));

        srv.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn slack_test_connection_validates_app_token_and_threads_only_tracked_after_dispatch() {
        struct PausedBridge(BotConfig);
        impl ConnectorBridge for PausedBridge {
            fn dispatch_external_message(
                &self,
                _msg: InboundExternalMessage,
            ) -> io::Result<DispatchOutcome> {
                Err(io::Error::other("unexpected"))
            }
            fn check_or_request_access(
                &self,
                _bot_id: &BotId,
                _source: ExternalSource,
                _sender_id: &str,
                _sender_name: &str,
                _channel_id: &str,
            ) -> io::Result<AccessDecision> {
                Ok(AccessDecision::Denied("nope".to_owned()))
            }
            fn resolve_secret(
                &self,
                _bot_id: &BotId,
                _explicit_ref: Option<&str>,
                _fallback_key: &str,
            ) -> io::Result<Option<Zeroizing<String>>> {
                Ok(None)
            }
            fn lookup_or_create_link(
                &self,
                _bot_id: &BotId,
                _source: ExternalSource,
                _workspace_id: Option<&str>,
                _channel_id: &str,
                _thread_id: Option<&str>,
                _acts_for: ActsFor,
            ) -> io::Result<krill_proto::bots::ExternalConversationLink> {
                Err(io::Error::other("unused"))
            }
            fn reply_orchestration_gate(
                &self,
                _gate_id: &str,
                _decision: &str,
                _rationale: Option<String>,
                _resolved_by: &str,
            ) -> io::Result<()> {
                Ok(())
            }
            fn update_connector_status(&self, _status: BotConnectorStatus) {}
            fn get_bot_config(&self, bot_id: &BotId) -> Option<BotConfig> {
                (&self.0.id == bot_id).then(|| self.0.clone())
            }
            fn list_bots(&self) -> Vec<BotConfig> {
                vec![self.0.clone()]
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = thread::spawn(move || {
            for idx in 0..2 {
                let (mut sock, _) = listener.accept().unwrap();
                let (path, _auth, _body) = read_http(&mut sock);
                let resp = if idx == 0 {
                    assert!(path.ends_with("/auth.test"));
                    json!({"ok": true, "user_id": "U_BOT", "team": "Acme"})
                } else {
                    assert!(path.ends_with("/apps.connections.open"));
                    json!({"ok": false, "error": "invalid_auth"})
                };
                let body = serde_json::to_vec(&resp).unwrap();
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
                sock.write_all(&body).unwrap();
            }
        });

        let sender = Arc::new(SlackSender::new(
            &format!("http://{addr}"),
            Zeroizing::new("xoxb-good".to_owned()),
        ));
        let res = sender.test_connection_with_app_token("xapp-bad");
        assert!(!res.ok);
        assert!(res.detail.contains("xapp-"));
        srv.join().unwrap();

        // D-23c: Paused bot mention in a Slack thread must NOT populate participated_threads.
        let bot_id = BotId::new("sl-part").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "SlPart", "claude", 1_000).unwrap();
        bot.paused = true;
        let bridge: Arc<dyn ConnectorBridge> = Arc::new(PausedBridge(bot));
        let router = Arc::new(ConversationRouter::new(Arc::clone(&bridge)));
        let mut seen = VecDeque::new();
        let mut participated = BTreeSet::new();
        let payload = json!({
            "team_id": "T1",
            "event": {
                "type": "app_mention",
                "user": "U_EVIL",
                "channel": "C1",
                "ts": "100.1",
                "thread_ts": "100.1",
                "text": "<@U_BOT> hi"
            }
        });
        handle_events_api(
            &bot_id,
            Some("U_BOT"),
            &sender,
            &bridge,
            &router,
            &payload,
            &mut seen,
            &mut participated,
        );
        assert!(participated.is_empty());
    }
}
