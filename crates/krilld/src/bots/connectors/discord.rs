// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! 100% local Discord Gateway v10 (`wss://`) + REST API connector.
//!
//! Fetches gateway URL via `GET /gateway/bot`, connects via [`WsClient`], and implements the
//! full Gateway v10 lifecycle: op 10 `Hello`, op 1 `Heartbeat` / op 11 `Heartbeat ACK`, op 2
//! `Identify`, op 6 `Resume` (persisting `seq`, `session_id`, `resume_gateway_url`), op 7
//! `Reconnect`, op 9 `Invalid Session`, and op 0 `Dispatch` (`READY`, `RESUMED`,
//! `MESSAGE_CREATE`, `INTERACTION_CREATE`). Outbound replies use REST `POST`/`PATCH` with safe
//! 2,000-character chunking.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use krill_proto::bots::{
    BotConfig, BotConnectorStatus, BotConnectorTestResult, BotErrorCategory, BotId,
    ConnectorHealthState, ExternalSource,
};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::bots::connectors::policy_for_api_base;
use crate::bots::connectors::router::{
    ConversationRouter, DeliveryTarget, InboundRouteOutcome, PlatformSender, classify_error,
    now_ms, sanitize_error_for_status,
};
use crate::bots::connectors::ws_client::{WsClient, WsEvent};
use crate::bots::connectors::{ConnectorBridge, InboundExternalMessage, StopFlag};
use crate::bots::http_client::{self, HttpMethod, HttpRequest, MultipartField, SsrfPolicy};
use crate::session::lock;

/// Production Discord REST API v10 base URL.
pub const DEFAULT_DISCORD_API_BASE: &str = "https://discord.com/api/v10";
/// Discord's hard single-message limit (2,000 characters).
pub const DISCORD_MAX_MESSAGE_CHARS: usize = 2_000;
/// Gateway intents: `GUILDS` (1<<0) | `GUILD_MESSAGES` (1<<9) | `DIRECT_MESSAGES` (1<<12) | `MESSAGE_CONTENT` (1<<15).
pub const DISCORD_GATEWAY_INTENTS: u32 = (1 << 0) | (1 << 9) | (1 << 12) | (1 << 15);

const WORKER_STACK_SIZE: usize = 128 * 1024;
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WS_HELLO_TIMEOUT: Duration = Duration::from_secs(15);
const WS_POLL_SLICE: Duration = Duration::from_millis(100);
const MAX_DISCORD_RATE_RETRIES: usize = 2;
const MAX_OUTBOUND_DISCORD_FILE_BYTES: usize = 25 * 1024 * 1024;

const OP_DISPATCH: u64 = 0;
const OP_HEARTBEAT: u64 = 1;
const OP_IDENTIFY: u64 = 2;
const OP_RESUME: u64 = 6;
const OP_RECONNECT: u64 = 7;
const OP_INVALID_SESSION: u64 = 9;
const OP_HELLO: u64 = 10;
const OP_HEARTBEAT_ACK: u64 = 11;

type ChannelMetaCache = Mutex<BTreeMap<String, (Option<u64>, Option<String>)>>;

/// Discord REST API v10 client & [`PlatformSender`] implementation.
pub struct DiscordSender {
    api_base: String,
    bot_token: Zeroizing<String>,
    ssrf_policy: SsrfPolicy,
    channel_cache: ChannelMetaCache,
}

impl std::fmt::Debug for DiscordSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordSender")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl DiscordSender {
    #[must_use]
    pub fn new(api_base: &str, bot_token: Zeroizing<String>) -> Self {
        let base = api_base.trim_end_matches('/').to_owned();
        let ssrf_policy = policy_for_api_base(&base);
        Self {
            api_base: base,
            bot_token,
            ssrf_policy,
            channel_cache: Mutex::new(BTreeMap::new()),
        }
    }

    fn send_with_rate_limit<F>(mut make_req: F) -> io::Result<http_client::HttpResponse>
    where
        F: FnMut() -> io::Result<HttpRequest>,
    {
        for attempt in 0..=MAX_DISCORD_RATE_RETRIES {
            let req = make_req()?;
            let resp = http_client::send(&req)
                .map_err(|err| io::Error::new(err.kind(), "Discord REST transport error"))?;
            if resp.status == 429 && attempt < MAX_DISCORD_RATE_RETRIES {
                let wait = parse_discord_retry_after(&resp);
                thread::sleep(wait);
                continue;
            }
            return Ok(resp);
        }
        Err(io::Error::other("Discord API retry limit reached"))
    }

    fn request_json(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<&Value>,
        with_auth: bool,
    ) -> io::Result<Value> {
        let url = format!("{}{path}", self.api_base);
        let encoded_body = if let Some(payload) = body {
            Some(serde_json::to_vec(payload).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "failed to encode Discord API payload",
                )
            })?)
        } else {
            None
        };
        let resp = Self::send_with_rate_limit(|| {
            let mut req = HttpRequest::new(method, &url)
                .with_timeout(API_REQUEST_TIMEOUT)
                .with_ssrf_policy(self.ssrf_policy);
            if with_auth {
                req = req.with_header("Authorization", format!("Bot {}", self.bot_token.as_str()));
            }
            if let Some(ref bytes) = encoded_body {
                req = req
                    .with_header("Content-Type", "application/json")
                    .with_body(bytes.clone());
            }
            Ok(req)
        })?;
        if resp.is_success() {
            if resp.body.is_empty() {
                return Ok(Value::Null);
            }
            Ok(resp.json().unwrap_or(Value::Null))
        } else {
            let msg = resp
                .json::<Value>()
                .ok()
                .and_then(|v| {
                    v.get("message")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| format!("Discord REST HTTP {}", resp.status));
            Err(io::Error::other(format!(
                "HTTP {}: {}",
                resp.status,
                sanitize_error_for_status(&msg)
            )))
        }
    }

    /// Resolves `(channel_type, parent_id)` lazily via cached `GET /channels/{id}`.
    /// Transient REST errors are NOT cached so subsequent messages can retry metadata lookup.
    fn resolve_channel_meta(&self, channel_id: &str) -> (Option<u64>, Option<String>) {
        if let Some(cached) = lock(&self.channel_cache).get(channel_id).cloned() {
            return cached;
        }
        let path = format!("/channels/{}", http_client::percent_encode(channel_id));
        let Ok(val) = self.request_json(HttpMethod::Get, &path, None, true) else {
            return (None, None);
        };
        let ctype = val.get("type").and_then(Value::as_u64);
        let parent = val
            .get("parent_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let resolved = (ctype, parent);
        let mut cache = lock(&self.channel_cache);
        if cache.len() >= 512
            && let Some(first) = cache.keys().next().cloned()
        {
            cache.remove(&first);
        }
        cache.insert(channel_id.to_owned(), resolved.clone());
        resolved
    }

    /// Calls `GET /gateway/bot` to discover the Discord Gateway WebSocket URL.
    pub fn fetch_gateway_url(&self) -> io::Result<String> {
        let parsed = self.request_json(HttpMethod::Get, "/gateway/bot", None, true)?;
        parsed
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Discord /gateway/bot omitted url field",
                )
            })
    }

    /// Runs an on-demand connectivity check for `TestConnector` via `GET /users/@me`.
    #[must_use]
    pub fn test_connection(&self) -> BotConnectorTestResult {
        match self.request_json(HttpMethod::Get, "/users/@me", None, true) {
            Ok(user) => {
                let username = user
                    .get("username")
                    .and_then(Value::as_str)
                    .map(|u| format!("@{u}"));
                BotConnectorTestResult {
                    source: ExternalSource::Discord,
                    ok: true,
                    account_label: username.clone(),
                    detail: format!(
                        "Connected to Discord ({})",
                        username.unwrap_or_else(|| "bot ok".to_owned())
                    ),
                }
            }
            Err(err) => BotConnectorTestResult {
                source: ExternalSource::Discord,
                ok: false,
                account_label: None,
                detail: sanitize_error_for_status(&err.to_string()),
            },
        }
    }

    /// Sends an interaction callback response (type 4 `CHANNEL_MESSAGE_WITH_SOURCE`).
    pub fn respond_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        content: &str,
        ephemeral: bool,
    ) -> io::Result<()> {
        let path = format!(
            "/interactions/{}/{}/callback",
            http_client::percent_encode(interaction_id),
            http_client::percent_encode(interaction_token)
        );
        let flags = if ephemeral { 64 } else { 0 };
        let payload = json!({
            "type": 4,
            "data": {
                "content": content,
                "flags": flags,
            }
        });
        self.request_json(HttpMethod::Post, &path, Some(&payload), false)?;
        Ok(())
    }
}

impl PlatformSender for DiscordSender {
    fn source(&self) -> ExternalSource {
        ExternalSource::Discord
    }

    fn max_chunk_chars(&self) -> usize {
        DISCORD_MAX_MESSAGE_CHARS
    }

