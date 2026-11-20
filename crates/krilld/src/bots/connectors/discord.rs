// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! 100% local Discord Gateway v10 (`wss://`) + REST API connector.
//!
//! Fetches gateway URL via `GET /gateway/bot`, connects via [`WsClient`], and implements the
//! full Gateway v10 lifecycle: op 10 `Hello`, op 1 `Heartbeat` / op 11 `Heartbeat ACK`, op 2
//! `Identify`, op 6 `Resume` (persisting `seq`, `session_id`, `resume_gateway_url`), op 7
//! `Reconnect`, op 9 `Invalid Session`, and op 0 `Dispatch` (`READY`, `RESUMED`,
//! `MESSAGE_CREATE`, `INTERACTION_CREATE`). Outbound replies use REST `POST`/`PATCH` with safe
//! 2,000-character chunking.

use std::io;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use krill_proto::bots::{
    BotConfig, BotConnectorStatus, BotErrorCategory, BotId, ConnectorHealthState, ExternalSource,
};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::bots::connectors::router::{
    ConversationRouter, DeliveryTarget, PlatformSender, classify_error, now_ms,
    sanitize_error_for_status,
};
use crate::bots::connectors::ws_client::{WsClient, WsEvent};
use crate::bots::connectors::{ConnectorBridge, InboundExternalMessage, StopFlag};
use crate::bots::http_client::{self, HttpMethod, HttpRequest};

/// Production Discord REST API v10 base URL.
pub const DEFAULT_DISCORD_API_BASE: &str = "https://discord.com/api/v10";
/// Discord's hard single-message limit (2,000 characters).
pub const DISCORD_MAX_MESSAGE_CHARS: usize = 2_000;
/// Gateway intents: `GUILDS` (1<<0) | `GUILD_MESSAGES` (1<<9) | `DIRECT_MESSAGES` (1<<12) | `MESSAGE_CONTENT` (1<<15).
pub const DISCORD_GATEWAY_INTENTS: u32 = (1 << 0) | (1 << 9) | (1 << 12) | (1 << 15);

const WORKER_STACK_SIZE: usize = 128 * 1024;
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const WS_POLL_SLICE: Duration = Duration::from_millis(100);

const OP_DISPATCH: u64 = 0;
const OP_HEARTBEAT: u64 = 1;
const OP_IDENTIFY: u64 = 2;
const OP_RESUME: u64 = 6;
const OP_RECONNECT: u64 = 7;
const OP_INVALID_SESSION: u64 = 9;
const OP_HELLO: u64 = 10;
const OP_HEARTBEAT_ACK: u64 = 11;

