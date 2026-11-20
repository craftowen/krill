// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! 100% local Telegram Bot API long-polling connector.
//!
//! Runs a 128 KiB std thread calling `POST /bot<token>/getUpdates` (`timeout=30`, monotonic
//! `offset`, exponential backoff on errors, responsive exit on stop signal). Handles DMs, group
//! messages, forum `message_thread_id`, and `callback_query` inline-keyboard gate approvals.

use std::io;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use krill_proto::bots::{
    BotConfig, BotConnectorStatus, BotErrorCategory, BotId, ConnectorHealthState, ExternalSource,
};
use serde::Deserialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::bots::connectors::router::{
    ConversationRouter, DeliveryTarget, PlatformSender, classify_error, now_ms,
    sanitize_error_for_status,
};
use crate::bots::connectors::{ConnectorBridge, InboundExternalMessage, StopFlag};
use crate::bots::http_client::{self, HttpRequest};

/// Production Telegram Bot API base URL.
pub const DEFAULT_TELEGRAM_API_BASE: &str = "https://api.telegram.org";
/// Safe single-message length under Telegram's 4,096-character limit.
pub const TELEGRAM_MAX_MESSAGE_CHARS: usize = 4_000;
/// Long-polling server-side wait duration (`timeout=30`) sent to `getUpdates`.
pub const TELEGRAM_POLL_TIMEOUT_SECS: u64 = 30;
const WORKER_STACK_SIZE: usize = 128 * 1024;
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_HTTP_TIMEOUT: Duration = Duration::from_secs(TELEGRAM_POLL_TIMEOUT_SECS + 10);

/// Telegram Bot API HTTP client & [`PlatformSender`] implementation.
pub struct TelegramSender {
    api_base: String,
    token: Zeroizing<String>,
}

impl std::fmt::Debug for TelegramSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramSender")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl TelegramSender {
    #[must_use]
    pub fn new(api_base: &str, token: Zeroizing<String>) -> Self {
        Self {
            api_base: api_base.trim_end_matches('/').to_owned(),
            token,
        }
    }

    fn endpoint(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.api_base, self.token.as_str())
    }

    fn call_json(&self, method: &str, payload: &Value, timeout: Duration) -> io::Result<Value> {
        let body = serde_json::to_vec(payload).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "failed to encode Telegram API payload",
            )
        })?;
        let req = HttpRequest::post_json(self.endpoint(method), body).with_timeout(timeout);
        let resp = http_client::send(&req).map_err(|err| {
            io::Error::new(err.kind(), format!("Telegram {method} transport error"))
        })?;
        let parsed: TgEnvelope = resp.json().unwrap_or(TgEnvelope {
            ok: false,
            result: None,
            description: Some(format!("HTTP {}", resp.status)),
            error_code: Some(u32::from(resp.status)),
        });
        if resp.is_success() && parsed.ok {
            Ok(parsed.result.unwrap_or(Value::Null))
        } else {
            let code = parsed.error_code.unwrap_or(u32::from(resp.status));
            let desc = parsed
                .description
                .unwrap_or_else(|| format!("Telegram API HTTP {code}"));
            let sanitized = sanitize_error_for_status(&desc);
            Err(io::Error::other(format!("HTTP {code}: {sanitized}")))
        }
    }

    /// Calls `getMe` to validate the bot token and obtain `@username`.
    pub fn get_me(&self) -> io::Result<Option<String>> {
        let result = self.call_json("getMe", &json!({}), API_REQUEST_TIMEOUT)?;
        Ok(result
            .get("username")
            .and_then(Value::as_str)
            .map(|u| format!("@{u}")))
    }

    /// Calls `POST /bot<token>/getUpdates` with `timeout=30` and monotonic `offset`.
    pub fn get_updates(&self, offset: Option<i64>) -> io::Result<Vec<TgUpdate>> {
        let mut payload = json!({
            "timeout": TELEGRAM_POLL_TIMEOUT_SECS,
            "allowed_updates": ["message", "callback_query"],
        });
        if let Some(off) = offset
            && let Some(obj) = payload.as_object_mut()
        {
            obj.insert("offset".to_owned(), json!(off));
        }
        let result = self.call_json("getUpdates", &payload, POLL_HTTP_TIMEOUT)?;
        serde_json::from_value(result).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Telegram getUpdates payload",
            )
        })
    }

    /// Acknowledges an inline-keyboard `callback_query` click.
    pub fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: &str,
        show_alert: bool,
    ) -> io::Result<()> {
        let payload = json!({
            "callback_query_id": callback_query_id,
            "text": text,
            "show_alert": show_alert,
        });
        self.call_json("answerCallbackQuery", &payload, API_REQUEST_TIMEOUT)?;
        Ok(())
    }
}

