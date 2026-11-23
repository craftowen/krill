// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! 100% local Telegram Bot API long-polling connector.
//!
//! Runs a 128 KiB std thread calling `POST /bot<token>/getUpdates` (`timeout=30`, monotonic
//! `offset`, exponential backoff on errors, responsive exit on stop signal). Handles DMs, group
//! messages, forum `message_thread_id`, and `callback_query` inline-keyboard gate approvals.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use krill_proto::bots::{
    BotConfig, BotConnectorStatus, BotConnectorTestResult, BotErrorCategory, BotId,
    ConnectorHealthState, ExternalSource,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::bots::connectors::policy_for_api_base;
use crate::bots::connectors::router::{
    ConversationRouter, DeliveryTarget, PlatformSender, chunk_text_utf16, classify_error, now_ms,
    sanitize_error_for_status,
};
use crate::bots::connectors::{ConnectorBridge, InboundExternalMessage, StopFlag};
use crate::bots::http_client::{self, HttpRequest, MultipartField, RequestCancelSlot, SsrfPolicy};
use crate::session::lock;

/// Production Telegram Bot API base URL.
pub const DEFAULT_TELEGRAM_API_BASE: &str = "https://api.telegram.org";
/// Telegram's hard single-message limit (4,096 UTF-16 code units).
pub const TELEGRAM_MAX_MESSAGE_CHARS: usize = 4_096;
/// Long-polling server-side wait duration (`timeout=30`) sent to `getUpdates`.
pub const TELEGRAM_POLL_TIMEOUT_SECS: u64 = 30;
const WORKER_STACK_SIZE: usize = 128 * 1024;
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_HTTP_TIMEOUT: Duration = Duration::from_secs(TELEGRAM_POLL_TIMEOUT_SECS + 10);
const MAX_INBOUND_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const MAX_OUTBOUND_DOCUMENT_BYTES: usize = 50 * 1024 * 1024;
const CONNECTORS_STATE_FILE: &str = "connectors-state.json";
static INBOX_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, Serialize, Deserialize)]
struct ConnectorsStateFile {
    #[serde(default)]
    telegram_offsets: BTreeMap<BotId, i64>,
}

fn state_file_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn load_persisted_offset(state_root: Option<&Path>, bot_id: &BotId) -> Option<i64> {
    let root = state_root?;
    let _g = lock(state_file_lock());
    let path = root.join(CONNECTORS_STATE_FILE);
    let meta = fs::symlink_metadata(&path).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    let raw = fs::read_to_string(&path).ok()?;
    let parsed: ConnectorsStateFile = serde_json::from_str(&raw).ok()?;
    parsed.telegram_offsets.get(bot_id).copied()
}