    fn send_message(&self, target: &DeliveryTarget, text: &str) -> io::Result<String> {
        let channel = target
            .thread_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&target.channel_id);
        let path = format!(
            "/channels/{}/messages",
            http_client::percent_encode(channel)
        );
        let mut payload = json!({ "content": text });
        if let Some(ref reply_id) = target.reply_to_message_id
            && !reply_id.trim().is_empty()
            && let Some(obj) = payload.as_object_mut()
        {
            obj.insert(
                "message_reference".to_owned(),
                json!({ "message_id": reply_id, "fail_if_not_exists": false }),
            );
        }
        let parsed = self.request_json(HttpMethod::Post, &path, Some(&payload), true)?;
        let id = parsed
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("0")
            .to_owned();
        Ok(id)
    }

    fn edit_message(
        &self,
        target: &DeliveryTarget,
        message_id: &str,
        text: &str,
    ) -> io::Result<()> {
        let channel = target
            .thread_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&target.channel_id);
        let path = format!(
            "/channels/{}/messages/{}",
            http_client::percent_encode(channel),
            http_client::percent_encode(message_id)
        );
        let payload = json!({ "content": text });
        self.request_json(HttpMethod::Patch, &path, Some(&payload), true)?;
        Ok(())
    }

    fn send_files(
        &self,
        target: &DeliveryTarget,
        files: &[String],
        caption: Option<&str>,
    ) -> io::Result<Option<String>> {
        if files.is_empty() {
            return Ok(None);
        }
        let channel = target
            .thread_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&target.channel_id);
        let url = format!(
            "{}/channels/{}/messages",
            self.api_base,
            http_client::percent_encode(channel)
        );

        let mut json_meta = json!({});
        if let Some(cap) = caption.filter(|c| !c.trim().is_empty())
            && let Some(obj) = json_meta.as_object_mut()
        {
            obj.insert("content".to_owned(), json!(cap));
        }
        if let Some(ref reply_id) = target.reply_to_message_id
            && !reply_id.trim().is_empty()
            && let Some(obj) = json_meta.as_object_mut()
        {
            obj.insert(
                "message_reference".to_owned(),
                json!({ "message_id": reply_id, "fail_if_not_exists": false }),
            );
        }
        let payload_bytes = serde_json::to_vec(&json_meta).unwrap_or_else(|_| b"{}".to_vec());
        let mut fields = vec![MultipartField {
            name: "payload_json".to_owned(),
            filename: None,
            content_type: Some("application/json".to_owned()),
            data: payload_bytes,
        }];
        for (idx, raw_path) in files.iter().enumerate() {
            let path = Path::new(raw_path);
            let meta = fs::symlink_metadata(path)?;
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("refusing non-regular attachment: {raw_path}"),
                ));
            }
            if usize::try_from(meta.len()).unwrap_or(usize::MAX) > MAX_OUTBOUND_DISCORD_FILE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "attachment exceeds 25 MiB Discord limit",
                ));
            }
            let data = fs::read(path)?;
            let fname = path.file_name().map_or_else(
                || format!("file_{idx}.bin"),
                |n| n.to_string_lossy().into_owned(),
            );
            fields.push(MultipartField {
                name: format!("files[{idx}]"),
                filename: Some(fname),
                content_type: Some("application/octet-stream".to_owned()),
                data,
            });
        }
        let resp = Self::send_with_rate_limit(|| {
            Ok(HttpRequest::post_multipart(&url, &fields)?
                .with_header("Authorization", format!("Bot {}", self.bot_token.as_str()))
                .with_timeout(API_REQUEST_TIMEOUT)
                .with_ssrf_policy(self.ssrf_policy))
        })?;
        if !resp.is_success() {
            return Err(io::Error::other(format!(
                "Discord upload HTTP {}",
                resp.status
            )));
        }
        let parsed: Value = resp.json().unwrap_or(Value::Null);
        Ok(parsed
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned))
    }

    fn add_reaction(
        &self,
        target: &DeliveryTarget,
        message_id: &str,
        reaction: &str,
    ) -> io::Result<()> {
        let emoji = reaction.trim();
        if emoji.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reaction emoji cannot be empty",
            ));
        }
        let channel = target
            .thread_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&target.channel_id);
        let path = format!(
            "/channels/{}/messages/{}/reactions/{}/@me",
            http_client::percent_encode(channel),
            http_client::percent_encode(message_id),
            http_client::percent_encode(emoji)
        );
        self.request_json(HttpMethod::Put, &path, None, true)?;
        Ok(())
    }
}

fn parse_discord_retry_after(resp: &http_client::HttpResponse) -> Duration {
    if let Ok(val) = resp.json::<Value>()
        && let Some(secs) = val.get("retry_after").and_then(Value::as_f64)
        && secs.is_finite()
    {
        return Duration::from_secs_f64(secs.clamp(0.05, 30.0));
    }
    if let Some(raw_hdr) = resp.header("retry-after")
        && let Ok(secs) = raw_hdr.trim().parse::<f64>()
        && secs.is_finite()
    {
        return Duration::from_secs_f64(secs.clamp(0.05, 30.0));
    }
    Duration::from_secs(1)
}

#[derive(Default)]
struct GatewaySession {
    seq: Option<u64>,
    session_id: Option<String>,
    resume_gateway_url: Option<String>,
    bot_user_id: Option<String>,
    account_label: Option<String>,
}

impl GatewaySession {
    fn clear_resume(&mut self) {
        self.seq = None;
        self.session_id = None;
        self.resume_gateway_url = None;
    }

    fn can_resume(&self) -> bool {
        self.session_id.is_some() && self.seq.is_some()
    }
}

