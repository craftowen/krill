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

use std::collections::{BTreeMap, VecDeque};
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
/// Cooldown (ms) before re-sending duplicate pending-approval notices to the same sender/channel.
pub const PENDING_NOTICE_COOLDOWN_MS: u64 = 60_000;
const MAX_HELD_MESSAGES_TOTAL: usize = 256;
const MAX_HELD_MESSAGES_PER_BOT: usize = 50;
const MAX_HELD_TEXT_BYTES: usize = 16 * 1024;
const MAX_TRACKED_LINK_INBOUND_IDS: usize = 2_048;

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
    fn split_chunks(&self, text: &str) -> Vec<String> {
        chunk_text(text, self.max_chunk_chars())
    }
    fn send_files(
        &self,
        _target: &DeliveryTarget,
        _files: &[String],
        _caption: Option<&str>,
    ) -> io::Result<Option<String>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} attachment uploads are not supported",
                self.source().as_str()
            ),
        ))
    }
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
    Paused,
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
            Self::Paused => "This bot is currently paused.".to_owned(),
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
    last_inbound_message_id: Option<String>,
    status_message_id: Option<String>,
    last_activity_at_ms: Option<u64>,
    last_activity_text: Option<String>,
}

type DeferredInboundLoader = Box<dyn FnOnce(&mut InboundExternalMessage) -> bool + Send + 'static>;

struct HeldInboundMessage {
    msg: InboundExternalMessage,
    target: DeliveryTarget,
    created_at_ms: u64,
    on_allowed: Option<DeferredInboundLoader>,
}

type SenderMap = BTreeMap<(BotId, ExternalSource), Arc<dyn PlatformSender>>;
type ThrottleKey = (BotId, ExternalSource, String, String);