impl PlatformSender for TelegramSender {
    fn source(&self) -> ExternalSource {
        ExternalSource::Telegram
    }

    fn max_chunk_chars(&self) -> usize {
        TELEGRAM_MAX_MESSAGE_CHARS
    }

    fn send_message(&self, target: &DeliveryTarget, text: &str) -> io::Result<String> {
        let chat_id_val = chat_id_value(&target.channel_id);
        let mut payload = json!({
            "chat_id": chat_id_val,
            "text": text,
        });
        if let Some(tid) = target
            .thread_id
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            && let Some(obj) = payload.as_object_mut()
        {
            obj.insert("message_thread_id".to_owned(), json!(tid));
        }
        let result = self.call_json("sendMessage", &payload, API_REQUEST_TIMEOUT)?;
        let msg_id = result
            .get("message_id")
            .and_then(Value::as_i64)
            .map_or_else(|| "0".to_owned(), |id| id.to_string());
        Ok(msg_id)
    }

    fn edit_message(
        &self,
        target: &DeliveryTarget,
        message_id: &str,
        text: &str,
    ) -> io::Result<()> {
        let msg_id = message_id.parse::<i64>().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid Telegram message_id")
        })?;
        let payload = json!({
            "chat_id": chat_id_value(&target.channel_id),
            "message_id": msg_id,
            "text": text,
        });
        match self.call_json("editMessageText", &payload, API_REQUEST_TIMEOUT) {
            Ok(_) => Ok(()),
            Err(err)
                if err
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("message is not modified") =>
            {
                Ok(())
            }
            Err(err) => Err(err),
        }
    }
}

fn chat_id_value(channel_id: &str) -> Value {
    channel_id
        .parse::<i64>()
        .map_or_else(|_| json!(channel_id), |id| json!(id))
}

