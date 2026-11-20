// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Conversation router for Krill Bots external messaging connectors.
//!
//! Responsibilities:
//! - Enforcing allowlist/member access flow (`AccessDecision::{Allowed, Pending, Denied}`) before
//!   ever forwarding untrusted external messages or approval callbacks.
//! - Maintaining per-`(bot_id, session_id)` in-place status messages throttled to
//!   [`TURN_ACTIVITY_MIN_GAP_MS`] (`3_000` ms).
//! - Posting final chunked replies on turn completion while honoring `[[KRILL_QUIET]]`,
//!   `BotNotifications` switches, and Slack `mirror_desktop`.
//! - Routing validated single-select approval/question callbacks (`gate:<gate_id>:<decision>`)
//!   exclusively through [`ConnectorBridge::reply_orchestration_gate`] (never injecting raw chat
//!   text into PTYs).
//! - Mapping internal failures into user-safe messages using [`BotErrorCategory`].

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::SessionId;
use krill_proto::bots::{
    BOT_QUIET_MARKER, BotAccessRequest, BotErrorCategory, BotId, ExternalConversationLink,
    ExternalSource,
};

use crate::bots::connectors::{
    AccessDecision, ConnectorBridge, DispatchOutcome, InboundExternalMessage, OutboundBotReply,
};
use crate::session::lock;

/// Minimum interval (ms) between consecutive in-place turn-activity status updates.
pub const TURN_ACTIVITY_MIN_GAP_MS: u64 = 3_000;

/// Platform-neutral delivery destination for outbound messages/edits/reactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryTarget {
    pub source: ExternalSource,
    pub workspace_id: Option<String>,
    pub channel_id: String,
    pub thread_id: Option<String>,
    pub reply_to_message_id: Option<String>,
}

impl DeliveryTarget {
    #[must_use]
    pub fn from_link(link: &ExternalConversationLink) -> Self {
        Self {
            source: link.source,
            workspace_id: link.workspace_id.clone(),
            channel_id: link.channel_id.clone(),
            thread_id: link.thread_id.clone(),
            reply_to_message_id: None,
        }
    }
}

/// Platform sender abstraction implemented by Telegram, Slack, and Discord connectors.
pub trait PlatformSender: Send + Sync {
    fn source(&self) -> ExternalSource;
    fn max_chunk_chars(&self) -> usize;
    fn send_message(&self, target: &DeliveryTarget, text: &str) -> io::Result<String>;
    fn edit_message(&self, target: &DeliveryTarget, message_id: &str, text: &str)
    -> io::Result<()>;
    fn add_reaction(
        &self,
        _target: &DeliveryTarget,
        _message_id: &str,
        _reaction: &str,
    ) -> io::Result<()> {
        Ok(())
    }
}

/// Outcome of routing an inbound external message through access control and bot dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundRouteOutcome {
    Dispatched(DispatchOutcome),
    PendingAccess(BotAccessRequest),
    Denied,
    Paused,
    Error(BotErrorCategory),
}

/// Outcome of handling an external quick-reply / button gate callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateCallbackOutcome {
    Resolved { gate_id: String, decision: String },
    PendingAccess(BotAccessRequest),
    Denied,
    InvalidPayload,
    Failed(String),
}

