// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Clean bridge trait and manager stub for Wave 2B (`daemon-channels`) external messaging
//! connectors (`Telegram`, `Slack` Socket Mode, `Discord` Gateway).
//!
//! Wave 2B implements the concrete connector workers inside `crates/krilld/src/bots/connectors/`
//! against [`ConnectorBridge`] and reuses [`http_client`] (zero-`tokio` HTTP/1.1 + `rustls`).

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex};

use krill_proto::SessionId;
use krill_proto::bots::{
    ActsFor, BotAccessRequest, BotConfig, BotConnectorStatus, BotId, ConnectorHealthState,
    ExternalConversationLink, ExternalSource,
};
use zeroize::Zeroizing;

pub use crate::bots::http_client;
use crate::session::lock;

/// Normalized inbound message delivered by an external messaging connector to a Krill Bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundExternalMessage {
    pub bot_id: BotId,
    pub source: ExternalSource,
    pub workspace_id: Option<String>,
    pub channel_id: String,
    pub thread_id: Option<String>,
    pub sender_id: String,
    pub sender_name: String,
    pub text: String,
    pub attachments: Vec<String>,
    pub external_message_id: Option<String>,
    pub acts_for: ActsFor,
}

/// Result of dispatching an inbound external message into a bot session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchOutcome {
    pub link: ExternalConversationLink,
    pub session_id: SessionId,
    pub started_new_session: bool,
}

/// Result of evaluating whether an external sender is authorized to talk to a bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessDecision {
    Allowed,
    Pending(BotAccessRequest),
    Denied(String),
}

/// Outbound reply requested by a bot (explicit `krill bot reply` or turn completion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundBotReply {
    pub bot_id: BotId,
    pub link_id: Option<String>,
    pub text: String,
    pub files: Vec<String>,
    pub reaction: Option<String>,
}

/// Callback interface exposed by `daemon-core` (`BotsManager` + `Registry`) for Wave 2B
/// messaging connector workers (`telegram.rs`, `slack.rs`, `discord.rs`).
pub trait ConnectorBridge: Send + Sync {
    /// Routes an authorized inbound external message to the bot's bound session (starting one if needed).
    fn dispatch_external_message(&self, msg: InboundExternalMessage)
    -> io::Result<DispatchOutcome>;

    /// Verifies sender allow-lists / approved members, or creates a pending [`BotAccessRequest`].
    fn check_or_request_access(
        &self,
        bot_id: &BotId,
        source: ExternalSource,
        sender_id: &str,
        sender_name: &str,
        channel_id: &str,
    ) -> io::Result<AccessDecision>;

    /// Resolves a secret from `SecretVault` (checking `explicit_ref` then `<bot_id>:<fallback_key>`).
    fn resolve_secret(
        &self,
        bot_id: &BotId,
        explicit_ref: Option<&str>,
        fallback_key: &str,
    ) -> io::Result<Option<Zeroizing<String>>>;

    /// Looks up or creates a persistent [`ExternalConversationLink`] for `(bot, source, channel, thread)`.
    fn lookup_or_create_link(
        &self,
        bot_id: &BotId,
        source: ExternalSource,
        workspace_id: Option<&str>,
        channel_id: &str,
        thread_id: Option<&str>,
        acts_for: ActsFor,
    ) -> io::Result<ExternalConversationLink>;

    /// Resolves a pending orchestration decision gate from an external quick-reply / button action.
    fn reply_orchestration_gate(
        &self,
        gate_id: &str,
        decision: &str,
        rationale: Option<String>,
        resolved_by: &str,
    ) -> io::Result<()>;

    /// Updates the live connector health snapshot surfaced by `BotRpcRequest::ConnectorStatus`.
    fn update_connector_status(&self, status: BotConnectorStatus);

    /// Returns the current [`BotConfig`] snapshot for `bot_id`, if present.
    fn get_bot_config(&self, bot_id: &BotId) -> Option<BotConfig>;

    /// Lists all configured bots.
    fn list_bots(&self) -> Vec<BotConfig>;
}