fn persist_offset(state_root: Option<&Path>, bot_id: &BotId, offset: i64) {
    let Some(root) = state_root else {
        return;
    };
    let _g = lock(state_file_lock());
    let path = root.join(CONNECTORS_STATE_FILE);
    let mut state: ConnectorsStateFile = fs::symlink_metadata(&path)
        .ok()
        .filter(|m| !m.file_type().is_symlink())
        .and_then(|_| fs::read_to_string(&path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let prev = state.telegram_offsets.get(bot_id).copied().unwrap_or(0);
    if offset <= prev && state.telegram_offsets.contains_key(bot_id) {
        return;
    }
    state.telegram_offsets.insert(bot_id.clone(), offset);
    if let Ok(mut bytes) = serde_json::to_vec_pretty(&state) {
        bytes.push(b'\n');
        krill_proto::os::write_atomic_0600(&path, &bytes).ok();
    }
}

/// Telegram Bot API HTTP client & [`PlatformSender`] implementation.
pub struct TelegramSender {
    api_base: String,
    token: Zeroizing<String>,
    ssrf_policy: SsrfPolicy,
}

impl std::fmt::Debug for TelegramSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramSender")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

enum TgCallError {
    RetryAfter(u64, io::Error),
    Other(io::Error),
}

impl TelegramSender {
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

    fn endpoint(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.api_base, self.token.as_str())
    }

    fn call_json(&self, method: &str, payload: &Value, timeout: Duration) -> io::Result<Value> {
        self.call_json_internal(method, payload, timeout, None, None)
            .map_err(|err| match err {
                TgCallError::RetryAfter(_, e) | TgCallError::Other(e) => e,
            })
    }

    fn call_json_internal(
        &self,
        method: &str,
        payload: &Value,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
        socket: Option<&RequestCancelSlot>,
    ) -> Result<Value, TgCallError> {
        let body = serde_json::to_vec(payload).map_err(|_| {
            TgCallError::Other(io::Error::new(
                io::ErrorKind::InvalidInput,
                "failed to encode Telegram API payload",
            ))
        })?;
        let req = HttpRequest::post_json(self.endpoint(method), body)
            .with_timeout(timeout)
            .with_ssrf_policy(self.ssrf_policy);
        let resp = http_client::send_interruptible(&req, cancel, socket).map_err(|err| {
            TgCallError::Other(io::Error::new(
                err.kind(),
                format!("Telegram {method} transport error"),
            ))
        })?;
        let parsed: TgEnvelope = resp.json().unwrap_or(TgEnvelope {
            ok: false,
            result: None,
            description: Some(format!("HTTP {}", resp.status)),
            error_code: Some(u32::from(resp.status)),
            parameters: None,
        });
        if resp.is_success() && parsed.ok {
            return Ok(parsed.result.unwrap_or(Value::Null));
        }
        let code = parsed.error_code.unwrap_or(u32::from(resp.status));
        if code == 409 {
            return Err(TgCallError::Other(io::Error::other(
                "Telegram 409 Conflict: another poller or active webhook is running; deleteWebhook or stop duplicate instance",
            )));
        }
        let desc = parsed
            .description
            .unwrap_or_else(|| format!("Telegram API HTTP {code}"));
        let sanitized = sanitize_error_for_status(&desc);
        let io_err = io::Error::other(format!("HTTP {code}: {sanitized}"));
        if code == 429
            && let Some(wait) = parsed.parameters.and_then(|p| p.retry_after)
        {
            return Err(TgCallError::RetryAfter(wait, io_err));
        }
        Err(TgCallError::Other(io_err))
    }

    /// Calls `getMe` to validate the bot token and obtain `@username`.
    pub fn get_me(&self) -> io::Result<Option<String>> {
        let result = self.call_json("getMe", &json!({}), API_REQUEST_TIMEOUT)?;
        Ok(result
            .get("username")
            .and_then(Value::as_str)
            .map(|u| format!("@{u}")))
    }

    /// Runs an on-demand connectivity check for `TestConnector`.
    #[must_use]
    pub fn test_connection(&self) -> BotConnectorTestResult {
        match self.get_me() {
            Ok(label) => BotConnectorTestResult {
                source: ExternalSource::Telegram,
                ok: true,
                account_label: label.clone(),
                detail: format!(
                    "Connected to Telegram ({})",
                    label.unwrap_or_else(|| "bot ok".to_owned())
                ),
            },
            Err(err) => BotConnectorTestResult {
                source: ExternalSource::Telegram,
                ok: false,
                account_label: None,
                detail: sanitize_error_for_status(&err.to_string()),
            },
        }
    }

    /// Calls `POST /bot<token>/getUpdates` with `timeout=30` and monotonic `offset`.
    pub fn get_updates(&self, offset: Option<i64>) -> io::Result<Vec<TgUpdate>> {
        self.get_updates_with_retry_info(offset, None, None)
            .map_err(|err| match err {
                TgCallError::RetryAfter(_, e) | TgCallError::Other(e) => e,
            })
    }

    fn get_updates_with_retry_info(
        &self,
        offset: Option<i64>,
        cancel: Option<&AtomicBool>,
        socket: Option<&RequestCancelSlot>,
    ) -> Result<Vec<TgUpdate>, TgCallError> {
        let mut payload = json!({
            "timeout": TELEGRAM_POLL_TIMEOUT_SECS,
            "allowed_updates": ["message", "callback_query"],
        });
        if let Some(off) = offset
            && let Some(obj) = payload.as_object_mut()
        {
            obj.insert("offset".to_owned(), json!(off));
        }
        let result =
            self.call_json_internal("getUpdates", &payload, POLL_HTTP_TIMEOUT, cancel, socket)?;
        serde_json::from_value(result).map_err(|_| {
            TgCallError::Other(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Telegram getUpdates payload",
            ))
        })
    }

    /// Downloads a bounded Telegram file by `file_id` into `<bot_home>/inbox/` cleanly.
    fn download_attachment_to_inbox(
        &self,
        file_id: &str,
        suggested_name: &str,
        expected_mime: Option<&str>,
        declared_size: Option<u64>,
        bot_home: &Path,
    ) -> Option<String> {
        if let Some(sz) = declared_size
            && usize::try_from(sz).unwrap_or(usize::MAX) > MAX_INBOUND_ATTACHMENT_BYTES
        {
            return None;
        }
        if let Some(mime) = expected_mime
            && !is_safe_attachment_mime(mime)
        {
            return None;
        }
        let meta = self
            .call_json(
                "getFile",
                &json!({ "file_id": file_id }),
                API_REQUEST_TIMEOUT,
            )
            .ok()?;
        let file_path = meta.get("file_path").and_then(Value::as_str)?;
        if file_path.contains("..") || file_path.trim().is_empty() {
            return None;
        }
        let download_url = format!(
            "{}/file/bot{}/{}",
            self.api_base,
            self.token.as_str(),
            file_path.trim_start_matches('/')
        );
        let resp = http_client::send(
            &HttpRequest::get(download_url)
                .with_timeout(API_REQUEST_TIMEOUT)
                .with_max_response_bytes(MAX_INBOUND_ATTACHMENT_BYTES)
                .with_ssrf_policy(self.ssrf_policy),
        )
        .ok()?;
        if !resp.is_success() || resp.body.is_empty() {
            return None;
        }
        let inbox = bot_home.join("inbox");
        fs::create_dir_all(&inbox).ok()?;
        let safe_name = sanitize_attachment_filename(suggested_name, file_path);
        let seq = INBOX_SEQ.fetch_add(1, Ordering::Relaxed);
        let dest = inbox.join(format!("{}-{seq}-{safe_name}", now_ms()));
        krill_proto::os::write_atomic_0600(&dest, &resp.body).ok()?;
        Some(dest.to_string_lossy().into_owned())
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

    fn split_chunks(&self, text: &str) -> Vec<String> {
        chunk_text_utf16(text, self.max_chunk_chars())
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
        if let Some(reply_id) = target
            .reply_to_message_id
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            && let Some(obj) = payload.as_object_mut()
        {
            obj.insert(
                "reply_parameters".to_owned(),
                json!({ "message_id": reply_id, "allow_sending_without_reply": true }),
            );
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

    fn send_files(
        &self,
        target: &DeliveryTarget,
        files: &[String],
        caption: Option<&str>,
    ) -> io::Result<Option<String>> {
        let mut last_id = None;
        for (idx, raw_path) in files.iter().enumerate() {
            let path = Path::new(raw_path);
            let meta = fs::symlink_metadata(path)?;
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("refusing non-regular file attachment: {raw_path}"),
                ));
            }
            if usize::try_from(meta.len()).unwrap_or(usize::MAX) > MAX_OUTBOUND_DOCUMENT_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "attachment exceeds 50 MiB Telegram limit",
                ));
            }
            let data = fs::read(path)?;
            let fname = path.file_name().map_or_else(
                || "attachment.bin".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            );
            let mut fields = vec![MultipartField {
                name: "chat_id".to_owned(),
                filename: None,
                content_type: None,
                data: target.channel_id.as_bytes().to_vec(),
            }];
            if let Some(tid) = target
                .thread_id
                .as_deref()
                .and_then(|s| s.parse::<i64>().ok())
            {
                fields.push(MultipartField {
                    name: "message_thread_id".to_owned(),
                    filename: None,
                    content_type: None,
                    data: tid.to_string().into_bytes(),
                });
            }
            if let Some(reply_id) = target
                .reply_to_message_id
                .as_deref()
                .and_then(|s| s.parse::<i64>().ok())
            {
                let rp = json!({ "message_id": reply_id, "allow_sending_without_reply": true });
                if let Ok(rp_bytes) = serde_json::to_vec(&rp) {
                    fields.push(MultipartField {
                        name: "reply_parameters".to_owned(),
                        filename: None,
                        content_type: Some("application/json".to_owned()),
                        data: rp_bytes,
                    });
                }
            }
            if idx == 0
                && let Some(cap) = caption.filter(|c| !c.trim().is_empty())
            {
                fields.push(MultipartField {
                    name: "caption".to_owned(),
                    filename: None,
                    content_type: None,
                    data: cap.as_bytes().to_vec(),
                });
            }
            fields.push(MultipartField {
                name: "document".to_owned(),
                filename: Some(fname),
                content_type: Some("application/octet-stream".to_owned()),
                data,
            });
            let req = HttpRequest::post_multipart(self.endpoint("sendDocument"), &fields)?
                .with_timeout(API_REQUEST_TIMEOUT)
                .with_ssrf_policy(self.ssrf_policy);
            let resp = http_client::send(&req).map_err(|err| {
                io::Error::new(err.kind(), "Telegram sendDocument transport error")
            })?;
            let parsed: TgEnvelope = resp.json().unwrap_or(TgEnvelope {
                ok: false,
                result: None,
                description: Some(format!("HTTP {}", resp.status)),
                error_code: Some(u32::from(resp.status)),
                parameters: None,
            });
            if !(resp.is_success() && parsed.ok) {
                let desc = parsed
                    .description
                    .unwrap_or_else(|| format!("HTTP {}", resp.status));
                return Err(io::Error::other(sanitize_error_for_status(&desc)));
            }
            if let Some(mid) = parsed
                .result
                .as_ref()
                .and_then(|r| r.get("message_id"))
                .and_then(Value::as_i64)
            {
                last_id = Some(mid.to_string());
            }
        }
        Ok(last_id)
    }

    fn add_reaction(
        &self,
        target: &DeliveryTarget,
        message_id: &str,
        reaction: &str,
    ) -> io::Result<()> {
        let msg_id = message_id.parse::<i64>().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid Telegram message_id")
        })?;
        let emoji = reaction.trim();
        if emoji.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "reaction emoji cannot be empty",
            ));
        }
        let payload = json!({
            "chat_id": chat_id_value(&target.channel_id),
            "message_id": msg_id,
            "reaction": [{ "type": "emoji", "emoji": emoji }],
        });
        self.call_json("setMessageReaction", &payload, API_REQUEST_TIMEOUT)?;
        Ok(())
    }
}