/// Shared conversation router coordinating allowlists, throttled status edits, and replies.
pub struct ConversationRouter {
    bridge: Arc<dyn ConnectorBridge>,
    senders: Arc<Mutex<SenderMap>>,
    active_turns: Mutex<BTreeMap<(BotId, SessionId), VecDeque<ActiveTurnState>>>,
    link_last_inbound: Mutex<BTreeMap<String, String>>,
    pending_notice_throttle: Mutex<BTreeMap<ThrottleKey, u64>>,
    held_messages: Mutex<BTreeMap<String, HeldInboundMessage>>,
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
            link_last_inbound: Mutex::new(BTreeMap::new()),
            pending_notice_throttle: Mutex::new(BTreeMap::new()),
            held_messages: Mutex::new(BTreeMap::new()),
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
        lock(&self.link_last_inbound).clear();
        lock(&self.pending_notice_throttle).clear();
        lock(&self.held_messages).clear();
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
        self.handle_inbound_message_at(msg, target, sender, now_ms())
    }

    /// Routes an inbound message and invokes `on_allowed` ONLY after `paused`, channel/guild,
    /// workspace, and user access checks all pass (e.g. for deferred attachment downloads).
    /// When access is `Pending`, `on_allowed` is retained with the held message and runs upon
    /// approval so attachments are never lost on replay yet never downloaded before approval (MB-45).
    /// Returns `None` if `on_allowed` decides there is nothing left to dispatch.
    pub fn handle_inbound_message_with_loader<F>(
        &self,
        msg: InboundExternalMessage,
        target: DeliveryTarget,
        sender: &dyn PlatformSender,
        on_allowed: F,
    ) -> Option<InboundRouteOutcome>
    where
        F: FnOnce(&mut InboundExternalMessage) -> bool + Send + 'static,
    {
        self.handle_inbound_message_at_with_loader(msg, target, sender, now_ms(), on_allowed)
    }

    /// Deterministic-clock version of [`Self::handle_inbound_message`] for testing throttling/replay.
    pub fn handle_inbound_message_at(
        &self,
        msg: InboundExternalMessage,
        target: DeliveryTarget,
        sender: &dyn PlatformSender,
        ts_ms: u64,
    ) -> InboundRouteOutcome {
        self.handle_inbound_message_at_with_loader(msg, target, sender, ts_ms, |_| true)
            .unwrap_or(InboundRouteOutcome::Denied)
    }

    fn handle_inbound_message_at_with_loader<F>(
        &self,
        mut msg: InboundExternalMessage,
        target: DeliveryTarget,
        sender: &dyn PlatformSender,
        ts_ms: u64,
        on_allowed: F,
    ) -> Option<InboundRouteOutcome>
    where
        F: FnOnce(&mut InboundExternalMessage) -> bool + Send + 'static,
    {
        let Some(bot) = self.bridge.get_bot_config(&msg.bot_id) else {
            return Some(InboundRouteOutcome::Error(BotErrorCategory::NotSetUp));
        };
        if bot.paused {
            return Some(InboundRouteOutcome::Paused);
        }
        if !passes_independent_channel_gate(
            &bot,
            msg.source,
            msg.workspace_id.as_deref(),
            &msg.channel_id,
            msg.thread_id.as_deref(),
        ) {
            return Some(InboundRouteOutcome::Denied);
        }

        let user_gate_denied = !passes_independent_user_gate(
            &bot,
            msg.source,
            msg.workspace_id.as_deref(),
            &msg.sender_id,
        );
        let bypass_possible = has_channel_or_bypass_overlap(&bot, msg.source);

        match self.bridge.check_or_request_access_with_workspace(
            &msg.bot_id,
            msg.source,
            msg.workspace_id.as_deref(),
            &msg.sender_id,
            &msg.sender_name,
            &msg.channel_id,
        ) {
            Ok(AccessDecision::Allowed) => {
                if user_gate_denied && bypass_possible {
                    let notice = "You do not have access to this bot. Contact the bot owner if you need access.";
                    send_chunked(sender, &target, notice).ok();
                    return Some(InboundRouteOutcome::Denied);
                }
            }
            Ok(AccessDecision::Pending(req)) => {
                self.record_held_message(
                    &req.token,
                    &msg,
                    &target,
                    ts_ms,
                    Some(Box::new(on_allowed)),
                );
                let throttle_key = (
                    msg.bot_id.clone(),
                    msg.source,
                    msg.sender_id.clone(),
                    msg.channel_id.clone(),
                );
                let should_notify = {
                    let mut map = lock(&self.pending_notice_throttle);
                    match map.get(&throttle_key).copied() {
                        Some(last) if ts_ms.saturating_sub(last) < PENDING_NOTICE_COOLDOWN_MS => {
                            false
                        }
                        _ => {
                            if map.len() > 2_048 {
                                map.clear();
                            }
                            map.insert(throttle_key, ts_ms);
                            true
                        }
                    }
                };
                if should_notify {
                    let notice = format!(
                        "Access approval is required before messaging {}. Ask the owner to approve request token `{}` in Krill (or run `krill bot access approve {}`).",
                        bot.name, req.token, req.token
                    );
                    send_chunked(sender, &target, &notice).ok();
                }
                return Some(InboundRouteOutcome::PendingAccess(req));
            }
            Ok(AccessDecision::Denied(_)) => {
                let notice =
                    "You do not have access to this bot. Contact the bot owner if you need access.";
                send_chunked(sender, &target, notice).ok();
                return Some(InboundRouteOutcome::Denied);
            }
            Err(err) => {
                let category = classify_error(&err.to_string());
                if bot.notifications.could_not_finish {
                    let safe = user_safe_error_message(category, &bot.name);
                    send_chunked(sender, &target, &safe).ok();
                }
                return Some(InboundRouteOutcome::Error(category));
            }
        }

        if !on_allowed(&mut msg) {
            return None;
        }

        Some(self.dispatch_allowed_message(
            msg,
            target,
            Some(sender),
            &bot.name,
            bot.notifications.could_not_finish,
        ))
    }

    fn record_held_message(
        &self,
        token: &str,
        msg: &InboundExternalMessage,
        target: &DeliveryTarget,
        ts_ms: u64,
        on_allowed: Option<DeferredInboundLoader>,
    ) {
        let mut held_msg = msg.clone();
        if held_msg.text.len() > MAX_HELD_TEXT_BYTES {
            let mut end = MAX_HELD_TEXT_BYTES;
            while end > 0 && !held_msg.text.is_char_boundary(end) {
                end -= 1;
            }
            held_msg.text.truncate(end);
        }
        let mut map = lock(&self.held_messages);
        let per_bot_count = map.values().filter(|h| h.msg.bot_id == msg.bot_id).count();
        if !map.contains_key(token)
            && per_bot_count >= MAX_HELD_MESSAGES_PER_BOT
            && let Some(oldest_key) = map
                .iter()
                .filter(|(_, h)| h.msg.bot_id == msg.bot_id)
                .min_by_key(|(_, h)| h.created_at_ms)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest_key);
        }
        if map.len() >= MAX_HELD_MESSAGES_TOTAL
            && !map.contains_key(token)
            && let Some(oldest_global) = map
                .iter()
                .min_by_key(|(_, h)| h.created_at_ms)
                .map(|(k, _)| k.clone())
        {
            map.remove(&oldest_global);
        }
        map.insert(
            token.to_owned(),
            HeldInboundMessage {
                msg: held_msg,
                target: target.clone(),
                created_at_ms: ts_ms,
                on_allowed,
            },
        );
    }

    /// Sends an approval notice and replays the sender's last blocked inbound message once.
    pub fn on_access_approved(&self, req: &BotAccessRequest) {
        let held = lock(&self.held_messages).remove(&req.token);
        lock(&self.pending_notice_throttle).remove(&(
            req.bot_id.clone(),
            req.source,
            req.sender_id.clone(),
            req.channel_id.clone(),
        ));
        let Some(bot) = self.bridge.get_bot_config(&req.bot_id) else {
            return;
        };
        if bot.paused {
            return;
        }
        let sender_opt = self.sender_for(&req.bot_id, req.source);
        let target = held.as_ref().map_or_else(
            || DeliveryTarget {
                source: req.source,
                workspace_id: None,
                channel_id: req.channel_id.clone(),
                thread_id: None,
                reply_to_message_id: None,
            },
            |h| h.target.clone(),
        );
        if let Some(ref sender) = sender_opt {
            let notice = format!(
                "Access approved! Processing your request for {} now.",
                bot.name
            );
            send_chunked(sender.as_ref(), &target, &notice).ok();
        }
        if let Some(mut held) = held {
            let should_dispatch = match held.on_allowed.take() {
                Some(loader) => loader(&mut held.msg),
                None => true,
            };
            if should_dispatch {
                self.dispatch_allowed_message(
                    held.msg,
                    held.target,
                    sender_opt.as_deref(),
                    &bot.name,
                    bot.notifications.could_not_finish,
                );
            }
        }
    }

    /// Drops any held message associated with a denied/expired access request token.
    pub fn discard_held_message(&self, token: &str) {
        lock(&self.held_messages).remove(token);
    }

    fn dispatch_allowed_message(
        &self,
        msg: InboundExternalMessage,
        target: DeliveryTarget,
        sender: Option<&dyn PlatformSender>,
        bot_name: &str,
        notify_could_not_finish: bool,
    ) -> InboundRouteOutcome {
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
            if notify_could_not_finish && let Some(s) = sender {
                let safe = user_safe_error_message(category, bot_name);
                send_chunked(s, &target, &safe).ok();
            }
            return InboundRouteOutcome::Error(category);
        }

        let bot_id = msg.bot_id.clone();
        let source = msg.source;
        let inbound_msg_id = msg.external_message_id.clone();
        match self.bridge.dispatch_external_message(msg) {
            Ok(outcome) => {
                if let Some(ref ext_id) = inbound_msg_id {
                    let mut links = lock(&self.link_last_inbound);
                    if links.len() >= MAX_TRACKED_LINK_INBOUND_IDS
                        && !links.contains_key(&outcome.link.id)
                        && let Some(first_key) = links.keys().next().cloned()
                    {
                        links.remove(&first_key);
                    }
                    links.insert(outcome.link.id.clone(), ext_id.clone());
                }
                let mut turns = lock(&self.active_turns);
                let queue = turns.entry((bot_id, outcome.session_id)).or_default();
                if queue.len() >= 64 {
                    queue.pop_front();
                }
                queue.push_back(ActiveTurnState {
                    source,
                    target,
                    last_inbound_message_id: inbound_msg_id,
                    status_message_id: None,
                    last_activity_at_ms: None,
                    last_activity_text: None,
                });
                InboundRouteOutcome::Dispatched(outcome)
            }
            Err(err) => {
                let category = classify_error(&err.to_string());
                if notify_could_not_finish && let Some(s) = sender {
                    let safe = user_safe_error_message(category, bot_name);
                    send_chunked(s, &target, &safe).ok();
                }
                InboundRouteOutcome::Error(category)
            }
        }
    }

    /// Validates and routes a single-select approval/question callback (`gate:<gate_id>:<decision>`)
    /// strictly AFTER verifying bot pause state, channel/guild gates, workspace isolation, and
    /// independent user allowlists.
    pub fn handle_gate_callback(
        &self,
        bot_id: &BotId,
        source: ExternalSource,
        sender_id: &str,
        sender_name: &str,
        channel_id: &str,
        payload: &str,
    ) -> GateCallbackOutcome {
        self.handle_gate_callback_with_context(
            bot_id,
            source,
            None,
            sender_id,
            sender_name,
            channel_id,
            None,
            payload,
        )
    }

    /// Context-aware gate callback handler enforcing the same `paused`, channel/guild/thread,
    /// workspace isolation, and independent user gates as [`Self::handle_inbound_message_at`].
    #[expect(clippy::too_many_arguments, reason = "external gate callback context")]
    pub fn handle_gate_callback_with_context(
        &self,
        bot_id: &BotId,
        source: ExternalSource,
        workspace_id: Option<&str>,
        sender_id: &str,
        sender_name: &str,
        channel_id: &str,
        thread_id: Option<&str>,
        payload: &str,
    ) -> GateCallbackOutcome {
        let Some((gate_id, decision)) = parse_gate_callback(payload) else {
            return GateCallbackOutcome::InvalidPayload;
        };

        let Some(bot) = self.bridge.get_bot_config(bot_id) else {
            return GateCallbackOutcome::Failed("bot is not set up".to_owned());
        };
        if bot.paused {
            return GateCallbackOutcome::Paused;
        }
        if !passes_independent_channel_gate(&bot, source, workspace_id, channel_id, thread_id) {
            return GateCallbackOutcome::Denied;
        }

        let user_gate_denied = !passes_independent_user_gate(&bot, source, workspace_id, sender_id);
        let bypass_possible = has_channel_or_bypass_overlap(&bot, source);

        match self.bridge.check_or_request_access_with_workspace(
            bot_id,
            source,
            workspace_id,
            sender_id,
            sender_name,
            channel_id,
        ) {
            Ok(AccessDecision::Allowed) => {
                if user_gate_denied && bypass_possible {
                    return GateCallbackOutcome::Denied;
                }
            }
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
            let Some(state) = turns.get_mut(&key).and_then(VecDeque::front_mut) else {
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
            if let Some(state) = turns.get_mut(&key).and_then(VecDeque::front_mut)
                && state.status_message_id.is_none()
            {
                state.status_message_id = Some(new_msg_id);
            }
        }
    }

    /// Cleans up queued active turn states when a bot session exits or is killed (MB-10).
    pub fn on_session_removed(&self, session_id: SessionId) {
        lock(&self.active_turns).retain(|(_, sid), _| *sid != session_id);
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
        let active = {
            let mut turns = lock(&self.active_turns);
            let popped = turns.get_mut(&key).and_then(VecDeque::pop_front);
            if turns.get(&key).is_some_and(VecDeque::is_empty) {
                turns.remove(&key);
            }
            popped
        };

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
            let _ = &turn.last_inbound_message_id;
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
    /// Fails closed (`NotFound`) when no matching external conversation link exists,
    /// propagates `add_reaction` errors and missing targets, and sends `--file` attachments.
    pub fn post_reply(
        &self,
        link: Option<&ExternalConversationLink>,
        reply: &OutboundBotReply,
    ) -> io::Result<Option<String>> {
        let Some(link) = link else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "no external conversation link found for bot '{}'",
                    reply.bot_id.as_str()
                ),
            ));
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
        let mut target = DeliveryTarget::from_link(link);
        let tracked_inbound_id = lock(&self.link_last_inbound).get(&link.id).cloned();
        if let Some(ref msg_id) = tracked_inbound_id {
            target.reply_to_message_id = Some(msg_id.clone());
        }

        let mut last_ref: Option<String> = None;
        if let Some(ref reaction) = reply.reaction {
            let react_target_id = tracked_inbound_id
                .or_else(|| link.thread_id.clone())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "cannot add reaction: no inbound message_id or thread_id for this conversation link",
                    )
                })?;
            sender.add_reaction(&target, &react_target_id, reaction)?;
            last_ref = Some(react_target_id);
        }

        let text = reply.text.trim();
        if text.is_empty() && reply.files.is_empty() {
            if reply.reaction.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "reply requires non-empty text, files, or reaction",
                ));
            }
            return Ok(last_ref);
        }

        if !text.is_empty()
            && let Some(msg_id) = send_chunked(sender.as_ref(), &target, text)?
        {
            last_ref = Some(msg_id);
        }

        if !reply.files.is_empty()
            && let Some(file_id) = sender.send_files(&target, &reply.files, None)?
        {
            last_ref = Some(file_id);
        }

        Ok(last_ref)
    }
}