/// Discord REST API v10 client & [`PlatformSender`] implementation.
pub struct DiscordSender {
    api_base: String,
    bot_token: Zeroizing<String>,
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
        Self {
            api_base: api_base.trim_end_matches('/').to_owned(),
            bot_token,
        }
    }

    fn request_json(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<&Value>,
        with_auth: bool,
    ) -> io::Result<Value> {
        let url = format!("{}{path}", self.api_base);
        let mut req = HttpRequest::new(method, url).with_timeout(API_REQUEST_TIMEOUT);
        if with_auth {
            req = req.with_header("Authorization", format!("Bot {}", self.bot_token.as_str()));
        }
        if let Some(payload) = body {
            let encoded = serde_json::to_vec(payload).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "failed to encode Discord API payload",
                )
            })?;
            req = req
                .with_header("Content-Type", "application/json")
                .with_body(encoded);
        }
        let resp = http_client::send(&req)
            .map_err(|err| io::Error::new(err.kind(), "Discord REST transport error"))?;
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

    /// Sends an interaction callback response (type 4 `CHANNEL_MESSAGE_WITH_SOURCE`).
    pub fn respond_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        content: &str,
        ephemeral: bool,
    ) -> io::Result<()> {
        let path = format!("/interactions/{interaction_id}/{interaction_token}/callback");
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
        let path = format!("/channels/{channel}/messages");
        let payload = json!({ "content": text });
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
        let path = format!("/channels/{channel}/messages/{message_id}");
        let payload = json!({ "content": text });
        self.request_json(HttpMethod::Patch, &path, Some(&payload), true)?;
        Ok(())
    }
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
    let mut session = GatewaySession::default();
    let mut backoff_ms: u64 = 1_000;

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

        let raw_url = if session.can_resume()
            && let Some(ref resume_url) = session.resume_gateway_url
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
        let mut ws = match WsClient::connect(&gateway_url, &[], WS_CONNECT_TIMEOUT) {
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
        let mut awaiting_ack = false;
        let mut immediate_reconnect = false;
        let mut fatal_close = false;

        while !stop.is_stopped() {
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

            let poll_wait = next_heartbeat.map_or(WS_POLL_SLICE, |due| {
                due.saturating_duration_since(Instant::now())
                    .min(WS_POLL_SLICE)
            });
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
                            next_heartbeat = Some(Instant::now() + (interval / 2));
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
                            immediate_reconnect = true;
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
                            );
                            if event_name == "READY" || event_name == "RESUMED" {
                                backoff_ms = 1_000;
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

        ws.close();
        if stop.is_stopped() || fatal_close {
            break;
        }
        if !immediate_reconnect {
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

fn handle_dispatch(
    bot_id: &BotId,
    event_name: &str,
    data: &Value,
    session: &mut GatewaySession,
    sender: &Arc<DiscordSender>,
    bridge: &Arc<dyn ConnectorBridge>,
    router: &Arc<ConversationRouter>,
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

    let raw_content = data
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let cleaned = strip_discord_bot_mention(raw_content, bot_user_id);
    if cleaned.is_empty() {
        return;
    }

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
        channel_id: channel_id.to_owned(),
        thread_id: None,
        reply_to_message_id: msg_id.clone(),
    };
    let inbound = InboundExternalMessage {
        bot_id: bot_id.clone(),
        source: ExternalSource::Discord,
        workspace_id: guild_id,
        channel_id: channel_id.to_owned(),
        thread_id: None,
        sender_id: sender_id.to_owned(),
        sender_name: sender_name.to_owned(),
        text: cleaned,
        attachments: Vec::new(),
        external_message_id: msg_id,
        acts_for,
    };
    router.handle_inbound_message(inbound, target, sender.as_ref());
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

    let outcome = router.handle_gate_callback(
        bot_id,
        ExternalSource::Discord,
        sender_id,
        sender_name,
        channel_id,
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
    if base.contains("v=10") {
        return base.to_owned();
    }
    if base.contains('?') {
        format!("{base}&v=10&encoding=json")
    } else if base.ends_with('/') {
        format!("{base}?v=10&encoding=json")
    } else {
        format!("{base}/?v=10&encoding=json")
    }
}

fn strip_discord_bot_mention(text: &str, bot_user_id: Option<&str>) -> String {
    let mut s = text.trim();
    if let Some(uid) = bot_user_id {
        for prefix in [format!("<@{uid}>"), format!("<@!{uid}>")] {
            if let Some(rest) = s.strip_prefix(&prefix) {
                s = rest.trim_start_matches([' ', ':', ',', '-']);
            }
        }
    }
    s.trim().to_owned()
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

            // Send Hello with short 120ms heartbeat interval.
            write_server_frame(
                &mut sock1,
                true,
                1,
                br#"{"op":10,"d":{"heartbeat_interval":120}}"#,
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

            // Wait briefly for client to process dispatch before triggering op 7 Reconnect.
            thread::sleep(Duration::from_millis(120));
            write_server_frame(&mut sock1, true, 1, br#"{"op":7,"d":null}"#).unwrap();
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

        let deadline = Instant::now() + Duration::from_secs(5);
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
}
