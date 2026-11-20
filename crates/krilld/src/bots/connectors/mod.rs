// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! External messaging connectors subsystem (`Telegram` long-polling, `Slack` Socket Mode,
//! `Discord` Gateway v10) built on zero-`tokio` std threads (128 KiB stacks) and `rustls`.
//!
//! Strictly lazy: spawns zero threads and opens zero sockets until at least one unpaused bot
//! enables Telegram, Slack, or Discord with configured credentials.

pub mod discord;
pub mod router;
pub mod slack;
pub mod telegram;
pub mod ws_client;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::{Hash as _, Hasher as _};
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use krill_proto::SessionId;
use krill_proto::bots::{
    ActsFor, BotAccessRequest, BotConfig, BotConnectorStatus, BotErrorCategory, BotId,
    ConnectorHealthState, ExternalConversationLink, ExternalSource,
};
use ring::digest;
use zeroize::Zeroizing;

use crate::bots::connectors::discord::{
    DEFAULT_DISCORD_API_BASE, DiscordSender, spawn_discord_worker,
};
use crate::bots::connectors::router::{ConversationRouter, PlatformSender};
use crate::bots::connectors::slack::{DEFAULT_SLACK_API_BASE, SlackSender, spawn_slack_worker};
use crate::bots::connectors::telegram::{
    DEFAULT_TELEGRAM_API_BASE, TelegramSender, spawn_telegram_worker,
};
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

/// Callback interface exposed by `daemon-core` (`BotsManager` + `Registry`) for messaging
/// connector workers (`telegram.rs`, `slack.rs`, `discord.rs`).
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

/// Configurable API base URLs for messaging connectors (overridable for loopback mock tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorEndpoints {
    pub telegram: String,
    pub slack: String,
    pub discord: String,
}

impl Default for ConnectorEndpoints {
    fn default() -> Self {
        Self {
            telegram: DEFAULT_TELEGRAM_API_BASE.to_owned(),
            slack: DEFAULT_SLACK_API_BASE.to_owned(),
            discord: DEFAULT_DISCORD_API_BASE.to_owned(),
        }
    }
}

/// Condvar-backed cooperative stop signal waking backoff sleeps immediately.
#[derive(Debug, Default)]
pub(crate) struct StopFlag {
    stopped: Mutex<bool>,
    changed: Condvar,
}

impl StopFlag {
    pub(crate) fn is_stopped(&self) -> bool {
        *lock(&self.stopped)
    }

    pub(crate) fn signal(&self) {
        *lock(&self.stopped) = true;
        self.changed.notify_all();
    }

    pub(crate) fn wait(&self, delay: Duration) -> bool {
        let guard = lock(&self.stopped);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, delay, |stopped| !*stopped)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard
    }
}

struct WorkerHandle {
    fingerprint: u64,
    stop: Arc<StopFlag>,
    handle: Option<JoinHandle<()>>,
}

/// Lifecycle coordinator for local-first Telegram, Slack Socket Mode, and Discord Gateway workers.
pub struct ConnectorManager {
    bridge: Arc<dyn ConnectorBridge>,
    endpoints: ConnectorEndpoints,
    router: Arc<ConversationRouter>,
    workers: Mutex<BTreeMap<(BotId, ExternalSource), WorkerHandle>>,
    statuses: Mutex<BTreeMap<(BotId, ExternalSource), BotConnectorStatus>>,
}

impl fmt::Debug for ConnectorManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let worker_count = self.workers.lock().map_or(0, |w| w.len());
        let status_count = self.statuses.lock().map_or(0, |s| s.len());
        f.debug_struct("ConnectorManager")
            .field("active_workers", &worker_count)
            .field("tracked_statuses", &status_count)
            .finish_non_exhaustive()
    }
}

impl Drop for ConnectorManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl ConnectorManager {
    /// Creates a strictly lazy manager. Spawns zero threads and opens zero sockets until `reload`
    /// finds enabled chat connectors with credentials.
    #[must_use]
    pub fn new(bridge: Arc<dyn ConnectorBridge>) -> Self {
        Self::with_endpoints(bridge, ConnectorEndpoints::default())
    }