/// Sends `text` split safely under `sender.max_chunk_chars()`, returning the last message ID.
pub fn send_chunked(
    sender: &dyn PlatformSender,
    target: &DeliveryTarget,
    text: &str,
) -> io::Result<Option<String>> {
    let chunks = sender.split_chunks(text);
    let mut last_id = None;
    for chunk in chunks {
        let msg_id = sender.send_message(target, &chunk)?;
        last_id = Some(msg_id);
    }
    Ok(last_id)
}

fn passes_independent_channel_gate(
    bot: &krill_proto::bots::BotConfig,
    source: ExternalSource,
    workspace_id: Option<&str>,
    channel_id: &str,
    thread_id: Option<&str>,
) -> bool {
    match source {
        ExternalSource::Telegram => {
            if bot.connectors.telegram.allowed_chat_ids.is_empty() {
                return true;
            }
            channel_id
                .parse::<i64>()
                .is_ok_and(|cid| bot.connectors.telegram.allowed_chat_ids.contains(&cid))
        }
        ExternalSource::Slack => {
            if bot.connectors.slack.allowed_channel_ids.is_empty() || channel_id.starts_with('D') {
                return true;
            }
            bot.connectors
                .slack
                .allowed_channel_ids
                .iter()
                .any(|c| c == channel_id)
        }
        ExternalSource::Discord => {
            if workspace_id.is_some()
                && !bot.connectors.discord.allowed_guild_ids.is_empty()
                && !workspace_id.is_some_and(|gid| {
                    bot.connectors
                        .discord
                        .allowed_guild_ids
                        .iter()
                        .any(|g| g == gid)
                })
            {
                return false;
            }
            if workspace_id.is_some() && !bot.connectors.discord.allowed_channel_ids.is_empty() {
                return bot
                    .connectors
                    .discord
                    .allowed_channel_ids
                    .iter()
                    .any(|c| c == channel_id || thread_id.is_some_and(|tid| c == tid));
            }
            true
        }
        _ => true,
    }
}