impl GateCallbackOutcome {
    /// Returns a concise, user-safe acknowledgment notice suitable for callback toasts/replies.
    #[must_use]
    pub fn user_notice(&self) -> String {
        match self {
            Self::Resolved { decision, .. } => format!("Recorded decision: {decision}"),
            Self::PendingAccess(req) => format!(
                "Access approval required before responding. Ask the owner to approve token: {}",
                req.token
            ),
            Self::Denied => "You do not have permission to resolve this request.".to_owned(),
            Self::InvalidPayload => "Unsupported or expired action.".to_owned(),
            Self::Failed(_) => {
                "Could not record that decision (the request may already be closed).".to_owned()
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveTurnState {
    source: ExternalSource,
    target: DeliveryTarget,
    status_message_id: Option<String>,
    last_activity_at_ms: Option<u64>,
    last_activity_text: Option<String>,
}

type SenderMap = BTreeMap<(BotId, ExternalSource), Arc<dyn PlatformSender>>;

/// Shared conversation router coordinating allowlists, throttled status edits, and replies.
pub struct ConversationRouter {
    bridge: Arc<dyn ConnectorBridge>,
    senders: Arc<Mutex<SenderMap>>,
    active_turns: Mutex<BTreeMap<(BotId, SessionId), ActiveTurnState>>,
}

impl std::fmt::Debug for ConversationRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sender_count = self.senders.lock().map_or(0, |s| s.len());
        let turn_count = self.active_turns.lock().map_or(0, |t| t.len());
        f.debug_struct("ConversationRouter")
            .field("senders", &sender_count)
            .field("active_turns", &turn_count)
            .finish_non_exhaustive()
    }
}

impl ConversationRouter {
    #[must_use]
    pub fn new(bridge: Arc<dyn ConnectorBridge>) -> Self {
        Self {
            bridge,
            senders: Arc::new(Mutex::new(BTreeMap::new())),
            active_turns: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn register_sender(
        &self,
        bot_id: BotId,
        source: ExternalSource,
        sender: Arc<dyn PlatformSender>,
    ) {
        lock(&self.senders).insert((bot_id, source), sender);
    }

    pub fn unregister_sender(&self, bot_id: &BotId, source: ExternalSource) {
        lock(&self.senders).remove(&(bot_id.clone(), source));
    }

    pub fn clear_all(&self) {
        lock(&self.senders).clear();
        lock(&self.active_turns).clear();
    }

    #[must_use]
    pub fn sender_for(
        &self,
        bot_id: &BotId,
        source: ExternalSource,
    ) -> Option<Arc<dyn PlatformSender>> {
        lock(&self.senders).get(&(bot_id.clone(), source)).cloned()
    }

    /// Routes an inbound message through access control, conversation link lookup, and bot dispatch.
    pub fn handle_inbound_message(
        &self,
        msg: InboundExternalMessage,
        target: DeliveryTarget,
        sender: &dyn PlatformSender,
    ) -> InboundRouteOutcome {
        let Some(bot) = self.bridge.get_bot_config(&msg.bot_id) else {
            return InboundRouteOutcome::Error(BotErrorCategory::NotSetUp);
        };
        if bot.paused {
            return InboundRouteOutcome::Paused;
        }

        match self.bridge.check_or_request_access(
            &msg.bot_id,
            msg.source,
            &msg.sender_id,
            &msg.sender_name,
            &msg.channel_id,
        ) {
            Ok(AccessDecision::Allowed) => {}
            Ok(AccessDecision::Pending(req)) => {
                let notice = format!(
                    "Access approval is required before messaging {}. Ask the owner to approve request token `{}` in Krill (or run `krill bot access approve {}`).",
                    bot.name, req.token, req.token
                );
                send_chunked(sender, &target, &notice).ok();
                return InboundRouteOutcome::PendingAccess(req);
            }
            Ok(AccessDecision::Denied(_)) => {
                let notice =
                    "You do not have access to this bot. Contact the bot owner if you need access.";
                send_chunked(sender, &target, notice).ok();
                return InboundRouteOutcome::Denied;
            }
            Err(err) => {
                let category = classify_error(&err.to_string());
                if bot.notifications.could_not_finish {
                    let safe = user_safe_error_message(category, &bot.name);
                    send_chunked(sender, &target, &safe).ok();
                }
                return InboundRouteOutcome::Error(category);
            }
        }

        // Ensure conversation link exists before dispatch.
        if let Err(err) = self.bridge.lookup_or_create_link(
            &msg.bot_id,
            msg.source,
            msg.workspace_id.as_deref(),
            &msg.channel_id,
            msg.thread_id.as_deref(),
            msg.acts_for.clone(),
        ) {
            let category = classify_error(&err.to_string());
            if bot.notifications.could_not_finish {
                let safe = user_safe_error_message(category, &bot.name);
                send_chunked(sender, &target, &safe).ok();
            }
            return InboundRouteOutcome::Error(category);
        }

        let bot_id = msg.bot_id.clone();
        let source = msg.source;
        match self.bridge.dispatch_external_message(msg) {
            Ok(outcome) => {
                lock(&self.active_turns).insert(
                    (bot_id, outcome.session_id),
                    ActiveTurnState {
                        source,
                        target,
                        status_message_id: None,
                        last_activity_at_ms: None,
                        last_activity_text: None,
                    },
                );
                InboundRouteOutcome::Dispatched(outcome)
            }
            Err(err) => {
                let category = classify_error(&err.to_string());
                if bot.notifications.could_not_finish {
                    let safe = user_safe_error_message(category, &bot.name);
                    send_chunked(sender, &target, &safe).ok();
                }
                InboundRouteOutcome::Error(category)
            }
        }
    }

    /// Validates and routes a single-select approval/question callback (`gate:<gate_id>:<decision>`)
    /// strictly AFTER verifying the sender's allowlist status.
    pub fn handle_gate_callback(
        &self,
        bot_id: &BotId,
        source: ExternalSource,
        sender_id: &str,
        sender_name: &str,
        channel_id: &str,
        payload: &str,
    ) -> GateCallbackOutcome {
        let Some((gate_id, decision)) = parse_gate_callback(payload) else {
            return GateCallbackOutcome::InvalidPayload;
        };

        match self.bridge.check_or_request_access(
            bot_id,
            source,
            sender_id,
            sender_name,
            channel_id,
        ) {
            Ok(AccessDecision::Allowed) => {}
            Ok(AccessDecision::Pending(req)) => return GateCallbackOutcome::PendingAccess(req),
            Ok(AccessDecision::Denied(_)) => return GateCallbackOutcome::Denied,
            Err(err) => {
                return GateCallbackOutcome::Failed(sanitize_error_for_status(&err.to_string()));
            }
        }

        let resolved_by = format!("{}:{sender_id}", source.as_str());
        let rationale = Some(format!(
            "Resolved via {} by {}",
            source.as_str(),
            sender_name
        ));
        match self
            .bridge
            .reply_orchestration_gate(gate_id, decision, rationale, &resolved_by)
        {
            Ok(()) => GateCallbackOutcome::Resolved {
                gate_id: gate_id.to_owned(),
                decision: decision.to_owned(),
            },
            Err(err) => GateCallbackOutcome::Failed(sanitize_error_for_status(&err.to_string())),
        }
    }

    /// Updates a single in-place status message for `(bot_id, session_id)` at most once every 3s.
    pub fn on_turn_activity(&self, bot_id: &BotId, session_id: SessionId, activity: &str) {
        self.on_turn_activity_at(bot_id, session_id, activity, now_ms());
    }

    /// Deterministic-clock version of [`Self::on_turn_activity`] (also used by tests).
    pub fn on_turn_activity_at(
        &self,
        bot_id: &BotId,
        session_id: SessionId,
        activity: &str,
        ts_ms: u64,
    ) {
        let trimmed = activity.trim();
        if trimmed.is_empty() {
            return;
        }
        let status_text = truncate_chars(trimmed, 240);

        let key = (bot_id.clone(), session_id);
        let plan = {
            let mut turns = lock(&self.active_turns);
            let Some(state) = turns.get_mut(&key) else {
                return;
            };
            if let Some(last_ts) = state.last_activity_at_ms
                && ts_ms.saturating_sub(last_ts) < TURN_ACTIVITY_MIN_GAP_MS
            {
                return;
            }
            if state.status_message_id.is_some()
                && state.last_activity_text.as_deref() == Some(status_text.as_str())
            {
                return;
            }
            state.last_activity_at_ms = Some(ts_ms);
            state.last_activity_text = Some(status_text.clone());
            (
                state.source,
                state.target.clone(),
                state.status_message_id.clone(),
            )
        };

        let (source, target, existing_msg_id) = plan;
        let Some(sender) = self.sender_for(bot_id, source) else {
            return;
        };

        if let Some(msg_id) = existing_msg_id {
            if sender.edit_message(&target, &msg_id, &status_text).is_err() {
                tracing::debug!(
                    bot_id = bot_id.as_str(),
                    source = source.as_str(),
                    "in-place turn activity edit failed"
                );
            }
        } else if let Ok(new_msg_id) = sender.send_message(&target, &status_text) {
            let mut turns = lock(&self.active_turns);
            if let Some(state) = turns.get_mut(&key)
                && state.status_message_id.is_none()
            {
                state.status_message_id = Some(new_msg_id);
            }
        }
    }

    /// Posts the final chunked reply for `(bot_id, session_id)` on turn completion.
    /// Honors `[[KRILL_QUIET]]`, `BotNotifications::task_complete`, and Slack `mirror_desktop`.
    pub fn on_turn_complete(
        &self,
        bot_id: &BotId,
        session_id: SessionId,
        final_reply: Option<&str>,
    ) {
        let key = (bot_id.clone(), session_id);
        let active = lock(&self.active_turns).remove(&key);

        let Some(raw_text) = final_reply.map(str::trim).filter(|s| !s.is_empty()) else {
            return;
        };
        if raw_text.trim_end().ends_with(BOT_QUIET_MARKER) {
            return;
        }

        let bot = self.bridge.get_bot_config(bot_id);
        if bot.as_ref().is_some_and(|b| !b.notifications.task_complete) {
            return;
        }

        if let Some(turn) = active {
            if let Some(sender) = self.sender_for(bot_id, turn.source)
                && send_chunked(sender.as_ref(), &turn.target, raw_text).is_err()
            {
                tracing::warn!(
                    bot_id = bot_id.as_str(),
                    source = turn.source.as_str(),
                    "failed posting final turn reply"
                );
            }
            return;
        }

        // Desktop-initiated turn completion: optionally mirror to Slack when `mirror_desktop` is on.
        if let Some(cfg) = bot
            && !cfg.paused
            && cfg.connectors.slack.enabled
            && cfg.connectors.slack.mirror_desktop
            && let Some(channel_id) = cfg.connectors.slack.allowed_channel_ids.first()
            && let Some(sender) = self.sender_for(bot_id, ExternalSource::Slack)
        {
            let target = DeliveryTarget {
                source: ExternalSource::Slack,
                workspace_id: None,
                channel_id: channel_id.clone(),
                thread_id: None,
                reply_to_message_id: None,
            };
            send_chunked(sender.as_ref(), &target, raw_text).ok();
        }
    }

    /// Posts an explicit reply (`krill bot reply` / `BotRpcRequest::PostBotReply`).
    pub fn post_reply(
        &self,
        link: Option<&ExternalConversationLink>,
        reply: &OutboundBotReply,
    ) -> io::Result<Option<String>> {
        let Some(link) = link else {
            return Ok(None);
        };
        let Some(sender) = self.sender_for(&reply.bot_id, link.source) else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                format!(
                    "connector '{}' is not active for this bot",
                    link.source.as_str()
                ),
            ));
        };
        let target = DeliveryTarget::from_link(link);
        if let Some(ref reaction) = reply.reaction
            && let Some(ref msg_id) = link.thread_id
        {
            sender.add_reaction(&target, msg_id, reaction).ok();
        }
        let text = reply.text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let last_id = send_chunked(sender.as_ref(), &target, text)?;
        Ok(last_id)
    }
}

/// Sends `text` split safely under `sender.max_chunk_chars()`, returning the last message ID.
pub fn send_chunked(
    sender: &dyn PlatformSender,
    target: &DeliveryTarget,
    text: &str,
) -> io::Result<Option<String>> {
    let chunks = chunk_text(text, sender.max_chunk_chars());
    let mut last_id = None;
    for chunk in chunks {
        let msg_id = sender.send_message(target, &chunk)?;
        last_id = Some(msg_id);
    }
    Ok(last_id)
}

/// Parses and strictly validates a gate callback payload of the form `gate:<gate_id>:<decision>`.
#[must_use]
pub fn parse_gate_callback(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.trim().strip_prefix("gate:")?;
    let (gate_id, decision) = rest.split_once(':')?;
    let gate_id = gate_id.trim();
    let decision = decision.trim();
    if gate_id.is_empty()
        || gate_id.len() > 128
        || !gate_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return None;
    }
    if decision.is_empty() || decision.len() > 128 || decision.chars().any(char::is_control) {
        return None;
    }
    Some((gate_id, decision))
}