fn chat_id_value(channel_id: &str) -> Value {
    channel_id
        .parse::<i64>()
        .map_or_else(|_| json!(channel_id), |id| json!(id))
}

#[derive(Debug, Deserialize)]
struct TgParameters {
    #[serde(default)]
    retry_after: Option<u64>,
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
    #[serde(default)]
    parameters: Option<TgParameters>,
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
    #[serde(default)]
    pub document: Option<TgDocument>,
    #[serde(default)]
    pub photo: Option<Vec<TgPhotoSize>>,
    #[serde(default)]
    pub audio: Option<TgMediaFile>,
    #[serde(default)]
    pub voice: Option<TgMediaFile>,
}

#[derive(Debug, Deserialize)]
pub struct TgDocument {
    pub file_id: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct TgPhotoSize {
    pub file_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct TgMediaFile {
    pub file_id: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

fn is_safe_attachment_mime(mime: &str) -> bool {
    let lower = mime.trim().to_ascii_lowercase();
    !lower.contains("x-msdownload") && !lower.contains("x-executable") && !lower.contains("x-sh")
}

fn sanitize_attachment_filename(suggested: &str, file_path: &str) -> String {
    let raw = if suggested.trim().is_empty() {
        Path::new(file_path).file_name().map_or_else(
            || "attachment.bin".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    } else {
        suggested.to_owned()
    };
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if clean.is_empty() || clean.starts_with('.') {
        format!("file{clean}")
    } else {
        clean
    }
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

    let state_root = bridge.connector_state_root();
    let mut offset: Option<i64> = load_persisted_offset(state_root.as_deref(), &bot.id);
    let mut backoff_ms: u64 = 1_000;

    while !stop.is_stopped() {
        match sender.get_updates_with_retry_info(
            offset,
            Some(stop.cancel_flag()),
            Some(stop.cancel_slot()),
        ) {
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
                    if stop.is_stopped() {
                        break;
                    }
                    let next = update.update_id.saturating_add(1);
                    handle_update(&bot.id, sender, bridge, router, update);
                    let updated_off = offset.map_or(next, |prev| prev.max(next));
                    offset = Some(updated_off);
                    persist_offset(state_root.as_deref(), &bot.id, updated_off);
                }
            }
            Err(err) => {
                if stop.is_stopped() {
                    break;
                }
                let (retry_after_secs, raw) = match err {
                    TgCallError::RetryAfter(secs, io_err) => (Some(secs), io_err.to_string()),
                    TgCallError::Other(io_err) => (None, io_err.to_string()),
                };
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
                let sleep_dur = if let Some(secs) = retry_after_secs {
                    Duration::from_secs(secs.clamp(1, 300))
                } else {
                    let cur = Duration::from_millis(backoff_ms);
                    backoff_ms = backoff_ms.saturating_mul(2).min(30_000);
                    cur
                };
                if stop.wait(sleep_dur) {
                    break;
                }
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

struct PendingAttachment {
    file_id: String,
    suggested_name: String,
    mime_type: Option<String>,
    file_size: Option<u64>,
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
        let thread_id = cq
            .message
            .as_ref()
            .and_then(|m| m.message_thread_id)
            .map(|id| id.to_string());
        let data = cq.data.as_deref().unwrap_or_default();
        let outcome = router.handle_gate_callback_with_context(
            bot_id,
            ExternalSource::Telegram,
            None,
            &sender_id,
            &sender_name,
            &channel_id,
            thread_id.as_deref(),
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

    let pending_attachment = if let Some(ref doc) = msg.document {
        Some(PendingAttachment {
            file_id: doc.file_id.clone(),
            suggested_name: doc
                .file_name
                .clone()
                .unwrap_or_else(|| "document.bin".to_owned()),
            mime_type: doc.mime_type.clone(),
            file_size: doc.file_size,
        })
    } else if let Some(ref photos) = msg.photo
        && let Some(largest) = photos.iter().max_by_key(|p| p.file_size.unwrap_or(0))
    {
        Some(PendingAttachment {
            file_id: largest.file_id.clone(),
            suggested_name: "photo.jpg".to_owned(),
            mime_type: Some("image/jpeg".to_owned()),
            file_size: largest.file_size,
        })
    } else if let Some(ref audio) = msg.audio {
        Some(PendingAttachment {
            file_id: audio.file_id.clone(),
            suggested_name: "audio.mp3".to_owned(),
            mime_type: audio.mime_type.clone(),
            file_size: audio.file_size,
        })
    } else {
        msg.voice.as_ref().map(|voice| PendingAttachment {
            file_id: voice.file_id.clone(),
            suggested_name: "voice.ogg".to_owned(),
            mime_type: voice.mime_type.clone(),
            file_size: voice.file_size,
        })
    };

    let explicit_text = msg
        .text
        .as_deref()
        .or(msg.caption.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let has_explicit_text = explicit_text.is_some();
    let Some(text) = explicit_text.or_else(|| {
        pending_attachment
            .as_ref()
            .map(|_| "[Attachment received]".to_owned())
    }) else {
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
        text,
        attachments: Vec::new(),
        external_message_id: Some(msg.message_id.to_string()),
        acts_for,
    };
    // Download inbound attachments ONLY after `paused` and allowlist/access checks pass (D-17),
    // retaining the deferred loader on `Pending` so approved messages replay WITH their file (MB-45).
    let bot_id_owned = bot_id.clone();
    let bridge_owned = Arc::clone(bridge);
    let sender_owned = Arc::clone(sender);
    router.handle_inbound_message_with_loader(
        inbound,
        target,
        sender.as_ref(),
        move |allowed_msg| {
            if let Some(att) = pending_attachment
                && let Some(bot_home) = bridge_owned.bot_home_dir(&bot_id_owned)
                && let Some(path) = sender_owned.download_attachment_to_inbox(
                    &att.file_id,
                    &att.suggested_name,
                    att.mime_type.as_deref(),
                    att.file_size,
                    &bot_home,
                )
            {
                allowed_msg.attachments.push(path);
            }
            has_explicit_text || !allowed_msg.attachments.is_empty()
        },
    );
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

    #[test]
    fn telegram_offset_persistence_409_429_interruptible_poll_and_media_work() {
        let dir = std::env::temp_dir().join(format!("krill-tg-w3-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let bot_id = BotId::new("tg-w3").unwrap();
        persist_offset(Some(&dir), &bot_id, 555);
        assert_eq!(load_persisted_offset(Some(&dir), &bot_id), Some(555));

        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = mock.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let cap_bg = Arc::clone(&captured);

        let srv = thread::spawn(move || {
            for idx in 0..4 {
                let (mut sock, _) = mock.accept().unwrap();
                let (path, body) = read_http_req(&mut sock);
                let txt = String::from_utf8_lossy(&body).into_owned();
                lock(&cap_bg).push((path.clone(), txt));
                let (status, resp_body) = if idx == 0 {
                    // 409 conflict
                    (
                        "409 Conflict",
                        r#"{"ok":false,"error_code":409,"description":"terminated by other getUpdates"}"#,
                    )
                } else if idx == 1 {
                    // 429 retry_after
                    (
                        "429 Too Many Requests",
                        r#"{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":7}}"#,
                    )
                } else if idx == 2 {
                    // sendDocument response
                    ("200 OK", r#"{"ok":true,"result":{"message_id":901}}"#)
                } else {
                    // setMessageReaction response
                    ("200 OK", r#"{"ok":true,"result":true}"#)
                };
                let wire = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
            }
        });

        let sender = TelegramSender::new(
            &format!("http://{addr}"),
            Zeroizing::new("123456:tok".to_owned()),
        );
        let err409 = sender.get_updates(None).unwrap_err();
        assert!(err409.to_string().contains("409 Conflict"));

        match sender.get_updates_with_retry_info(None, None, None) {
            Err(TgCallError::RetryAfter(secs, _)) => assert_eq!(secs, 7),
            _ => panic!("expected TgCallError::RetryAfter"),
        }

        let attach = dir.join("note.txt");
        std::fs::write(&attach, b"hello document").unwrap();
        let target = DeliveryTarget {
            source: ExternalSource::Telegram,
            workspace_id: None,
            channel_id: "-1001".to_owned(),
            thread_id: Some("9".to_owned()),
            reply_to_message_id: Some("88".to_owned()),
        };
        let fid = sender
            .send_files(
                &target,
                &[attach.to_string_lossy().into_owned()],
                Some("cap"),
            )
            .unwrap();
        assert_eq!(fid.as_deref(), Some("901"));
        sender.add_reaction(&target, "88", "👍").unwrap();

        srv.join().unwrap();
        let reqs = lock(&captured);
        assert!(reqs[2].0.ends_with("/sendDocument"));
        assert!(reqs[2].1.contains("hello document"));
        assert!(reqs[3].0.ends_with("/setMessageReaction"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn telegram_skips_attachment_download_for_denied_or_paused_senders_and_downloads_for_allowed() {
        struct AttBridge {
            bot: Mutex<BotConfig>,
            decision: Mutex<AccessDecision>,
            home: std::path::PathBuf,
            dispatched: Mutex<Vec<InboundExternalMessage>>,
        }
        impl ConnectorBridge for AttBridge {
            fn dispatch_external_message(
                &self,
                msg: InboundExternalMessage,
            ) -> io::Result<DispatchOutcome> {
                let link = krill_proto::bots::ExternalConversationLink {
                    id: "l-att".to_owned(),
                    bot_id: msg.bot_id.clone(),
                    source: msg.source,
                    workspace_id: None,
                    channel_id: msg.channel_id.clone(),
                    thread_id: None,
                    session_id: Some(SessionId(5)),
                    acts_for: msg.acts_for.clone(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                };
                lock(&self.dispatched).push(msg);
                Ok(DispatchOutcome {
                    link,
                    session_id: SessionId(5),
                    started_new_session: true,
                })
            }
            fn check_or_request_access(
                &self,
                _bot_id: &BotId,
                _source: ExternalSource,
                _sender_id: &str,
                _sender_name: &str,
                _channel_id: &str,
            ) -> io::Result<AccessDecision> {
                Ok(lock(&self.decision).clone())
            }
            fn bot_home_dir(&self, _bot_id: &BotId) -> Option<std::path::PathBuf> {
                Some(self.home.clone())
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
                bot_id: &BotId,
                source: ExternalSource,
                workspace_id: Option<&str>,
                channel_id: &str,
                thread_id: Option<&str>,
                acts_for: ActsFor,
            ) -> io::Result<krill_proto::bots::ExternalConversationLink> {
                Ok(krill_proto::bots::ExternalConversationLink {
                    id: "l-att".to_owned(),
                    bot_id: bot_id.clone(),
                    source,
                    workspace_id: workspace_id.map(ToOwned::to_owned),
                    channel_id: channel_id.to_owned(),
                    thread_id: thread_id.map(ToOwned::to_owned),
                    session_id: None,
                    acts_for,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                })
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
                let b = lock(&self.bot);
                (&b.id == bot_id).then(|| b.clone())
            }
            fn list_bots(&self) -> Vec<BotConfig> {
                vec![lock(&self.bot).clone()]
            }
        }

        let dir = std::env::temp_dir().join(format!("krill-tg-att-gate-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let get_file_hits = Arc::new(AtomicU64::new(0));
        let hits_bg = Arc::clone(&get_file_hits);

        let srv = thread::spawn(move || {
            // Expect: 1 sendMessage (denial notice), then 1 getFile + 1 file download for allowed.
            for _ in 0..3 {
                let (mut sock, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (ctype, body) = if path.ends_with("/getFile") {
                    hits_bg.fetch_add(1, Ordering::AcqRel);
                    (
                        "application/json",
                        r#"{"ok":true,"result":{"file_path":"docs/hello.txt"}}"#.as_bytes(),
                    )
                } else if path.contains("/file/bot") {
                    ("application/octet-stream", b"secret-ok".as_slice())
                } else {
                    (
                        "application/json",
                        r#"{"ok":true,"result":{"message_id":1}}"#.as_bytes(),
                    )
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                sock.write_all(head.as_bytes()).unwrap();
                sock.write_all(body).unwrap();
            }
        });

        let bot_id = BotId::new("tg-att").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "TgAtt", "claude", 1_000).unwrap();
        let bridge = Arc::new(AttBridge {
            bot: Mutex::new(bot),
            decision: Mutex::new(AccessDecision::Denied("nope".to_owned())),
            home: dir.clone(),
            dispatched: Mutex::new(Vec::new()),
        });
        let bridge_dyn: Arc<dyn ConnectorBridge> = Arc::clone(&bridge) as Arc<dyn ConnectorBridge>;
        let router = Arc::new(ConversationRouter::new(Arc::clone(&bridge_dyn)));
        let sender = Arc::new(TelegramSender::new(
            &format!("http://{addr}"),
            Zeroizing::new("123456:tok".to_owned()),
        ));

        let make_update = |id: i64| TgUpdate {
            update_id: id,
            message: Some(TgMessage {
                message_id: id,
                message_thread_id: None,
                from: Some(TgUser {
                    id: 77,
                    is_bot: false,
                    first_name: "Mallory".to_owned(),
                    username: None,
                }),
                chat: TgChat { id: 77 },
                text: None,
                caption: None,
                document: Some(TgDocument {
                    file_id: "file-abc".to_owned(),
                    file_name: Some("hello.txt".to_owned()),
                    mime_type: Some("text/plain".to_owned()),
                    file_size: Some(9),
                }),
                photo: None,
                audio: None,
                voice: None,
            }),
            callback_query: None,
        };

        // 1. Paused bot -> zero getFile calls, zero inbox files.
        lock(&bridge.bot).paused = true;
        handle_update(&bot_id, &sender, &bridge_dyn, &router, make_update(1));
        assert_eq!(get_file_hits.load(Ordering::Acquire), 0);
        assert!(!dir.join("inbox").exists());

        // 2. Denied user -> zero getFile calls, zero inbox files.
        lock(&bridge.bot).paused = false;
        handle_update(&bot_id, &sender, &bridge_dyn, &router, make_update(2));
        assert_eq!(get_file_hits.load(Ordering::Acquire), 0);
        assert!(!dir.join("inbox").exists());

        // 3. Allowed user -> downloads attachment to inbox and dispatches with path!
        *lock(&bridge.decision) = AccessDecision::Allowed;
        handle_update(&bot_id, &sender, &bridge_dyn, &router, make_update(3));
        assert_eq!(get_file_hits.load(Ordering::Acquire), 1);
        let disp = lock(&bridge.dispatched);
        assert_eq!(disp.len(), 1);
        assert_eq!(disp[0].attachments.len(), 1);
        assert!(std::path::Path::new(&disp[0].attachments[0]).is_file());

        srv.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