fn is_bot_member(
    bot: &krill_proto::bots::BotConfig,
    source: ExternalSource,
    workspace_id: Option<&str>,
    sender_id: &str,
) -> bool {
    bot.members.iter().any(|m| {
        m.source == source
            && m.id == sender_id
            && match (&m.workspace_id, workspace_id) {
                (Some(expected), Some(actual)) => expected == actual,
                _ => true,
            }
    })
}

fn passes_independent_user_gate(
    bot: &krill_proto::bots::BotConfig,
    source: ExternalSource,
    workspace_id: Option<&str>,
    sender_id: &str,
) -> bool {
    if is_bot_member(bot, source, workspace_id, sender_id) {
        return true;
    }
    match source {
        ExternalSource::Telegram => {
            bot.connectors.telegram.acts_for == krill_proto::bots::ActsFor::Anyone
                || bot.connectors.telegram.allowed_user_ids.is_empty()
                || sender_id
                    .parse::<i64>()
                    .is_ok_and(|uid| bot.connectors.telegram.allowed_user_ids.contains(&uid))
        }
        ExternalSource::Slack => {
            bot.connectors.slack.acts_for == krill_proto::bots::ActsFor::Anyone
                || bot.connectors.slack.allowed_user_ids.is_empty()
                || bot
                    .connectors
                    .slack
                    .allowed_user_ids
                    .iter()
                    .any(|u| u == sender_id)
        }
        ExternalSource::Discord => {
            bot.connectors.discord.acts_for == krill_proto::bots::ActsFor::Anyone
                || bot.connectors.discord.allowed_user_ids.is_empty()
                || bot
                    .connectors
                    .discord
                    .allowed_user_ids
                    .iter()
                    .any(|u| u == sender_id)
        }
        _ => true,
    }
}