/// Lifecycle coordinator for external messaging connectors (Wave 2B replaces internal stub workers).
pub struct ConnectorManager {
    bridge: Arc<dyn ConnectorBridge>,
    statuses: Mutex<BTreeMap<(BotId, ExternalSource), BotConnectorStatus>>,
    last_activity: Mutex<BTreeMap<(BotId, SessionId), String>>,
}

impl fmt::Debug for ConnectorManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.statuses.lock().map_or(0, |s| s.len());
        f.debug_struct("ConnectorManager")
            .field("tracked_statuses", &count)
            .finish_non_exhaustive()
    }
}

impl ConnectorManager {
    /// Creates a strictly lazy manager. Spawns zero threads until `reload` finds enabled connectors.
    #[must_use]
    pub fn new(bridge: Arc<dyn ConnectorBridge>) -> Self {
        Self {
            bridge,
            statuses: Mutex::new(BTreeMap::new()),
            last_activity: Mutex::new(BTreeMap::new()),
        }
    }

    /// Reconciles messaging connector workers (`Telegram`, `Slack`, `Discord`) for `bot_id` (or all bots).
    #[expect(
        clippy::unnecessary_wraps,
        reason = "Wave 2B connector workers return io::Result on reload"
    )]
    pub fn reload(&self, bot_id: Option<&BotId>) -> io::Result<()> {
        let bots = match bot_id {
            Some(id) => self.bridge.get_bot_config(id).into_iter().collect(),
            None => self.bridge.list_bots(),
        };
        let mut map = lock(&self.statuses);
        if let Some(id) = bot_id {
            map.retain(|(bid, _), _| bid != id);
        } else {
            map.clear();
        }
        for bot in bots {
            for (source, enabled) in [
                (ExternalSource::Telegram, bot.connectors.telegram.enabled),
                (ExternalSource::Slack, bot.connectors.slack.enabled),
                (ExternalSource::Discord, bot.connectors.discord.enabled),
            ] {
                if !enabled {
                    continue;
                }
                let status = BotConnectorStatus {
                    bot_id: bot.id.clone(),
                    source,
                    enabled: true,
                    connected: false,
                    state: ConnectorHealthState::Disconnected,
                    account_label: None,
                    last_sync_ms: None,
                    last_error: None,
                    error_category: None,
                };
                map.insert((bot.id.clone(), source), status);
            }
        }
        Ok(())
    }

    /// Returns current messaging connector statuses, optionally filtered by `bot_id`.
    #[must_use]
    pub fn statuses(&self, bot_id: Option<&BotId>) -> Vec<BotConnectorStatus> {
        let map = lock(&self.statuses);
        map.values()
            .filter(|s| bot_id.is_none_or(|id| &s.bot_id == id))
            .cloned()
            .collect()
    }

    /// Records a status update reported by a connector worker.
    pub fn update_status(&self, status: BotConnectorStatus) {
        let key = (status.bot_id.clone(), status.source);
        lock(&self.statuses).insert(key, status);
    }

    /// Posts an explicit bot reply back to an external conversation link.
    ///
    /// Wave 2B wires live Telegram/Slack/Discord senders here.
    #[expect(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "Wave 2B post_reply dispatches through live connector handles on self and returns io::Result"
    )]
    pub fn post_reply(
        &self,
        link: Option<&ExternalConversationLink>,
        reply: &OutboundBotReply,
    ) -> io::Result<Option<String>> {
        let _ = reply;
        Ok(link.map(|l| format!("{}:{}", l.source.as_str(), l.channel_id)))
    }

    /// Receives live tool/activity updates from a bot turn (used by Slack/Discord/Telegram typing indicators).
    pub fn on_turn_activity(&self, bot_id: &BotId, session_id: SessionId, activity: &str) {
        lock(&self.last_activity).insert((bot_id.clone(), session_id), activity.to_owned());
    }

    /// Receives turn-completion notifications with the extracted final reply text (if not quiet).
    pub fn on_turn_complete(
        &self,
        bot_id: &BotId,
        session_id: SessionId,
        final_reply: Option<&str>,
    ) {
        lock(&self.last_activity).remove(&(bot_id.clone(), session_id));
        let _ = final_reply;
    }

    /// Stops all connector workers cleanly.
    pub fn shutdown(&self) {
        lock(&self.statuses).clear();
        lock(&self.last_activity).clear();
    }
}