#[derive(Debug, Deserialize)]
struct TgEnvelope {
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    error_code: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct TgUpdate {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TgMessage>,
    #[serde(default)]
    pub callback_query: Option<TgCallbackQuery>,
}

#[derive(Debug, Deserialize)]
pub struct TgMessage {
    pub message_id: i64,
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    #[serde(default)]
    pub from: Option<TgUser>,
    pub chat: TgChat,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TgUser {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TgChat {
    pub id: i64,
}

#[derive(Debug, Deserialize)]
pub struct TgCallbackQuery {
    pub id: String,
    pub from: TgUser,
    #[serde(default)]
    pub message: Option<TgMessage>,
    #[serde(default)]
    pub data: Option<String>,
}

/// Spawns a 128 KiB long-polling worker thread for a single bot's Telegram connector.
pub(crate) fn spawn_telegram_worker(
    bot: BotConfig,
    sender: Arc<TelegramSender>,
    bridge: Arc<dyn ConnectorBridge>,
    router: Arc<ConversationRouter>,
    stop: Arc<StopFlag>,
) -> io::Result<JoinHandle<()>> {
    let thread_name = format!("bot-tg-{}", bot.slug.as_str());
    thread::Builder::new()
        .name(thread_name)
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || {
            let bot_id = bot.id.clone();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_telegram_loop(&bot, &sender, &bridge, &router, &stop);
            }));
            if outcome.is_err() {
                tracing::warn!(
                    bot_id = bot_id.as_str(),
                    source = "telegram",
                    "telegram connector worker panicked; isolated"
                );
                bridge.update_connector_status(BotConnectorStatus {
                    bot_id,
                    source: ExternalSource::Telegram,
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

fn run_telegram_loop(
    bot: &BotConfig,
    sender: &Arc<TelegramSender>,
    bridge: &Arc<dyn ConnectorBridge>,
    router: &Arc<ConversationRouter>,
    stop: &Arc<StopFlag>,
) {
    let mut account_label = match sender.get_me() {
        Ok(label) => label,
        Err(err) => {
            let raw = err.to_string();
            let cat = classify_error(&raw);
            report_status(
                bridge,
                &bot.id,
                false,
                ConnectorHealthState::Error,
                None,
                None,
                Some(sanitize_error_for_status(&raw)),
                Some(cat),
            );
            None
        }
    };

    let mut offset: Option<i64> = None;
    let mut backoff_ms: u64 = 1_000;

    while !stop.is_stopped() {
        match sender.get_updates(offset) {
            Ok(updates) => {
                backoff_ms = 1_000;
                if account_label.is_none()
                    && let Ok(label) = sender.get_me()
                {
                    account_label = label;
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
                for update in updates {
                    let next = update.update_id.saturating_add(1);
                    offset = Some(offset.map_or(next, |prev| prev.max(next)));
                    if stop.is_stopped() {
                        break;
                    }
                    handle_update(&bot.id, sender, bridge, router, update);
                }
            }
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
            }
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

fn handle_update(
    bot_id: &BotId,
    sender: &Arc<TelegramSender>,
    bridge: &Arc<dyn ConnectorBridge>,
    router: &Arc<ConversationRouter>,
    update: TgUpdate,
) {
    if let Some(cq) = update.callback_query {
        let sender_id = cq.from.id.to_string();
        let sender_name = format_tg_user_name(&cq.from);
        let channel_id = cq
            .message
            .as_ref()
            .map_or_else(|| sender_id.clone(), |m| m.chat.id.to_string());
        let data = cq.data.as_deref().unwrap_or_default();
        let outcome = router.handle_gate_callback(
            bot_id,
            ExternalSource::Telegram,
            &sender_id,
            &sender_name,
            &channel_id,
            data,
        );
        let notice = outcome.user_notice();
        sender.answer_callback_query(&cq.id, &notice, false).ok();
        return;
    }

    let Some(msg) = update.message else {
        return;
    };
    let Some(from) = msg.from else {
        return;
    };
    if from.is_bot {
        return;
    }
    let text = msg
        .text
        .as_deref()
        .or(msg.caption.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(text) = text else {
        return;
    };

    let channel_id = msg.chat.id.to_string();
    let thread_id = msg.message_thread_id.map(|id| id.to_string());
    let acts_for = bridge
        .get_bot_config(bot_id)
        .map_or_else(Default::default, |b| b.connectors.telegram.acts_for);

    let target = DeliveryTarget {
        source: ExternalSource::Telegram,
        workspace_id: None,
        channel_id: channel_id.clone(),
        thread_id: thread_id.clone(),
        reply_to_message_id: Some(msg.message_id.to_string()),
    };
    let inbound = InboundExternalMessage {
        bot_id: bot_id.clone(),
        source: ExternalSource::Telegram,
        workspace_id: None,
        channel_id,
        thread_id,
        sender_id: from.id.to_string(),
        sender_name: format_tg_user_name(&from),
        text: text.to_owned(),
        attachments: Vec::new(),
        external_message_id: Some(msg.message_id.to_string()),
        acts_for,
    };
    router.handle_inbound_message(inbound, target, sender.as_ref());
}

fn format_tg_user_name(user: &TgUser) -> String {
    if !user.first_name.trim().is_empty() {
        user.first_name.trim().to_owned()
    } else if let Some(ref handle) = user.username
        && !handle.trim().is_empty()
    {
        format!("@{}", handle.trim())
    } else {
        user.id.to_string()
    }
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
        source: ExternalSource::Telegram,
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
    use std::collections::VecDeque;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::time::Instant;

    use krill_proto::SessionId;
    use krill_proto::bots::{ActsFor, BotAccessRequestStatus, CreateBotParams};
    use krill_proto::orchestration::OrchestrationStore;

    use crate::bots::connectors::{
        AccessDecision, ConnectorEndpoints, ConnectorManager, DispatchOutcome,
    };
    use crate::bots::secrets::SecretVault;
    use crate::bots::store::BotStore;
    use crate::session::lock;

    struct MockTgState {
        pending_updates: VecDeque<Value>,
        get_updates_calls: Vec<Value>,
        sent_messages: Vec<Value>,
        edited_messages: Vec<Value>,
        callback_answers: Vec<Value>,
        next_msg_id: i64,
    }

    fn start_mock_telegram_server() -> (
        String,
        Arc<Mutex<MockTgState>>,
        Arc<StopFlag>,
        JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(MockTgState {
            pending_updates: VecDeque::new(),
            get_updates_calls: Vec::new(),
            sent_messages: Vec::new(),
            edited_messages: Vec::new(),
            callback_answers: Vec::new(),
            next_msg_id: 100,
        }));
        let stop = Arc::new(StopFlag::default());
        let st = Arc::clone(&state);
        let stop_clone = Arc::clone(&stop);

        let handle = thread::spawn(move || {
            while !stop_clone.is_stopped() {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        let (path, body) = read_http_req(&mut stream);
                        let req_json: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
                        let resp_json = if path.ends_with("/getMe") {
                            json!({"ok": true, "result": {"id": 1, "is_bot": true, "username": "krill_test_bot"}})
                        } else if path.ends_with("/getUpdates") {
                            let mut g = lock(&st);
                            g.get_updates_calls.push(req_json);
                            let mut batch = Vec::new();
                            while let Some(u) = g.pending_updates.pop_front() {
                                batch.push(u);
                            }
                            drop(g);
                            if batch.is_empty() {
                                thread::sleep(Duration::from_millis(20));
                            }
                            json!({"ok": true, "result": batch})
                        } else if path.ends_with("/sendMessage") {
                            let mut g = lock(&st);
                            g.next_msg_id += 1;
                            let id = g.next_msg_id;
                            g.sent_messages.push(req_json);
                            json!({"ok": true, "result": {"message_id": id}})
                        } else if path.ends_with("/editMessageText") {
                            let mut g = lock(&st);
                            g.edited_messages.push(req_json);
                            json!({"ok": true, "result": true})
                        } else if path.ends_with("/answerCallbackQuery") {
                            let mut g = lock(&st);
                            g.callback_answers.push(req_json);
                            json!({"ok": true, "result": true})
                        } else {
                            json!({"ok": false, "description": "unknown method"})
                        };
                        let payload = serde_json::to_vec(&resp_json).unwrap();
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            payload.len()
                        );
                        stream.write_all(header.as_bytes()).ok();
                        stream.write_all(&payload).ok();
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        (format!("http://{addr}"), state, stop, handle)
    }

    fn read_http_req(stream: &mut std::net::TcpStream) -> (String, Vec<u8>) {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let header_end = loop {
            if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => return (String::new(), Vec::new()),
                Ok(n) => raw.extend_from_slice(&buf[..n]),
            }
        };
        let head = String::from_utf8_lossy(&raw[..header_end]).into_owned();
        let path = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or_default()
            .to_owned();
        let content_len = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        let mut body = raw[header_end + 4..].to_vec();
        while body.len() < content_len {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => body.extend_from_slice(&buf[..n]),
            }
        }
        body.truncate(content_len);
        (path, body)
    }

    struct StoreBackedBridge {
        state_dir: std::path::PathBuf,
        store: Arc<BotStore>,
        secrets: Arc<SecretVault>,
        dispatched: Mutex<Vec<InboundExternalMessage>>,
        statuses: Mutex<Vec<BotConnectorStatus>>,
    }

    impl ConnectorBridge for StoreBackedBridge {
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
            let updated = self
                .store
                .bind_link_session(&link.id, SessionId(77), now_ms())?
                .unwrap_or(link);
            lock(&self.dispatched).push(msg);
            Ok(DispatchOutcome {
                link: updated,
                session_id: SessionId(77),
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
        fn update_connector_status(&self, status: BotConnectorStatus) {
            lock(&self.statuses).push(status);
        }
        fn get_bot_config(&self, bot_id: &BotId) -> Option<BotConfig> {
            self.store.get_bot(bot_id).ok().flatten()
        }
        fn list_bots(&self) -> Vec<BotConfig> {
            self.store.list_bots().unwrap_or_default()
        }
    }

    #[test]
    fn telegram_e2e_access_dispatch_throttled_activity_chunked_reply_and_gate_approval() {
        let dir = std::env::temp_dir().join(format!("krill-tg-e2e-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let (base_url, mock, server_stop, server_handle) = start_mock_telegram_server();
        let bots_root = dir.join("bots");
        let store = Arc::new(BotStore::new(bots_root.clone()));
        let secrets = Arc::new(SecretVault::with_static_key(bots_root, [0x22; 32]));

        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "TG Bot".to_owned(),
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
        connectors.telegram.enabled = true;
        connectors.telegram.secret_ref = Some(SecretVault::scoped_ref(&bot.id, "telegram"));
        store
            .set_connector_config(&bot.id, connectors, 1_100)
            .unwrap();
        secrets
            .set_secret(
                &bot.id,
                "telegram",
                Zeroizing::new("999:test-tg-token".to_owned()),
            )
            .unwrap();

        let gate = OrchestrationStore::mutate(&dir, |s| {
            s.create_gate(
                "worker-tg",
                "Ship release?",
                vec!["approve".to_owned(), "reject".to_owned()],
                None,
                None,
            )
        })
        .unwrap();

        let bridge = Arc::new(StoreBackedBridge {
            state_dir: dir.clone(),
            store: Arc::clone(&store),
            secrets: Arc::clone(&secrets),
            dispatched: Mutex::new(Vec::new()),
            statuses: Mutex::new(Vec::new()),
        });
        let mgr = ConnectorManager::with_endpoints(
            Arc::clone(&bridge) as Arc<dyn ConnectorBridge>,
            ConnectorEndpoints {
                telegram: base_url,
                ..ConnectorEndpoints::default()
            },
        );
        mgr.reload(None).unwrap();

        // 1. Unauthorized message -> pending access request + polite reply with token.
        lock(&mock).pending_updates.push_back(json!({
            "update_id": 10,
            "message": {
                "message_id": 1,
                "message_thread_id": 42,
                "from": {"id": 5001, "is_bot": false, "first_name": "Alice"},
                "chat": {"id": -100_777},
                "text": "hello before approval"
            }
        }));

        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(&mock).sent_messages.is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(lock(&bridge.dispatched).is_empty());
        let pending_reqs = store.list_access_requests(Some(&bot.id)).unwrap();
        assert_eq!(pending_reqs.len(), 1);
        let token = pending_reqs[0].token.clone();
        assert_eq!(pending_reqs[0].status, BotAccessRequestStatus::Pending);
        assert!(
            lock(&mock).sent_messages[0]["text"]
                .as_str()
                .unwrap()
                .contains(&token)
        );
        // Verify getUpdates sent timeout=30.
        assert_eq!(lock(&mock).get_updates_calls[0]["timeout"], 30);

        // 2. Owner approves access request -> subsequent message dispatches!
        store.decide_access_request(&token, true, 2_000).unwrap();
        lock(&mock).sent_messages.clear();
        lock(&mock).pending_updates.push_back(json!({
            "update_id": 11,
            "message": {
                "message_id": 2,
                "message_thread_id": 42,
                "from": {"id": 5001, "is_bot": false, "first_name": "Alice"},
                "chat": {"id": -100_777},
                "text": "run health check"
            }
        }));
        while lock(&bridge.dispatched).is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(lock(&bridge.dispatched).len(), 1);
        assert_eq!(lock(&bridge.dispatched)[0].thread_id.as_deref(), Some("42"));

        // 3. 3s-throttled activity -> first sends status message, second (<3s) skipped, third (≥3s) edits in place!
        mgr.router()
            .on_turn_activity_at(&bot.id, SessionId(77), "Using Bash...", 10_000);
        mgr.router()
            .on_turn_activity_at(&bot.id, SessionId(77), "Using Read...", 11_000);
        mgr.router()
            .on_turn_activity_at(&bot.id, SessionId(77), "Thinking...", 13_500);
        assert_eq!(lock(&mock).sent_messages.len(), 1);
        assert_eq!(lock(&mock).sent_messages[0]["text"], "Using Bash...");
        assert_eq!(lock(&mock).sent_messages[0]["message_thread_id"], 42);
        assert_eq!(lock(&mock).edited_messages.len(), 1);
        assert_eq!(lock(&mock).edited_messages[0]["text"], "Thinking...");

        // 4. Turn completion posts final chunked reply (>4000 chars -> 2 sendMessage calls).
        lock(&mock).sent_messages.clear();
        let long_reply = format!("{}\n\n{}", "A".repeat(3_500), "B".repeat(1_200));
        mgr.on_turn_complete(&bot.id, SessionId(77), Some(&long_reply));
        assert_eq!(lock(&mock).sent_messages.len(), 2);

        // 5. callback_query inline keyboard gate approval resolves OrchestrationStore gate + answers callback!
        lock(&mock).pending_updates.push_back(json!({
            "update_id": 12,
            "callback_query": {
                "id": "cq-88",
                "from": {"id": 5001, "is_bot": false, "first_name": "Alice"},
                "message": {"message_id": 5, "chat": {"id": -100_777}},
                "data": format!("gate:{}:approve", gate.id)
            }
        }));
        while lock(&mock).callback_answers.is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            lock(&mock).callback_answers[0]["callback_query_id"],
            "cq-88"
        );
        assert!(
            lock(&mock).callback_answers[0]["text"]
                .as_str()
                .unwrap()
                .contains("approve")
        );
        let orch = OrchestrationStore::load(&dir).unwrap();
        assert_eq!(orch.gates[0].decision.as_deref(), Some("approve"));

        // Verify monotonic offset progressed to 13 on subsequent poll.
        let saw_offset_13 = loop {
            if lock(&mock)
                .get_updates_calls
                .iter()
                .any(|c| c.get("offset").and_then(Value::as_i64) == Some(13))
            {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert!(saw_offset_13);

        mgr.shutdown();
        server_stop.signal();
        server_handle.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