fn has_channel_or_bypass_overlap(
    bot: &krill_proto::bots::BotConfig,
    source: ExternalSource,
) -> bool {
    match source {
        ExternalSource::Telegram => !bot.connectors.telegram.allowed_chat_ids.is_empty(),
        ExternalSource::Slack => !bot.connectors.slack.allowed_channel_ids.is_empty(),
        ExternalSource::Discord => !bot.connectors.discord.allowed_channel_ids.is_empty(),
        _ => false,
    }
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

/// Splits UTF-8 `text` into chunks whose UTF-16 code-unit length never exceeds `max_utf16_units`
/// (required by Telegram Bot API's 4,096 UTF-16 code-unit message cap).
#[must_use]
pub fn chunk_text_utf16(text: &str, max_utf16_units: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let limit = max_utf16_units.max(2);
    if trimmed.encode_utf16().count() <= limit {
        return vec![trimmed.to_owned()];
    }

    let mut chunks = Vec::new();
    let mut remaining = trimmed;
    while !remaining.is_empty() {
        if remaining.encode_utf16().count() <= limit {
            chunks.push(remaining.to_owned());
            break;
        }
        let mut u16_count = 0usize;
        let mut hard_byte = 0usize;
        for (idx, ch) in remaining.char_indices() {
            let w = ch.len_utf16();
            if u16_count.saturating_add(w) > limit {
                break;
            }
            u16_count = u16_count.saturating_add(w);
            hard_byte = idx.saturating_add(ch.len_utf8());
        }
        if hard_byte == 0 {
            if let Some(first) = remaining.chars().next() {
                hard_byte = first.len_utf8();
            } else {
                break;
            }
        }
        let window = remaining.get(..hard_byte).unwrap_or(remaining);
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
        || lower.contains("xoxp-")
        || lower.contains("xoxe-")
        || lower.contains("xapp-")
        || lower.contains("ghp_")
        || lower.contains("gho_")
        || lower.contains("ghs_")
        || lower.contains("ghu_")
        || lower.contains("lin_api_")
        || lower.contains("ya29.")
        || lower.contains("1//")
        || lower.contains("bearer ")
        || lower.contains("authorization")
        || lower.contains("/users/")
        || lower.contains("/home/")
        || lower.contains(":\\")
        || contains_raw_telegram_token(trimmed)
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

fn contains_raw_telegram_token(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    for (idx, &b) in bytes.iter().enumerate() {
        if b != b':' {
            continue;
        }
        let left = bytes.get(..idx).unwrap_or_default();
        let digits_left = left.iter().rev().take_while(|c| c.is_ascii_digit()).count();
        let right = bytes.get(idx.saturating_add(1)..).unwrap_or_default();
        let tok_right = right
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(**c, b'-' | b'_'))
            .count();
        if digits_left >= 5 && tok_right >= 35 {
            return true;
        }
    }
    false
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

        for secret in [
            "xoxp-12345-secret failed 401",
            "xoxe.xoxp-secret-token",
            "token ghp_abc1234567890 failed",
            "gho_oauth999 error",
            "lin_api_xyz123 unauthorized",
            "ya29.a0AfB_secret error",
            "refresh 1//0gSecretRefresh failed",
            "bad 123456789:AAHsecretTokenValueWithOverThirtyFiveCharsHere",
        ] {
            let red = sanitize_error_for_status(secret);
            assert!(
                !red.contains("secret") && !red.contains("Secret") && !red.contains("AAH"),
                "{red}"
            );
        }
    }

    #[test]
    fn fail_closed_reply_attachments_reactions_utf16_chunks_and_access_replay_work() {
        #[derive(Default)]
        struct FullMockSender {
            sent: Mutex<Vec<String>>,
            files: Mutex<Vec<Vec<String>>>,
            reactions: Mutex<Vec<(String, String)>>,
            fail_reaction: Mutex<bool>,
        }
        impl PlatformSender for FullMockSender {
            fn source(&self) -> ExternalSource {
                ExternalSource::Telegram
            }
            fn max_chunk_chars(&self) -> usize {
                4096
            }
            fn send_message(&self, _target: &DeliveryTarget, text: &str) -> io::Result<String> {
                lock(&self.sent).push(text.to_owned());
                Ok("sent-1".to_owned())
            }
            fn edit_message(&self, _t: &DeliveryTarget, _id: &str, _txt: &str) -> io::Result<()> {
                Ok(())
            }
            fn send_files(
                &self,
                _target: &DeliveryTarget,
                files: &[String],
                _caption: Option<&str>,
            ) -> io::Result<Option<String>> {
                lock(&self.files).push(files.to_vec());
                Ok(Some("file-1".to_owned()))
            }
            fn add_reaction(
                &self,
                _target: &DeliveryTarget,
                message_id: &str,
                reaction: &str,
            ) -> io::Result<()> {
                if *lock(&self.fail_reaction) {
                    return Err(io::Error::other("reaction HTTP 400"));
                }
                lock(&self.reactions).push((message_id.to_owned(), reaction.to_owned()));
                Ok(())
            }
        }

        let bot_id = BotId::new("router-replay").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "ReplayBot", "claude", 1_000).unwrap();
        let bridge = Arc::new(MockBridge {
            bot: Mutex::new(bot),
            decision: Mutex::new(AccessDecision::Pending(BotAccessRequest {
                token: "tok-1".to_owned(),
                bot_id: bot_id.clone(),
                source: ExternalSource::Telegram,
                sender_id: "u-9".to_owned(),
                sender_name: "Bob".to_owned(),
                channel_id: "chat-9".to_owned(),
                status: BotAccessRequestStatus::Pending,
                created_at: 100,
                decided_at: None,
            })),
            dispatched: Mutex::new(Vec::new()),
            gates: Mutex::new(Vec::new()),
        });
        let router = ConversationRouter::new(Arc::clone(&bridge) as Arc<dyn ConnectorBridge>);
        let sender = Arc::new(FullMockSender::default());
        router.register_sender(
            bot_id.clone(),
            ExternalSource::Telegram,
            Arc::clone(&sender) as Arc<dyn PlatformSender>,
        );

        // Fail closed when link is None.
        let missing_err = router
            .post_reply(
                None,
                &OutboundBotReply {
                    bot_id: bot_id.clone(),
                    link_id: None,
                    text: "hi".to_owned(),
                    files: Vec::new(),
                    reaction: None,
                },
            )
            .unwrap_err();
        assert_eq!(missing_err.kind(), io::ErrorKind::NotFound);

        let target = DeliveryTarget {
            source: ExternalSource::Telegram,
            workspace_id: None,
            channel_id: "chat-9".to_owned(),
            thread_id: None,
            reply_to_message_id: Some("msg-100".to_owned()),
        };

        // Pending throttling: burst within 60s sends notice only once, holds latest message.
        let m1 = InboundExternalMessage {
            bot_id: bot_id.clone(),
            source: ExternalSource::Telegram,
            workspace_id: None,
            channel_id: "chat-9".to_owned(),
            thread_id: None,
            sender_id: "u-9".to_owned(),
            sender_name: "Bob".to_owned(),
            text: "first blocked".to_owned(),
            attachments: Vec::new(),
            external_message_id: Some("msg-100".to_owned()),
            acts_for: ActsFor::Anyone,
        };
        let mut m2 = m1.clone();
        m2.text = "second blocked".to_owned();
        m2.external_message_id = Some("msg-101".to_owned());

        assert!(matches!(
            router.handle_inbound_message_at(m1, target.clone(), sender.as_ref(), 10_000),
            InboundRouteOutcome::PendingAccess(_)
        ));
        assert!(matches!(
            router.handle_inbound_message_at(m2, target, sender.as_ref(), 20_000),
            InboundRouteOutcome::PendingAccess(_)
        ));
        assert_eq!(lock(&sender.sent).len(), 1);
        assert!(lock(&bridge.dispatched).is_empty());

        // Approving replays the held message once and clears it.
        *lock(&bridge.decision) = AccessDecision::Allowed;
        let req = BotAccessRequest {
            token: "tok-1".to_owned(),
            bot_id: bot_id.clone(),
            source: ExternalSource::Telegram,
            sender_id: "u-9".to_owned(),
            sender_name: "Bob".to_owned(),
            channel_id: "chat-9".to_owned(),
            status: BotAccessRequestStatus::Approved,
            created_at: 100,
            decided_at: Some(30_000),
        };
        router.on_access_approved(&req);
        assert_eq!(lock(&bridge.dispatched).len(), 1);
        assert_eq!(lock(&bridge.dispatched)[0].text, "second blocked");

        // link_last_inbound allows --react + --file when link.thread_id is None!
        let link = ExternalConversationLink {
            id: "link-1".to_owned(),
            bot_id: bot_id.clone(),
            source: ExternalSource::Telegram,
            workspace_id: None,
            channel_id: "chat-9".to_owned(),
            thread_id: None,
            session_id: Some(SessionId(9)),
            acts_for: ActsFor::Anyone,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let out = router
            .post_reply(
                Some(&link),
                &OutboundBotReply {
                    bot_id: bot_id.clone(),
                    link_id: Some("link-1".to_owned()),
                    text: String::new(),
                    files: vec!["/tmp/a.txt".to_owned()],
                    reaction: Some("👍".to_owned()),
                },
            )
            .unwrap();
        assert_eq!(out.as_deref(), Some("file-1"));
        assert_eq!(
            *lock(&sender.reactions),
            vec![("msg-101".to_owned(), "👍".to_owned())]
        );
        assert_eq!(*lock(&sender.files), vec![vec!["/tmp/a.txt".to_owned()]]);

        // Reaction error propagates!
        *lock(&sender.fail_reaction) = true;
        assert!(
            router
                .post_reply(
                    Some(&link),
                    &OutboundBotReply {
                        bot_id: bot_id.clone(),
                        link_id: Some("link-1".to_owned()),
                        text: "hello".to_owned(),
                        files: Vec::new(),
                        reaction: Some("👍".to_owned()),
                    },
                )
                .is_err()
        );

        // UTF-16 chunking keeps surrogate pairs (2 code units each) within limit.
        let emojis = "🚀".repeat(5); // 5 chars = 10 UTF-16 code units.
        let u16_chunks = chunk_text_utf16(&emojis, 4);
        assert!(u16_chunks.len() >= 3);
        for c in u16_chunks {
            assert!(c.encode_utf16().count() <= 4);
        }
    }

    #[test]
    fn gate_callback_enforces_paused_workspace_channel_and_independent_user_gates_and_discord_thread_allowlist()
     {
        struct WorkspaceCheckingBridge {
            bot: Mutex<BotConfig>,
            seen_workspaces: Mutex<Vec<Option<String>>>,
            gates: Mutex<Vec<(String, String)>>,
            dispatched: Mutex<Vec<InboundExternalMessage>>,
        }
        impl ConnectorBridge for WorkspaceCheckingBridge {
            fn dispatch_external_message(
                &self,
                msg: InboundExternalMessage,
            ) -> io::Result<DispatchOutcome> {
                let link = ExternalConversationLink {
                    id: "link-ws".to_owned(),
                    bot_id: msg.bot_id.clone(),
                    source: msg.source,
                    workspace_id: msg.workspace_id.clone(),
                    channel_id: msg.channel_id.clone(),
                    thread_id: msg.thread_id.clone(),
                    session_id: Some(SessionId(11)),
                    acts_for: msg.acts_for.clone(),
                    created_at_ms: 1,
                    updated_at_ms: 1,
                };
                lock(&self.dispatched).push(msg);
                Ok(DispatchOutcome {
                    link,
                    session_id: SessionId(11),
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
                // Must never bypass workspace check.
                Ok(AccessDecision::Denied("missing workspace".to_owned()))
            }
            fn check_or_request_access_with_workspace(
                &self,
                _bot_id: &BotId,
                _source: ExternalSource,
                workspace_id: Option<&str>,
                _sender_id: &str,
                _sender_name: &str,
                _channel_id: &str,
            ) -> io::Result<AccessDecision> {
                lock(&self.seen_workspaces).push(workspace_id.map(ToOwned::to_owned));
                if workspace_id == Some("G-GOOD") {
                    Ok(AccessDecision::Allowed)
                } else {
                    Ok(AccessDecision::Denied("wrong workspace".to_owned()))
                }
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
                    id: "link-ws".to_owned(),
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
                _resolved_by: &str,
            ) -> io::Result<()> {
                lock(&self.gates).push((gate_id.to_owned(), decision.to_owned()));
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

        let bot_id = BotId::new("bot-gate-sec").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "GateSec", "claude", 1_000).unwrap();
        bot.connectors.discord.allowed_guild_ids = vec!["G-GOOD".to_owned()];
        bot.connectors.discord.allowed_channel_ids = vec!["TH-ALLOWED".to_owned()];
        bot.connectors.discord.allowed_user_ids = vec!["U-ADMIN".to_owned()];
        bot.connectors.discord.acts_for = ActsFor::Owner;

        let bridge = Arc::new(WorkspaceCheckingBridge {
            bot: Mutex::new(bot.clone()),
            seen_workspaces: Mutex::new(Vec::new()),
            gates: Mutex::new(Vec::new()),
            dispatched: Mutex::new(Vec::new()),
        });
        let router = ConversationRouter::new(Arc::clone(&bridge) as Arc<dyn ConnectorBridge>);
        let sender = MockSender::default();

        // 1. Paused bot must reject gate callback.
        lock(&bridge.bot).paused = true;
        assert_eq!(
            router.handle_gate_callback_with_context(
                &bot_id,
                ExternalSource::Discord,
                Some("G-GOOD"),
                "U-ADMIN",
                "Admin",
                "CH-PARENT",
                Some("TH-ALLOWED"),
                "gate:g1:approve",
            ),
            GateCallbackOutcome::Paused
        );
        lock(&bridge.bot).paused = false;

        // 2. Wrong guild denied before reaching bridge.
        assert_eq!(
            router.handle_gate_callback_with_context(
                &bot_id,
                ExternalSource::Discord,
                Some("G-EVIL"),
                "U-ADMIN",
                "Admin",
                "CH-PARENT",
                Some("TH-ALLOWED"),
                "gate:g1:approve",
            ),
            GateCallbackOutcome::Denied
        );

        // 3. Allowed channel + unauthorized user (overlap bypass attempt) MUST be denied.
        assert_eq!(
            router.handle_gate_callback_with_context(
                &bot_id,
                ExternalSource::Discord,
                Some("G-GOOD"),
                "U-INTRUDER",
                "Intruder",
                "CH-PARENT",
                Some("TH-ALLOWED"),
                "gate:g1:approve",
            ),
            GateCallbackOutcome::Denied
        );
        assert!(lock(&bridge.gates).is_empty());

        // 4. Allowed guild + allowed thread ID + allowed user resolves gate cleanly.
        assert_eq!(
            router.handle_gate_callback_with_context(
                &bot_id,
                ExternalSource::Discord,
                Some("G-GOOD"),
                "U-ADMIN",
                "Admin",
                "CH-PARENT",
                Some("TH-ALLOWED"),
                "gate:g1:approve",
            ),
            GateCallbackOutcome::Resolved {
                gate_id: "g1".to_owned(),
                decision: "approve".to_owned(),
            }
        );
        assert_eq!(lock(&bridge.gates).len(), 1);
        assert!(lock(&bridge.seen_workspaces).contains(&Some("G-GOOD".to_owned())));

        // 5. D-23b: Discord inbound message with parent channel_id + allowlisted thread_id passes!
        let inbound = InboundExternalMessage {
            bot_id: bot_id.clone(),
            source: ExternalSource::Discord,
            workspace_id: Some("G-GOOD".to_owned()),
            channel_id: "CH-PARENT".to_owned(),
            thread_id: Some("TH-ALLOWED".to_owned()),
            sender_id: "U-ADMIN".to_owned(),
            sender_name: "Admin".to_owned(),
            text: "hello from allowlisted thread".to_owned(),
            attachments: Vec::new(),
            external_message_id: Some("m-1".to_owned()),
            acts_for: ActsFor::Owner,
        };
        let target = DeliveryTarget {
            source: ExternalSource::Discord,
            workspace_id: Some("G-GOOD".to_owned()),
            channel_id: "CH-PARENT".to_owned(),
            thread_id: Some("TH-ALLOWED".to_owned()),
            reply_to_message_id: Some("m-1".to_owned()),
        };
        assert!(matches!(
            router.handle_inbound_message(inbound, target, &sender),
            InboundRouteOutcome::Dispatched(_)
        ));
        assert_eq!(lock(&bridge.dispatched).len(), 1);
    }

    #[test]
    fn telegram_gates_overlap_deferred_attachment_replay_and_queued_turns_work() {
        let bot_id = BotId::new("bot-tg-mb").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "TgBot", "claude", 1_000).unwrap();
        bot.connectors.telegram.enabled = true;
        bot.connectors.telegram.allowed_chat_ids = vec![1001];
        bot.connectors.telegram.allowed_user_ids = vec![2001];

        let bridge = Arc::new(MockBridge {
            bot: Mutex::new(bot),
            decision: Mutex::new(AccessDecision::Allowed),
            dispatched: Mutex::new(Vec::new()),
            gates: Mutex::new(Vec::new()),
        });
        let sender = Arc::new(MockSender::default());
        let router = ConversationRouter::new(Arc::clone(&bridge) as Arc<dyn ConnectorBridge>);
        router.register_sender(
            bot_id.clone(),
            ExternalSource::Telegram,
            Arc::clone(&sender) as Arc<dyn PlatformSender>,
        );

        // MB-13: Telegram unauthorized chat is denied for gate callbacks & inbound messages.
        assert_eq!(
            router.handle_gate_callback_with_context(
                &bot_id,
                ExternalSource::Telegram,
                None,
                "2001",
                "Admin",
                "9999",
                None,
                "gate:g1:approve",
            ),
            GateCallbackOutcome::Denied
        );
        // MB-13: Allowed chat + unauthorized user (overlap bypass) is denied even when bridge returns Allowed.
        assert_eq!(
            router.handle_gate_callback_with_context(
                &bot_id,
                ExternalSource::Telegram,
                None,
                "8888",
                "Stranger",
                "1001",
                None,
                "gate:g1:approve",
            ),
            GateCallbackOutcome::Denied
        );
        let mk_tg = |sender_id: &str, text: &str, msg_id: &str| {
            (
                InboundExternalMessage {
                    bot_id: bot_id.clone(),
                    source: ExternalSource::Telegram,
                    workspace_id: None,
                    channel_id: "1001".to_owned(),
                    thread_id: None,
                    sender_id: sender_id.to_owned(),
                    sender_name: "User".to_owned(),
                    text: text.to_owned(),
                    attachments: Vec::new(),
                    external_message_id: Some(msg_id.to_owned()),
                    acts_for: ActsFor::Owner,
                },
                DeliveryTarget {
                    source: ExternalSource::Telegram,
                    workspace_id: None,
                    channel_id: "1001".to_owned(),
                    thread_id: None,
                    reply_to_message_id: Some(msg_id.to_owned()),
                },
            )
        };
        let (bad_user_msg, bad_user_target) = mk_tg("8888", "sneak", "m-bad");
        assert_eq!(
            router.handle_inbound_message(bad_user_msg, bad_user_target, sender.as_ref()),
            InboundRouteOutcome::Denied
        );

        // MB-10: Two queued turns on the SAME `(bot_id, SessionId(9))` both deliver replies!
        let (m1, t1) = mk_tg("2001", "first", "m-1");
        let (m2, t2) = mk_tg("2001", "second", "m-2");
        assert!(matches!(
            router.handle_inbound_message(m1, t1, sender.as_ref()),
            InboundRouteOutcome::Dispatched(_)
        ));
        assert!(matches!(
            router.handle_inbound_message(m2, t2, sender.as_ref()),
            InboundRouteOutcome::Dispatched(_)
        ));
        let before_count = lock(&sender.sent).len();
        router.on_turn_complete(&bot_id, SessionId(9), Some("Reply One"));
        router.on_turn_complete(&bot_id, SessionId(9), Some("Reply Two"));
        let sent_now = lock(&sender.sent).clone();
        assert_eq!(sent_now.len(), before_count + 2);
        assert_eq!(sent_now[before_count].1, "Reply One");
        assert_eq!(sent_now[before_count + 1].1, "Reply Two");

        // MB-45: Pending access holds deferred attachment loader (does NOT run before approval),
        // and runs it upon `on_access_approved`, attaching the file to the replayed message!
        let req = BotAccessRequest {
            token: "tok-att-45".to_owned(),
            bot_id: bot_id.clone(),
            source: ExternalSource::Telegram,
            sender_id: "3001".to_owned(),
            sender_name: "Newbie".to_owned(),
            channel_id: "1001".to_owned(),
            status: krill_proto::bots::BotAccessRequestStatus::Pending,
            created_at: 5_000,
            decided_at: None,
        };
        *lock(&bridge.decision) = AccessDecision::Pending(req.clone());
        let loaded_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let loaded_bg = Arc::clone(&loaded_flag);
        let (att_only_msg, att_target) = mk_tg("3001", "", "m-att");
        let outcome = router.handle_inbound_message_with_loader(
            att_only_msg,
            att_target,
            sender.as_ref(),
            move |allowed_msg| {
                loaded_bg.store(true, std::sync::atomic::Ordering::Release);
                allowed_msg
                    .attachments
                    .push("/tmp/inbox/doc.pdf".to_owned());
                true
            },
        );
        assert!(matches!(
            outcome,
            Some(InboundRouteOutcome::PendingAccess(_))
        ));
        assert!(
            !loaded_flag.load(std::sync::atomic::Ordering::Acquire),
            "loader must NOT run while pending approval"
        );
        let dispatched_before = lock(&bridge.dispatched).len();
        router.on_access_approved(&req);
        assert!(
            loaded_flag.load(std::sync::atomic::Ordering::Acquire),
            "loader MUST run upon access approval"
        );
        let dispatched_after = lock(&bridge.dispatched);
        assert_eq!(dispatched_after.len(), dispatched_before + 1);
        assert_eq!(
            dispatched_after.last().unwrap().attachments,
            vec!["/tmp/inbox/doc.pdf".to_owned()]
        );
    }
}