/// Spawns a 128 KiB Discord Gateway v10 worker thread for `bot`.
pub(crate) fn spawn_discord_worker(
    bot: BotConfig,
    bot_token: Zeroizing<String>,
    sender: Arc<DiscordSender>,
    bridge: Arc<dyn ConnectorBridge>,
    router: Arc<ConversationRouter>,
    stop: Arc<StopFlag>,
) -> io::Result<JoinHandle<()>> {
    let thread_name = format!("bot-dc-{}", bot.slug.as_str());
    thread::Builder::new()
        .name(thread_name)
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || {
            let bot_id = bot.id.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_discord_loop(&bot, &bot_token, &sender, &bridge, &router, &stop);
            }));
            if outcome.is_err() {
                tracing::warn!(
                    bot_id = bot_id.as_str(),
                    source = "discord",
                    "discord gateway worker panicked; isolated"
                );
                bridge.update_connector_status(BotConnectorStatus {
                    bot_id,
                    source: ExternalSource::Discord,
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

fn run_discord_loop(
    bot: &BotConfig,
    bot_token: &Zeroizing<String>,
    sender: &Arc<DiscordSender>,
    bridge: &Arc<dyn ConnectorBridge>,
    router: &Arc<ConversationRouter>,
    stop: &Arc<StopFlag>,
) {
    run_discord_loop_with_timeouts(
        bot,
        bot_token,
        sender,
        bridge,
        router,
        stop,
        WS_HELLO_TIMEOUT,
    );
}

fn run_discord_loop_with_timeouts(
    bot: &BotConfig,
    bot_token: &Zeroizing<String>,
    sender: &Arc<DiscordSender>,
    bridge: &Arc<dyn ConnectorBridge>,
    router: &Arc<ConversationRouter>,
    stop: &Arc<StopFlag>,
    hello_timeout: Duration,
) {
    let mut session = GatewaySession::default();
    let mut participated_threads: BTreeSet<String> = BTreeSet::new();
    let mut backoff_ms: u64 = 1_000;
    let mut consecutive_resume_failures: u32 = 0;
    let mut consecutive_reconnects: u32 = 0;

    while !stop.is_stopped() {
        report_status(
            bridge,
            &bot.id,
            false,
            ConnectorHealthState::Connecting,
            session.account_label.clone(),
            None,
            None,
            None,
        );

        let attempting_resume = session.can_resume() && session.resume_gateway_url.is_some();
        let raw_url = if attempting_resume && let Some(ref resume_url) = session.resume_gateway_url
        {
            resume_url.clone()
        } else {
            match sender.fetch_gateway_url() {
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
                        session.account_label.clone(),
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
            }
        };

        let gateway_url = append_gateway_query(&raw_url);
        let mut ws = match WsClient::connect_with_policy(
            &gateway_url,
            &[],
            WS_CONNECT_TIMEOUT,
            sender.ssrf_policy,
        ) {
            Ok(client) => client,
            Err(err) => {
                if stop.is_stopped() {
                    break;
                }
                if attempting_resume {
                    consecutive_resume_failures = consecutive_resume_failures.saturating_add(1);
                    if consecutive_resume_failures >= 2 {
                        session.clear_resume();
                        consecutive_resume_failures = 0;
                    }
                }
                let raw = err.to_string();
                let cat = classify_error(&raw);
                report_status(
                    bridge,
                    &bot.id,
                    false,
                    ConnectorHealthState::Error,
                    session.account_label.clone(),
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

        let mut heartbeat_interval: Option<Duration> = None;
        let mut next_heartbeat: Option<Instant> = None;
        let hello_deadline = Instant::now() + hello_timeout;
        let mut awaiting_ack = false;
        let mut immediate_reconnect = false;
        let mut custom_reconnect_wait: Option<Duration> = None;
        let mut fatal_close = false;
        let mut resumed_or_ready = false;

        while !stop.is_stopped() {
            if heartbeat_interval.is_none() && Instant::now() >= hello_deadline {
                report_status(
                    bridge,
                    &bot.id,
                    false,
                    ConnectorHealthState::Error,
                    session.account_label.clone(),
                    None,
                    Some("Discord gateway HELLO timed out".to_owned()),
                    Some(classify_error("Discord gateway HELLO timed out")),
                );
                break;
            }
            // Send scheduled heartbeat if due.
            if let (Some(interval), Some(due)) = (heartbeat_interval, next_heartbeat)
                && Instant::now() >= due
            {
                if awaiting_ack {
                    // Missed previous heartbeat ACK: reconnect and resume.
                    break;
                }
                let hb = json!({"op": OP_HEARTBEAT, "d": session.seq}).to_string();
                if ws.send_text(&hb).is_err() {
                    break;
                }
                awaiting_ack = true;
                next_heartbeat = Some(Instant::now() + interval);
            }

            let poll_wait = match next_heartbeat {
                Some(due) => due
                    .saturating_duration_since(Instant::now())
                    .min(WS_POLL_SLICE),
                None => hello_deadline
                    .saturating_duration_since(Instant::now())
                    .min(WS_POLL_SLICE),
            };
            match ws.poll_event(poll_wait) {
                Ok(WsEvent::Timeout | WsEvent::Pong(_) | WsEvent::Binary(_)) => {}
                Ok(WsEvent::Text(raw)) => {
                    let Ok(frame) = serde_json::from_str::<Value>(&raw) else {
                        continue;
                    };
                    if let Some(s) = frame.get("s").and_then(Value::as_u64) {
                        session.seq = Some(s);
                    }
                    let op = frame.get("op").and_then(Value::as_u64).unwrap_or(u64::MAX);
                    match op {
                        OP_HELLO => {
                            let interval_ms = frame
                                .get("d")
                                .and_then(|d| d.get("heartbeat_interval"))
                                .and_then(Value::as_u64)
                                .unwrap_or(41_250)
                                .clamp(50, 120_000);
                            let interval = Duration::from_millis(interval_ms);
                            heartbeat_interval = Some(interval);
                            // Jitter initial heartbeat across `[0..interval)` per Discord Gateway v10 spec.
                            let initial_jitter_ms = discord_random_u64() % interval_ms.max(1);
                            next_heartbeat =
                                Some(Instant::now() + Duration::from_millis(initial_jitter_ms));
                            awaiting_ack = false;

                            let handshake = if let (Some(sid), Some(seq)) =
                                (session.session_id.as_deref(), session.seq)
                            {
                                json!({
                                    "op": OP_RESUME,
                                    "d": {
                                        "token": bot_token.as_str(),
                                        "session_id": sid,
                                        "seq": seq,
                                    }
                                })
                            } else {
                                json!({
                                    "op": OP_IDENTIFY,
                                    "d": {
                                        "token": bot_token.as_str(),
                                        "intents": DISCORD_GATEWAY_INTENTS,
                                        "properties": {
                                            "os": std::env::consts::OS,
                                            "browser": "krill",
                                            "device": "krill",
                                        }
                                    }
                                })
                            };
                            if ws.send_text(&handshake.to_string()).is_err() {
                                break;
                            }
                        }
                        OP_HEARTBEAT => {
                            let hb = json!({"op": OP_HEARTBEAT, "d": session.seq}).to_string();
                            if ws.send_text(&hb).is_err() {
                                break;
                            }
                        }
                        OP_HEARTBEAT_ACK => {
                            awaiting_ack = false;
                            report_status(
                                bridge,
                                &bot.id,
                                true,
                                ConnectorHealthState::Connected,
                                session.account_label.clone(),
                                Some(now_ms()),
                                None,
                                None,
                            );
                        }
                        OP_RECONNECT => {
                            immediate_reconnect = true;
                            break;
                        }
                        OP_INVALID_SESSION => {
                            let resumable =
                                frame.get("d").and_then(Value::as_bool).unwrap_or(false);
                            if !resumable {
                                session.clear_resume();
                            }
                            let wait_ms = 1_000 + (discord_random_u64() % 4_001);
                            custom_reconnect_wait = Some(Duration::from_millis(wait_ms));
                            break;
                        }
                        OP_DISPATCH => {
                            let event_name =
                                frame.get("t").and_then(Value::as_str).unwrap_or_default();
                            let data = frame.get("d").unwrap_or(&Value::Null);
                            handle_dispatch(
                                &bot.id,
                                event_name,
                                data,
                                &mut session,
                                sender,
                                bridge,
                                router,
                                &mut participated_threads,
                            );
                            if event_name == "READY" || event_name == "RESUMED" {
                                backoff_ms = 1_000;
                                consecutive_resume_failures = 0;
                                consecutive_reconnects = 0;
                                resumed_or_ready = true;
                            }
                        }
                        _ => {}
                    }
                }
                Ok(WsEvent::Closed { code, .. }) => {
                    if let Some(c) = code {
                        if matches!(c, 4004 | 4010 | 4011 | 4012 | 4013 | 4014) {
                            session.clear_resume();
                            fatal_close = true;
                            let err_msg = format!("Discord gateway close {c}");
                            report_status(
                                bridge,
                                &bot.id,
                                false,
                                ConnectorHealthState::Error,
                                session.account_label.clone(),
                                None,
                                Some(err_msg.clone()),
                                Some(classify_error(&err_msg)),
                            );
                        } else if matches!(c, 4007 | 4009) {
                            session.clear_resume();
                        }
                    }
                    break;
                }
                Err(err) => {
                    if !stop.is_stopped() {
                        let raw = err.to_string();
                        report_status(
                            bridge,
                            &bot.id,
                            false,
                            ConnectorHealthState::Error,
                            session.account_label.clone(),
                            None,
                            Some(sanitize_error_for_status(&raw)),
                            Some(classify_error(&raw)),
                        );
                    }
                    break;
                }
            }
        }

        if stop.is_stopped() || fatal_close {
            ws.close_with_code(1000, "shutdown");
            break;
        }
        // Discord Gateway v10 invalidates sessions on close codes 1000/1001; use 4000 so OP_RESUME works.
        ws.close_with_code(4000, "reconnect");
        if attempting_resume && !resumed_or_ready {
            consecutive_resume_failures = consecutive_resume_failures.saturating_add(1);
            if consecutive_resume_failures >= 2 {
                session.clear_resume();
                consecutive_resume_failures = 0;
            }
        }
        if immediate_reconnect {
            consecutive_reconnects = consecutive_reconnects.saturating_add(1);
        } else {
            consecutive_reconnects = 0;
        }
        let skip_backoff = immediate_reconnect && consecutive_reconnects < 2;
        if let Some(wait_dur) = custom_reconnect_wait {
            if stop.wait(wait_dur) {
                break;
            }
        } else if !skip_backoff {
            if stop.wait(Duration::from_millis(backoff_ms)) {
                break;
            }
            backoff_ms = backoff_ms.saturating_mul(2).min(30_000);
        }
    }

    if stop.is_stopped() {
        report_status(
            bridge,
            &bot.id,
            false,
            ConnectorHealthState::Disconnected,
            session.account_label,
            None,
            None,
            None,
        );
    }
}

#[expect(clippy::too_many_arguments, reason = "Discord event dispatch context")]
fn handle_dispatch(
    bot_id: &BotId,
    event_name: &str,
    data: &Value,
    session: &mut GatewaySession,
    sender: &Arc<DiscordSender>,
    bridge: &Arc<dyn ConnectorBridge>,
    router: &Arc<ConversationRouter>,
    participated_threads: &mut BTreeSet<String>,
) {
    match event_name {
        "READY" => {
            session.session_id = data
                .get("session_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            session.resume_gateway_url = data
                .get("resume_gateway_url")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            session.bot_user_id = data
                .get("user")
                .and_then(|u| u.get("id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            session.account_label = data
                .get("user")
                .and_then(|u| u.get("username"))
                .and_then(Value::as_str)
                .map(|u| format!("@{u}"));
            report_status(
                bridge,
                bot_id,
                true,
                ConnectorHealthState::Connected,
                session.account_label.clone(),
                Some(now_ms()),
                None,
                None,
            );
        }
        "RESUMED" => {
            report_status(
                bridge,
                bot_id,
                true,
                ConnectorHealthState::Connected,
                session.account_label.clone(),
                Some(now_ms()),
                None,
                None,
            );
        }
        "MESSAGE_CREATE" => {
            handle_message_create(
                bot_id,
                session.bot_user_id.as_deref(),
                data,
                sender,
                bridge,
                router,
                participated_threads,
            );
        }
        "INTERACTION_CREATE" => {
            handle_interaction_create(bot_id, data, sender, router);
        }
        _ => {}
    }
}

fn handle_message_create(
    bot_id: &BotId,
    bot_user_id: Option<&str>,
    data: &Value,
    sender: &Arc<DiscordSender>,
    bridge: &Arc<dyn ConnectorBridge>,
    router: &Arc<ConversationRouter>,
    participated_threads: &mut BTreeSet<String>,
) {
    let Some(author) = data.get("author") else {
        return;
    };
    if author.get("bot").and_then(Value::as_bool) == Some(true)
        || data.get("webhook_id").and_then(Value::as_str).is_some()
    {
        return;
    }
    let Some(sender_id) = author.get("id").and_then(Value::as_str) else {
        return;
    };
    if bot_user_id.is_some_and(|self_id| self_id == sender_id) {
        return;
    }
    let Some(channel_id) = data.get("channel_id").and_then(Value::as_str) else {
        return;
    };
    let guild_id = data
        .get("guild_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    let bot_cfg = bridge.get_bot_config(bot_id);
    if let (Some(cfg), Some(gid)) = (bot_cfg.as_ref(), guild_id.as_deref())
        && !cfg.connectors.discord.allowed_guild_ids.is_empty()
        && !cfg
            .connectors
            .discord
            .allowed_guild_ids
            .iter()
            .any(|allowed| allowed == gid)
    {
        return;
    }

    let mut channel_type = data.get("channel_type").and_then(Value::as_u64);
    let thread_obj_parent_id = data
        .get("thread")
        .and_then(|t| t.get("parent_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let raw_content = data
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mentions_array_hit = bot_user_id.is_some_and(|uid| {
        data.get("mentions")
            .and_then(Value::as_array)
            .is_some_and(|arr| {
                arr.iter()
                    .any(|m| m.get("id").and_then(Value::as_str) == Some(uid))
            })
    });
    let mentions_text_hit = bot_user_id.is_some_and(|uid| {
        raw_content.contains(&format!("<@{uid}>")) || raw_content.contains(&format!("<@!{uid}>"))
    });
    let mentions_bot = mentions_array_hit || mentions_text_hit;

    // Short-circuit unmentioned guild messages outside participated threads BEFORE calling
    // `GET /channels/{id}` so busy servers never trigger spurious REST calls or 429 sleeps (CORE-15).
    if guild_id.is_some() && !mentions_bot && !participated_threads.contains(channel_id) {
        return;
    }

    let mut parent_id = thread_obj_parent_id.clone().or_else(|| {
        data.get("parent_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    });

    // Regular guild text channels under a category also carry `parent_id`, so resolve channel
    // metadata whenever `channel_type` is absent and no explicit `thread` object exists (MB-12).
    if guild_id.is_some() && channel_type.is_none() && thread_obj_parent_id.is_none() {
        let (fetched_type, fetched_parent) = sender.resolve_channel_meta(channel_id);
        channel_type = fetched_type;
        if fetched_parent.is_some() {
            parent_id = fetched_parent;
        }
    }

    let is_thread = matches!(channel_type, Some(10..=12))
        || (channel_type.is_none() && thread_obj_parent_id.is_some());
    let gate_channel_id = if is_thread {
        parent_id.as_deref().unwrap_or(channel_id)
    } else {
        channel_id
    };

    if let Some(ref cfg) = bot_cfg
        && guild_id.is_some()
        && !cfg.connectors.discord.allowed_channel_ids.is_empty()
        && !cfg
            .connectors
            .discord
            .allowed_channel_ids
            .iter()
            .any(|allowed| allowed == gate_channel_id || allowed == channel_id)
    {
        return;
    }

    if guild_id.is_some() {
        let in_participated_thread = is_thread && participated_threads.contains(channel_id);
        if !mentions_bot && !in_participated_thread {
            return;
        }
    }

    let cleaned = strip_discord_bot_mention(raw_content, bot_user_id);
    if cleaned.is_empty() {
        return;
    }

    let thread_id = is_thread.then(|| channel_id.to_owned());
    let sender_name = author
        .get("global_name")
        .or_else(|| author.get("username"))
        .and_then(Value::as_str)
        .unwrap_or(sender_id);
    let msg_id = data
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let acts_for = bot_cfg.map_or_else(Default::default, |b| b.connectors.discord.acts_for);

    let target = DeliveryTarget {
        source: ExternalSource::Discord,
        workspace_id: guild_id.clone(),
        channel_id: gate_channel_id.to_owned(),
        thread_id: thread_id.clone(),
        reply_to_message_id: msg_id.clone(),
    };
    let inbound = InboundExternalMessage {
        bot_id: bot_id.clone(),
        source: ExternalSource::Discord,
        workspace_id: guild_id,
        channel_id: gate_channel_id.to_owned(),
        thread_id,
        sender_id: sender_id.to_owned(),
        sender_name: sender_name.to_owned(),
        text: cleaned,
        attachments: Vec::new(),
        external_message_id: msg_id,
        acts_for,
    };
    let outcome = router.handle_inbound_message(inbound, target, sender.as_ref());
    if is_thread && matches!(outcome, InboundRouteOutcome::Dispatched(_)) {
        if participated_threads.len() >= 1_024
            && let Some(first) = participated_threads.iter().next().cloned()
        {
            participated_threads.remove(&first);
        }
        participated_threads.insert(channel_id.to_owned());
    }
}

fn handle_interaction_create(
    bot_id: &BotId,
    data: &Value,
    sender: &Arc<DiscordSender>,
    router: &Arc<ConversationRouter>,
) {
    // Message component interaction (buttons / select menus) = type 3.
    if data.get("type").and_then(Value::as_u64) != Some(3) {
        return;
    }
    let Some(interaction_id) = data.get("id").and_then(Value::as_str) else {
        return;
    };
    let Some(interaction_token) = data.get("token").and_then(Value::as_str) else {
        return;
    };
    let channel_id = data
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let guild_id = data.get("guild_id").and_then(Value::as_str);
    let mut channel_type = data
        .get("channel")
        .and_then(|c| c.get("type"))
        .or_else(|| data.get("channel_type"))
        .and_then(Value::as_u64);
    let thread_obj_parent_id = data
        .get("thread")
        .and_then(|t| t.get("parent_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let mut parent_id = thread_obj_parent_id.clone().or_else(|| {
        data.get("channel")
            .and_then(|c| c.get("parent_id"))
            .or_else(|| data.get("parent_id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    });
    if guild_id.is_some()
        && !channel_id.is_empty()
        && channel_type.is_none()
        && thread_obj_parent_id.is_none()
    {
        let (fetched_type, fetched_parent) = sender.resolve_channel_meta(channel_id);
        channel_type = fetched_type;
        if fetched_parent.is_some() {
            parent_id = fetched_parent;
        }
    }
    let is_thread = matches!(channel_type, Some(10..=12))
        || (channel_type.is_none() && thread_obj_parent_id.is_some());
    let gate_channel_id = if is_thread {
        parent_id.as_deref().unwrap_or(channel_id)
    } else {
        channel_id
    };
    let thread_id = is_thread.then_some(channel_id);

    let user = data
        .get("member")
        .and_then(|m| m.get("user"))
        .or_else(|| data.get("user"));
    let Some(user) = user else {
        return;
    };
    let Some(sender_id) = user.get("id").and_then(Value::as_str) else {
        return;
    };
    let sender_name = user
        .get("global_name")
        .or_else(|| user.get("username"))
        .and_then(Value::as_str)
        .unwrap_or(sender_id);
    let custom_id = data
        .get("data")
        .and_then(|d| d.get("custom_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let outcome = router.handle_gate_callback_with_context(
        bot_id,
        ExternalSource::Discord,
        guild_id,
        sender_id,
        sender_name,
        gate_channel_id,
        thread_id,
        custom_id,
    );
    sender
        .respond_interaction(
            interaction_id,
            interaction_token,
            &outcome.user_notice(),
            true,
        )
        .ok();
}

fn append_gateway_query(base: &str) -> String {
    let mut out = if base.contains('?') {
        base.to_owned()
    } else if base.ends_with('/') {
        format!("{base}?")
    } else {
        format!("{base}/?")
    };
    let query = out.split('?').nth(1).unwrap_or_default().to_owned();
    let mut has_v = false;
    let mut has_enc = false;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let key = pair.split('=').next().unwrap_or_default();
        if key == "v" {
            has_v = true;
        } else if key == "encoding" {
            has_enc = true;
        }
    }
    if !has_v {
        if !out.ends_with('?') && !out.ends_with('&') {
            out.push('&');
        }
        out.push_str("v=10");
    }
    if !has_enc {
        if !out.ends_with('?') && !out.ends_with('&') {
            out.push('&');
        }
        out.push_str("encoding=json");
    }
    out
}

fn strip_discord_bot_mention(text: &str, bot_user_id: Option<&str>) -> String {
    let mut s = text.trim().to_owned();
    if let Some(uid) = bot_user_id {
        for tok in [format!("<@{uid}>"), format!("<@!{uid}>")] {
            if s.contains(&tok) {
                s = s.replace(&tok, " ");
            }
        }
    }
    let normalized = s.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized
        .trim_start_matches([':', ',', '-'])
        .trim()
        .to_owned()
}

fn discord_random_u64() -> u64 {
    let mut bytes = [0u8; 8];
    if SystemRandom::new().fill(&mut bytes).is_ok() {
        return u64::from_le_bytes(bytes);
    }
    now_ms()
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
        source: ExternalSource::Discord,
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

    struct DiscordMockState {
        posted: Vec<(String, Value)>,
        patched: Vec<(String, Value)>,
        interactions: Vec<(String, Value)>,
        identified: Vec<Value>,
        resumed: Vec<Value>,
        heartbeats: usize,
    }

    fn read_http(stream: &mut std::net::TcpStream) -> (String, String, String, Vec<u8>) {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let end = loop {
            if let Some(p) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break p;
            }
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => return (String::new(), String::new(), String::new(), Vec::new()),
                Ok(n) => raw.extend_from_slice(&buf[..n]),
            }
        };
        let head = String::from_utf8_lossy(&raw[..end]).into_owned();
        let mut req_line = head.lines().next().unwrap_or_default().split_whitespace();
        let method = req_line.next().unwrap_or_default().to_owned();
        let path = req_line.next().unwrap_or_default().to_owned();
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
        (method, path, auth, body)
    }

    struct BridgeForDiscord {
        state_dir: std::path::PathBuf,
        store: Arc<BotStore>,
        secrets: Arc<SecretVault>,
        dispatched: Mutex<Vec<InboundExternalMessage>>,
    }

    impl ConnectorBridge for BridgeForDiscord {
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
                .bind_link_session(&link.id, SessionId(99), now_ms())?
                .unwrap_or(link);
            lock(&self.dispatched).push(msg);
            Ok(DispatchOutcome {
                link,
                session_id: SessionId(99),
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
    fn discord_gateway_e2e_hello_identify_heartbeat_ready_message_interaction_and_resume() {
        let dir = std::env::temp_dir().join(format!("krill-discord-e2e-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let ws_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let http_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        http_listener.set_nonblocking(true).unwrap();
        let http_addr = http_listener.local_addr().unwrap();

        let state = Arc::new(Mutex::new(DiscordMockState {
            posted: Vec::new(),
            patched: Vec::new(),
            interactions: Vec::new(),
            identified: Vec::new(),
            resumed: Vec::new(),
            heartbeats: 0,
        }));
        let stop = Arc::new(StopFlag::default());

        let gate = OrchestrationStore::mutate(&dir, |s| {
            s.create_gate(
                "worker-dc",
                "Deploy canary?",
                vec!["deploy".to_owned(), "hold".to_owned()],
                None,
                None,
            )
        })
        .unwrap();

        // Loopback HTTP server for Discord REST v10.
        let http_state = Arc::clone(&state);
        let http_stop = Arc::clone(&stop);
        let gw_ws_url = format!("ws://{ws_addr}");
        let http_thread = thread::spawn(move || {
            let mut next_id = 500_u64;
            while !http_stop.is_stopped() {
                match http_listener.accept() {
                    Ok((mut sock, _)) => {
                        sock.set_nonblocking(false).ok();
                        sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        let (method, path, auth, body) = read_http(&mut sock);
                        let req_json: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
                        let (status, resp) = if method == "GET" && path == "/gateway/bot" {
                            assert_eq!(auth, "Bot dc-test-token");
                            (200, json!({"url": gw_ws_url}))
                        } else if method == "POST" && path.contains("/interactions/") {
                            lock(&http_state).interactions.push((path, req_json));
                            (204, Value::Null)
                        } else if method == "POST" && path.ends_with("/messages") {
                            assert_eq!(auth, "Bot dc-test-token");
                            next_id += 1;
                            lock(&http_state).posted.push((path, req_json));
                            (200, json!({"id": next_id.to_string()}))
                        } else if method == "PATCH" && path.contains("/messages/") {
                            assert_eq!(auth, "Bot dc-test-token");
                            lock(&http_state).patched.push((path, req_json));
                            (200, json!({"id": "501"}))
                        } else {
                            (404, json!({"message": "not found"}))
                        };
                        let payload = if status == 204 {
                            Vec::new()
                        } else {
                            serde_json::to_vec(&resp).unwrap()
                        };
                        let header = format!(
                            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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

        // Loopback Discord Gateway v10 server accepting initial Identify + Reconnect -> Resume.
        let ws_state = Arc::clone(&state);
        let resume_url = format!("ws://{ws_addr}/resume");
        let gate_id_clone = gate.id.clone();
        let ws_thread = thread::spawn(move || {
            // Connection #1: Hello -> Identify -> Heartbeat/ACK -> READY -> MESSAGE_CREATE -> INTERACTION_CREATE -> op 7 Reconnect.
            let (mut sock1, _) = ws_listener.accept().unwrap();
            let target1 = accept_ws_test_handshake(&mut sock1).unwrap();
            assert!(target1.contains("v=10&encoding=json"));
            sock1.set_read_timeout(Some(Duration::from_secs(5))).ok();

            // Send Hello with a 1500ms heartbeat interval so the initial jittered heartbeat
            // fires promptly without triggering missed-ACK disconnects under Windows CI jitter.
            write_server_frame(
                &mut sock1,
                true,
                1,
                br#"{"op":10,"d":{"heartbeat_interval":1500}}"#,
            )
            .unwrap();

            // Receive Identify (and possibly Heartbeat).
            let mut got_identify = false;
            let mut got_hb = false;
            while !(got_identify && got_hb) {
                let (_, body) = read_client_frame(&mut sock1).unwrap();
                let frame: Value = serde_json::from_slice(&body).unwrap();
                match frame.get("op").and_then(Value::as_u64) {
                    Some(OP_IDENTIFY) => {
                        lock(&ws_state).identified.push(frame);
                        got_identify = true;
                        let ready = json!({
                            "op": 0,
                            "s": 1,
                            "t": "READY",
                            "d": {
                                "session_id": "sess-xyz",
                                "resume_gateway_url": resume_url,
                                "user": {"id": "900", "username": "krill_dc"}
                            }
                        })
                        .to_string();
                        write_server_frame(&mut sock1, true, 1, ready.as_bytes()).unwrap();
                    }
                    Some(OP_HEARTBEAT) => {
                        lock(&ws_state).heartbeats += 1;
                        got_hb = true;
                        write_server_frame(&mut sock1, true, 1, br#"{"op":11,"d":null}"#).unwrap();
                    }
                    _ => {}
                }
            }

            // Bot self message (must be ignored).
            let self_msg = json!({
                "op": 0,
                "s": 2,
                "t": "MESSAGE_CREATE",
                "d": {
                    "id": "m-self",
                    "channel_id": "ch-1",
                    "guild_id": "g-1",
                    "author": {"id": "900", "username": "krill_dc", "bot": true},
                    "content": "ignored self message"
                }
            })
            .to_string();
            write_server_frame(&mut sock1, true, 1, self_msg.as_bytes()).unwrap();

            // Real human message from allowed user.
            let user_msg = json!({
                "op": 0,
                "s": 3,
                "t": "MESSAGE_CREATE",
                "d": {
                    "id": "m-user",
                    "channel_id": "ch-1",
                    "guild_id": "g-1",
                    "author": {"id": "u-42", "username": "owen", "bot": false},
                    "content": "<@900> status report"
                }
            })
            .to_string();
            write_server_frame(&mut sock1, true, 1, user_msg.as_bytes()).unwrap();

            // Component INTERACTION_CREATE for gate approval.
            let interaction = json!({
                "op": 0,
                "s": 4,
                "t": "INTERACTION_CREATE",
                "d": {
                    "id": "int-77",
                    "token": "int-tok-77",
                    "type": 3,
                    "channel_id": "ch-1",
                    "member": {"user": {"id": "u-42", "username": "owen"}},
                    "data": {"custom_id": format!("gate:{gate_id_clone}:deploy")}
                }
            })
            .to_string();
            write_server_frame(&mut sock1, true, 1, interaction.as_bytes()).unwrap();

            // Wait until the client has processed s=4 (INTERACTION_CREATE) and posted the callback
            // before triggering op 7 Reconnect, avoiding seq=3/4 races under CI scheduling jitter.
            let wait_deadline = Instant::now() + Duration::from_secs(5);
            while lock(&ws_state).interactions.is_empty() && Instant::now() < wait_deadline {
                thread::sleep(Duration::from_millis(10));
            }
            write_server_frame(&mut sock1, true, 1, br#"{"op":7,"d":null}"#).unwrap();
            let (close_op, close_body) = read_client_frame(&mut sock1).unwrap();
            assert_eq!(
                close_op, 8,
                "expected WebSocket close frame on OP_RECONNECT"
            );
            assert!(close_body.len() >= 2);
            let close_code = u16::from_be_bytes([close_body[0], close_body[1]]);
            assert!(
                close_code != 1000 && close_code != 1001,
                "OP_RECONNECT must not close with 1000/1001 (got {close_code}), which invalidates OP_RESUME"
            );
            drop(sock1);

            // Connection #2: Client reconnects to resume_gateway_url and sends op 6 Resume with seq=4!
            let (mut sock2, _) = ws_listener.accept().unwrap();
            let target2 = accept_ws_test_handshake(&mut sock2).unwrap();
            assert!(target2.starts_with("/resume"));
            sock2.set_read_timeout(Some(Duration::from_secs(5))).ok();
            write_server_frame(
                &mut sock2,
                true,
                1,
                br#"{"op":10,"d":{"heartbeat_interval":5000}}"#,
            )
            .unwrap();
            let (_, body2) = read_client_frame(&mut sock2).unwrap();
            let resume_frame: Value = serde_json::from_slice(&body2).unwrap();
            lock(&ws_state).resumed.push(resume_frame);
            write_server_frame(
                &mut sock2,
                true,
                1,
                br#"{"op":0,"s":5,"t":"RESUMED","d":{}}"#,
            )
            .unwrap();

            // Wait for graceful shutdown close.
            read_client_frame(&mut sock2).ok();
        });

        let bots_root = dir.join("bots");
        let store = Arc::new(BotStore::new(bots_root.clone()));
        let secrets = Arc::new(SecretVault::with_static_key(bots_root, [0x44; 32]));
        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "Discord Bot".to_owned(),
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
        connectors.discord.enabled = true;
        connectors.discord.allowed_guild_ids = vec!["g-1".to_owned()];
        connectors.discord.allowed_user_ids = vec!["u-42".to_owned()];
        store
            .set_connector_config(&bot.id, connectors, 1_100)
            .unwrap();
        secrets
            .set_secret(
                &bot.id,
                "discord",
                Zeroizing::new("dc-test-token".to_owned()),
            )
            .unwrap();

        let bridge = Arc::new(BridgeForDiscord {
            state_dir: dir.clone(),
            store: Arc::clone(&store),
            secrets: Arc::clone(&secrets),
            dispatched: Mutex::new(Vec::new()),
        });
        let mgr = ConnectorManager::with_endpoints(
            Arc::clone(&bridge) as Arc<dyn ConnectorBridge>,
            ConnectorEndpoints {
                discord: format!("http://{http_addr}"),
                ..ConnectorEndpoints::default()
            },
        );
        mgr.reload(None).unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        while (lock(&state).resumed.is_empty()
            || lock(&state).interactions.is_empty()
            || lock(&bridge.dispatched).is_empty())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(lock(&state).identified.len(), 1);
        assert!(lock(&state).heartbeats >= 1);
        assert_eq!(lock(&bridge.dispatched).len(), 1);
        assert_eq!(lock(&bridge.dispatched)[0].text, "status report");

        // Verify op 6 Resume payload carried session_id and seq=4.
        assert_eq!(lock(&state).resumed.len(), 1);
        assert_eq!(lock(&state).resumed[0]["op"], OP_RESUME);
        assert_eq!(lock(&state).resumed[0]["d"]["session_id"], "sess-xyz");
        assert_eq!(lock(&state).resumed[0]["d"]["seq"], 4);

        // Verify interaction callback + gate resolution.
        assert_eq!(lock(&state).interactions.len(), 1);
        assert_eq!(
            lock(&state).interactions[0].0,
            "/interactions/int-77/int-tok-77/callback"
        );
        let orch = OrchestrationStore::load(&dir).unwrap();
        assert_eq!(orch.gates[0].decision.as_deref(), Some("deploy"));

        // Verify REST POST + PATCH activity and 2,000-char chunked final reply.
        mgr.router()
            .on_turn_activity_at(&bot.id, SessionId(99), "Using Git...", 30_000);
        mgr.router()
            .on_turn_activity_at(&bot.id, SessionId(99), "Formatting...", 33_500);
        assert_eq!(lock(&state).posted.len(), 1);
        assert_eq!(lock(&state).patched.len(), 1);
        assert_eq!(lock(&state).patched[0].1["content"], "Formatting...");

        lock(&state).posted.clear();
        let big = format!("{}\n\n{}", "X".repeat(1_800), "Y".repeat(600));
        mgr.on_turn_complete(&bot.id, SessionId(99), Some(&big));
        assert_eq!(lock(&state).posted.len(), 2);

        mgr.shutdown();
        stop.signal();
        ws_thread.join().unwrap();
        http_thread.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discord_429_retry_query_params_multipart_and_reaction_work() {
        assert_eq!(
            append_gateway_query("wss://gateway.discord.gg?v=10"),
            "wss://gateway.discord.gg?v=10&encoding=json"
        );
        assert_eq!(
            strip_discord_bot_mention("yo <@!900> help", Some("900")),
            "yo help"
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = std::env::temp_dir().join(format!("krill-dc-w3-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let fpath = dir.join("log.txt");
        std::fs::write(&fpath, b"discord attachment").unwrap();

        let srv = thread::spawn(move || {
            for idx in 0..4 {
                let (mut sock, _) = listener.accept().unwrap();
                let (method, path, _auth, body) = read_http(&mut sock);
                let (status, resp_body) = if idx == 0 {
                    (
                        "429 Too Many Requests",
                        r#"{"retry_after":0.05}"#.to_owned(),
                    )
                } else if idx == 1 {
                    assert_eq!(method, "GET");
                    assert_eq!(path, "/users/@me");
                    ("200 OK", r#"{"id":"900","username":"krill_dc"}"#.to_owned())
                } else if idx == 2 {
                    assert_eq!(method, "POST");
                    assert!(String::from_utf8_lossy(&body).contains("discord attachment"));
                    ("200 OK", r#"{"id":"dc-file-msg-1"}"#.to_owned())
                } else {
                    assert_eq!(method, "PUT");
                    assert!(path.contains("/reactions/"));
                    ("204 No Content", String::new())
                };
                let wire = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
            }
        });

        let sender =
            DiscordSender::new(&format!("http://{addr}"), Zeroizing::new("tok".to_owned()));
        let test = sender.test_connection();
        assert!(test.ok);
        assert_eq!(test.account_label.as_deref(), Some("@krill_dc"));

        let target = DeliveryTarget {
            source: ExternalSource::Discord,
            workspace_id: Some("g-1".to_owned()),
            channel_id: "ch-1".to_owned(),
            thread_id: Some("th-9".to_owned()),
            reply_to_message_id: Some("m-1".to_owned()),
        };
        let fid = sender
            .send_files(
                &target,
                &[fpath.to_string_lossy().into_owned()],
                Some("logs"),
            )
            .unwrap();
        assert_eq!(fid.as_deref(), Some("dc-file-msg-1"));
        sender.add_reaction(&target, "m-1", "🔥").unwrap();

        srv.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discord_does_not_cache_transient_channel_meta_error_encodes_paths_and_only_tracks_dispatched_threads()
     {
        struct DenyBridge(BotConfig);
        impl ConnectorBridge for DenyBridge {
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
        let seen_paths = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_bg = Arc::clone(&seen_paths);

        let srv = thread::spawn(move || {
            for idx in 0..3 {
                let (mut sock, _) = listener.accept().unwrap();
                let (_method, path, _auth, _body) = read_http(&mut sock);
                lock(&seen_bg).push(path);
                let (status, body) = if idx == 0 {
                    // First resolve_channel_meta fails transiently.
                    ("500 Internal Server Error", r#"{"message":"oops"}"#)
                } else if idx == 1 {
                    // Retry must hit the server again and succeed!
                    (
                        "200 OK",
                        r#"{"id":"th-1","type":11,"parent_id":"ch-parent"}"#,
                    )
                } else {
                    ("200 OK", r#"{"id":"m-enc"}"#)
                };
                let wire = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
            }
        });

        let sender = Arc::new(DiscordSender::new(
            &format!("http://{addr}"),
            Zeroizing::new("tok".to_owned()),
        ));

        // Transient error returns (None, None) and is NOT cached.
        assert_eq!(sender.resolve_channel_meta("th-1"), (None, None));
        // Next call retries and gets real thread metadata, which IS cached.
        assert_eq!(
            sender.resolve_channel_meta("th-1"),
            (Some(11), Some("ch-parent".to_owned()))
        );
        // Third call hits cache (no extra HTTP request).
        assert_eq!(
            sender.resolve_channel_meta("th-1"),
            (Some(11), Some("ch-parent".to_owned()))
        );

        // Path parameters with special chars are percent-encoded.
        let enc_target = DeliveryTarget {
            source: ExternalSource::Discord,
            workspace_id: Some("g1".to_owned()),
            channel_id: "ch/1?x=2".to_owned(),
            thread_id: None,
            reply_to_message_id: None,
        };
        assert_eq!(sender.send_message(&enc_target, "hi").unwrap(), "m-enc");

        srv.join().unwrap();
        let paths = lock(&seen_paths);
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[2], "/channels/ch%2F1%3Fx%3D2/messages");

        // D-23c: Denied/paused thread mention must NOT add thread to participated_threads.
        let bot_id = BotId::new("dc-part").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "DcPart", "claude", 1_000).unwrap();
        bot.paused = true;
        let bridge: Arc<dyn ConnectorBridge> = Arc::new(DenyBridge(bot));
        let router = Arc::new(ConversationRouter::new(Arc::clone(&bridge)));
        let mut participated = BTreeSet::new();
        let event = json!({
            "id": "m-1",
            "guild_id": "g-1",
            "channel_id": "th-99",
            "channel_type": 11,
            "parent_id": "ch-1",
            "content": "<@900> hello",
            "author": { "id": "u-evil", "username": "evil" },
            "mentions": [{ "id": "900" }]
        });
        handle_message_create(
            &bot_id,
            Some("900"),
            &event,
            &sender,
            &bridge,
            &router,
            &mut participated,
        );
        assert!(participated.is_empty());
    }

    #[test]
    fn discord_categorized_guild_channel_is_not_misclassified_as_thread() {
        // MB-12: A standard guild text channel (`type: 0`) inside a category has `parent_id = cat-1`.
        // It must NOT be treated as a thread, must keep `channel_id = "ch-general"` (not "cat-1"),
        // must have `thread_id = None`, and must NOT enter `participated_threads`.
        struct CaptureBridge {
            bot: BotConfig,
            dispatched: Mutex<Vec<InboundExternalMessage>>,
        }
        impl ConnectorBridge for CaptureBridge {
            fn dispatch_external_message(
                &self,
                msg: InboundExternalMessage,
            ) -> io::Result<DispatchOutcome> {
                let link = krill_proto::bots::ExternalConversationLink {
                    id: "link-dc-cat".to_owned(),
                    bot_id: msg.bot_id.clone(),
                    source: msg.source,
                    workspace_id: msg.workspace_id.clone(),
                    channel_id: msg.channel_id.clone(),
                    thread_id: msg.thread_id.clone(),
                    session_id: Some(SessionId(55)),
                    acts_for: msg.acts_for.clone(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                };
                lock(&self.dispatched).push(msg);
                Ok(DispatchOutcome {
                    link,
                    session_id: SessionId(55),
                    started_new_session: false,
                })
            }
            fn check_or_request_access(
                &self,
                _: &BotId,
                _: ExternalSource,
                _: &str,
                _: &str,
                _: &str,
            ) -> io::Result<AccessDecision> {
                Ok(AccessDecision::Allowed)
            }
            fn resolve_secret(
                &self,
                _: &BotId,
                _: Option<&str>,
                _: &str,
            ) -> io::Result<Option<Zeroizing<String>>> {
                Ok(None)
            }
            fn lookup_or_create_link(
                &self,
                bot_id: &BotId,
                source: ExternalSource,
                workspace_id: Option<&str>,
                channel_id: &str,
                thread_id: Option<&str>,
                acts_for: krill_proto::bots::ActsFor,
            ) -> io::Result<krill_proto::bots::ExternalConversationLink> {
                Ok(krill_proto::bots::ExternalConversationLink {
                    id: "link-dc-cat".to_owned(),
                    bot_id: bot_id.clone(),
                    source,
                    workspace_id: workspace_id.map(ToOwned::to_owned),
                    channel_id: channel_id.to_owned(),
                    thread_id: thread_id.map(ToOwned::to_owned),
                    session_id: Some(SessionId(55)),
                    acts_for,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                })
            }
            fn reply_orchestration_gate(
                &self,
                _: &str,
                _: &str,
                _: Option<String>,
                _: &str,
            ) -> io::Result<()> {
                Ok(())
            }
            fn update_connector_status(&self, _: BotConnectorStatus) {}
            fn get_bot_config(&self, bot_id: &BotId) -> Option<BotConfig> {
                (&self.bot.id == bot_id).then(|| self.bot.clone())
            }
            fn list_bots(&self) -> Vec<BotConfig> {
                vec![self.bot.clone()]
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).unwrap_or(0);
            assert!(n > 0);
            let body = r#"{"id":"ch-general","type":0,"parent_id":"cat-999"}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(wire.as_bytes()).unwrap();
        });

        let bot_id = BotId::new("dc-cat").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "DcCat", "claude", 1_000).unwrap();
        bot.connectors.discord.enabled = true;
        bot.connectors.discord.allowed_guild_ids = vec!["g-1".to_owned()];
        bot.connectors.discord.allowed_channel_ids = vec!["ch-general".to_owned()];

        let bridge = Arc::new(CaptureBridge {
            bot,
            dispatched: Mutex::new(Vec::new()),
        });
        let bridge_dyn: Arc<dyn ConnectorBridge> = bridge.clone();
        let router = Arc::new(ConversationRouter::new(Arc::clone(&bridge_dyn)));
        let sender = Arc::new(DiscordSender::new(
            &format!("http://{addr}"),
            Zeroizing::new("tok".to_owned()),
        ));

        let mut participated = BTreeSet::new();
        // Payload omits `channel_type`, so DiscordSender fetches `GET /channels/ch-general`
        // returning `type: 0, parent_id: "cat-999"`.
        let mention_ev = json!({
            "id": "m-cat-1",
            "guild_id": "g-1",
            "channel_id": "ch-general",
            "content": "<@900> hello channel",
            "author": { "id": "u-1", "username": "alice" },
            "mentions": [{ "id": "900" }]
        });
        handle_message_create(
            &bot_id,
            Some("900"),
            &mention_ev,
            &sender,
            &bridge_dyn,
            &router,
            &mut participated,
        );
        srv.join().unwrap();

        let dispatched = lock(&bridge.dispatched);
        assert_eq!(dispatched.len(), 1);
        assert_eq!(dispatched[0].channel_id, "ch-general");
        assert_eq!(dispatched[0].thread_id, None);
        assert!(
            participated.is_empty(),
            "regular guild text channel must NOT be added to participated_threads"
        );
        drop(dispatched);

        // Follow-up unmentioned message in the same regular channel MUST be ignored.
        let unmentioned_ev = json!({
            "id": "m-cat-2",
            "guild_id": "g-1",
            "channel_id": "ch-general",
            "content": "random chatter without mention",
            "author": { "id": "u-1", "username": "alice" },
            "mentions": []
        });
        handle_message_create(
            &bot_id,
            Some("900"),
            &unmentioned_ev,
            &sender,
            &bridge_dyn,
            &router,
            &mut participated,
        );
        assert_eq!(lock(&bridge.dispatched).len(), 1);
    }

    #[test]
    fn r2m17_hello_timeout_disconnects_and_repeated_op_reconnect_applies_backoff() {
        struct StatusBridge {
            bot: BotConfig,
            statuses: Mutex<Vec<BotConnectorStatus>>,
        }
        impl ConnectorBridge for StatusBridge {
            fn dispatch_external_message(
                &self,
                _: InboundExternalMessage,
            ) -> io::Result<crate::bots::connectors::DispatchOutcome> {
                Err(io::Error::other("unused"))
            }
            fn check_or_request_access(
                &self,
                _: &BotId,
                _: ExternalSource,
                _: &str,
                _: &str,
                _: &str,
            ) -> io::Result<crate::bots::connectors::AccessDecision> {
                Ok(crate::bots::connectors::AccessDecision::Allowed)
            }
            fn lookup_or_create_link(
                &self,
                _: &BotId,
                _: ExternalSource,
                _: Option<&str>,
                _: &str,
                _: Option<&str>,
                _: krill_proto::bots::ActsFor,
            ) -> io::Result<krill_proto::bots::ExternalConversationLink> {
                Err(io::Error::other("unused"))
            }
            fn reply_orchestration_gate(
                &self,
                _: &str,
                _: &str,
                _: Option<String>,
                _: &str,
            ) -> io::Result<()> {
                Ok(())
            }
            fn update_connector_status(&self, st: BotConnectorStatus) {
                lock(&self.statuses).push(st);
            }
            fn get_bot_config(&self, bot_id: &BotId) -> Option<BotConfig> {
                (&self.bot.id == bot_id).then(|| self.bot.clone())
            }
            fn list_bots(&self) -> Vec<BotConfig> {
                vec![self.bot.clone()]
            }
            fn resolve_secret(
                &self,
                _: &BotId,
                _: Option<&str>,
                _: &str,
            ) -> io::Result<Option<Zeroizing<String>>> {
                Ok(None)
            }
        }

        // 1. Pre-OP_HELLO stall must disconnect after hello_timeout instead of hanging forever.
        let ws_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ws_addr = ws_listener.local_addr().unwrap();
        let rest_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let rest_addr = rest_listener.local_addr().unwrap();
        let stop_servers = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_rest = Arc::clone(&stop_servers);
        let stop_ws = Arc::clone(&stop_servers);

        let rest_srv = thread::spawn(move || {
            rest_listener.set_nonblocking(true).ok();
            while !stop_rest.load(std::sync::atomic::Ordering::Acquire) {
                let Ok((mut sock, _)) = rest_listener.accept() else {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                };
                sock.set_nonblocking(false).ok();
                let mut buf = [0u8; 2048];
                if sock.read(&mut buf).unwrap_or(0) == 0 {
                    continue;
                }
                let body = format!(r#"{{"url":"ws://{ws_addr}"}}"#);
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).ok();
            }
        });

        let ws_accepts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ws_accepts_bg = Arc::clone(&ws_accepts);
        let ws_srv = thread::spawn(move || {
            ws_listener.set_nonblocking(true).ok();
            let mut held_sockets = Vec::new();
            while !stop_ws.load(std::sync::atomic::Ordering::Acquire) {
                let Ok((mut sock, _)) = ws_listener.accept() else {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                };
                sock.set_nonblocking(false).ok();
                if accept_ws_test_handshake(&mut sock).is_ok() {
                    ws_accepts_bg.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    // Hold socket open without EVER sending OP_HELLO!
                    held_sockets.push(sock);
                }
            }
        });

        let bot_id = BotId::new("dc-hello-to").unwrap();
        let mut bot = BotConfig::new(bot_id, "DcHello", "claude", 1_000).unwrap();
        bot.connectors.discord.enabled = true;
        let bridge = Arc::new(StatusBridge {
            bot: bot.clone(),
            statuses: Mutex::new(Vec::new()),
        });
        let bridge_dyn: Arc<dyn ConnectorBridge> = bridge.clone();
        let router = Arc::new(ConversationRouter::new(Arc::clone(&bridge_dyn)));
        let sender = Arc::new(DiscordSender::new(
            &format!("http://{rest_addr}"),
            Zeroizing::new("tok".to_owned()),
        ));
        let stop = Arc::new(StopFlag::default());
        let stop_bg = Arc::clone(&stop);
        let tok = Zeroizing::new("tok".to_owned());
        let worker = thread::spawn(move || {
            run_discord_loop_with_timeouts(
                &bot,
                &tok,
                &sender,
                &bridge_dyn,
                &router,
                &stop_bg,
                Duration::from_millis(120),
            );
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if lock(&bridge.statuses).iter().any(|s| {
                s.last_error
                    .as_deref()
                    .is_some_and(|e| e.contains("HELLO timed out"))
            }) {
                break;
            }
            thread::sleep(Duration::from_millis(15));
        }
        stop.signal();
        stop_servers.store(true, std::sync::atomic::Ordering::Release);
        worker.join().unwrap();
        rest_srv.join().ok();
        ws_srv.join().ok();

        assert!(
            lock(&bridge.statuses).iter().any(|s| s
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("HELLO timed out"))),
            "pre-OP_HELLO stall must hit hello_deadline and report timeout"
        );

        // 2. Repeated OP_RECONNECT without READY/RESUMED must back off on >=2nd consecutive reconnect.
        let ws2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let ws2_addr = ws2.local_addr().unwrap();
        let rest2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let rest2_addr = rest2.local_addr().unwrap();
        let stop2_srv = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2_r = Arc::clone(&stop2_srv);
        let stop2_w = Arc::clone(&stop2_srv);
        let r2_thread = thread::spawn(move || {
            while !stop2_r.load(std::sync::atomic::Ordering::Acquire) {
                let Ok((mut sock, _)) = rest2.accept() else {
                    break;
                };
                if stop2_r.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let (method, _, _, _) = read_http(&mut sock);
                if method.is_empty() {
                    continue;
                }
                let body = format!(r#"{{"url":"ws://{ws2_addr}"}}"#);
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).ok();
                sock.flush().ok();
            }
        });
        let reconnect_times = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let reconnect_times_bg = Arc::clone(&reconnect_times);
        let w2_thread = thread::spawn(move || {
            // Keep accepted sockets open until shutdown so the client can finish reading
            // OP_HELLO/OP_RECONNECT and send OP_IDENTIFY + close code 4000 without hitting
            // a Windows loopback TCP RST (`WSAECONNRESET`) on a dropped server socket.
            let mut held_sockets = Vec::new();
            while !stop2_w.load(std::sync::atomic::Ordering::Acquire) {
                let Ok((mut sock, _)) = ws2.accept() else {
                    break;
                };
                if stop2_w.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                sock.set_nodelay(true).ok();
                sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
                if accept_ws_test_handshake(&mut sock).is_ok() {
                    lock(&reconnect_times_bg).push(Instant::now());
                    write_server_frame(
                        &mut sock,
                        true,
                        1,
                        br#"{"op":10,"d":{"heartbeat_interval":5000}}"#,
                    )
                    .ok();
                    // Immediately send OP_RECONNECT without READY/RESUMED.
                    write_server_frame(&mut sock, true, 1, br#"{"op":7,"d":null}"#).ok();
                    sock.flush().ok();
                    held_sockets.push(sock);
                }
            }
        });

        let bot2 = BotConfig::new(
            BotId::new("dc-reconn").unwrap(),
            "DcReconn",
            "claude",
            1_000,
        )
        .unwrap();
        let b2 = Arc::new(StatusBridge {
            bot: bot2.clone(),
            statuses: Mutex::new(Vec::new()),
        });
        let b2_dyn: Arc<dyn ConnectorBridge> = b2;
        let router2 = Arc::new(ConversationRouter::new(Arc::clone(&b2_dyn)));
        let sender2 = Arc::new(DiscordSender::new(
            &format!("http://{rest2_addr}"),
            Zeroizing::new("tok".to_owned()),
        ));
        let stop2 = Arc::new(StopFlag::default());
        let stop2_bg = Arc::clone(&stop2);
        let tok2 = Zeroizing::new("tok".to_owned());
        let worker2 = thread::spawn(move || {
            run_discord_loop_with_timeouts(
                &bot2,
                &tok2,
                &sender2,
                &b2_dyn,
                &router2,
                &stop2_bg,
                Duration::from_secs(5),
            );
        });
        let second_deadline = Instant::now() + Duration::from_secs(3);
        while lock(&reconnect_times).len() < 2 && Instant::now() < second_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        // After the 2nd consecutive OP_RECONNECT, backoff (1,000ms) must prevent a rapid 3rd connect.
        thread::sleep(Duration::from_millis(450));
        stop2.signal();
        stop2_srv.store(true, std::sync::atomic::Ordering::Release);
        std::net::TcpStream::connect(rest2_addr).ok();
        std::net::TcpStream::connect(ws2_addr).ok();
        worker2.join().unwrap();
        r2_thread.join().ok();
        w2_thread.join().ok();

        let times = lock(&reconnect_times);
        let count = times.len();
        assert!(
            count >= 2,
            "1st OP_RECONNECT must reconnect promptly (expected >=2 connections, got {count})"
        );
        let first_gap = times[1].saturating_duration_since(times[0]);
        assert!(
            first_gap < Duration::from_millis(850),
            "1st OP_RECONNECT must skip 1000ms backoff (gap was {first_gap:?})"
        );
        if count > 2 {
            let second_gap = times[2].saturating_duration_since(times[1]);
            assert!(
                second_gap >= Duration::from_millis(850),
                "2nd consecutive OP_RECONNECT must apply 1000ms backoff (gap was {second_gap:?}, total {count} connections)"
            );
        }
    }

    #[test]
    fn unmentioned_guild_message_triggers_zero_channel_meta_rest_calls() {
        struct QuietBridge {
            bot: BotConfig,
        }
        impl ConnectorBridge for QuietBridge {
            fn dispatch_external_message(
                &self,
                _: InboundExternalMessage,
            ) -> io::Result<crate::bots::connectors::DispatchOutcome> {
                Err(io::Error::other("unused"))
            }
            fn check_or_request_access(
                &self,
                _: &BotId,
                _: ExternalSource,
                _: &str,
                _: &str,
                _: &str,
            ) -> io::Result<crate::bots::connectors::AccessDecision> {
                Ok(crate::bots::connectors::AccessDecision::Allowed)
            }
            fn lookup_or_create_link(
                &self,
                _: &BotId,
                _: ExternalSource,
                _: Option<&str>,
                _: &str,
                _: Option<&str>,
                _: krill_proto::bots::ActsFor,
            ) -> io::Result<krill_proto::bots::ExternalConversationLink> {
                Err(io::Error::other("unused"))
            }
            fn reply_orchestration_gate(
                &self,
                _: &str,
                _: &str,
                _: Option<String>,
                _: &str,
            ) -> io::Result<()> {
                Ok(())
            }
            fn update_connector_status(&self, _: BotConnectorStatus) {}
            fn get_bot_config(&self, bot_id: &BotId) -> Option<BotConfig> {
                (&self.bot.id == bot_id).then(|| self.bot.clone())
            }
            fn list_bots(&self) -> Vec<BotConfig> {
                vec![self.bot.clone()]
            }
            fn resolve_secret(
                &self,
                _: &BotId,
                _: Option<&str>,
                _: &str,
            ) -> io::Result<Option<Zeroizing<String>>> {
                Ok(None)
            }
        }

        let rest_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rest_hits_srv = Arc::clone(&rest_hits);
        let rest_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let rest_addr = rest_listener.local_addr().unwrap();
        rest_listener.set_nonblocking(true).ok();
        let stop_srv = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_srv_bg = Arc::clone(&stop_srv);
        let rest_thread = thread::spawn(move || {
            while !stop_srv_bg.load(std::sync::atomic::Ordering::Acquire) {
                match rest_listener.accept() {
                    Ok((mut stream, _)) => {
                        rest_hits_srv.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                        let mut buf = [0u8; 1024];
                        if stream.read(&mut buf).unwrap_or(0) > 0 {
                            stream
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                                )
                                .ok();
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        let bot =
            BotConfig::new(BotId::new("dc-quiet").unwrap(), "DcQuiet", "claude", 1_000).unwrap();
        let bridge: Arc<dyn ConnectorBridge> = Arc::new(QuietBridge { bot: bot.clone() });
        let router = Arc::new(ConversationRouter::new(Arc::clone(&bridge)));
        let sender = Arc::new(DiscordSender::new(
            &format!("http://{rest_addr}"),
            Zeroizing::new("tok".to_owned()),
        ));
        let mut session = GatewaySession {
            session_id: Some("s1".to_owned()),
            resume_gateway_url: None,
            seq: Some(1),
            bot_user_id: Some("bot-uid-999".to_owned()),
            account_label: Some("DcQuiet".to_owned()),
        };
        let mut participated_threads = BTreeSet::new();
        let unmentioned_msg = json!({
            "id": "m-1",
            "guild_id": "g-1",
            "channel_id": "ch-busy-1",
            "content": "just regular guild chatter without mentioning the bot",
            "mentions": [],
            "author": { "id": "user-1", "username": "alice", "bot": false }
        });

        handle_dispatch(
            &bot.id,
            "MESSAGE_CREATE",
            &unmentioned_msg,
            &mut session,
            &sender,
            &bridge,
            &router,
            &mut participated_threads,
        );

        stop_srv.store(true, std::sync::atomic::Ordering::Release);
        rest_thread.join().ok();
        assert_eq!(
            rest_hits.load(std::sync::atomic::Ordering::Acquire),
            0,
            "unmentioned guild MESSAGE_CREATE must NOT trigger GET /channels/{{id}} REST calls"
        );
    }
}
