// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Krill Bots daemon subsystem (`BotsManager`): strictly-lazy facade owning bot persistence,
//! encrypted secret vault, SQLite items inbox, loopback MCP tool relay, local Google/GitHub/Linear/
//! Composio integrations, session runtime hooks, and the Wave 2B [`connectors::ConnectorBridge`].

pub mod composio;
pub mod connectors;
pub mod google;
pub mod http_client;
pub mod items;
pub mod look_around;
pub mod memory_tidy;
pub mod runtime;
pub mod secrets;
pub mod store;
pub mod tool_relay;
pub mod trackers;

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::SessionId;
use krill_proto::bots::{
    ActsFor, BotConfig, BotConnectorStatus, BotId, BotItemActor, BotRpcRequest, BotRpcResponse,
    ConnectorHealthState, ExternalConversationLink, ExternalSource,
};
use krill_proto::hook::HookEvent;
use krill_proto::orchestration::OrchestrationStore;
use krill_proto::schedule::Cron;
use zeroize::Zeroizing;

use crate::bots::connectors::{
    AccessDecision, ConnectorBridge, ConnectorManager, DispatchOutcome, InboundExternalMessage,
    OutboundBotReply,
};
use crate::bots::google::{GoogleEndpoints, GoogleTriggerChecker};
use crate::bots::items::BotItemsStore;
use crate::bots::look_around::LookAroundTracker;
use crate::bots::memory_tidy::MemoryTidyTracker;
use crate::bots::runtime::BotRuntime;
use crate::bots::secrets::SecretVault;
use crate::bots::store::{BotStore, MemoryUpdateError};
use crate::bots::tool_relay::BotToolRelay;
use crate::bots::trackers::{TrackerEndpoints, TrackerPoller};
use crate::registry::Registry;
use crate::session::lock;

/// Daemon-wide Krill Bots manager, lazily instantiated inside [`Registry`].
///
/// Guarantees zero background threads, zero loopback sockets, and zero SQLite connections until
/// a bot feature actually needs them.
pub struct BotsManager {
    state_dir: PathBuf,
    store: Arc<BotStore>,
    items: Arc<BotItemsStore>,
    secrets: Arc<SecretVault>,
    tool_relay: Arc<BotToolRelay>,
    runtime: Arc<BotRuntime>,
    connectors: OnceLock<Arc<ConnectorManager>>,
    google_poller: OnceLock<Arc<GoogleTriggerChecker>>,
    tracker_poller: OnceLock<Arc<TrackerPoller>>,
    composio_statuses: Mutex<BTreeMap<BotId, BotConnectorStatus>>,
    memory_tidy_tracker: MemoryTidyTracker,
    look_around_tracker: LookAroundTracker,
    bot_schedule_claimed: Mutex<HashSet<(BotId, String, u64)>>,
    google_endpoints: GoogleEndpoints,
    tracker_endpoints: TrackerEndpoints,
    composio_base_url: String,
    registry: Weak<Registry>,
    self_weak: OnceLock<Weak<Self>>,
}

impl fmt::Debug for BotsManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotsManager")
            .field("connectors_init", &self.connectors.get().is_some())
            .field("google_init", &self.google_poller.get().is_some())
            .field("trackers_init", &self.tracker_poller.get().is_some())
            .field("tool_relay", &self.tool_relay)
            .finish_non_exhaustive()
    }
}

impl BotsManager {
    /// Creates a strictly-lazy [`BotsManager`] rooted at `<state_dir>/bots/`.
    /// Opens zero SQLite connections, binds zero sockets, and spawns zero threads.
    #[must_use]
    pub fn new(state_dir: PathBuf, registry: Weak<Registry>) -> Arc<Self> {
        Self::with_endpoints(
            state_dir,
            registry,
            GoogleEndpoints::default(),
            TrackerEndpoints::default(),
            composio::DEFAULT_COMPOSIO_BASE_URL.to_owned(),
        )
    }

    /// Creates a [`BotsManager`] with custom API endpoints (for loopback mock HTTP tests).
    #[must_use]
    pub fn with_endpoints(
        state_dir: PathBuf,
        registry: Weak<Registry>,
        google_endpoints: GoogleEndpoints,
        tracker_endpoints: TrackerEndpoints,
        composio_base_url: String,
    ) -> Arc<Self> {
        let bots_root = state_dir.join("bots");
        let store = Arc::new(BotStore::new(bots_root.clone()));
        let items = Arc::new(BotItemsStore::new(bots_root.join("bot-items.db")));
        let secrets = Arc::new(SecretVault::new(bots_root));
        let tool_relay = Arc::new(BotToolRelay::new(Arc::clone(&secrets)));
        let runtime = Arc::new(BotRuntime::new());

        let manager = Arc::new(Self {
            state_dir,
            store,
            items,
            secrets,
            tool_relay,
            runtime,
            connectors: OnceLock::new(),
            google_poller: OnceLock::new(),
            tracker_poller: OnceLock::new(),
            composio_statuses: Mutex::new(BTreeMap::new()),
            memory_tidy_tracker: MemoryTidyTracker::default(),
            look_around_tracker: LookAroundTracker::default(),
            bot_schedule_claimed: Mutex::new(HashSet::new()),
            google_endpoints,
            tracker_endpoints,
            composio_base_url,
            registry,
            self_weak: OnceLock::new(),
        });
        manager.self_weak.set(Arc::downgrade(&manager)).ok();
        manager
    }

    /// Access to the persistent bot store.
    #[must_use]
    pub fn store(&self) -> &Arc<BotStore> {
        &self.store
    }

    /// Access to the lazily-opened SQLite items store.
    #[must_use]
    pub fn items(&self) -> &Arc<BotItemsStore> {
        &self.items
    }

    /// Access to the encrypted secret vault.
    #[must_use]
    pub fn secrets(&self) -> &Arc<SecretVault> {
        &self.secrets
    }

    /// Access to the strictly-lazy loopback MCP tool relay.
    #[must_use]
    pub fn tool_relay(&self) -> &Arc<BotToolRelay> {
        &self.tool_relay
    }