    /// Creates a manager with custom connector API endpoints (for loopback mock server tests).
    #[must_use]
    pub fn with_endpoints(bridge: Arc<dyn ConnectorBridge>, endpoints: ConnectorEndpoints) -> Self {
        let router = Arc::new(ConversationRouter::new(Arc::clone(&bridge)));
        Self {
            bridge,
            endpoints,
            router,
            workers: Mutex::new(BTreeMap::new()),
            statuses: Mutex::new(BTreeMap::new()),
        }
    }

    /// Access to the underlying [`ConversationRouter`] (useful for tests and diagnostics).
    #[must_use]
    pub fn router(&self) -> &Arc<ConversationRouter> {
        &self.router
    }

    /// Reconciles messaging connector workers (`Telegram`, `Slack`, `Discord`) for `bot_id` (or all bots).
    pub fn reload(&self, bot_id: Option<&BotId>) -> io::Result<()> {
        let bots: Vec<BotConfig> = match bot_id {
            Some(id) => self.bridge.get_bot_config(id).into_iter().collect(),
            None => self.bridge.list_bots(),
        };

        let mut desired: BTreeMap<(BotId, ExternalSource), DesiredWorker> = BTreeMap::new();
        let mut missing_secret_statuses: Vec<BotConnectorStatus> = Vec::new();
        let mut enabled_keys: BTreeSet<(BotId, ExternalSource)> = BTreeSet::new();

        for bot in bots {
            if bot.paused {
                continue;
            }
            if bot.connectors.telegram.enabled {
                let key = (bot.id.clone(), ExternalSource::Telegram);
                enabled_keys.insert(key.clone());
                match resolve_with_fallback(
                    self.bridge.as_ref(),
                    &bot.id,
                    bot.connectors.telegram.secret_ref.as_deref(),
                    "telegram",
                    "telegram_token",
                )? {
                    Some(token) => {
                        let fp = hash_telegram(&bot, token.as_str());
                        desired.insert(
                            key,
                            DesiredWorker::Telegram {
                                bot: bot.clone(),
                                token,
                                fp,
                            },
                        );
                    }
                    None => {
                        missing_secret_statuses.push(not_set_up_status(
                            &bot.id,
                            ExternalSource::Telegram,
                            "Missing Telegram bot token in secret vault",
                        ));
                    }
                }
            }
            if bot.connectors.slack.enabled {
                let key = (bot.id.clone(), ExternalSource::Slack);
                enabled_keys.insert(key.clone());
                let app_tok = resolve_with_fallback(
                    self.bridge.as_ref(),
                    &bot.id,
                    bot.connectors.slack.app_token_secret_ref.as_deref(),
                    "slack-app",
                    "slack_app_token",
                )?;
                let bot_tok = resolve_with_fallback(
                    self.bridge.as_ref(),
                    &bot.id,
                    bot.connectors.slack.bot_token_secret_ref.as_deref(),
                    "slack-bot",
                    "slack_bot_token",
                )?;
                match (app_tok, bot_tok) {
                    (Some(app_token), Some(bot_token)) => {
                        let fp = hash_slack(&bot, app_token.as_str(), bot_token.as_str());
                        desired.insert(
                            key,
                            DesiredWorker::Slack {
                                bot: bot.clone(),
                                app_token,
                                bot_token,
                                fp,
                            },
                        );
                    }
                    _ => {
                        missing_secret_statuses.push(not_set_up_status(
                            &bot.id,
                            ExternalSource::Slack,
                            "Missing Slack app (xapp-) or bot (xoxb-) token in secret vault",
                        ));
                    }
                }
            }
            if bot.connectors.discord.enabled {
                let key = (bot.id.clone(), ExternalSource::Discord);
                enabled_keys.insert(key.clone());
                match resolve_with_fallback(
                    self.bridge.as_ref(),
                    &bot.id,
                    bot.connectors.discord.bot_token_secret_ref.as_deref(),
                    "discord",
                    "discord_token",
                )? {
                    Some(token) => {
                        let fp = hash_discord(&bot, token.as_str());
                        desired.insert(
                            key,
                            DesiredWorker::Discord {
                                bot: bot.clone(),
                                token,
                                fp,
                            },
                        );
                    }
                    None => {
                        missing_secret_statuses.push(not_set_up_status(
                            &bot.id,
                            ExternalSource::Discord,
                            "Missing Discord bot token in secret vault",
                        ));
                    }
                }
            }
        }

        // Stop workers in scope that are no longer desired, changed config/secrets, or finished.
        let mut to_stop: Vec<WorkerHandle> = Vec::new();
        {
            let mut workers = lock(&self.workers);
            let stale_keys: Vec<(BotId, ExternalSource)> = workers
                .iter()
                .filter(|(key, handle)| {
                    if let Some(target_id) = bot_id
                        && &key.0 != target_id
                    {
                        return false;
                    }
                    let finished = handle.handle.as_ref().is_some_and(JoinHandle::is_finished);
                    if finished {
                        return true;
                    }
                    match desired.get(key) {
                        None => true,
                        Some(spec) => spec.fingerprint() != handle.fingerprint,
                    }
                })
                .map(|(k, _)| k.clone())
                .collect();

            for key in stale_keys {
                if let Some(handle) = workers.remove(&key) {
                    handle.stop.signal();
                    to_stop.push(handle);
                }
                self.router.unregister_sender(&key.0, key.1);
            }
        }

        // Join stopped workers outside the lock.
        for mut handle in to_stop {
            if let Some(jh) = handle.handle.take() {
                jh.join().ok();
            }
        }

        // Remove status rows for disabled/removed connectors in scope.
        {
            let mut st = lock(&self.statuses);
            st.retain(|key, _| {
                if let Some(target_id) = bot_id
                    && &key.0 != target_id
                {
                    return true;
                }
                enabled_keys.contains(key)
            });
            for missing in missing_secret_statuses {
                st.insert((missing.bot_id.clone(), missing.source), missing);
            }
        }

        // Start any newly desired workers that are not already running.
        for (key, spec) in desired {
            let already_running = lock(&self.workers).contains_key(&key);
            if already_running {
                continue;
            }
            let stop = Arc::new(StopFlag::default());
            let fp = spec.fingerprint();
            let (source, handle_res) = match spec {
                DesiredWorker::Telegram { bot, token, .. } => {
                    let sender = Arc::new(TelegramSender::new(&self.endpoints.telegram, token));
                    self.router.register_sender(
                        bot.id.clone(),
                        ExternalSource::Telegram,
                        Arc::clone(&sender) as Arc<dyn PlatformSender>,
                    );
                    self.update_status(connecting_status(&bot.id, ExternalSource::Telegram));
                    let res = spawn_telegram_worker(
                        bot,
                        sender,
                        Arc::clone(&self.bridge),
                        Arc::clone(&self.router),
                        Arc::clone(&stop),
                    );
                    (ExternalSource::Telegram, res)
                }
                DesiredWorker::Slack {
                    bot,
                    app_token,
                    bot_token,
                    ..
                } => {
                    let sender = Arc::new(SlackSender::new(&self.endpoints.slack, bot_token));
                    self.router.register_sender(
                        bot.id.clone(),
                        ExternalSource::Slack,
                        Arc::clone(&sender) as Arc<dyn PlatformSender>,
                    );
                    self.update_status(connecting_status(&bot.id, ExternalSource::Slack));
                    let res = spawn_slack_worker(
                        bot,
                        app_token,
                        sender,
                        Arc::clone(&self.bridge),
                        Arc::clone(&self.router),
                        Arc::clone(&stop),
                    );
                    (ExternalSource::Slack, res)
                }
                DesiredWorker::Discord { bot, token, .. } => {
                    let sender =
                        Arc::new(DiscordSender::new(&self.endpoints.discord, token.clone()));
                    self.router.register_sender(
                        bot.id.clone(),
                        ExternalSource::Discord,
                        Arc::clone(&sender) as Arc<dyn PlatformSender>,
                    );
                    self.update_status(connecting_status(&bot.id, ExternalSource::Discord));
                    let res = spawn_discord_worker(
                        bot,
                        token,
                        sender,
                        Arc::clone(&self.bridge),
                        Arc::clone(&self.router),
                        Arc::clone(&stop),
                    );
                    (ExternalSource::Discord, res)
                }
            };

            if let Ok(jh) = handle_res {
                lock(&self.workers).insert(
                    key,
                    WorkerHandle {
                        fingerprint: fp,
                        stop,
                        handle: Some(jh),
                    },
                );
            } else {
                self.router.unregister_sender(&key.0, source);
                self.update_status(BotConnectorStatus {
                    bot_id: key.0,
                    source,
                    enabled: true,
                    connected: false,
                    state: ConnectorHealthState::Error,
                    account_label: None,
                    last_sync_ms: None,
                    last_error: Some("failed to spawn connector worker thread".to_owned()),
                    error_category: Some(BotErrorCategory::AgentError),
                });
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
    pub fn post_reply(
        &self,
        link: Option<&ExternalConversationLink>,
        reply: &OutboundBotReply,
    ) -> io::Result<Option<String>> {
        self.router.post_reply(link, reply)
    }

    /// Receives live tool/activity updates from a bot turn (3s-throttled in-place status edit).
    pub fn on_turn_activity(&self, bot_id: &BotId, session_id: SessionId, activity: &str) {
        self.router.on_turn_activity(bot_id, session_id, activity);
    }

    /// Receives turn-completion notifications with the extracted final reply text (if not quiet).
    pub fn on_turn_complete(
        &self,
        bot_id: &BotId,
        session_id: SessionId,
        final_reply: Option<&str>,
    ) {
        self.router
            .on_turn_complete(bot_id, session_id, final_reply);
    }

    /// Stops all connector workers cleanly and clears runtime state.
    pub fn shutdown(&self) {
        let drained: Vec<WorkerHandle> = {
            let mut workers = lock(&self.workers);
            workers.values().for_each(|w| w.stop.signal());
            std::mem::take(&mut *workers).into_values().collect()
        };
        for mut handle in drained {
            if let Some(jh) = handle.handle.take() {
                jh.join().ok();
            }
        }
        self.router.clear_all();
        lock(&self.statuses).clear();
    }
}

enum DesiredWorker {
    Telegram {
        bot: BotConfig,
        token: Zeroizing<String>,
        fp: u64,
    },
    Slack {
        bot: BotConfig,
        app_token: Zeroizing<String>,
        bot_token: Zeroizing<String>,
        fp: u64,
    },
    Discord {
        bot: BotConfig,
        token: Zeroizing<String>,
        fp: u64,
    },
}

impl DesiredWorker {
    const fn fingerprint(&self) -> u64 {
        match self {
            Self::Telegram { fp, .. } | Self::Slack { fp, .. } | Self::Discord { fp, .. } => *fp,
        }
    }
}

fn resolve_with_fallback(
    bridge: &dyn ConnectorBridge,
    bot_id: &BotId,
    explicit_ref: Option<&str>,
    primary_key: &str,
    secondary_key: &str,
) -> io::Result<Option<Zeroizing<String>>> {
    if let Some(val) = bridge.resolve_secret(bot_id, explicit_ref, primary_key)?
        && !val.trim().is_empty()
    {
        return Ok(Some(val));
    }
    if let Some(val) = bridge.resolve_secret(bot_id, None, secondary_key)?
        && !val.trim().is_empty()
    {
        return Ok(Some(val));
    }
    Ok(None)
}

fn connecting_status(bot_id: &BotId, source: ExternalSource) -> BotConnectorStatus {
    BotConnectorStatus {
        bot_id: bot_id.clone(),
        source,
        enabled: true,
        connected: false,
        state: ConnectorHealthState::Connecting,
        account_label: None,
        last_sync_ms: None,
        last_error: None,
        error_category: None,
    }
}

fn not_set_up_status(bot_id: &BotId, source: ExternalSource, msg: &str) -> BotConnectorStatus {
    BotConnectorStatus {
        bot_id: bot_id.clone(),
        source,
        enabled: true,
        connected: false,
        state: ConnectorHealthState::Error,
        account_label: None,
        last_sync_ms: None,
        last_error: Some(msg.to_owned()),
        error_category: Some(BotErrorCategory::NotSetUp),
    }
}

fn hash_telegram(bot: &BotConfig, token: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "telegram".hash(&mut h);
    bot.id.hash(&mut h);
    bot.connectors.telegram.secret_ref.hash(&mut h);
    digest::digest(&digest::SHA256, token.as_bytes())
        .as_ref()
        .hash(&mut h);
    h.finish()
}

fn hash_slack(bot: &BotConfig, app_token: &str, bot_token: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "slack".hash(&mut h);
    bot.id.hash(&mut h);
    bot.connectors.slack.app_token_secret_ref.hash(&mut h);
    bot.connectors.slack.bot_token_secret_ref.hash(&mut h);
    digest::digest(&digest::SHA256, app_token.as_bytes())
        .as_ref()
        .hash(&mut h);
    digest::digest(&digest::SHA256, bot_token.as_bytes())
        .as_ref()
        .hash(&mut h);
    h.finish()
}

fn hash_discord(bot: &BotConfig, token: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "discord".hash(&mut h);
    bot.id.hash(&mut h);
    bot.connectors.discord.bot_token_secret_ref.hash(&mut h);
    digest::digest(&digest::SHA256, token.as_bytes())
        .as_ref()
        .hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EmptyBridge {
        bots: Mutex<Vec<BotConfig>>,
    }

    impl ConnectorBridge for EmptyBridge {
        fn dispatch_external_message(
            &self,
            _msg: InboundExternalMessage,
        ) -> io::Result<DispatchOutcome> {
            Err(io::Error::other("unused"))
        }
        fn check_or_request_access(
            &self,
            _bot_id: &BotId,
            _source: ExternalSource,
            _sender_id: &str,
            _sender_name: &str,
            _channel_id: &str,
        ) -> io::Result<AccessDecision> {
            Ok(AccessDecision::Denied("unused".to_owned()))
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
        ) -> io::Result<ExternalConversationLink> {
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
            lock(&self.bots).iter().find(|b| &b.id == bot_id).cloned()
        }
        fn list_bots(&self) -> Vec<BotConfig> {
            lock(&self.bots).clone()
        }
    }

    #[test]
    fn connector_manager_is_strictly_lazy_and_reports_missing_secrets_without_spawning_threads() {
        let bot_id = BotId::new("lazy-bot").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "Lazy Bot", "claude", 1_000).unwrap();
        let bridge = Arc::new(EmptyBridge {
            bots: Mutex::new(vec![bot.clone()]),
        });
        let mgr = ConnectorManager::new(Arc::clone(&bridge) as Arc<dyn ConnectorBridge>);

        // Zero enabled connectors -> zero workers, zero statuses.
        mgr.reload(None).unwrap();
        assert!(lock(&mgr.workers).is_empty());
        assert!(mgr.statuses(None).is_empty());

        // Enabling Telegram without storing a token records NotSetUp status with ZERO threads.
        bot.connectors.telegram.enabled = true;
        *lock(&bridge.bots) = vec![bot];
        mgr.reload(Some(&bot_id)).unwrap();
        assert!(lock(&mgr.workers).is_empty());
        let statuses = mgr.statuses(Some(&bot_id));
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].state, ConnectorHealthState::Error);
        assert_eq!(statuses[0].error_category, Some(BotErrorCategory::NotSetUp));

        mgr.shutdown();
    }
}