/// Splits UTF-8 `text` into non-empty chunks each containing at most `max_chars` Unicode scalar
/// values, preferring paragraph, newline, or space boundaries when available.
#[must_use]
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let limit = max_chars.max(1);
    if trimmed.chars().count() <= limit {
        return vec![trimmed.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut remaining = trimmed;
    while !remaining.is_empty() {
        let total_chars = remaining.chars().count();
        if total_chars <= limit {
            chunks.push(remaining.to_owned());
            break;
        }
        // Byte offset of the `limit`-th char.
        let hard_byte = remaining
            .char_indices()
            .nth(limit)
            .map_or(remaining.len(), |(idx, _)| idx);
        let window = remaining.get(..hard_byte).unwrap_or(remaining);

        // Prefer paragraph break, then newline, then whitespace in the second half of the window.
        let min_break = hard_byte / 2;
        let split_at = window
            .rfind("\n\n")
            .filter(|&pos| pos >= min_break)
            .or_else(|| window.rfind('\n').filter(|&pos| pos >= min_break))
            .or_else(|| {
                window
                    .rfind(char::is_whitespace)
                    .filter(|&pos| pos >= min_break)
            })
            .unwrap_or(hard_byte);

        let piece = remaining.get(..split_at).unwrap_or_default().trim_end();
        if !piece.is_empty() {
            chunks.push(piece.to_owned());
        }
        let rest = remaining.get(split_at..).unwrap_or_default().trim_start();
        if rest.len() == remaining.len() {
            // Defensive progress guarantee even if leading whitespace filled the slice.
            let fallback = remaining.get(..hard_byte).unwrap_or_default();
            if !fallback.is_empty() {
                chunks.push(fallback.to_owned());
            }
            remaining = remaining.get(hard_byte..).unwrap_or_default().trim_start();
        } else {
            remaining = rest;
        }
    }
    chunks
}

/// Classifies an error string into the user-facing [`BotErrorCategory`] taxonomy.
#[must_use]
pub fn classify_error(raw: &str) -> BotErrorCategory {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("invalid_auth")
        || lower.contains("not_authed")
        || lower.contains("token_revoked")
        || lower.contains("account_inactive")
        || lower.contains("signed out")
        || lower.contains("sign in")
        || lower.contains("authentication failed")
        || lower.contains("close 4004")
    {
        BotErrorCategory::SignedOut
    } else if lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("ratelimited")
        || lower.contains("rate_limited")
        || lower.contains("too many requests")
        || lower.contains("usage limit")
        || lower.contains("quota")
        || lower.contains("retry_after")
    {
        BotErrorCategory::UsageLimit
    } else if lower.contains("not set up")
        || lower.contains("not configured")
        || lower.contains("missing connector")
        || lower.contains("missing token")
        || lower.contains("no token")
        || lower.contains("secret")
        || lower.contains("disallowed intents")
        || lower.contains("close 4014")
    {
        BotErrorCategory::NotSetUp
    } else if lower.contains("folder")
        || lower.contains("workspace")
        || lower.contains("directory")
        || lower.contains("no such file")
    {
        BotErrorCategory::FolderMissing
    } else {
        BotErrorCategory::AgentError
    }
}

/// Returns a safe, concise external message for a [`BotErrorCategory`] with zero internal leaks.
#[must_use]
pub fn user_safe_error_message(category: BotErrorCategory, bot_name: &str) -> String {
    match category {
        BotErrorCategory::SignedOut => {
            format!("{bot_name} needs to be signed in again. Check credentials in Krill.")
        }
        BotErrorCategory::UsageLimit => {
            format!("{bot_name} hit a temporary rate or usage limit. Please try again shortly.")
        }
        BotErrorCategory::NotSetUp => {
            format!("{bot_name} is not fully configured yet. Check connector setup in Krill.")
        }
        BotErrorCategory::FolderMissing => {
            format!(
                "{bot_name}'s workspace folder is unavailable on this machine. Check bot settings in Krill."
            )
        }
        BotErrorCategory::AgentError => {
            format!(
                "{bot_name} ran into an unexpected error handling your message. Please try again."
            )
        }
    }
}

/// Strips potentially sensitive URL segments, tokens, or filesystem paths from status error text.
#[must_use]
pub fn sanitize_error_for_status(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "connector error".to_owned();
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("/bot")
        || lower.contains("xoxb-")
        || lower.contains("xapp-")
        || lower.contains("bearer ")
        || lower.contains("authorization")
        || lower.contains("/users/")
        || lower.contains("/home/")
        || lower.contains(":\\")
    {
        return match classify_error(trimmed) {
            BotErrorCategory::SignedOut => "authentication failed".to_owned(),
            BotErrorCategory::UsageLimit => "rate limited".to_owned(),
            BotErrorCategory::NotSetUp => "connector not configured".to_owned(),
            BotErrorCategory::FolderMissing => "workspace folder missing".to_owned(),
            BotErrorCategory::AgentError => "connector request failed".to_owned(),
        };
    }
    truncate_chars(trimmed, 180)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(idx, _)| idx);
    format!("{}...", text.get(..end).unwrap_or_default())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::{ActsFor, BotAccessRequestStatus, BotConfig, BotConnectorStatus};
    use zeroize::Zeroizing;

    #[derive(Default)]
    struct MockSender {
        sent: Mutex<Vec<(String, String)>>,
        edits: Mutex<Vec<(String, String)>>,
    }

    impl PlatformSender for MockSender {
        fn source(&self) -> ExternalSource {
            ExternalSource::Telegram
        }
        fn max_chunk_chars(&self) -> usize {
            40
        }
        fn send_message(&self, target: &DeliveryTarget, text: &str) -> io::Result<String> {
            let mut s = lock(&self.sent);
            let id = format!("m{}", s.len() + 1);
            s.push((target.channel_id.clone(), text.to_owned()));
            Ok(id)
        }
        fn edit_message(
            &self,
            _target: &DeliveryTarget,
            message_id: &str,
            text: &str,
        ) -> io::Result<()> {
            lock(&self.edits).push((message_id.to_owned(), text.to_owned()));
            Ok(())
        }
    }

    struct MockBridge {
        bot: Mutex<BotConfig>,
        decision: Mutex<AccessDecision>,
        dispatched: Mutex<Vec<InboundExternalMessage>>,
        gates: Mutex<Vec<(String, String, String)>>,
    }

    impl ConnectorBridge for MockBridge {
        fn dispatch_external_message(
            &self,
            msg: InboundExternalMessage,
        ) -> io::Result<DispatchOutcome> {
            let link = ExternalConversationLink {
                id: "link-1".to_owned(),
                bot_id: msg.bot_id.clone(),
                source: msg.source,
                workspace_id: msg.workspace_id.clone(),
                channel_id: msg.channel_id.clone(),
                thread_id: msg.thread_id.clone(),
                session_id: Some(SessionId(9)),
                acts_for: msg.acts_for.clone(),
                created_at_ms: 1,
                updated_at_ms: 1,
            };
            lock(&self.dispatched).push(msg);
            Ok(DispatchOutcome {
                link,
                session_id: SessionId(9),
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
        ) -> io::Result<ExternalConversationLink> {
            Ok(ExternalConversationLink {
                id: "link-1".to_owned(),
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
            gate_id: &str,
            decision: &str,
            _rationale: Option<String>,
            resolved_by: &str,
        ) -> io::Result<()> {
            lock(&self.gates).push((
                gate_id.to_owned(),
                decision.to_owned(),
                resolved_by.to_owned(),
            ));
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

    #[test]
    fn router_enforces_access_throttles_activity_respects_quiet_and_gates() {
        let bot_id = BotId::new("bot-r1").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "Ops Bot", "claude", 1_000).unwrap();
        let pending_req = BotAccessRequest {
            token: "tok-123".to_owned(),
            bot_id: bot_id.clone(),
            source: ExternalSource::Telegram,
            sender_id: "u1".to_owned(),
            sender_name: "Alice".to_owned(),
            channel_id: "c1".to_owned(),
            status: BotAccessRequestStatus::Pending,
            created_at: 1_000,
            decided_at: None,
        };
        let bridge = Arc::new(MockBridge {
            bot: Mutex::new(bot),
            decision: Mutex::new(AccessDecision::Pending(pending_req)),
            dispatched: Mutex::new(Vec::new()),
            gates: Mutex::new(Vec::new()),
        });
        let router = ConversationRouter::new(Arc::clone(&bridge) as Arc<dyn ConnectorBridge>);
        let sender = Arc::new(MockSender::default());
        router.register_sender(
            bot_id.clone(),
            ExternalSource::Telegram,
            Arc::clone(&sender) as Arc<dyn PlatformSender>,
        );

        let target = DeliveryTarget {
            source: ExternalSource::Telegram,
            workspace_id: None,
            channel_id: "c1".to_owned(),
            thread_id: None,
            reply_to_message_id: None,
        };
        let msg = InboundExternalMessage {
            bot_id: bot_id.clone(),
            source: ExternalSource::Telegram,
            workspace_id: None,
            channel_id: "c1".to_owned(),
            thread_id: None,
            sender_id: "u1".to_owned(),
            sender_name: "Alice".to_owned(),
            text: "deploy prod".to_owned(),
            attachments: Vec::new(),
            external_message_id: Some("10".to_owned()),
            acts_for: ActsFor::Owner,
        };

        // Pending access: replies with token, NEVER dispatches or resolves gates.
        assert!(matches!(
            router.handle_inbound_message(msg.clone(), target.clone(), sender.as_ref()),
            InboundRouteOutcome::PendingAccess(_)
        ));
        assert!(lock(&bridge.dispatched).is_empty());
        assert!(
            lock(&sender.sent)
                .iter()
                .any(|(_, txt)| txt.contains("tok-123"))
        );

        assert!(matches!(
            router.handle_gate_callback(
                &bot_id,
                ExternalSource::Telegram,
                "u1",
                "Alice",
                "c1",
                "gate:g-1:approve"
            ),
            GateCallbackOutcome::PendingAccess(_)
        ));
        assert!(lock(&bridge.gates).is_empty());

        // Approve sender -> dispatches, 3s activity throttling works, final reply chunks cleanly.
        *lock(&bridge.decision) = AccessDecision::Allowed;
        lock(&sender.sent).clear();
        assert!(matches!(
            router.handle_inbound_message(msg, target, sender.as_ref()),
            InboundRouteOutcome::Dispatched(_)
        ));
        assert_eq!(lock(&bridge.dispatched).len(), 1);

        router.on_turn_activity_at(&bot_id, SessionId(9), "Using Bash...", 10_000);
        // Within 3s -> throttled!
        router.on_turn_activity_at(&bot_id, SessionId(9), "Using Read...", 11_500);
        // After 3s -> edits the single status message in place!
        router.on_turn_activity_at(&bot_id, SessionId(9), "Thinking...", 13_100);
        assert_eq!(lock(&sender.sent).len(), 1);
        assert_eq!(lock(&sender.edits).len(), 1);
        assert_eq!(
            lock(&sender.edits)
                .first()
                .map(|(id, t)| (id.as_str(), t.as_str())),
            Some(("m1", "Thinking..."))
        );

        // Final reply >40 chars chunks into multiple messages.
        lock(&sender.sent).clear();
        router.on_turn_complete(
            &bot_id,
            SessionId(9),
            Some(
                "First paragraph of the deployment summary.\n\nSecond paragraph confirming health.",
            ),
        );
        assert!(lock(&sender.sent).len() >= 2);

        // Allowed gate callback resolves gate cleanly.
        assert_eq!(
            router.handle_gate_callback(
                &bot_id,
                ExternalSource::Telegram,
                "u1",
                "Alice",
                "c1",
                "gate:g-1:approve"
            ),
            GateCallbackOutcome::Resolved {
                gate_id: "g-1".to_owned(),
                decision: "approve".to_owned(),
            }
        );
        assert_eq!(lock(&bridge.gates).len(), 1);

        // Quiet reply and disabled task_complete notification both suppress outbound messages.
        lock(&sender.sent).clear();
        router.on_turn_complete(&bot_id, SessionId(9), Some("Nothing new.\n[[KRILL_QUIET]]"));
        assert!(lock(&sender.sent).is_empty());
    }

    #[test]
    fn chunk_text_error_taxonomy_and_gate_validation_cover_edge_cases() {
        let chunks = chunk_text(
            "한글 테스트 문장입니다. 아주 긴 응답을 안전하게 나눕니다.",
            10,
        );
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(c.chars().count() <= 10);
        }

        assert_eq!(
            classify_error("HTTP 401 invalid_auth"),
            BotErrorCategory::SignedOut
        );
        assert_eq!(
            classify_error("HTTP 429 rate_limited"),
            BotErrorCategory::UsageLimit
        );
        assert_eq!(
            classify_error("missing token in secret vault"),
            BotErrorCategory::NotSetUp
        );
        assert_eq!(
            classify_error("workspace folder missing"),
            BotErrorCategory::FolderMissing
        );
        assert_eq!(
            classify_error("unexpected failure"),
            BotErrorCategory::AgentError
        );

        assert_eq!(
            parse_gate_callback("gate:gate-42:yes"),
            Some(("gate-42", "yes"))
        );
        assert!(parse_gate_callback("not-a-gate").is_none());
        assert!(parse_gate_callback("gate:bad id:yes").is_none());
        assert!(parse_gate_callback("gate:g1:").is_none());

        let sanitized = sanitize_error_for_status(
            "POST https://api.telegram.org/bot12345:SECRET/sendMessage failed 401",
        );
        assert!(!sanitized.contains("SECRET"));
        assert!(!sanitized.contains("/bot"));
    }
}