    /// Lazily initializes and returns the Wave 2B [`ConnectorManager`].
    #[must_use]
    pub fn connector_manager(&self) -> &Arc<ConnectorManager> {
        self.connectors.get_or_init(|| {
            let bridge: Arc<dyn ConnectorBridge> = Arc::new(DaemonConnectorBridge {
                registry: self.registry.clone(),
                manager: self.self_weak.get().cloned().unwrap_or_default(),
            });
            Arc::new(ConnectorManager::new(bridge))
        })
    }

    fn google_checker(&self) -> &Arc<GoogleTriggerChecker> {
        self.google_poller.get_or_init(|| {
            Arc::new(GoogleTriggerChecker::new(
                Arc::clone(&self.store),
                Arc::clone(&self.secrets),
                Arc::clone(&self.items),
                self.google_endpoints.clone(),
            ))
        })
    }

    fn tracker_poller(&self) -> &Arc<TrackerPoller> {
        self.tracker_poller.get_or_init(|| {
            Arc::new(TrackerPoller::new(
                Arc::clone(&self.store),
                Arc::clone(&self.secrets),
                Arc::clone(&self.items),
                self.tracker_endpoints.clone(),
            ))
        })
    }

    /// Dispatches any [`BotRpcRequest`] with `catch_unwind` panic isolation.
    #[must_use]
    pub fn handle_rpc(&self, registry: &Registry, req: BotRpcRequest) -> BotRpcResponse {
        if let Ok(resp) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.dispatch_rpc(registry, req)
        })) {
            resp
        } else {
            tracing::warn!("bot RPC handler panicked; isolated");
            BotRpcResponse::Error("internal bot RPC panic".to_owned())
        }
    }

    /// Hook event notification forwarded from [`Registry`] (fast-paths out for non-bot sessions).
    pub fn on_hook_event(&self, registry: &Registry, session_id: SessionId, event: &HookEvent) {
        if !self.runtime.is_bot_session(session_id) {
            return;
        }
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.runtime.on_hook_event(
                registry,
                &self.store,
                self.connectors.get().map(Arc::as_ref),
                session_id,
                event,
            );
        }))
        .ok();
    }

    /// Session cleanup hook forwarded from [`Registry::remove`].
    pub fn on_session_removed(&self, session_id: SessionId) {
        self.runtime
            .on_session_removed(&self.tool_relay, session_id);
    }

    /// Periodic scheduler tick called once per UTC minute from `registry/schedule.rs` ONLY when
    /// bots exist on disk or this manager is already initialized.
    pub fn tick_periodic(&self, registry: &Registry, now: SystemTime) {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let ts_ms = secs.saturating_mul(1_000);
            let minute = secs / 60;
            let calendar = crate::sys::cron_calendar(secs).ok();

            let Ok(bots) = self.store.list_bots() else {
                return;
            };
            if bots.is_empty() {
                return;
            }

            // Reconcile lazy background pollers ONLY when ≥1 bot actually enables them.
            let any_google = bots
                .iter()
                .any(|b| !b.paused && b.connectors.google.enabled);
            if any_google || self.google_poller.get().is_some() {
                self.google_checker().reconcile();
            }
            let any_tracker = bots
                .iter()
                .any(|b| !b.paused && (b.connectors.github.enabled || b.connectors.linear.enabled));
            if any_tracker || self.tracker_poller.get().is_some() {
                self.tracker_poller().reconcile();
            }
            let any_chat_conn = bots.iter().any(|b| {
                !b.paused
                    && (b.connectors.telegram.enabled
                        || b.connectors.slack.enabled
                        || b.connectors.discord.enabled)
            });
            if any_chat_conn && self.connectors.get().is_none() {
                self.connector_manager().reload(None).ok();
            }

            for bot in &bots {
                if bot.paused {
                    continue;
                }
                if bot.memory_tidy.enabled
                    && self.memory_tidy_tracker.is_due(
                        &bot.id,
                        bot.memory_tidy.interval_secs,
                        ts_ms,
                    )
                    && memory_tidy::run_memory_tidy(&self.store, &bot.id, ts_ms).is_ok()
                {
                    self.memory_tidy_tracker.record(&bot.id, ts_ms);
                }
                if bot.look_around.enabled
                    && self.look_around_tracker.is_due(
                        &bot.id,
                        bot.look_around.interval_secs,
                        bot.look_around.weekdays_only,
                        ts_ms,
                    )
                    && look_around::run_look_around(&self.store, &self.items, &bot.id, ts_ms)
                        .is_ok()
                {
                    self.look_around_tracker.record(&bot.id, ts_ms);
                }
                if let Some(cal) = calendar {
                    for sched in &bot.schedules {
                        if !sched.enabled {
                            continue;
                        }
                        if let Ok(cron) = Cron::parse(&sched.cron)
                            && cron.matches(cal)
                        {
                            let claim = (bot.id.clone(), sched.id.clone(), minute);
                            let fresh = {
                                let mut claimed = lock(&self.bot_schedule_claimed);
                                if claimed.len() > 1024 {
                                    claimed.retain(|(_, _, m)| *m + 60 >= minute);
                                }
                                claimed.insert(claim)
                            };
                            if fresh {
                                self.runtime
                                    .start_bot_session(
                                        registry,
                                        &self.store,
                                        &self.secrets,
                                        &self.tool_relay,
                                        &self.composio_base_url,
                                        &bot.id,
                                        None,
                                        Some(&sched.prompt),
                                        None,
                                    )
                                    .ok();
                            }
                        }
                    }
                }
            }
        }))
        .ok();
    }

    /// Cleanly shuts down all lazy background components.
    pub fn shutdown(&self) {
        if let Some(mgr) = self.connectors.get() {
            mgr.shutdown();
        }
        if let Some(g) = self.google_poller.get() {
            g.stop();
        }
        if let Some(t) = self.tracker_poller.get() {
            t.stop();
        }
        self.tool_relay.shutdown();
    }

    fn dispatch_rpc(&self, registry: &Registry, req: BotRpcRequest) -> BotRpcResponse {
        let ts = now_ms();
        match req {
            BotRpcRequest::ListBots => match self.store.list_bots() {
                Ok(bots) => BotRpcResponse::Bots(bots),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::GetBot { bot_id } => match self.store.get_bot(&bot_id) {
                Ok(Some(bot)) => BotRpcResponse::Bot(Box::new(bot)),
                Ok(None) => BotRpcResponse::Error(format!("bot not found: {}", bot_id.as_str())),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::CreateBot(params) => match self.store.create_bot(*params, ts) {
                Ok(bot) => BotRpcResponse::Bot(Box::new(bot)),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::UpdateBot(params) => match self.store.update_bot(*params, ts) {
                Ok(bot) => {
                    self.reconcile_for_bot(&bot);
                    BotRpcResponse::Bot(Box::new(bot))
                }
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::DeleteBot {
                bot_id,
                delete_home,
            } => match self.store.delete_bot(&bot_id, delete_home) {
                Ok(removed) => {
                    self.items.delete_bot_items(&bot_id).ok();
                    let extra_refs = collect_bot_secret_refs(&removed);
                    self.secrets.delete_bot_secrets(&bot_id, &extra_refs).ok();
                    self.tool_relay.unregister_bot(&bot_id);
                    lock(&self.composio_statuses).remove(&bot_id);
                    self.reconcile_all_lazy_workers();
                    BotRpcResponse::Deleted { bot_id }
                }
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::StartBotSession {
                bot_id,
                cwd,
                prompt,
            } => match self.runtime.start_bot_session(
                registry,
                &self.store,
                &self.secrets,
                &self.tool_relay,
                &self.composio_base_url,
                &bot_id,
                cwd.as_deref(),
                prompt.as_deref(),
                None,
            ) {
                Ok((session_id, pid)) => BotRpcResponse::SessionStarted {
                    bot_id,
                    session_id,
                    pid,
                },
                Err(err) => BotRpcResponse::Error(err),
            },
            BotRpcRequest::GetSoul { bot_id } => match self.store.get_soul(&bot_id) {
                Ok(text) => BotRpcResponse::Text(text),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::SetSoul { bot_id, content } => {
                match self.store.set_soul(&bot_id, &content, ts) {
                    Ok(()) => BotRpcResponse::Ok,
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::GetMemory { bot_id } => match self.store.get_memory(&bot_id) {
                Ok(text) => BotRpcResponse::Text(text),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::UpdateMemory {
                bot_id,
                content,
                allow_deletions,
            } => match self.store.update_memory(&bot_id, &content, allow_deletions) {
                Ok(saved) => BotRpcResponse::MemoryUpdated {
                    content: saved,
                    restored_facts: Vec::new(),
                    removed_count: 0,
                },
                Err(MemoryUpdateError::DeletionBlocked(violation)) => {
                    BotRpcResponse::Error(format!(
                        "MemoryDeletionViolation: {violation} ({})",
                        violation.deleted_facts.join("; ")
                    ))
                }
                Err(MemoryUpdateError::Io(err)) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::ForgetMemoryFact { bot_id, fact } => {
                match self.store.forget_memory_fact(&bot_id, &fact) {
                    Ok((content, removed_count)) => BotRpcResponse::MemoryUpdated {
                        content,
                        restored_facts: Vec::new(),
                        removed_count,
                    },
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::RunMemoryTidy { bot_id } => {
                match memory_tidy::run_memory_tidy(&self.store, &bot_id, ts) {
                    Ok((summary, changes)) => {
                        self.memory_tidy_tracker.record(&bot_id, ts);
                        BotRpcResponse::MemoryTidyResult {
                            bot_id,
                            summary,
                            changes,
                        }
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::RunLookAround { bot_id } => {
                match look_around::run_look_around(&self.store, &self.items, &bot_id, ts) {
                    Ok(suggestions) => {
                        self.look_around_tracker.record(&bot_id, ts);
                        BotRpcResponse::LookAroundResult {
                            bot_id,
                            suggestions,
                        }
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::ListItems {
                bot_id,
                include_closed,
            } => match self
                .items
                .list_items(bot_id.as_ref(), None, include_closed, true, ts)
            {
                Ok(list) => BotRpcResponse::Items(list),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::CreateItem(params) => {
                let actor = BotItemActor::Owner { device: None };
                match self.items.create_item(&params, &actor, ts) {
                    Ok(item) => BotRpcResponse::Item(Box::new(item)),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::UpdateItem(params) => {
                let actor = BotItemActor::Owner { device: None };
                match self.items.update_item(&params, &actor, ts) {
                    Ok(item) => BotRpcResponse::Item(Box::new(item)),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::AddItemComment {
                item_id,
                author,
                text,
            } => {
                let actor = if author.eq_ignore_ascii_case("owner") || author.trim().is_empty() {
                    BotItemActor::Owner { device: None }
                } else if let Ok(bid) = BotId::new(&author) {
                    BotItemActor::Bot {
                        bot_id: bid,
                        session_id: None,
                    }
                } else {
                    BotItemActor::Owner {
                        device: Some(author),
                    }
                };
                match self.items.add_comment(&item_id, &actor, &text, &[], ts) {
                    Ok(item) => BotRpcResponse::Item(Box::new(item)),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::AddItemLink { item_id, link } => {
                let actor = BotItemActor::Owner { device: None };
                match self.items.add_link(&item_id, &link, &actor, ts) {
                    Ok(item) => BotRpcResponse::Item(Box::new(item)),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::ListGroups => match self.store.list_groups() {
                Ok(groups) => BotRpcResponse::Groups(groups),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::CreateGroup {
                name,
                bot_ids,
                description,
            } => match self.store.create_group(&name, bot_ids, description, ts) {
                Ok(group) => BotRpcResponse::Group(group),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::SendBotMail {
                from,
                to_bot,
                subject,
                body,
                thread_id,
            } => match self.runtime.send_bot_mail(
                registry,
                &self.store,
                &from,
                &to_bot,
                &subject,
                &body,
                thread_id.as_deref(),
            ) {
                Ok(message_id) => BotRpcResponse::MailSent { message_id },
                Err(err) => BotRpcResponse::Error(err),
            },
            BotRpcRequest::PostBotReply {
                bot_id,
                link_id,
                text,
                files,
                reaction,
            } => {
                let mgr = self.connector_manager();
                match self.runtime.post_bot_reply(
                    &self.store,
                    mgr,
                    &OutboundBotReply {
                        bot_id,
                        link_id,
                        text,
                        files,
                        reaction,
                    },
                ) {
                    Ok(external_ref) => BotRpcResponse::ReplyPosted { external_ref },
                    Err(err) => BotRpcResponse::Error(err),
                }
            }
            BotRpcRequest::SetConnectorConfig { bot_id, connectors } => {
                match self.store.set_connector_config(&bot_id, *connectors, ts) {
                    Ok(bot) => {
                        self.reconcile_for_bot(&bot);
                        BotRpcResponse::Bot(Box::new(bot))
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::SetSecret { secret_ref, value } => {
                match self.secrets.set_ref(&secret_ref, Zeroizing::new(value)) {
                    Ok(()) => BotRpcResponse::Ok,
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::DeleteSecret { secret_ref } => {
                match self.secrets.delete_ref(&secret_ref) {
                    Ok(_) => BotRpcResponse::Ok,
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::ConnectorStatus { bot_id } => {
                BotRpcResponse::ConnectorStatuses(self.collect_connector_statuses(bot_id.as_ref()))
            }
            BotRpcRequest::ReloadConnectors { bot_id } => {
                if let Err(err) = self.reload_connectors(bot_id.as_ref(), ts) {
                    return BotRpcResponse::Error(err.to_string());
                }
                BotRpcResponse::ConnectorStatuses(self.collect_connector_statuses(bot_id.as_ref()))
            }
            BotRpcRequest::ListAccessRequests { bot_id } => {
                match self.store.list_access_requests(bot_id.as_ref()) {
                    Ok(reqs) => BotRpcResponse::AccessRequests(reqs),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::DecideAccessRequest { token, approve } => {
                match self.store.decide_access_request(&token, approve, ts) {
                    Ok(decided) => BotRpcResponse::AccessRequestDecided(decided),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::StartGoogleOAuth { bot_id } => {
                let bot = match self.store.get_bot(&bot_id) {
                    Ok(Some(b)) => b,
                    Ok(None) => {
                        return BotRpcResponse::Error(format!(
                            "bot not found: {}",
                            bot_id.as_str()
                        ));
                    }
                    Err(err) => return BotRpcResponse::Error(err.to_string()),
                };
                let checker = Arc::clone(self.google_checker());
                let cb = Arc::new(move |st: BotConnectorStatus| {
                    checker.record_status(st);
                });
                match google::start_google_oauth(
                    &bot,
                    Arc::clone(&self.secrets),
                    self.google_endpoints.clone(),
                    Some(cb),
                ) {
                    Ok((auth_url, state)) => BotRpcResponse::GoogleOAuthStarted { auth_url, state },
                    Err(err) => BotRpcResponse::Error(err),
                }
            }
            BotRpcRequest::TriggerSyncNow { bot_id, source } => {
                let bot = match self.store.get_bot(&bot_id) {
                    Ok(Some(b)) => b,
                    Ok(None) => {
                        return BotRpcResponse::Error(format!(
                            "bot not found: {}",
                            bot_id.as_str()
                        ));
                    }
                    Err(err) => return BotRpcResponse::Error(err.to_string()),
                };
                let sources_synced = self.sync_bot_sources(&bot, source, ts);
                BotRpcResponse::SyncTriggered {
                    bot_id,
                    sources_synced,
                }
            }
            BotRpcRequest::ToolRelayStatus => {
                BotRpcResponse::ToolRelayStatus(self.tool_relay.status())
            }
            BotRpcRequest::ExportBotZip { bot_id, options } => {
                match self.store.export_bot_zip(&bot_id, options) {
                    Ok((filename, zip_bytes)) => BotRpcResponse::ExportedZip {
                        filename,
                        zip_bytes,
                    },
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::ImportBotZip {
                zip_bytes,
                name_override,
                agent_override,
            } => match self.store.import_bot_zip(
                &zip_bytes,
                name_override.as_deref(),
                agent_override.as_deref(),
                ts,
            ) {
                Ok(bot) => BotRpcResponse::Bot(Box::new(bot)),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::ImportExternalBots {
                source,
                path,
                dry_run,
            } => {
                let custom = path
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(Path::new);
                match self.store.import_external_bots(source, custom, dry_run, ts) {
                    Ok((candidates, imported)) => BotRpcResponse::ImportedExternalBots {
                        candidates,
                        imported,
                    },
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
        }
    }

    fn reconcile_for_bot(&self, bot: &BotConfig) {
        if bot.connectors.telegram.enabled
            || bot.connectors.slack.enabled
            || bot.connectors.discord.enabled
            || self.connectors.get().is_some()
        {
            self.connector_manager().reload(Some(&bot.id)).ok();
        }
        if bot.connectors.google.enabled || self.google_poller.get().is_some() {
            self.google_checker().reconcile();
        }
        if bot.connectors.github.enabled
            || bot.connectors.linear.enabled
            || self.tracker_poller.get().is_some()
        {
            self.tracker_poller().reconcile();
        }
        if !bot.connectors.composio.enabled {
            lock(&self.composio_statuses).remove(&bot.id);
        }
    }

    fn reconcile_all_lazy_workers(&self) {
        if let Some(mgr) = self.connectors.get() {
            mgr.reload(None).ok();
        }
        if let Some(g) = self.google_poller.get() {
            g.reconcile();
        }
        if let Some(t) = self.tracker_poller.get() {
            t.reconcile();
        }
    }

    fn reload_connectors(&self, bot_id: Option<&BotId>, ts: u64) -> io::Result<()> {
        let bots = match bot_id {
            Some(id) => self.store.get_bot(id)?.into_iter().collect(),
            None => self.store.list_bots()?,
        };
        let has_chat = bots.iter().any(|b| {
            b.connectors.telegram.enabled
                || b.connectors.slack.enabled
                || b.connectors.discord.enabled
        });
        if has_chat || self.connectors.get().is_some() {
            self.connector_manager().reload(bot_id)?;
        }
        let has_google = bots.iter().any(|b| b.connectors.google.enabled);
        if has_google || self.google_poller.get().is_some() {
            self.google_checker().reconcile();
        }
        let has_tracker = bots
            .iter()
            .any(|b| b.connectors.github.enabled || b.connectors.linear.enabled);
        if has_tracker || self.tracker_poller.get().is_some() {
            self.tracker_poller().reconcile();
        }
        for bot in &bots {
            if bot.connectors.composio.enabled {
                let st = composio::sync_composio_bot(
                    bot,
                    &self.secrets,
                    &self.tool_relay,
                    &self.composio_base_url,
                    ts,
                );
                lock(&self.composio_statuses).insert(bot.id.clone(), st);
            } else {
                lock(&self.composio_statuses).remove(&bot.id);
            }
        }
        Ok(())
    }

    fn sync_bot_sources(
        &self,
        bot: &BotConfig,
        filter: Option<ExternalSource>,
        ts: u64,
    ) -> Vec<ExternalSource> {
        let mut synced = Vec::new();
        let want = |src: ExternalSource, enabled: bool| -> bool {
            match filter {
                Some(target) => target == src,
                None => enabled,
            }
        };

        if want(ExternalSource::Google, bot.connectors.google.enabled) {
            let _ = self.google_checker().sync_bot_now(bot, ts);
            synced.push(ExternalSource::Google);
        }
        if want(ExternalSource::GitHub, bot.connectors.github.enabled) {
            let _ = self.tracker_poller().sync_github_now(bot, ts);
            synced.push(ExternalSource::GitHub);
        }
        if want(ExternalSource::Linear, bot.connectors.linear.enabled) {
            let _ = self.tracker_poller().sync_linear_now(bot, ts);
            synced.push(ExternalSource::Linear);
        }
        if want(ExternalSource::Composio, bot.connectors.composio.enabled) {
            let st = composio::sync_composio_bot(
                bot,
                &self.secrets,
                &self.tool_relay,
                &self.composio_base_url,
                ts,
            );
            lock(&self.composio_statuses).insert(bot.id.clone(), st);
            synced.push(ExternalSource::Composio);
        }
        synced
    }

    fn collect_connector_statuses(&self, bot_id: Option<&BotId>) -> Vec<BotConnectorStatus> {
        let mut map: BTreeMap<(BotId, ExternalSource), BotConnectorStatus> = BTreeMap::new();

        // Seed baseline status rows from configured bots so every enabled connector appears even before first sync.
        let bots = match bot_id {
            Some(id) => self.store.get_bot(id).ok().flatten().into_iter().collect(),
            None => self.store.list_bots().unwrap_or_default(),
        };
        for bot in bots {
            for source in bot.connectors.enabled_sources() {
                map.insert(
                    (bot.id.clone(), source),
                    BotConnectorStatus {
                        bot_id: bot.id.clone(),
                        source,
                        enabled: true,
                        connected: false,
                        state: ConnectorHealthState::Disconnected,
                        account_label: None,
                        last_sync_ms: None,
                        last_error: None,
                        error_category: None,
                    },
                );
            }
        }

        if let Some(mgr) = self.connectors.get() {
            for st in mgr.statuses(bot_id) {
                map.insert((st.bot_id.clone(), st.source), st);
            }
        }
        if let Some(g) = self.google_poller.get() {
            for st in g.statuses(bot_id) {
                map.insert((st.bot_id.clone(), st.source), st);
            }
        }
        if let Some(t) = self.tracker_poller.get() {
            for st in t.statuses(bot_id) {
                map.insert((st.bot_id.clone(), st.source), st);
            }
        }
        for st in lock(&self.composio_statuses)
            .values()
            .filter(|s| bot_id.is_none_or(|id| &s.bot_id == id))
        {
            map.insert((st.bot_id.clone(), st.source), st.clone());
        }

        map.into_values().collect()
    }
}

/// Concrete [`ConnectorBridge`] implementation wiring Wave 2B channel workers into `BotsManager` + `Registry`.
struct DaemonConnectorBridge {
    registry: Weak<Registry>,
    manager: Weak<BotsManager>,
}

impl ConnectorBridge for DaemonConnectorBridge {
    fn dispatch_external_message(
        &self,
        msg: InboundExternalMessage,
    ) -> io::Result<DispatchOutcome> {
        let mgr = self
            .manager
            .upgrade()
            .ok_or_else(|| io::Error::other("bots manager shut down"))?;
        let reg = self
            .registry
            .upgrade()
            .ok_or_else(|| io::Error::other("daemon registry unavailable"))?;
        let (link, session_id, started_new_session) = mgr.runtime.dispatch_external_message(
            &reg,
            &mgr.store,
            &mgr.secrets,
            &mgr.tool_relay,
            &mgr.composio_base_url,
            &msg,
            now_ms(),
        )?;
        Ok(DispatchOutcome {
            link,
            session_id,
            started_new_session,
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
        let mgr = self
            .manager
            .upgrade()
            .ok_or_else(|| io::Error::other("bots manager shut down"))?;
        mgr.store.check_or_request_access(
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
        let mgr = self
            .manager
            .upgrade()
            .ok_or_else(|| io::Error::other("bots manager shut down"))?;
        mgr.secrets
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
    ) -> io::Result<ExternalConversationLink> {
        let mgr = self
            .manager
            .upgrade()
            .ok_or_else(|| io::Error::other("bots manager shut down"))?;
        mgr.store.lookup_or_create_link(
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
        let mgr = self
            .manager
            .upgrade()
            .ok_or_else(|| io::Error::other("bots manager shut down"))?;
        OrchestrationStore::mutate(&mgr.state_dir, |s| {
            s.resolve_gate(gate_id, decision, rationale, resolved_by)?;
            Ok(())
        })?;
        if let Some(reg) = self.registry.upgrade() {
            reg.notify_orch();
        }
        Ok(())
    }

    fn update_connector_status(&self, status: BotConnectorStatus) {
        if let Some(mgr) = self.manager.upgrade()
            && let Some(cm) = mgr.connectors.get()
        {
            cm.update_status(status);
        }
    }

    fn get_bot_config(&self, bot_id: &BotId) -> Option<BotConfig> {
        self.manager
            .upgrade()
            .and_then(|mgr| mgr.store.get_bot(bot_id).ok().flatten())
    }

    fn list_bots(&self) -> Vec<BotConfig> {
        self.manager
            .upgrade()
            .and_then(|mgr| mgr.store.list_bots().ok())
            .unwrap_or_default()
    }
}

fn collect_bot_secret_refs(bot: &BotConfig) -> Vec<String> {
    let mut refs = Vec::new();
    for r in [
        &bot.connectors.telegram.secret_ref,
        &bot.connectors.slack.app_token_secret_ref,
        &bot.connectors.slack.bot_token_secret_ref,
        &bot.connectors.discord.bot_token_secret_ref,
        &bot.connectors.google.oauth_client_secret_ref,
        &bot.connectors.google.token_secret_ref,
        &bot.connectors.github.token_secret_ref,
        &bot.connectors.linear.api_key_secret_ref,
        &bot.connectors.composio.api_key_secret_ref,
    ]
    .into_iter()
    .flatten()
    {
        refs.push(r.clone());
    }
    for tool in &bot.mcp_tools {
        if let Some(r) = &tool.secret_ref {
            refs.push(r.clone());
        }
    }
    refs
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::{
        BotConnectorConfig, BotItemLink, BotItemStatus, BotPackageExportOptions,
        CreateBotItemParams, CreateBotParams, FieldPatch, UpdateBotItemParams, UpdateBotParams,
    };
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_state(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bots-mgr-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn bots_manager_is_strictly_lazy_and_dispatches_all_core_rpcs() {
        let state = temp_state("rpc");
        let reg = Arc::new(Registry::new(None, Some(&state)));
        reg.bind_self_arc();

        // Zero background threads/sockets/SQLite before bots exist.
        assert!(reg.bots_if_initialized().is_none());
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));
        assert!(!mgr.items().is_open());
        assert!(!mgr.tool_relay().status().running);
        assert!(mgr.connectors.get().is_none());
        assert!(mgr.google_poller.get().is_none());
        assert!(mgr.tracker_poller.get().is_none());

        // CreateBot
        let created = match mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: "Support Helper".to_owned(),
                agent_id: "claude".to_owned(),
                description: String::new(),
                emoji: Some("S".to_owned()),
                avatar: None,
                model: None,
                effort: None,
                permission_mode: None,
                workspace: None,
                template_id: Some("pr-review".to_owned()),
                initial_soul: None,
            })),
        ) {
            BotRpcResponse::Bot(b) => Some(*b),
            _ => None,
        }
        .unwrap();

        // ListBots & GetBot & UpdateBot
        assert!(matches!(
            mgr.handle_rpc(&reg, BotRpcRequest::ListBots),
            BotRpcResponse::Bots(ref list) if list.len() == 1
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::GetBot {
                    bot_id: created.id.clone()
                }
            ),
            BotRpcResponse::Bot(_)
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                    bot_id: created.id.clone(),
                    name: Some("Support Helper v2".to_owned()),
                    description: None,
                    emoji: FieldPatch::Unchanged,
                    avatar: FieldPatch::Unchanged,
                    agent_id: None,
                    model: FieldPatch::Set("claude-sonnet-4-5".to_owned()),
                    effort: FieldPatch::Unchanged,
                    permission_mode: None,
                    workspace: FieldPatch::Unchanged,
                    paused: None,
                    first_run_completed: None,
                    notifications: None,
                    schedules: None,
                    look_around: None,
                    memory_tidy: None,
                    mcp_tools: None,
                }))
            ),
            BotRpcResponse::Bot(ref b) if b.name == "Support Helper v2"
        ));

        // Soul & Memory guard
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::SetSoul {
                    bot_id: created.id.clone(),
                    content: "# Support Helper\n\nHelps triage bugs.\n".to_owned(),
                }
            ),
            BotRpcResponse::Ok
        ));
        assert!(matches!(
            mgr.handle_rpc(&reg, BotRpcRequest::GetSoul { bot_id: created.id.clone() }),
            BotRpcResponse::Text(ref t) if t.contains("triage bugs")
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::UpdateMemory {
                    bot_id: created.id.clone(),
                    content: "# Memory\n\n- 2026-11-20 Fact A\n- 2026-11-20 Fact A\n".to_owned(),
                    allow_deletions: false,
                }
            ),
            BotRpcResponse::MemoryUpdated { .. }
        ));
        // Blocking silent deletion returns clear MemoryDeletionViolation error.
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::UpdateMemory {
                    bot_id: created.id.clone(),
                    content: "# Memory\n\n".to_owned(),
                    allow_deletions: false,
                }
            ),
            BotRpcResponse::Error(ref msg) if msg.contains("MemoryDeletionViolation")
        ));
        // Memory tidy deduplicates Fact A.
        assert!(matches!(
            mgr.handle_rpc(&reg, BotRpcRequest::RunMemoryTidy { bot_id: created.id.clone() }),
            BotRpcResponse::MemoryTidyResult { changes, .. } if changes >= 1
        ));
        // ForgetMemoryFact explicitly removes Fact A.
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::ForgetMemoryFact {
                    bot_id: created.id.clone(),
                    fact: "Fact A".to_owned(),
                }
            ),
            BotRpcResponse::MemoryUpdated {
                removed_count: 1,
                ..
            }
        ));

        // Items CRUD + Look Around
        let item = match mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateItem(Box::new(CreateBotItemParams {
                bot_id: created.id.clone(),
                title: "Check failing CI".to_owned(),
                description: None,
                done_means: None,
                status: Some(BotItemStatus::NeedsYou),
                links: Vec::new(),
            })),
        ) {
            BotRpcResponse::Item(i) => Some(*i),
            _ => None,
        }
        .unwrap();
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::AddItemComment {
                    item_id: item.uid.clone(),
                    author: "owner".to_owned(),
                    text: "Investigate timeout".to_owned(),
                }
            ),
            BotRpcResponse::Item(_)
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::AddItemLink {
                    item_id: item.uid.clone(),
                    link: BotItemLink::Pr("https://github.com/craftowen/krill/pull/5".to_owned()),
                }
            ),
            BotRpcResponse::Item(_)
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::UpdateItem(Box::new(UpdateBotItemParams {
                    item_id: item.uid.clone(),
                    title: None,
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::Working),
                    comment: None,
                    decisions: Vec::new(),
                    close_note: None,
                    snoozed_until_ms: FieldPatch::Unchanged,
                }))
            ),
            BotRpcResponse::Item(_)
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::ListItems {
                    bot_id: Some(created.id.clone()),
                    include_closed: false,
                }
            ),
            BotRpcResponse::Items(ref items) if items.len() == 1
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::RunLookAround {
                    bot_id: created.id.clone()
                }
            ),
            BotRpcResponse::LookAroundResult { .. }
        ));

        // Groups, Mail, Reply, Secrets, Connectors, ToolRelayStatus
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::CreateGroup {
                    name: "Triage".to_owned(),
                    bot_ids: vec![created.id.clone()],
                    description: "Bug triage group".to_owned(),
                }
            ),
            BotRpcResponse::Group(_)
        ));
        assert!(matches!(
            mgr.handle_rpc(&reg, BotRpcRequest::ListGroups),
            BotRpcResponse::Groups(ref gs) if gs.len() == 1
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::SendBotMail {
                    from: "owner".to_owned(),
                    to_bot: format!("bot:{}", created.slug.as_str()),
                    subject: "Triage update".to_owned(),
                    body: "Check PR #5".to_owned(),
                    thread_id: None,
                }
            ),
            BotRpcResponse::MailSent { .. }
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::PostBotReply {
                    bot_id: created.id.clone(),
                    link_id: None,
                    text: "On it!".to_owned(),
                    files: Vec::new(),
                    reaction: None,
                }
            ),
            BotRpcResponse::ReplyPosted { .. }
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::SetSecret {
                    secret_ref: "test-ref".to_owned(),
                    value: "top-secret".to_owned(),
                }
            ),
            BotRpcResponse::Ok
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::DeleteSecret {
                    secret_ref: "test-ref".to_owned(),
                }
            ),
            BotRpcResponse::Ok
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::SetConnectorConfig {
                    bot_id: created.id.clone(),
                    connectors: Box::new(BotConnectorConfig::default()),
                }
            ),
            BotRpcResponse::Bot(_)
        ));
        assert!(matches!(
            mgr.handle_rpc(&reg, BotRpcRequest::ConnectorStatus { bot_id: None }),
            BotRpcResponse::ConnectorStatuses(_)
        ));
        assert!(matches!(
            mgr.handle_rpc(&reg, BotRpcRequest::ReloadConnectors { bot_id: None }),
            BotRpcResponse::ConnectorStatuses(_)
        ));
        assert!(matches!(
            mgr.handle_rpc(&reg, BotRpcRequest::ToolRelayStatus),
            BotRpcResponse::ToolRelayStatus(ref st) if !st.running
        ));

        // Export & Import Zip + External dry-run + DeleteBot
        let zip_bytes = match mgr.handle_rpc(
            &reg,
            BotRpcRequest::ExportBotZip {
                bot_id: created.id.clone(),
                options: BotPackageExportOptions::default(),
            },
        ) {
            BotRpcResponse::ExportedZip { zip_bytes, .. } => Some(zip_bytes),
            _ => None,
        }
        .unwrap();
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::ImportBotZip {
                    zip_bytes,
                    name_override: Some("Imported Clone".to_owned()),
                    agent_override: None,
                }
            ),
            BotRpcResponse::Bot(_)
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::ImportExternalBots {
                    source: None,
                    path: Some(state.to_string_lossy().into_owned()),
                    dry_run: true,
                }
            ),
            BotRpcResponse::ImportedExternalBots { .. }
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::DeleteBot {
                    bot_id: created.id.clone(),
                    delete_home: true,
                }
            ),
            BotRpcResponse::Deleted { .. }
        ));

        mgr.shutdown();
        fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn runtime_hook_heals_memory_and_marks_first_run_completed_and_bridge_resolves_gates() {
        use crate::bots::runtime::BotSessionBinding;
        use krill_proto::bots::{MemorySnapshot, parse_memory_facts};
        use krill_proto::hook::{EventId, HookEvent};

        let state = temp_state("hook-bridge");
        let reg = Arc::new(Registry::new(None, Some(&state)));
        reg.bind_self_arc();
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));

        let created = match mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: "Hook Bot".to_owned(),
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
            })),
        ) {
            BotRpcResponse::Bot(b) => Some(*b),
            _ => None,
        }
        .unwrap();
        assert!(!created.first_run_completed);

        let initial_mem = "# Memory\n\n- 2026-11-20 Must never forget this fact\n";
        mgr.store()
            .update_memory(&created.id, initial_mem, true)
            .unwrap();

        // Bind a synthetic bot session and simulate silent deletion of MEMORY.md during turn.
        let sid = SessionId(42);
        lock(mgr.runtime.sessions_for_test()).insert(
            sid,
            BotSessionBinding {
                bot_id: created.id.clone(),
                link_id: None,
                memory_snapshot: MemorySnapshot::capture(initial_mem),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
            },
        );
        // Simulate bot overwriting MEMORY.md directly on disk, dropping the fact.
        let mem_path = mgr.store().bot_home(&created).join("MEMORY.md");
        fs::write(&mem_path, "# Memory\n\n").unwrap();

        let stop_event = HookEvent {
            id: EventId {
                unix_ns: 1_795_000_000_000_000_000,
                pid: 1234,
            },
            ppid: 1234,
            agent: "claude".to_owned(),
            input: krill_proto::hook::Input::Complete,
            payload: br#"{"hook_event_name":"Stop"}"#.to_vec(),
        };
        mgr.on_hook_event(&reg, sid, &stop_event);

        // Fact restored + first_run_completed marked true.
        let healed = mgr.store().get_memory(&created.id).unwrap();
        let facts = parse_memory_facts(&healed);
        assert_eq!(facts.len(), 1);
        assert_eq!(
            facts.first().map(|f| f.text.as_str()),
            Some("Must never forget this fact")
        );
        assert!(
            mgr.store()
                .get_bot(&created.id)
                .unwrap()
                .unwrap()
                .first_run_completed
        );

        // Test ConnectorBridge methods including orchestration gate resolution.
        let gate = OrchestrationStore::mutate(&state, |s| {
            s.create_gate(
                "worker-1",
                "Deploy now?",
                vec!["yes".to_owned(), "no".to_owned()],
                None,
                None,
            )
        })
        .unwrap();

        let bridge = DaemonConnectorBridge {
            registry: Arc::downgrade(&reg),
            manager: Arc::downgrade(&mgr),
        };
        assert_eq!(bridge.list_bots().len(), 1);
        assert!(bridge.get_bot_config(&created.id).is_some());

        mgr.secrets()
            .set_secret(
                &created.id,
                "tg_token",
                Zeroizing::new("123:abc".to_owned()),
            )
            .unwrap();
        assert_eq!(
            bridge
                .resolve_secret(&created.id, None, "tg_token")
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("123:abc")
        );

        let access = bridge
            .check_or_request_access(
                &created.id,
                ExternalSource::Telegram,
                "555",
                "Carol",
                "chat-555",
            )
            .unwrap();
        assert!(matches!(access, AccessDecision::Pending(_)));

        let link = bridge
            .lookup_or_create_link(
                &created.id,
                ExternalSource::Telegram,
                None,
                "chat-555",
                None,
                ActsFor::Owner,
            )
            .unwrap();
        assert_eq!(link.channel_id, "chat-555");

        bridge
            .reply_orchestration_gate(
                &gate.id,
                "yes",
                Some("Approved via Telegram".to_owned()),
                "owner",
            )
            .unwrap();
        let orch = OrchestrationStore::load(&state).unwrap();
        assert_eq!(
            orch.gates.first().and_then(|g| g.decision.as_deref()),
            Some("yes")
        );

        mgr.shutdown();
        fs::remove_dir_all(&state).ok();
    }

    #[cfg(unix)]
    #[test]
    fn registry_ipc_serves_control_and_standalone_bot_rpc_and_enforces_observer_role() {
        use krill_proto::control::{FEAT_CONTROL, Reply, Request};
        use krill_proto::ipc::{self, FEAT_BOTS, PROTO_MAX, PROTO_MIN, ServerHello};
        use krill_proto::os::net::UnixStream;
        use krill_proto::roles::{self, FEAT_ROLES, Role};
        use krill_proto::screen::Msg as _;
        use std::io::Write as _;

        let state = temp_state("ipc-wire");
        let reg = Arc::new(Registry::new(None, Some(&state)));
        reg.bind_self_arc();

        let hello = ServerHello {
            proto_min: PROTO_MIN,
            proto_max: PROTO_MAX,
            features: FEAT_CONTROL | FEAT_BOTS | FEAT_ROLES,
            build: "test".to_owned(),
            instance_id: 1,
        };
        let mut client_hello =
            krill_proto::daemon::client_hello(ipc::ClientRole::App, "bots-ipc-test");
        client_hello.features = FEAT_CONTROL | FEAT_BOTS | FEAT_ROLES;

        // 1. Control-channel `Request::Bot` round-trip + Observer write denial.
        let (mut client, server) = UnixStream::pair().unwrap();
        let reg_srv = Arc::clone(&reg);
        let h_srv = hello.clone();
        let srv_thread = std::thread::spawn(move || reg_srv.serve(server, &h_srv));

        let _ = ipc::read_frame(&mut client).unwrap().unwrap();
        client.write_all(&client_hello.encode().unwrap()).unwrap();

        // Create a bot via Request::Bot.
        let create_req = Request::Bot(BotRpcRequest::CreateBot(Box::new(CreateBotParams {
            name: "IPC Bot".to_owned(),
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
        })));
        client.write_all(&create_req.encode().unwrap()).unwrap();
        let (k, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        let reply = Reply::decode(k, &body).unwrap();
        assert!(matches!(reply, Reply::Bot(BotRpcResponse::Bot(_))));

        // Downgrade connection to Observer: ListBots allowed, CreateBot denied!
        client
            .write_all(&roles::Request::Set(Role::Observer).encode().unwrap())
            .unwrap();
        let _ = ipc::read_frame(&mut client).unwrap().unwrap();

        client
            .write_all(&Request::Bot(BotRpcRequest::ListBots).encode().unwrap())
            .unwrap();
        let (k, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert!(matches!(
            Reply::decode(k, &body).unwrap(),
            Reply::Bot(BotRpcResponse::Bots(ref list)) if list.len() == 1
        ));

        client.write_all(&create_req.encode().unwrap()).unwrap();
        let (k, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert!(matches!(
            Reply::decode(k, &body).unwrap(),
            Reply::Failed(krill_proto::control::Failure::Denied)
        ));
        drop(client);
        srv_thread.join().ok();

        // 2. Dedicated standalone `BOT_REQ` / `BOT_REPLY` frame round-trip.
        let (mut client2, server2) = UnixStream::pair().unwrap();
        let reg_srv2 = Arc::clone(&reg);
        let h_srv2 = hello;
        let srv_thread2 = std::thread::spawn(move || reg_srv2.serve(server2, &h_srv2));

        let _ = ipc::read_frame(&mut client2).unwrap().unwrap();
        client2.write_all(&client_hello.encode().unwrap()).unwrap();
        client2
            .write_all(&BotRpcRequest::ListBots.encode().unwrap())
            .unwrap();
        let (k, body) = ipc::read_frame(&mut client2).unwrap().unwrap();
        assert_eq!(k, krill_proto::bots::BOT_REPLY);
        assert!(matches!(
            BotRpcResponse::decode(k, &body).unwrap(),
            BotRpcResponse::Bots(ref list) if list.len() == 1
        ));
        drop(client2);
        srv_thread2.join().ok();

        if let Some(b) = reg.bots_if_initialized() {
            b.shutdown();
        }
        fs::remove_dir_all(&state).ok();
    }
}
