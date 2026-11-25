// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Krill Bots daemon subsystem (`BotsManager`): strictly-lazy facade owning bot persistence,
//! encrypted secret vault, SQLite items inbox, memory history/topics, loopback MCP tool relay,
//! local Google/GitHub/Linear/Composio integrations, group chats, session runtime hooks, and the
//! [`connectors::ConnectorBridge`].

pub mod composio;
pub mod connectors;
pub mod google;
pub mod groups;
pub mod http_client;
pub mod items;
pub mod look_around;
pub mod mcp_oauth;
pub mod memory_history;
pub mod memory_tidy;
pub mod runtime;
pub mod secrets;
pub mod store;
pub mod tool_relay;
pub mod trackers;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::SessionId;
use krill_proto::bots::{
    ActsFor, BotAccessRequestStatus, BotConfig, BotConnectorStatus, BotId, BotItemActor,
    BotItemDecision, BotItemStatus, BotRpcRequest, BotRpcResponse, BotSlug, ConnectorHealthState,
    CreateBotItemParams, ExternalConversationLink, ExternalSource, FieldPatch, UpdateBotItemParams,
    UpdateBotParams,
};
use krill_proto::hook::HookEvent;
use krill_proto::orchestration::OrchestrationStore;
use krill_proto::schedule::Cron;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::bots::connectors::{
    AccessDecision, ConnectorBridge, ConnectorEndpoints, ConnectorManager, DispatchOutcome,
    ExternalResumeResult, InboundExternalMessage, OutboundBotReply,
};
use crate::bots::google::{GoogleEndpoints, GoogleTriggerChecker};
use crate::bots::groups::GroupCoordinator;
use crate::bots::items::BotItemsStore;
use crate::bots::look_around::{LookAroundTracker, SchedulerStateStore};
use crate::bots::memory_tidy::MemoryTidyTracker;
use crate::bots::runtime::{BotRuntime, RestartCutTurn};
use crate::bots::secrets::SecretVault;
use crate::bots::store::{BotStore, MemoryUpdateError};
use crate::bots::tool_relay::{BotToolRelay, BuiltinMcpCallContext, BuiltinMcpHandler};
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
    groups: Arc<GroupCoordinator>,
    scheduler_state: Arc<SchedulerStateStore>,
    connectors: OnceLock<Arc<ConnectorManager>>,
    google_poller: OnceLock<Arc<GoogleTriggerChecker>>,
    tracker_poller: OnceLock<Arc<TrackerPoller>>,
    mcp_oauth: OnceLock<Arc<mcp_oauth::McpOAuthManager>>,
    composio_statuses: Mutex<BTreeMap<BotId, BotConnectorStatus>>,
    composio_last_poll_ms: Mutex<BTreeMap<BotId, u64>>,
    memory_tidy_tracker: MemoryTidyTracker,
    look_around_tracker: LookAroundTracker,
    active_cron_sessions: Mutex<HashMap<(BotId, String), SessionId>>,
    in_flight_composio: Mutex<HashSet<BotId>>,
    in_flight_schedules: Mutex<HashSet<(BotId, String)>>,
    boot_reconciled: OnceLock<()>,
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
        let secrets = Arc::new(SecretVault::new(bots_root.clone()));
        let tool_relay = Arc::new(BotToolRelay::new(Arc::clone(&secrets)));
        let runtime = Arc::new(BotRuntime::with_root(bots_root.clone()));
        let groups = Arc::new(GroupCoordinator::new(bots_root.clone()));
        let scheduler_state = Arc::new(SchedulerStateStore::new(bots_root));

        let manager = Arc::new(Self {
            state_dir,
            store,
            items,
            secrets,
            tool_relay,
            runtime,
            groups,
            scheduler_state,
            connectors: OnceLock::new(),
            google_poller: OnceLock::new(),
            tracker_poller: OnceLock::new(),
            mcp_oauth: OnceLock::new(),
            composio_statuses: Mutex::new(BTreeMap::new()),
            composio_last_poll_ms: Mutex::new(BTreeMap::new()),
            memory_tidy_tracker: MemoryTidyTracker::default(),
            look_around_tracker: LookAroundTracker::default(),
            active_cron_sessions: Mutex::new(HashMap::new()),
            in_flight_composio: Mutex::new(HashSet::new()),
            in_flight_schedules: Mutex::new(HashSet::new()),
            boot_reconciled: OnceLock::new(),
            google_endpoints,
            tracker_endpoints,
            composio_base_url,
            registry: registry.clone(),
            self_weak: OnceLock::new(),
        });
        manager.self_weak.set(Arc::downgrade(&manager)).ok();
        manager.runtime.attach_callbacks(
            manager.registry.clone(),
            Arc::downgrade(&manager.store),
            Arc::downgrade(&manager.groups),
        );
        manager
            .tool_relay
            .set_builtin_handler(Arc::new(DaemonBuiltinMcpHandler {
                registry,
                manager: Arc::downgrade(&manager),
            }));
        manager
    }

    /// Rehydrates kept bot session bindings, starts any configured connectors/pollers once on boot,
    /// and notifies linked external conversations of any in-flight turns cut off by restart (Orca #47898).
    pub fn boot_recover(&self, registry: &Registry) {
        if self.boot_reconciled.set(()).is_err() {
            return;
        }
        let (_rehydrated, cut_turns) = self
            .runtime
            .rehydrate_from_disk_with_cuts(&self.store, registry);
        self.reconcile_enabled_workers();
        if !cut_turns.is_empty() {
            self.report_pending_restart_cuts(&cut_turns);
        }
    }

    fn report_pending_restart_cuts(&self, cuts: &[RestartCutTurn]) {
        if cuts.is_empty() {
            return;
        }
        let had_connectors = self.connectors.get().is_some();
        let mgr = self.connector_manager();
        if !had_connectors {
            mgr.reload(None).ok();
        }
        for cut in cuts {
            if cut.notified {
                continue;
            }
            let Ok(Some(bot)) = self.store.get_bot(&cut.bot_id) else {
                self.runtime
                    .mark_restart_cut_notified(&cut.bot_id, &cut.link_id, cut.turn_seq);
                continue;
            };
            let Ok(Some(link)) = self.store.get_link(&cut.link_id) else {
                self.runtime
                    .mark_restart_cut_notified(&cut.bot_id, &cut.link_id, cut.turn_seq);
                continue;
            };
            if mgr.report_restart_cut_turn(
                &bot,
                &link,
                cut.turn_seq,
                cut.status_message_id.as_deref(),
            ) {
                self.runtime
                    .mark_restart_cut_notified(&cut.bot_id, &cut.link_id, cut.turn_seq);
            }
        }
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

    /// Access to the group coordinator.
    #[must_use]
    pub fn groups(&self) -> &Arc<GroupCoordinator> {
        &self.groups
    }

    /// Lazily initializes and returns the Wave 2B [`ConnectorManager`].
    #[must_use]
    pub fn connector_manager(&self) -> &Arc<ConnectorManager> {
        self.connectors.get_or_init(|| {
            let bridge: Arc<dyn ConnectorBridge> = Arc::new(DaemonConnectorBridge {
                registry: self.registry.clone(),
                manager: self.self_weak.get().cloned().unwrap_or_default(),
            });
            let endpoints = ConnectorEndpoints {
                github: self.tracker_endpoints.github_api_base.clone(),
                linear: self.tracker_endpoints.linear_graphql_url.clone(),
                ..ConnectorEndpoints::default()
            };
            let cm = Arc::new(ConnectorManager::with_endpoints(bridge, endpoints));
            self.runtime.attach_connectors(Arc::downgrade(&cm));
            cm
        })
    }

    fn mcp_oauth_mgr(&self) -> &Arc<mcp_oauth::McpOAuthManager> {
        self.mcp_oauth
            .get_or_init(|| Arc::new(mcp_oauth::McpOAuthManager::new(Arc::clone(&self.secrets))))
    }

    fn google_checker(&self) -> &Arc<GoogleTriggerChecker> {
        self.google_poller.get_or_init(|| {
            let checker = Arc::new(GoogleTriggerChecker::new(
                Arc::clone(&self.store),
                Arc::clone(&self.secrets),
                Arc::clone(&self.items),
                self.google_endpoints.clone(),
            ));
            let reg_w = self.registry.clone();
            let mgr_w = self.self_weak.get().cloned().unwrap_or_default();
            checker.set_on_new_items(Arc::new(move |bot, count| {
                if let (Some(reg), Some(mgr)) = (reg_w.upgrade(), mgr_w.upgrade()) {
                    let prompt = format!(
                        "[{count} new Google Workspace item(s) detected. Inspect `items_list` and act or summarize if important.]"
                    );
                    mgr.runtime
                        .deliver_or_wake_bot(
                            &reg,
                            &mgr.store,
                            &mgr.secrets,
                            &mgr.tool_relay,
                            &mgr.composio_base_url,
                            &bot.id,
                            &prompt,
                            now_ms(),
                        )
                        .ok();
                }
            }));
            checker
        })
    }

    fn tracker_poller(&self) -> &Arc<TrackerPoller> {
        self.tracker_poller.get_or_init(|| {
            let poller = Arc::new(TrackerPoller::new(
                Arc::clone(&self.store),
                Arc::clone(&self.secrets),
                Arc::clone(&self.items),
                self.tracker_endpoints.clone(),
            ));
            let reg_w = self.registry.clone();
            let mgr_w = self.self_weak.get().cloned().unwrap_or_default();
            poller.set_on_new_items(Arc::new(move |bot, source, count| {
                if let (Some(reg), Some(mgr)) = (reg_w.upgrade(), mgr_w.upgrade()) {
                    let prompt = format!(
                        "[{count} new {} tracker item(s) synced. Review `items_list` for NeedsYou or open tasks.]",
                        source.as_str()
                    );
                    mgr.runtime
                        .deliver_or_wake_bot(
                            &reg,
                            &mgr.store,
                            &mgr.secrets,
                            &mgr.tool_relay,
                            &mgr.composio_base_url,
                            &bot.id,
                            &prompt,
                            now_ms(),
                        )
                        .ok();
                }
            }));
            poller
        })
    }

    /// Dispatches any [`BotRpcRequest`] as owner (`Full`) with `catch_unwind` panic isolation.
    #[must_use]
    pub fn handle_rpc(&self, registry: &Registry, req: BotRpcRequest) -> BotRpcResponse {
        self.handle_rpc_for_peer(registry, None, false, req)
    }

    /// Dispatches any [`BotRpcRequest`] with explicit IPC session/observer context and `catch_unwind`.
    #[must_use]
    pub fn handle_rpc_for_peer(
        &self,
        registry: &Registry,
        peer_session: Option<SessionId>,
        is_observer: bool,
        req: BotRpcRequest,
    ) -> BotRpcResponse {
        if let Ok(resp) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.dispatch_rpc(registry, peer_session, is_observer, req)
        })) {
            resp
        } else {
            tracing::warn!("bot RPC handler panicked; isolated");
            BotRpcResponse::Error("internal bot RPC panic".to_owned())
        }
    }

    /// Records a direct owner prompt submitted to a live bot session (`SendInput` / mobile `Prompt`).
    pub fn on_owner_prompt_submitted(&self, session_id: SessionId, prompt: &str) {
        self.runtime.on_owner_prompt_submitted(session_id, prompt);
    }

    /// Hook event notification forwarded from [`Registry`] (fast-paths out for non-bot sessions).
    pub fn on_hook_event(&self, registry: &Registry, session_id: SessionId, event: &HookEvent) {
        if !self.runtime.is_bot_session(session_id) {
            return;
        }
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let turn_err = self.runtime.on_hook_event_with_groups(
                registry,
                &self.store,
                self.connectors.get().map(Arc::as_ref),
                Some(self.groups.as_ref()),
                session_id,
                event,
            );
            if let Some(err_text) = turn_err {
                let matching_scheds: Vec<(BotId, String)> = lock(&self.active_cron_sessions)
                    .iter()
                    .filter(|(_, sid)| **sid == session_id)
                    .map(|((bid, sched_id), _)| (bid.clone(), sched_id.clone()))
                    .collect();
                let ts = now_ms();
                for (bid, sched_id) in matching_scheds {
                    let refusal = self
                        .store
                        .get_bot(&bid)
                        .ok()
                        .flatten()
                        .and_then(|bot| classify_turn_error_refusal(&bot.agent_id, &err_text));
                    self.scheduler_state
                        .record_schedule_error(&bid, &sched_id, err_text.clone(), refusal, ts)
                        .ok();
                }
            }
        }))
        .ok();
    }

    /// Prepares bot launch inputs when `old` is a bot session switching accounts to `new`.
    pub(crate) fn prepare_account_switch_launch(
        &self,
        old: SessionId,
        new: SessionId,
        launch_cwd: &std::path::Path,
    ) -> Result<Option<runtime::BotAccountSwitchLaunch>, String> {
        self.runtime.prepare_account_switch_launch(
            &self.store,
            &self.secrets,
            &self.tool_relay,
            &self.composio_base_url,
            old,
            new,
            launch_cwd,
        )
    }

    /// Rebinds a bot session across runtime bindings, active group turns, and cron tracking.
    /// Returns `Some(ReboundBotSession)` if `old` was bound to a bot.
    pub(crate) fn rebind_session(
        &self,
        old: SessionId,
        new: SessionId,
        new_pid: u32,
        new_token: Option<zeroize::Zeroizing<String>>,
    ) -> Option<runtime::ReboundBotSession> {
        let rebound = self.runtime.rebind_session(old, new, new_pid, new_token)?;
        self.groups.rebind_session(old, new);
        for sid in lock(&self.active_cron_sessions).values_mut() {
            if *sid == old {
                *sid = new;
            }
        }
        Some(rebound)
    }

    /// Revokes an ephemeral bot tool relay launch token.
    pub(crate) fn revoke_launch_token(&self, token: Option<&zeroize::Zeroizing<String>>) {
        if let Some(tok) = token {
            self.tool_relay.revoke_token(tok.as_str());
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn runtime_for_test(&self) -> &runtime::BotRuntime {
        &self.runtime
    }

    /// Session cleanup hook forwarded from [`Registry::remove`].
    pub fn on_session_removed(&self, session_id: SessionId) {
        self.runtime
            .on_session_removed(&self.tool_relay, session_id);
        self.groups.on_session_removed(session_id);
        if let Some(connectors) = self.connectors.get() {
            connectors.on_session_removed(session_id);
        }
        lock(&self.active_cron_sessions).retain(|_, &mut sid| sid != session_id);
    }

    /// Periodic scheduler tick called on an independent bot scheduler loop ONLY when bots exist.
    pub fn tick_periodic(&self, registry: &Registry, now: SystemTime) {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.boot_recover(registry);

            let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let ts_ms = secs.saturating_mul(1_000);
            let minute = secs / 60;

            let Ok(bots) = self.store.list_bots() else {
                return;
            };
            if bots.is_empty() {
                return;
            }

            self.reconcile_enabled_workers_for(&bots);
            let pending_cuts = self.runtime.pending_unnotified_restart_cuts();
            if !pending_cuts.is_empty() {
                self.report_pending_restart_cuts(&pending_cuts);
            }
            if self.runtime.has_pending_rate_limit_pauses() {
                let auto_continue_enabled =
                    crate::rejoin::recovery::read_chat_auto_continue_setting(&self.state_dir);
                if auto_continue_enabled {
                    let home = crate::usage::accounts::default_home();
                    let accounts_snap =
                        crate::usage::accounts::refresh_accounts(&home, Some(&self.state_dir));
                    self.runtime.poll_rate_limit_auto_continues(
                        registry,
                        &self.store,
                        auto_continue_enabled,
                        &accounts_snap,
                        secs,
                    );
                }
            }

            for bot in &bots {
                if bot.paused {
                    continue;
                }
                let bot_tz = bot
                    .schedules
                    .iter()
                    .find_map(|s| s.timezone.clone())
                    .or_else(look_around::local_tz);
                let bot_tz = bot_tz.as_deref();
                let extras = self.store.settings_extras(&bot.id).unwrap_or_default();
                if bot.memory_tidy.enabled
                    && self
                        .scheduler_state
                        .tidy_is_due(&bot.id, extras.tidy_every, bot_tz, ts_ms)
                    && self
                        .memory_tidy_tracker
                        .is_due(&bot.id, extras.tidy_every, bot_tz, ts_ms)
                {
                    if memory_tidy::run_memory_tidy_tracked(
                        &self.store,
                        Some(&self.scheduler_state),
                        &bot.id,
                        ts_ms,
                    )
                    .is_ok()
                        && let Ok(content) = self.store.get_memory(&bot.id)
                    {
                        self.runtime.refresh_memory_snapshot(&bot.id, &content);
                    }
                    self.memory_tidy_tracker.record(&bot.id, ts_ms);
                }
                if bot.look_around.enabled
                    && self.scheduler_state.look_around_is_due(
                        &bot.id,
                        extras.look_around_every,
                        bot.look_around.weekdays_only,
                        bot_tz,
                        ts_ms,
                    )
                    && self.look_around_tracker.is_due(
                        &bot.id,
                        extras.look_around_every,
                        bot.look_around.weekdays_only,
                        bot_tz,
                        ts_ms,
                    )
                    && look_around::run_look_around_tracked(
                        &self.store,
                        &self.items,
                        Some(&self.scheduler_state),
                        &bot.id,
                        ts_ms,
                    )
                    .is_ok()
                {
                    self.look_around_tracker.record(&bot.id, ts_ms);
                }

                if bot.connectors.composio.enabled {
                    let last = lock(&self.composio_last_poll_ms)
                        .get(&bot.id)
                        .copied()
                        .unwrap_or(0);
                    if ts_ms.saturating_sub(last) >= 60_000
                        && lock(&self.in_flight_composio).insert(bot.id.clone())
                    {
                        lock(&self.composio_last_poll_ms).insert(bot.id.clone(), ts_ms);
                        self.spawn_composio_poll(bot.clone(), ts_ms);
                    }
                }

                for sched in &bot.schedules {
                    if !sched.enabled {
                        continue;
                    }
                    let Ok(cal) =
                        look_around::cron_calendar_for_tz(secs, sched.timezone.as_deref())
                    else {
                        continue;
                    };
                    if let Ok(cron) = Cron::parse(&sched.cron)
                        && cron.matches(cal)
                    {
                        self.spawn_due_schedule(bot.id.clone(), sched.clone(), minute);
                    }
                }
            }
        }))
        .ok();
    }

    fn spawn_composio_poll(&self, bot: BotConfig, ts_ms: u64) {
        let Some(mgr_w) = self.self_weak.get().cloned() else {
            lock(&self.in_flight_composio).remove(&bot.id);
            return;
        };
        let reg_w = self.registry.clone();
        let bid = bot.id.clone();
        let bid_err = bid.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("bot-composio-{}", bot.slug.as_str()))
            .stack_size(256 * 1024)
            .spawn(move || {
                struct InFlightGuard {
                    mgr: Weak<BotsManager>,
                    bot_id: BotId,
                }
                impl Drop for InFlightGuard {
                    fn drop(&mut self) {
                        if let Some(m) = self.mgr.upgrade() {
                            lock(&m.in_flight_composio).remove(&self.bot_id);
                        }
                    }
                }
                let _guard = InFlightGuard {
                    mgr: mgr_w.clone(),
                    bot_id: bid,
                };
                let (Some(mgr), Some(reg)) = (mgr_w.upgrade(), reg_w.upgrade()) else {
                    return;
                };
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if let Ok(created) = composio::poll_composio_triggers(
                        &bot,
                        &mgr.secrets,
                        &mgr.items,
                        &mgr.composio_base_url,
                        ts_ms,
                    ) && !created.is_empty()
                    {
                        let prompt = format!(
                            "[{} new Composio trigger item(s) arrived. Review `items_list` and take action if needed.]",
                            created.len()
                        );
                        mgr.runtime
                            .deliver_or_wake_bot(
                                &reg,
                                &mgr.store,
                                &mgr.secrets,
                                &mgr.tool_relay,
                                &mgr.composio_base_url,
                                &bot.id,
                                &prompt,
                                ts_ms,
                            )
                            .ok();
                    }
                }))
                .ok();
            });
        if spawned.is_err() {
            lock(&self.in_flight_composio).remove(&bid_err);
        }
    }

    fn spawn_due_schedule(
        &self,
        bot_id: BotId,
        sched: krill_proto::bots::BotScheduleConfig,
        minute: u64,
    ) {
        let key = (bot_id.clone(), sched.id.clone());
        if !lock(&self.in_flight_schedules).insert(key.clone()) {
            return;
        }
        let Some(mgr_w) = self.self_weak.get().cloned() else {
            lock(&self.in_flight_schedules).remove(&key);
            return;
        };
        let reg_w = self.registry.clone();
        let key_err = key.clone();
        let sched_thread_token: String = sched
            .id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(32)
            .collect();
        let sched_thread_name = if sched_thread_token.is_empty() {
            "bot-sched".to_owned()
        } else {
            format!("bot-sched-{sched_thread_token}")
        };
        let spawned = std::thread::Builder::new()
            .name(sched_thread_name)
            .stack_size(256 * 1024)
            .spawn(move || {
                struct SchedGuard {
                    mgr: Weak<BotsManager>,
                    key: (BotId, String),
                }
                impl Drop for SchedGuard {
                    fn drop(&mut self) {
                        if let Some(m) = self.mgr.upgrade() {
                            lock(&m.in_flight_schedules).remove(&self.key);
                        }
                    }
                }
                let _guard = SchedGuard {
                    mgr: mgr_w.clone(),
                    key,
                };
                let (Some(mgr), Some(reg)) = (mgr_w.upgrade(), reg_w.upgrade()) else {
                    return;
                };
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    mgr.run_schedule(&reg, &bot_id, &sched, Some(minute)).ok();
                }))
                .ok();
            });
        if spawned.is_err() {
            lock(&self.in_flight_schedules).remove(&key_err);
        }
    }

    /// Runs one due schedule as its bot. While the schedule's conversation is still on its last
    /// turn this minute is skipped; once idle, the prompt becomes its next turn instead of a new session.
    /// `minute` is the cron minute it fires for; `None` runs it now (Run now), unclaimed.
    fn run_schedule(
        &self,
        registry: &Registry,
        bot_id: &BotId,
        sched: &krill_proto::bots::BotScheduleConfig,
        minute: Option<u64>,
    ) -> Result<(), runtime::BotStartError> {
        let bot = self
            .store
            .get_bot(bot_id)
            .map_err(|err| runtime::BotStartError::Other(err.to_string()))?
            .ok_or_else(|| {
                runtime::BotStartError::Other(format!("bot not found: {}", bot_id.as_str()))
            })?;
        if bot.paused {
            let err = runtime::BotStartError::Refused(
                krill_proto::bots::BotSessionRefusal::bot_paused(&bot.name),
            );
            self.scheduler_state
                .record_schedule_error(
                    bot_id,
                    &sched.id,
                    err.message(),
                    err.refusal().cloned(),
                    now_ms(),
                )
                .ok();
            return Err(err);
        }
        if self.schedule_running(registry, bot_id, &sched.id)
            || minute.is_some_and(|minute| {
                !self
                    .scheduler_state
                    .try_claim_cron_minute(bot_id, &sched.id, minute)
                    .unwrap_or(false)
            })
        {
            return Ok(());
        }
        let key = (bot_id.clone(), sched.id.clone());
        let target = lock(&self.active_cron_sessions)
            .get(&key)
            .copied()
            .filter(|&sid| registry.is_session_live(sid));
        if let Some(sid) = target {
            match self
                .runtime
                .deliver_turn(registry, &self.store, sid, bot_id, &sched.prompt)
            {
                Ok(()) => {
                    self.scheduler_state
                        .clear_schedule_error(bot_id, &sched.id)
                        .ok();
                    return Ok(());
                }
                Err(err) => {
                    let start_err = runtime::BotStartError::Other(err);
                    self.scheduler_state
                        .record_schedule_error(
                            bot_id,
                            &sched.id,
                            start_err.message(),
                            None,
                            now_ms(),
                        )
                        .ok();
                    return Err(start_err);
                }
            }
        }
        match self.runtime.start_bot_session(
            registry,
            &self.store,
            &self.secrets,
            &self.tool_relay,
            &self.composio_base_url,
            bot_id,
            None,
            Some(&sched.prompt),
            None,
        ) {
            Ok((sid, _)) => {
                lock(&self.active_cron_sessions).insert(key, sid);
                self.scheduler_state
                    .clear_schedule_error(bot_id, &sched.id)
                    .ok();
                Ok(())
            }
            Err(err) => {
                self.scheduler_state
                    .record_schedule_error(
                        bot_id,
                        &sched.id,
                        err.message(),
                        err.refusal().cloned(),
                        now_ms(),
                    )
                    .ok();
                Err(err)
            }
        }
    }

    /// The schedule's conversation is still on a turn.
    fn schedule_running(&self, registry: &Registry, bot_id: &BotId, sched_id: &str) -> bool {
        lock(&self.active_cron_sessions)
            .get(&(bot_id.clone(), sched_id.to_owned()))
            .copied()
            .is_some_and(|sid| registry.is_session_live(sid) && self.runtime.turn_running(sid))
    }

    /// Cleanly shuts down all lazy background components.
    pub fn shutdown(&self) {
        if let Some(mgr) = self.connectors.get() {
            mgr.shutdown();
        }
        if let Some(oauth) = self.mcp_oauth.get() {
            oauth.cancel_all();
        }
        google::cancel_all_google_oauth();
        if let Some(g) = self.google_poller.get() {
            g.stop();
        }
        google::cancel_all_google_oauth();
        if let Some(oauth) = self.mcp_oauth.get() {
            oauth.cancel_all();
        }
        if let Some(t) = self.tracker_poller.get() {
            t.stop();
        }
        self.tool_relay.shutdown();
    }

    fn dispatch_rpc(
        &self,
        registry: &Registry,
        peer_session: Option<SessionId>,
        is_observer: bool,
        req: BotRpcRequest,
    ) -> BotRpcResponse {
        if is_observer && !req.is_read_only() {
            return BotRpcResponse::Error("permission denied".to_owned());
        }

        let caller_bot = peer_session.and_then(|sid| {
            self.runtime
                .bot_for_session(sid)
                .map(|bot_id| (bot_id, sid))
        });

        if let Some((ref caller_bot_id, _)) = caller_bot
            && let Err(reason) =
                authorize_bot_session_rpc(&self.store, &self.items, caller_bot_id, &req)
        {
            return BotRpcResponse::Error(reason);
        }

        let default_actor = match &caller_bot {
            Some((bot_id, sid)) => BotItemActor::Bot {
                bot_id: bot_id.clone(),
                session_id: Some(*sid),
            },
            None => BotItemActor::Owner { device: None },
        };
        let memory_actor = match &caller_bot {
            Some((bot_id, _)) => format!("bot:{}", bot_id.as_str()),
            None => "owner".to_owned(),
        };

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
            BotRpcRequest::CreateBot(mut params) => {
                if !params.agent_id.trim().is_empty()
                    && let Err(err) = validate_agent_profile(registry, &params.agent_id)
                {
                    return BotRpcResponse::Error(err);
                }
                if krill_proto::agent::canonical_id(params.agent_id.trim()) == "agy"
                    && let Some(ref model) = params.model
                    && let Some((family, eff)) = crate::mobile::fold_agy_model_selection(
                        None,
                        model,
                        params.effort.as_deref(),
                    )
                {
                    params.model = Some(family);
                    params.effort = Some(eff);
                }
                match self.store.create_bot(*params, ts) {
                    Ok(bot) => {
                        self.reconcile_for_bot(&bot);
                        BotRpcResponse::Bot(Box::new(bot))
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::CreateBotV2(mut params) => {
                if !params.base.agent_id.trim().is_empty()
                    && let Err(err) = validate_agent_profile(registry, &params.base.agent_id)
                {
                    return BotRpcResponse::Error(err);
                }
                if krill_proto::agent::canonical_id(params.base.agent_id.trim()) == "agy"
                    && let Some(ref model) = params.base.model
                    && let Some((family, eff)) = crate::mobile::fold_agy_model_selection(
                        None,
                        model,
                        params.base.effort.as_deref(),
                    )
                {
                    params.base.model = Some(family);
                    params.base.effort = Some(eff);
                }
                match self.store.create_bot_v2(*params, ts) {
                    Ok(bot) => {
                        self.reconcile_for_bot(&bot);
                        BotRpcResponse::Bot(Box::new(bot))
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::UpdateBot(mut params) => {
                if let Some(ref raw_agent) = params.agent_id
                    && let Err(err) = validate_agent_profile(registry, raw_agent)
                {
                    return BotRpcResponse::Error(err);
                }
                let prev_bot = self.store.get_bot(&params.bot_id).ok().flatten();
                let prev_agent = prev_bot
                    .as_ref()
                    .map(|b| krill_proto::agent::canonical_id(&b.agent_id).to_owned());
                let effective_agent = params
                    .agent_id
                    .as_deref()
                    .map(|a| krill_proto::agent::canonical_id(a.trim()))
                    .or(prev_agent.as_deref());
                let provider_changed = prev_agent.as_deref() != effective_agent;
                if provider_changed && params.effort.is_unchanged() {
                    params.effort = krill_proto::bots::FieldPatch::Clear;
                }
                if effective_agent == Some("agy")
                    && let krill_proto::bots::FieldPatch::Set(ref model) = params.model
                {
                    let req_effort = match &params.effort {
                        krill_proto::bots::FieldPatch::Set(e) => Some(e.as_str()),
                        krill_proto::bots::FieldPatch::Clear => None,
                        krill_proto::bots::FieldPatch::Unchanged => {
                            if krill_proto::agent::strip_agy_effort_suffix(model)
                                .1
                                .is_some()
                            {
                                None
                            } else {
                                prev_bot.as_ref().and_then(|b| b.effort.as_deref())
                            }
                        }
                    };
                    if let Some((family, eff)) =
                        crate::mobile::fold_agy_model_selection(None, model, req_effort)
                    {
                        params.model = krill_proto::bots::FieldPatch::Set(family);
                        params.effort = krill_proto::bots::FieldPatch::Set(eff);
                    }
                }
                match self.store.update_bot(*params, ts) {
                    Ok(bot) => {
                        let next_agent = krill_proto::agent::canonical_id(&bot.agent_id);
                        if prev_agent.as_deref().is_some_and(|prev| prev != next_agent) {
                            // Main provider changed: retire old-provider live sessions, cron
                            // sessions, and external link session bindings so the next turn
                            // launches cleanly on the new runtime instead of reusing the old one.
                            self.runtime
                                .reset_chat(
                                    registry,
                                    &self.store,
                                    &self.tool_relay,
                                    &bot.id,
                                    None,
                                    ts,
                                )
                                .ok();
                            lock(&self.active_cron_sessions).retain(|(bid, _), _| bid != &bot.id);
                            self.store.unbind_bot_links(&bot.id, ts).ok();
                        }
                        if bot.memory_tidy.enabled {
                            self.scheduler_state.reset_tidy_auto_disabled(&bot.id).ok();
                        }
                        self.reconcile_for_bot(&bot);
                        BotRpcResponse::Bot(Box::new(bot))
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::UpdateBotExtras(params) => {
                match self.store.update_bot_extras(*params, ts) {
                    Ok(bot) => BotRpcResponse::Bot(Box::new(bot)),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::DeleteBot {
                bot_id,
                delete_home,
            } => {
                match self.store.get_bot(&bot_id) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return BotRpcResponse::Error(format!(
                            "bot not found: {}",
                            bot_id.as_str()
                        ));
                    }
                    Err(err) => return BotRpcResponse::Error(err.to_string()),
                }
                // Cancel any pending OAuth sign-in listeners and stop all live PTY sessions
                // BEFORE removing the bot and deleting its secrets so nothing races back in.
                if let Some(oauth) = self.mcp_oauth.get() {
                    oauth.cancel_bot(&bot_id);
                }
                google::cancel_google_oauth(&bot_id);
                self.runtime
                    .stop_all_for_bot(registry, &self.tool_relay, &bot_id);
                google::cancel_google_oauth(&bot_id);
                if let Some(oauth) = self.mcp_oauth.get() {
                    oauth.cancel_bot(&bot_id);
                }
                lock(&self.active_cron_sessions).retain(|(bid, _), _| bid != &bot_id);
                match self.store.delete_bot_with_cleanup(&bot_id, delete_home) {
                    Ok((removed, removed_group_ids)) => {
                        self.groups.on_bot_deleted(&bot_id, &removed_group_ids);
                        self.items.delete_bot_items(&bot_id).ok();
                        let mut extra_refs = collect_bot_secret_refs(&removed);
                        if let Ok(remaining_bots) = self.store.list_bots() {
                            let in_use: std::collections::HashSet<String> = remaining_bots
                                .iter()
                                .flat_map(collect_bot_secret_refs)
                                .collect();
                            extra_refs.retain(|r| !in_use.contains(r));
                        }
                        self.secrets.delete_bot_secrets(&bot_id, &extra_refs).ok();
                        self.tool_relay.unregister_bot(&bot_id);
                        composio::cleanup_composio_bot(
                            &bot_id,
                            Some(self.store.root()),
                            &self.tool_relay,
                        );
                        self.scheduler_state.forget_bot(&bot_id).ok();
                        lock(&self.composio_statuses).remove(&bot_id);
                        lock(&self.composio_last_poll_ms).remove(&bot_id);
                        self.reconcile_all_lazy_workers();
                        BotRpcResponse::Deleted { bot_id }
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
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
                Err(err) => err.into_rpc_response(),
            },
            BotRpcRequest::ListBotSessions { bot_id } => BotRpcResponse::BotSessions(
                self.runtime
                    .list_sessions_for_store(&self.store, registry, bot_id.as_ref()),
            ),
            BotRpcRequest::ResetBotChat { bot_id, session_id } => {
                match self.runtime.reset_chat(
                    registry,
                    &self.store,
                    &self.tool_relay,
                    &bot_id,
                    session_id,
                    ts,
                ) {
                    Ok(()) => BotRpcResponse::Ok,
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::StopBotSession { bot_id, session_id } => {
                match self
                    .runtime
                    .stop_session(registry, &self.tool_relay, &bot_id, session_id)
                {
                    Ok(()) => BotRpcResponse::Ok,
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::RetryBotKickoff { bot_id } => match self.runtime.retry_kickoff(
                registry,
                &self.store,
                &self.secrets,
                &self.tool_relay,
                &self.composio_base_url,
                &bot_id,
                ts,
            ) {
                Ok((session_id, pid)) => BotRpcResponse::SessionStarted {
                    bot_id,
                    session_id,
                    pid,
                },
                Err(err) => err.into_rpc_response(),
            },
            BotRpcRequest::TakeOverWorkspace { bot_id, session_id } => {
                match self.runtime.take_over_workspace(
                    registry,
                    &self.tool_relay,
                    &bot_id,
                    session_id,
                ) {
                    Ok(()) => {
                        self.store.unbind_session_links(session_id, ts).ok();
                        lock(&self.active_cron_sessions).retain(|_, &mut sid| sid != session_id);
                        self.groups.on_session_removed(session_id);
                        if let Some(connectors) = self.connectors.get() {
                            connectors.on_session_removed(session_id);
                        }
                        BotRpcResponse::Ok
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::SendBotRunAgain { bot_id, session_id } => {
                match self
                    .runtime
                    .send_run_again(registry, &self.store, &bot_id, session_id)
                {
                    Ok(()) => BotRpcResponse::Ok,
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
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
            } => match self.store.apply_memory_with_history(
                &bot_id,
                &content,
                allow_deletions,
                &memory_actor,
                "update",
                ts,
            ) {
                Ok(_) => {
                    if caller_bot.is_none() {
                        self.runtime.refresh_memory_snapshot(&bot_id, &content);
                    }
                    BotRpcResponse::MemoryUpdated {
                        content,
                        restored_facts: Vec::new(),
                        removed_count: 0,
                    }
                }
                Err(MemoryUpdateError::DeletionBlocked(violation)) => {
                    BotRpcResponse::Error(format!(
                        "MemoryDeletionViolation: {violation} ({})",
                        violation.deleted_facts.join("; ")
                    ))
                }
                Err(MemoryUpdateError::Io(err)) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::ForgetMemoryFact { bot_id, fact } => {
                match self.store.forget_memory_fact(&bot_id, &fact, &memory_actor) {
                    Ok((content, removed_count)) => {
                        self.runtime.refresh_memory_snapshot(&bot_id, &content);
                        BotRpcResponse::MemoryUpdated {
                            content,
                            restored_facts: Vec::new(),
                            removed_count,
                        }
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::GetMemoryHistory { bot_id, limit } => {
                match self.store.get_memory_history(&bot_id, limit) {
                    Ok(entries) => BotRpcResponse::MemoryHistory(entries),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::EditMemoryFact {
                bot_id,
                old_fact,
                new_fact,
            } => match self
                .store
                .edit_memory_fact(&bot_id, &old_fact, &new_fact, &memory_actor, ts)
            {
                Ok(content) => {
                    self.runtime.refresh_memory_snapshot(&bot_id, &content);
                    BotRpcResponse::MemoryUpdated {
                        content,
                        restored_facts: Vec::new(),
                        removed_count: 0,
                    }
                }
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::UndoMemoryChange { bot_id, entry_id } => {
                match self
                    .store
                    .undo_memory_entry(&bot_id, &entry_id, &memory_actor, ts)
                {
                    Ok(content) => {
                        self.runtime.refresh_memory_snapshot(&bot_id, &content);
                        BotRpcResponse::MemoryUpdated {
                            content,
                            restored_facts: Vec::new(),
                            removed_count: 0,
                        }
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::RunMemoryTidy { bot_id } => {
                match memory_tidy::run_memory_tidy_tracked(
                    &self.store,
                    Some(&self.scheduler_state),
                    &bot_id,
                    ts,
                ) {
                    Ok((summary, changes)) => {
                        if let Ok(content) = self.store.get_memory(&bot_id) {
                            self.runtime.refresh_memory_snapshot(&bot_id, &content);
                        }
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
            BotRpcRequest::GetMemoryTidyStatus { bot_id } => {
                match memory_tidy::get_memory_tidy_status(
                    &self.store,
                    &self.scheduler_state,
                    &bot_id,
                ) {
                    Ok(status) => BotRpcResponse::MemoryTidyStatus(status),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::UndoMemoryTidy { bot_id, tidy_id } => {
                match memory_tidy::undo_memory_tidy(
                    &self.store,
                    &self.scheduler_state,
                    &bot_id,
                    tidy_id.as_deref(),
                    &memory_actor,
                    ts,
                ) {
                    Ok((content, restored_facts, removed_count)) => {
                        self.runtime.refresh_memory_snapshot(&bot_id, &content);
                        BotRpcResponse::MemoryUpdated {
                            content,
                            restored_facts,
                            removed_count,
                        }
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::ListMemoryTopics { bot_id } => {
                match self.store.list_memory_topics(&bot_id) {
                    Ok(topics) => BotRpcResponse::MemoryTopics(topics),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::GetMemoryTopic { bot_id, topic } => {
                match self.store.get_memory_topic(&bot_id, &topic) {
                    Ok(text) => BotRpcResponse::Text(text),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::SetMemoryTopic {
                bot_id,
                topic,
                content,
            } => match self.store.set_memory_topic(&bot_id, &topic, &content) {
                Ok(_) => BotRpcResponse::Ok,
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::RunLookAround { bot_id } => {
                match look_around::run_look_around_tracked(
                    &self.store,
                    &self.items,
                    Some(&self.scheduler_state),
                    &bot_id,
                    ts,
                ) {
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
            BotRpcRequest::ListSuggestions {
                bot_id,
                include_dismissed,
            } => {
                match look_around::list_suggestions(&self.store, bot_id.as_ref(), include_dismissed)
                {
                    Ok(list) => BotRpcResponse::Suggestions(list),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::DismissSuggestion {
                bot_id,
                suggestion_id,
            } => match look_around::dismiss_suggestion(
                &self.store,
                &self.scheduler_state,
                &bot_id,
                &suggestion_id,
                ts,
            ) {
                Ok(_) => BotRpcResponse::Ok,
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::StartSuggestion {
                bot_id,
                suggestion_id,
            } => {
                match look_around::preview_start_suggestion(&self.store, &bot_id, &suggestion_id) {
                    Ok((_sugg, prompt)) => match self.runtime.start_bot_session(
                        registry,
                        &self.store,
                        &self.secrets,
                        &self.tool_relay,
                        &self.composio_base_url,
                        &bot_id,
                        None,
                        Some(&prompt),
                        None,
                    ) {
                        Ok((session_id, pid)) => {
                            look_around::dismiss_suggestion(
                                &self.store,
                                &self.scheduler_state,
                                &bot_id,
                                &suggestion_id,
                                ts,
                            )
                            .ok();
                            BotRpcResponse::SessionStarted {
                                bot_id,
                                session_id,
                                pid,
                            }
                        }
                        Err(err) => err.into_rpc_response(),
                    },
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::MarkUpNextSeen { bot_id } => {
                match look_around::mark_up_next_seen(&self.scheduler_state, bot_id.as_ref(), ts) {
                    Ok(()) => BotRpcResponse::Ok,
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
            BotRpcRequest::ListItemsPage {
                bot_id,
                include_closed,
                include_snoozed,
                limit,
                cursor,
            } => match self.items.list_items_page(
                bot_id.as_ref(),
                include_closed,
                include_snoozed,
                limit,
                cursor.as_deref(),
                ts,
            ) {
                Ok(page) => BotRpcResponse::ItemsPage(page),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::GetBotItem { item_id } => {
                let effective_id = match &caller_bot {
                    Some((bot_id, _)) => qualify_bot_item_id(bot_id, &item_id),
                    None => item_id.clone(),
                };
                match self.items.get_item(&effective_id) {
                    Ok(Some(item)) => BotRpcResponse::Item(Box::new(item)),
                    Ok(None) => BotRpcResponse::Error(format!("bot item not found: {item_id}")),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::AnswerBotItem {
                item_id,
                answer_text,
                option_index,
                decision_id,
                request_id,
            } => match self.items.answer_item(
                &item_id,
                answer_text.as_deref(),
                option_index,
                decision_id.as_deref(),
                request_id.as_deref(),
                &default_actor,
                ts,
            ) {
                Ok(outcome) => {
                    if outcome.should_wake_bot
                        && let Ok(Some(bot)) = self.store.get_bot(&outcome.item.owner_bot_id)
                        && !bot.paused
                    {
                        let wake_prompt = format!(
                            "[Owner answered item #{}: {}]\nDecision: {}",
                            outcome.item.number, outcome.item.title, outcome.answer_summary
                        );
                        self.runtime
                            .deliver_or_wake_bot(
                                registry,
                                &self.store,
                                &self.secrets,
                                &self.tool_relay,
                                &self.composio_base_url,
                                &bot.id,
                                &wake_prompt,
                                ts,
                            )
                            .ok();
                    }
                    BotRpcResponse::Item(Box::new(outcome.item))
                }
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::MarkBotItemsOpened {
                bot_id,
                up_to_event_id,
            } => match self
                .items
                .mark_items_opened(&bot_id, up_to_event_id.as_deref(), ts)
            {
                Ok(()) => BotRpcResponse::Ok,
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::CreateItem(params) => {
                match self.items.create_item(&params, &default_actor, ts) {
                    Ok(item) => BotRpcResponse::Item(Box::new(item)),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::UpdateItem(mut params) => {
                if let Some((ref bot_id, _)) = caller_bot {
                    params.item_id = qualify_bot_item_id(bot_id, &params.item_id);
                }
                match self.items.update_item(&params, &default_actor, ts) {
                    Ok(item) => BotRpcResponse::Item(Box::new(item)),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::AddItemComment {
                item_id,
                author,
                text,
            } => {
                let effective_id = match &caller_bot {
                    Some((bot_id, _)) => qualify_bot_item_id(bot_id, &item_id),
                    None => item_id,
                };
                let actor = match caller_bot {
                    Some((bot_id, sid)) => BotItemActor::Bot {
                        bot_id,
                        session_id: Some(sid),
                    },
                    None => {
                        if author.eq_ignore_ascii_case("owner") || author.trim().is_empty() {
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
                        }
                    }
                };
                match self
                    .items
                    .add_comment(&effective_id, &actor, &text, &[], ts)
                {
                    Ok(item) => BotRpcResponse::Item(Box::new(item)),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::AddItemLink { item_id, link } => {
                let effective_id = match &caller_bot {
                    Some((bot_id, _)) => qualify_bot_item_id(bot_id, &item_id),
                    None => item_id,
                };
                match self
                    .items
                    .add_link(&effective_id, &link, &default_actor, ts)
                {
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
            BotRpcRequest::UpdateGroup {
                group_id,
                name,
                description,
                bot_ids,
            } => match GroupCoordinator::update_group(
                &self.store,
                &group_id,
                name.as_deref(),
                description,
                bot_ids,
                ts,
            ) {
                Ok(group) => {
                    self.groups.prune_audience_for_updated_group(&group);
                    BotRpcResponse::Group(group)
                }
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::DeleteGroup { group_id } => {
                match self.groups.delete_group(&self.store, &group_id) {
                    Ok(()) => BotRpcResponse::Ok,
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::ReorderGroups { group_ids } => {
                match GroupCoordinator::reorder_groups(&self.store, &group_ids) {
                    Ok(groups) => BotRpcResponse::Groups(groups),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::NewGroupThread { group_id, title } => {
                match self
                    .groups
                    .new_thread(&self.store, &group_id, title.as_deref(), ts)
                {
                    Ok(thread) => match self.groups.read_group(
                        &self.store,
                        &group_id,
                        Some(&thread.thread_id),
                        None,
                    ) {
                        Ok(view) => BotRpcResponse::GroupView(view),
                        Err(err) => BotRpcResponse::Error(err.to_string()),
                    },
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::ReadGroup {
                group_id,
                thread_id,
                limit,
            } => match self
                .groups
                .read_group(&self.store, &group_id, thread_id.as_deref(), limit)
            {
                Ok(view) => BotRpcResponse::GroupView(view),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::SendGroupMessage {
                group_id,
                thread_id,
                sender,
                text,
                reply_mode,
            } => {
                let effective_sender = if sender.trim().is_empty() {
                    caller_bot.as_ref().map_or("", |(bid, _)| bid.as_str())
                } else {
                    sender.as_str()
                };
                match self.groups.prepare_message_dispatch(
                    &self.store,
                    &group_id,
                    thread_id.as_deref(),
                    effective_sender,
                    &text,
                    reply_mode,
                    ts,
                ) {
                    Ok(plan) => self.execute_group_dispatch(registry, plan, ts),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::StopGroup {
                group_id,
                thread_id,
            } => match self
                .groups
                .stop_group(&self.store, &group_id, thread_id.as_deref())
            {
                Ok(stopped) => {
                    for (bid, sid) in stopped {
                        self.runtime
                            .stop_session(registry, &self.tool_relay, &bid, sid)
                            .ok();
                    }
                    BotRpcResponse::Ok
                }
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::ListGroupSummaries => {
                match self.groups.list_group_summaries(&self.store) {
                    Ok(summaries) => BotRpcResponse::GroupSummaries(summaries),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::ReadGroupRoom {
                group_id,
                thread_id,
                before_message_id,
                limit,
            } => match self.groups.read_group_room(
                &self.store,
                &group_id,
                thread_id.as_deref(),
                before_message_id.as_deref(),
                limit,
            ) {
                Ok(room) => BotRpcResponse::GroupRoom(room),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::SetGroupReplyAudience { group_id, audience } => {
                match self
                    .groups
                    .set_reply_audience(&self.store, &group_id, audience)
                {
                    Ok(saved) => BotRpcResponse::GroupReplyAudienceUpdated {
                        group_id,
                        audience: saved,
                    },
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::SendGroupMessageWithAudience {
                group_id,
                thread_id,
                sender,
                text,
                audience,
            } => {
                let effective_sender = if sender.trim().is_empty() {
                    caller_bot.as_ref().map_or("", |(bid, _)| bid.as_str())
                } else {
                    sender.as_str()
                };
                match self.groups.prepare_message_dispatch_with_audience(
                    &self.store,
                    &group_id,
                    thread_id.as_deref(),
                    effective_sender,
                    &text,
                    audience,
                    ts,
                ) {
                    Ok(plan) => self.execute_group_dispatch(registry, plan, ts),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::StopGroupMember {
                group_id,
                thread_id,
                bot_id,
            } => match self.groups.stop_group_member(
                &self.store,
                &group_id,
                thread_id.as_deref(),
                bot_id.as_ref(),
            ) {
                Ok(stopped) => {
                    for (bid, sid) in stopped {
                        self.runtime
                            .stop_session(registry, &self.tool_relay, &bid, sid)
                            .ok();
                    }
                    BotRpcResponse::Ok
                }
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
                &self.secrets,
                &self.tool_relay,
                &self.composio_base_url,
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
                // Validate bot and link BEFORE initializing the lazy ConnectorManager threads.
                if let Err(err) = validate_reply_target(
                    &self.store,
                    &self.runtime,
                    peer_session,
                    &bot_id,
                    link_id.as_deref(),
                ) {
                    return BotRpcResponse::Error(err);
                }
                let mgr = self.connector_manager();
                match self.runtime.post_bot_reply_for_session(
                    &self.store,
                    mgr,
                    peer_session,
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
                    Ok(()) => {
                        self.reconcile_enabled_workers();
                        self.reconcile_all_lazy_workers();
                        BotRpcResponse::Ok
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::DeleteSecret { secret_ref } => {
                match self.secrets.delete_ref(&secret_ref) {
                    Ok(_) => {
                        self.reconcile_all_lazy_workers();
                        BotRpcResponse::Ok
                    }
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
                    Ok(reqs) => BotRpcResponse::AccessRequests(if is_observer {
                        store::redact_access_requests_for_observer(reqs)
                    } else {
                        reqs
                    }),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::DecideAccessRequest { token, approve } => {
                match self.store.decide_access_request(&token, approve, ts) {
                    Ok(decided) => {
                        if let Some(mgr) = self.connectors.get() {
                            if decided.status == BotAccessRequestStatus::Approved {
                                mgr.on_access_approved(&decided);
                            } else {
                                mgr.discard_held_message(&decided.token);
                            }
                        }
                        BotRpcResponse::AccessRequestDecided(decided)
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::RedeemPairingCode { code } => {
                match self.store.redeem_pairing_code(&code, ts) {
                    Ok(decided) => {
                        if let Some(mgr) = self.connectors.get()
                            && decided.status == BotAccessRequestStatus::Approved
                        {
                            mgr.on_access_approved(&decided);
                        }
                        BotRpcResponse::AccessRequestDecided(decided)
                    }
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
            } => {
                if let Some(ref raw_agent) = agent_override
                    && !raw_agent.trim().is_empty()
                    && let Err(err) = validate_agent_profile(registry, raw_agent)
                {
                    return BotRpcResponse::Error(err);
                }
                match self.store.import_bot_zip(
                    &zip_bytes,
                    name_override.as_deref(),
                    agent_override.as_deref(),
                    ts,
                ) {
                    Ok(bot) => {
                        self.reconcile_for_bot(&bot);
                        BotRpcResponse::Bot(Box::new(bot))
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::ImportBotZipV2 {
                zip_bytes,
                slug_override,
                name_override,
                agent_override,
            } => {
                if let Some(ref raw_agent) = agent_override
                    && !raw_agent.trim().is_empty()
                    && let Err(err) = validate_agent_profile(registry, raw_agent)
                {
                    return BotRpcResponse::Error(err);
                }
                match self.store.import_bot_zip_v2(
                    &zip_bytes,
                    slug_override.as_ref().map(BotSlug::as_str),
                    name_override.as_deref(),
                    agent_override.as_deref(),
                    ts,
                ) {
                    Ok(bot) => {
                        self.reconcile_for_bot(&bot);
                        BotRpcResponse::Bot(Box::new(bot))
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::UploadBotAvatar { bot_id, png_bytes } => {
                match self.store.upload_avatar(&bot_id, &png_bytes, ts) {
                    Ok(data) => BotRpcResponse::BotAvatar(data),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::GetBotAvatar { bot_id } => match self.store.get_avatar(&bot_id) {
                Ok(data) => BotRpcResponse::BotAvatar(data),
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
                    Ok((candidates, imported)) => {
                        if !imported.is_empty() {
                            for bot in &imported {
                                self.reconcile_for_bot(bot);
                            }
                            self.reconcile_enabled_workers();
                        }
                        BotRpcResponse::ImportedExternalBots {
                            candidates,
                            imported,
                        }
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::SetDefaultTrackerBot { source, bot_id } => {
                match self.store.set_default_tracker_bot(source, bot_id) {
                    Ok(defaults) => {
                        self.reconcile_enabled_workers();
                        BotRpcResponse::DefaultTrackerBots(defaults)
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::GetBotUpcoming { bot_id } => match self.store.get_bot(&bot_id) {
                Ok(Some(bot)) => {
                    let extras = self.store.settings_extras(&bot_id).unwrap_or_default();
                    BotRpcResponse::Upcoming(krill_proto::bots::BotUpcoming {
                        items_seen_event_id: self.items.items_seen_event_id(&bot_id).unwrap_or(0),
                        ..look_around::bot_upcoming_with_extras(
                            &bot,
                            extras,
                            &self.scheduler_state,
                            |id| self.schedule_running(registry, &bot_id, id),
                            ts,
                        )
                    })
                }
                Ok(None) => BotRpcResponse::Error(format!("bot not found: {}", bot_id.as_str())),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::RunScheduleNow {
                bot_id,
                schedule_id,
            } => match self.store.get_bot(&bot_id) {
                Ok(Some(bot)) => match bot.schedules.iter().find(|s| s.id == schedule_id) {
                    Some(sched) if self.schedule_running(registry, &bot_id, &schedule_id) => {
                        BotRpcResponse::Error(format!("{} is already running", sched.name))
                    }
                    Some(sched) => match self.run_schedule(registry, &bot_id, sched, None) {
                        Ok(()) => BotRpcResponse::Ok,
                        Err(err) => err.into_rpc_response(),
                    },
                    None => BotRpcResponse::Error(format!("schedule not found: {schedule_id}")),
                },
                Ok(None) => BotRpcResponse::Error(format!("bot not found: {}", bot_id.as_str())),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::OfferBot {
                bot_id,
                name,
                purpose,
            } => match self.store.offer_bot(
                &bot_id,
                caller_bot.as_ref().map(|(_, sid)| sid.0),
                &name,
                &purpose,
                ts,
            ) {
                Ok(_) => BotRpcResponse::Text(format!(
                    "Your owner now sees a card proposing {}. Nothing is created until they press Create bot; if they create it, it starts with this purpose in its SOUL.md.",
                    name.trim()
                )),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::ListBotOffers { bot_id } => match self.store.list_bot_offers(&bot_id) {
                Ok(offers) => BotRpcResponse::BotOffers(offers),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::ResolveBotOffer { offer_id, created } => {
                match self.store.resolve_bot_offer(&offer_id, created) {
                    Ok(()) => BotRpcResponse::Ok,
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::GetDefaultTrackerBots => match self.store.get_default_tracker_bots() {
                Ok(defaults) => BotRpcResponse::DefaultTrackerBots(defaults),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::ListAgentModels { agent_id } => {
                let Some(home) = krill_proto::os::home_dir() else {
                    return BotRpcResponse::Error("home directory unavailable".to_owned());
                };
                match registry.agent_control().agents() {
                    Ok(agents) => BotRpcResponse::AgentModels(crate::mobile::bot_launch_models(
                        &home, &agents, &agent_id,
                    )),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::GetBotSettingsExtras { bot_id } => {
                match self.store.settings_extras(&bot_id) {
                    Ok(extras) => BotRpcResponse::BotSettingsExtras(extras),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::UpdateBotSettingsExtras {
                bot_id,
                items_stale,
                look_around_every,
                tidy_every,
            } => match self.store.update_settings_extras(
                &bot_id,
                items_stale,
                look_around_every,
                tidy_every,
            ) {
                Ok(extras) => BotRpcResponse::BotSettingsExtras(extras),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::RemoveBotMember { bot_id, member } => {
                match self.store.remove_member(&bot_id, &member, now_ms()) {
                    Ok(bot) => BotRpcResponse::Bot(Box::new(bot)),
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::ListImportedSources => match self.store.list_imported_sources() {
                Ok(sources) => BotRpcResponse::ImportedSources(sources),
                Err(err) => BotRpcResponse::Error(err.to_string()),
            },
            BotRpcRequest::ImportExternalBotCopy {
                source,
                path,
                mut params,
            } => {
                if !params.base.agent_id.trim().is_empty()
                    && let Err(err) = validate_agent_profile(registry, &params.base.agent_id)
                {
                    return BotRpcResponse::Error(err);
                }
                if krill_proto::agent::canonical_id(params.base.agent_id.trim()) == "agy"
                    && let Some(ref model) = params.base.model
                    && let Some((family, eff)) = crate::mobile::fold_agy_model_selection(
                        None,
                        model,
                        params.base.effort.as_deref(),
                    )
                {
                    params.base.model = Some(family);
                    params.base.effort = Some(eff);
                }
                match self
                    .store
                    .import_external_bot_copy(source, &path, *params, ts)
                {
                    Ok(bot) => {
                        self.reconcile_for_bot(&bot);
                        BotRpcResponse::Bot(Box::new(bot))
                    }
                    Err(err) => BotRpcResponse::Error(err.to_string()),
                }
            }
            BotRpcRequest::TestConnector { bot_id, source } => {
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
                let result = match source {
                    ExternalSource::Telegram | ExternalSource::Slack | ExternalSource::Discord => {
                        self.connector_manager().test_connector(&bot, source)
                    }
                    ExternalSource::Google => {
                        google::test_google(&bot, &self.secrets, &self.google_endpoints, ts)
                    }
                    ExternalSource::GitHub => {
                        trackers::test_github(&bot, &self.secrets, &self.tracker_endpoints)
                    }
                    ExternalSource::Linear => {
                        trackers::test_linear(&bot, &self.secrets, &self.tracker_endpoints)
                    }
                    ExternalSource::Composio => {
                        composio::test_composio(&bot, &self.secrets, &self.composio_base_url)
                    }
                };
                BotRpcResponse::ConnectorTest(result)
            }
            BotRpcRequest::StartMcpOAuth { bot_id, tool_id } => {
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
                match self.mcp_oauth_mgr().start_oauth(&bot, &tool_id) {
                    Ok(st) => BotRpcResponse::McpOAuth(st),
                    Err(err) => BotRpcResponse::Error(err),
                }
            }
            BotRpcRequest::McpOAuthStatus { bot_id, tool_id } => {
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
                BotRpcResponse::McpOAuth(self.mcp_oauth_mgr().status(&bot, &tool_id))
            }
            BotRpcRequest::CancelMcpOAuth { bot_id, tool_id } => {
                match self.store.get_bot(&bot_id) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return BotRpcResponse::Error(format!(
                            "bot not found: {}",
                            bot_id.as_str()
                        ));
                    }
                    Err(err) => return BotRpcResponse::Error(err.to_string()),
                }
                BotRpcResponse::McpOAuth(self.mcp_oauth_mgr().cancel(&bot_id, &tool_id))
            }
            BotRpcRequest::ListComposioConnections { bot_id } => {
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
                match composio::list_connected_accounts(
                    &bot,
                    &self.secrets,
                    &self.composio_base_url,
                ) {
                    Ok(conns) => BotRpcResponse::ComposioConnections(conns),
                    Err(detail) => BotRpcResponse::Error(detail.message),
                }
            }
            BotRpcRequest::StartComposioConnect { bot_id, app_id } => {
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
                match composio::start_composio_connect(
                    &bot,
                    &self.secrets,
                    &self.tool_relay,
                    Some(self.store.root()),
                    &self.composio_base_url,
                    &app_id,
                    ts,
                ) {
                    Ok((connection_id, redirect_url)) => BotRpcResponse::ComposioConnectStarted {
                        connection_id,
                        redirect_url,
                    },
                    Err(detail) => BotRpcResponse::Error(detail.message),
                }
            }
            BotRpcRequest::DisconnectComposioAccount {
                bot_id,
                connection_id,
            } => {
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
                match composio::disconnect_composio_account(
                    &bot,
                    &self.secrets,
                    &self.composio_base_url,
                    &connection_id,
                ) {
                    Ok(()) => BotRpcResponse::Ok,
                    Err(detail) => BotRpcResponse::Error(detail.message),
                }
            }
        }
    }

    fn execute_group_dispatch(
        &self,
        registry: &Registry,
        plan: groups::GroupDispatchPlan,
        ts: u64,
    ) -> BotRpcResponse {
        let mut dispatched_bots = Vec::new();
        let mut failures = Vec::new();
        for target in &plan.target_bots {
            let registered = std::cell::Cell::new(None::<(SessionId, u64)>);
            let mut on_bound = |sid: SessionId, turn_seq: u64| {
                if registered.get().is_some() {
                    return;
                }
                self.groups
                    .record_turn_started_seq(
                        &plan.group.id,
                        &plan.thread.thread_id,
                        target,
                        sid,
                        Some(turn_seq),
                        ts,
                    )
                    .ok();
                registered.set(Some((sid, turn_seq)));
            };
            let mut on_rollback = |sid: SessionId, turn_seq: u64| {
                if registered.get() == Some((sid, turn_seq)) {
                    registered.set(None);
                    self.groups
                        .on_bot_turn_finished_seq(&target.id, sid, Some(turn_seq), "", ts)
                        .ok();
                }
            };
            match self.runtime.deliver_group_turn_with_hooks(
                registry,
                &self.store,
                &self.secrets,
                &self.tool_relay,
                &self.composio_base_url,
                &target.id,
                &plan.group.id,
                &plan.thread.thread_id,
                &plan.prompt_for_bot,
                ts,
                Some(&mut on_bound),
                Some(&mut on_rollback),
            ) {
                Ok((sid, turn_seq, _)) => {
                    if registered.get().is_none() {
                        on_bound(sid, turn_seq);
                    }
                    dispatched_bots.push(target.id.clone());
                }
                Err(err) => {
                    if let Some((sid, turn_seq)) = registered.get() {
                        on_rollback(sid, turn_seq);
                    }
                    tracing::warn!(
                        group = %plan.group.id,
                        bot = %target.id.as_str(),
                        "group dispatch failed for target bot"
                    );
                    self.groups
                        .record_dispatch_failure(
                            &plan.group.id,
                            &plan.thread.thread_id,
                            target,
                            &err,
                            ts,
                        )
                        .ok();
                    failures.push(format!("{}: {err}", target.id.as_str()));
                }
            }
        }
        if !plan.target_bots.is_empty() && dispatched_bots.is_empty() {
            return BotRpcResponse::Error(format!(
                "group dispatch failed: {}",
                failures.join("; ")
            ));
        }
        BotRpcResponse::GroupMessagePosted {
            message: plan.posted_message,
            dispatched_bots,
        }
    }

    fn reconcile_enabled_workers(&self) {
        if let Ok(bots) = self.store.list_bots() {
            self.reconcile_enabled_workers_for(&bots);
        }
    }

    fn reconcile_enabled_workers_for(&self, bots: &[BotConfig]) {
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
                    || b.connectors.discord.enabled
                    || b.connectors.github.enabled
                    || b.connectors.linear.enabled)
        });
        if any_chat_conn && self.connectors.get().is_none() {
            self.connector_manager().reload(None).ok();
        }
    }

    fn reconcile_for_bot(&self, bot: &BotConfig) {
        if bot.connectors.telegram.enabled
            || bot.connectors.slack.enabled
            || bot.connectors.discord.enabled
            || bot.connectors.github.enabled
            || bot.connectors.linear.enabled
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
            lock(&self.composio_last_poll_ms).remove(&bot.id);
            composio::cleanup_composio_bot(&bot.id, Some(self.store.root()), &self.tool_relay);
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
                || b.connectors.github.enabled
                || b.connectors.linear.enabled
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
                let st = composio::sync_composio_bot_with_root(
                    bot,
                    &self.secrets,
                    &self.tool_relay,
                    Some(self.store.root()),
                    &self.composio_base_url,
                    ts,
                );
                lock(&self.composio_statuses).insert(bot.id.clone(), st);
            } else {
                lock(&self.composio_statuses).remove(&bot.id);
                composio::cleanup_composio_bot(&bot.id, Some(self.store.root()), &self.tool_relay);
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
            let st = composio::sync_composio_bot_with_root(
                bot,
                &self.secrets,
                &self.tool_relay,
                Some(self.store.root()),
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

/// Authorizes a request coming from inside a live bot session (`Scope::Session(bot_sid)`).
/// Enforces owner-only secrets/bot-management isolation and restricts self-targeted mutations.
fn authorize_bot_session_rpc(
    store: &BotStore,
    items: &BotItemsStore,
    caller_bot: &BotId,
    req: &BotRpcRequest,
) -> Result<(), String> {
    let deny = |why: &str| Err(format!("permission denied for bot session: {why}"));
    match req {
        // Safe reads:
        BotRpcRequest::ListBots
        | BotRpcRequest::GetBot { .. }
        | BotRpcRequest::GetSoul { .. }
        | BotRpcRequest::GetMemory { .. }
        | BotRpcRequest::GetMemoryHistory { .. }
        | BotRpcRequest::GetMemoryTidyStatus { .. }
        | BotRpcRequest::ListMemoryTopics { .. }
        | BotRpcRequest::GetMemoryTopic { .. }
        | BotRpcRequest::ListSuggestions { .. }
        | BotRpcRequest::ListItems { .. }
        | BotRpcRequest::ListItemsPage { .. }
        | BotRpcRequest::GetBotItem { .. }
        | BotRpcRequest::ListGroups
        | BotRpcRequest::ReadGroup { .. }
        | BotRpcRequest::ListGroupSummaries
        | BotRpcRequest::ReadGroupRoom { .. }
        | BotRpcRequest::ConnectorStatus { .. }
        | BotRpcRequest::ToolRelayStatus
        | BotRpcRequest::ListBotSessions { .. }
        | BotRpcRequest::GetBotAvatar { .. }
        | BotRpcRequest::GetDefaultTrackerBots
        | BotRpcRequest::ListAgentModels { .. }
        | BotRpcRequest::GetBotUpcoming { .. }
        | BotRpcRequest::GetBotSettingsExtras { .. }
        | BotRpcRequest::ListBotOffers { .. }
        | BotRpcRequest::ListImportedSources => Ok(()),
        BotRpcRequest::SendBotMail { from, .. } => {
            let trimmed = from.trim();
            if trimmed == caller_bot.as_str() || trimmed == format!("bot:{}", caller_bot.as_str()) {
                Ok(())
            } else {
                deny("bot cannot spoof SendBotMail sender")
            }
        }
        // A bot proposes bots only from its own chat; the owner answers them.
        BotRpcRequest::OfferBot { bot_id, .. } if bot_id == caller_bot => Ok(()),
        BotRpcRequest::OfferBot { .. } => deny("cannot propose a bot for another bot"),
        BotRpcRequest::SendGroupMessage {
            group_id, sender, ..
        }
        | BotRpcRequest::SendGroupMessageWithAudience {
            group_id, sender, ..
        } => {
            let group = store
                .get_group(group_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("group not found: {group_id}"))?;
            if !group.bot_ids.contains(caller_bot) {
                return deny("bot is not a member of target group");
            }
            let trimmed = sender.trim();
            if trimmed.is_empty() {
                return deny("bot group message sender cannot be empty");
            }
            if trimmed == caller_bot.as_str() || trimmed == format!("bot:{}", caller_bot.as_str()) {
                Ok(())
            } else {
                deny("bot cannot impersonate another group sender")
            }
        }

        // Self-scoped safe actions:
        BotRpcRequest::SetSoul { bot_id, .. }
        | BotRpcRequest::ForgetMemoryFact { bot_id, .. }
        | BotRpcRequest::EditMemoryFact { bot_id, .. }
        | BotRpcRequest::UndoMemoryChange { bot_id, .. }
        | BotRpcRequest::RunMemoryTidy { bot_id }
        | BotRpcRequest::UndoMemoryTidy { bot_id, .. }
        | BotRpcRequest::SetMemoryTopic { bot_id, .. }
        | BotRpcRequest::RunLookAround { bot_id }
        | BotRpcRequest::RunScheduleNow { bot_id, .. }
        | BotRpcRequest::DismissSuggestion { bot_id, .. }
        | BotRpcRequest::MarkBotItemsOpened { bot_id, .. }
        | BotRpcRequest::PostBotReply { bot_id, .. } => {
            if bot_id == caller_bot {
                Ok(())
            } else {
                deny("cannot modify another bot's resources")
            }
        }
        BotRpcRequest::UpdateMemory {
            bot_id,
            allow_deletions,
            ..
        } => {
            if *allow_deletions {
                deny("bots cannot bypass MemoryGuard allow_deletions")
            } else if bot_id != caller_bot {
                deny("cannot update another bot's memory")
            } else {
                Ok(())
            }
        }
        BotRpcRequest::MarkUpNextSeen { bot_id } => match bot_id {
            Some(id) if id == caller_bot => Ok(()),
            _ => deny("cannot mark another bot's Up Next state"),
        },
        BotRpcRequest::CreateItem(params) => {
            if &params.bot_id == caller_bot {
                Ok(())
            } else {
                deny("cannot create items for another bot")
            }
        }
        BotRpcRequest::UpdateItem(params) => {
            ensure_bot_owns_item(items, caller_bot, &params.item_id)
        }
        // A bot manages its own schedules (Orca `automations create --bot <self>`). A schedule only
        // wakes the bot with a prompt in its own session, so it grants nothing the bot lacks.
        BotRpcRequest::UpdateBot(params) if params.schedules.is_some() => {
            if &params.bot_id != caller_bot {
                deny("cannot modify another bot's schedules")
            } else if updates_only_schedules(params) {
                Ok(())
            } else {
                deny("owner-only bot RPC")
            }
        }
        BotRpcRequest::AddItemComment { item_id, .. }
        | BotRpcRequest::AddItemLink { item_id, .. } => {
            ensure_bot_owns_item(items, caller_bot, item_id)
        }

        // Owner-only RPCs denied to bot sessions:
        BotRpcRequest::CreateBot(_)
        | BotRpcRequest::CreateBotV2(_)
        | BotRpcRequest::UpdateBot(_)
        | BotRpcRequest::UpdateBotExtras(_)
        | BotRpcRequest::DeleteBot { .. }
        | BotRpcRequest::StartBotSession { .. }
        | BotRpcRequest::StartSuggestion { .. }
        | BotRpcRequest::ResetBotChat { .. }
        | BotRpcRequest::StopBotSession { .. }
        | BotRpcRequest::RetryBotKickoff { .. }
        | BotRpcRequest::TakeOverWorkspace { .. }
        | BotRpcRequest::SendBotRunAgain { .. }
        | BotRpcRequest::AnswerBotItem { .. }
        | BotRpcRequest::CreateGroup { .. }
        | BotRpcRequest::UpdateGroup { .. }
        | BotRpcRequest::DeleteGroup { .. }
        | BotRpcRequest::ReorderGroups { .. }
        | BotRpcRequest::NewGroupThread { .. }
        | BotRpcRequest::StopGroup { .. }
        | BotRpcRequest::SetGroupReplyAudience { .. }
        | BotRpcRequest::StopGroupMember { .. }
        | BotRpcRequest::SetConnectorConfig { .. }
        | BotRpcRequest::SetSecret { .. }
        | BotRpcRequest::DeleteSecret { .. }
        | BotRpcRequest::ReloadConnectors { .. }
        | BotRpcRequest::ListAccessRequests { .. }
        | BotRpcRequest::DecideAccessRequest { .. }
        | BotRpcRequest::RedeemPairingCode { .. }
        | BotRpcRequest::StartGoogleOAuth { .. }
        | BotRpcRequest::TriggerSyncNow { .. }
        | BotRpcRequest::ExportBotZip { .. }
        | BotRpcRequest::ImportBotZip { .. }
        | BotRpcRequest::ImportBotZipV2 { .. }
        | BotRpcRequest::UploadBotAvatar { .. }
        | BotRpcRequest::ImportExternalBots { .. }
        | BotRpcRequest::ImportExternalBotCopy { .. }
        | BotRpcRequest::SetDefaultTrackerBot { .. }
        | BotRpcRequest::TestConnector { .. }
        | BotRpcRequest::StartMcpOAuth { .. }
        | BotRpcRequest::McpOAuthStatus { .. }
        | BotRpcRequest::CancelMcpOAuth { .. }
        | BotRpcRequest::ListComposioConnections { .. }
        | BotRpcRequest::StartComposioConnect { .. }
        | BotRpcRequest::DisconnectComposioAccount { .. }
        | BotRpcRequest::UpdateBotSettingsExtras { .. }
        | BotRpcRequest::RemoveBotMember { .. }
        | BotRpcRequest::ResolveBotOffer { .. } => deny("owner-only bot RPC"),
    }
}

/// True when an `UpdateBot` changes nothing but `schedules`. Destructured so a new field must be classified here.
fn updates_only_schedules(params: &UpdateBotParams) -> bool {
    let UpdateBotParams {
        bot_id: _,
        schedules: _,
        name,
        description,
        emoji,
        avatar,
        agent_id,
        model,
        effort,
        permission_mode,
        workspace,
        paused,
        first_run_completed,
        notifications,
        look_around,
        memory_tidy,
        mcp_tools,
    } = params;
    name.is_none()
        && description.is_none()
        && emoji.is_unchanged()
        && avatar.is_unchanged()
        && agent_id.is_none()
        && model.is_unchanged()
        && effort.is_unchanged()
        && permission_mode.is_none()
        && workspace.is_unchanged()
        && paused.is_none()
        && first_run_completed.is_none()
        && notifications.is_none()
        && look_around.is_none()
        && memory_tidy.is_none()
        && mcp_tools.is_none()
}

fn qualify_bot_item_id(bot_id: &BotId, raw: &str) -> String {
    let trimmed = raw.trim();
    let num_str = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if !num_str.is_empty() && num_str.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}#{num_str}", bot_id.as_str())
    } else {
        trimmed.to_owned()
    }
}

fn ensure_bot_owns_item(
    items: &BotItemsStore,
    caller_bot: &BotId,
    item_id: &str,
) -> Result<(), String> {
    let effective_id = qualify_bot_item_id(caller_bot, item_id);
    match items.get_item(&effective_id) {
        Ok(Some(item)) if &item.owner_bot_id == caller_bot => Ok(()),
        Ok(Some(_)) => Err("permission denied: item owned by another bot".to_owned()),
        Ok(None) => Err(format!("bot item not found: {item_id}")),
        Err(err) => Err(err.to_string()),
    }
}

fn validate_reply_target(
    store: &BotStore,
    runtime: &BotRuntime,
    caller_session: Option<SessionId>,
    bot_id: &BotId,
    explicit_link_id: Option<&str>,
) -> Result<(), String> {
    match store.get_bot(bot_id) {
        Ok(Some(_)) => {}
        Ok(None) => return Err(format!("bot not found: {}", bot_id.as_str())),
        Err(err) => return Err(err.to_string()),
    }
    let session_link_id = if explicit_link_id.is_none() {
        caller_session.and_then(|sid| runtime.session_link_for_bot(sid, bot_id))
    } else {
        None
    };
    let resolved_link_id = explicit_link_id.or(session_link_id.as_deref());
    if let Some(lid) = resolved_link_id {
        match store.get_link(lid) {
            Ok(Some(l)) if l.bot_id == *bot_id => Ok(()),
            Ok(Some(_)) => Err(format!(
                "link '{lid}' does not belong to bot {}",
                bot_id.as_str()
            )),
            Ok(None) => Err(format!("link not found: {lid}")),
            Err(err) => Err(err.to_string()),
        }
    } else {
        match store.latest_link_for_bot(bot_id) {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(format!(
                "no external conversation link found for bot {}",
                bot_id.as_str()
            )),
            Err(err) => Err(err.to_string()),
        }
    }
}

/// Concrete [`ConnectorBridge`] implementation wiring channel workers into `BotsManager` + `Registry`.
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

    fn dispatch_external_message_with_hooks(
        &self,
        msg: InboundExternalMessage,
        on_bound: &mut dyn FnMut(SessionId, Option<u64>),
        on_rollback: &mut dyn FnMut(SessionId, Option<u64>),
    ) -> io::Result<DispatchOutcome> {
        let mgr = self
            .manager
            .upgrade()
            .ok_or_else(|| io::Error::other("bots manager shut down"))?;
        let reg = self
            .registry
            .upgrade()
            .ok_or_else(|| io::Error::other("daemon registry unavailable"))?;
        let (link, session_id, started_new_session) =
            mgr.runtime.dispatch_external_message_with_hooks(
                &reg,
                &mgr.store,
                &mgr.secrets,
                &mgr.tool_relay,
                &mgr.composio_base_url,
                &msg,
                now_ms(),
                Some(on_bound),
                Some(on_rollback),
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
        self.check_or_request_access_with_workspace(
            bot_id,
            source,
            None,
            sender_id,
            sender_name,
            channel_id,
        )
    }

    fn check_or_request_access_with_workspace(
        &self,
        bot_id: &BotId,
        source: ExternalSource,
        workspace_id: Option<&str>,
        sender_id: &str,
        sender_name: &str,
        channel_id: &str,
    ) -> io::Result<AccessDecision> {
        let mgr = self
            .manager
            .upgrade()
            .ok_or_else(|| io::Error::other("bots manager shut down"))?;
        mgr.store.check_or_request_access_with_workspace(
            bot_id,
            source,
            workspace_id,
            sender_id,
            sender_name,
            channel_id,
            now_ms(),
        )
    }

    fn bot_home_dir(&self, bot_id: &BotId) -> Option<PathBuf> {
        self.manager.upgrade().and_then(|mgr| {
            mgr.store
                .get_bot(bot_id)
                .ok()
                .flatten()
                .map(|b| mgr.store.bot_home(&b))
        })
    }

    fn connector_state_root(&self) -> Option<PathBuf> {
        self.manager
            .upgrade()
            .map(|mgr| mgr.store.root().to_path_buf())
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

    fn get_link(&self, link_id: &str) -> io::Result<Option<ExternalConversationLink>> {
        let mgr = self
            .manager
            .upgrade()
            .ok_or_else(|| io::Error::other("bots manager shut down"))?;
        mgr.store.get_link(link_id)
    }

    fn resume_failed_external_turn(
        &self,
        bot_id: &BotId,
        link_id: &str,
        turn_seq: u64,
        _sender_id: &str,
        _sender_name: &str,
    ) -> io::Result<ExternalResumeResult> {
        let mgr = self
            .manager
            .upgrade()
            .ok_or_else(|| io::Error::other("bots manager shut down"))?;
        let reg = self
            .registry
            .upgrade()
            .ok_or_else(|| io::Error::other("daemon registry unavailable"))?;
        mgr.runtime.resume_failed_external_turn(
            &reg,
            &mgr.store,
            &mgr.secrets,
            &mgr.tool_relay,
            &mgr.composio_base_url,
            bot_id,
            link_id,
            turn_seq,
        )
    }
}

struct DaemonBuiltinMcpHandler {
    registry: Weak<Registry>,
    manager: Weak<BotsManager>,
}

impl BuiltinMcpHandler for DaemonBuiltinMcpHandler {
    fn call_tool(
        &self,
        ctx: &BuiltinMcpCallContext,
        tool_name: &str,
        arguments: &Value,
    ) -> Result<String, String> {
        let mgr = self
            .manager
            .upgrade()
            .ok_or_else(|| "bots manager shut down".to_owned())?;
        let reg = self
            .registry
            .upgrade()
            .ok_or_else(|| "daemon registry unavailable".to_owned())?;
        let ts = now_ms();
        let actor = BotItemActor::Bot {
            bot_id: ctx.bot_id.clone(),
            session_id: ctx.session_id,
        };
        let memory_actor = format!("bot:{}", ctx.bot_id.as_str());

        match tool_name {
            "items_list" => {
                let include_closed = arguments
                    .get("include_closed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let include_snoozed = arguments
                    .get("include_snoozed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(50);
                let cursor = arguments.get("cursor").and_then(Value::as_str);
                let page = mgr
                    .items
                    .list_items_page(
                        Some(&ctx.bot_id),
                        include_closed,
                        include_snoozed,
                        Some(limit),
                        cursor,
                        ts,
                    )
                    .map_err(|e| e.to_string())?;
                serde_json::to_string(&page).map_err(|e| e.to_string())
            }
            "items_create" => {
                let title = arguments
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "missing required 'title'".to_owned())?
                    .to_owned();
                let status = parse_optional_mcp_status(arguments)?;
                let item = mgr
                    .items
                    .create_item(
                        &CreateBotItemParams {
                            bot_id: ctx.bot_id.clone(),
                            title,
                            description: arguments
                                .get("description")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            done_means: arguments
                                .get("done_means")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            status,
                            links: Vec::new(),
                        },
                        &actor,
                        ts,
                    )
                    .map_err(|e| e.to_string())?;
                reg.notify_orch();
                serde_json::to_string(&item).map_err(|e| e.to_string())
            }
            "items_update" => {
                let raw_item_id = arguments
                    .get("item_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "missing required 'item_id'".to_owned())?;
                let item_id = qualify_bot_item_id(&ctx.bot_id, raw_item_id);
                ensure_bot_owns_item(&mgr.items, &ctx.bot_id, &item_id)?;
                let status = parse_optional_mcp_status(arguments)?;
                let item = mgr
                    .items
                    .update_item(
                        &UpdateBotItemParams {
                            item_id,
                            title: arguments
                                .get("title")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            description: arguments
                                .get("description")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            done_means: arguments
                                .get("done_means")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            status,
                            comment: arguments
                                .get("comment")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            decisions: Vec::new(),
                            close_note: arguments
                                .get("close_note")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            snoozed_until_ms: FieldPatch::Unchanged,
                        },
                        &actor,
                        ts,
                    )
                    .map_err(|e| e.to_string())?;
                reg.notify_orch();
                serde_json::to_string(&item).map_err(|e| e.to_string())
            }
            "memory_get" => {
                let content = mgr
                    .store
                    .get_memory(&ctx.bot_id)
                    .map_err(|e| e.to_string())?;
                serde_json::to_string(&json!({ "content": content })).map_err(|e| e.to_string())
            }
            "memory_update" => {
                let content = arguments
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing required 'content'".to_owned())?;
                if arguments
                    .get("allow_deletions")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return Err(
                        "permission denied for bot session: bots cannot bypass MemoryGuard allow_deletions"
                            .to_owned(),
                    );
                }
                mgr.store
                    .apply_memory_with_history(
                        &ctx.bot_id,
                        content,
                        false,
                        &memory_actor,
                        "mcp_update",
                        ts,
                    )
                    .map_err(|e| match e {
                        MemoryUpdateError::DeletionBlocked(v) => {
                            format!("MemoryDeletionViolation: {}", v.deleted_facts.join("; "))
                        }
                        MemoryUpdateError::Io(io_e) => io_e.to_string(),
                    })?;
                Ok(json!({ "ok": true }).to_string())
            }
            "memory_forget" => {
                let fact = arguments
                    .get("fact")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing required 'fact'".to_owned())?;
                let memory_actor = format!("bot:{}", ctx.bot_id.as_str());
                let (content, removed_count) = mgr
                    .store
                    .forget_memory_fact(&ctx.bot_id, fact, &memory_actor)
                    .map_err(|e| e.to_string())?;
                mgr.runtime.refresh_memory_snapshot(&ctx.bot_id, &content);
                Ok(json!({ "removed_count": removed_count, "content": content }).to_string())
            }
            "memory_history" => {
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .and_then(|v| u32::try_from(v).ok());
                let entries = mgr
                    .store
                    .get_memory_history(&ctx.bot_id, limit)
                    .map_err(|e| e.to_string())?;
                serde_json::to_string(&entries).map_err(|e| e.to_string())
            }
            "mail_send" => {
                let to_raw = arguments
                    .get("to_bot")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
                    .ok_or_else(|| "missing or invalid required 'to_bot'".to_owned())?;
                let subject = arguments
                    .get("subject")
                    .and_then(Value::as_str)
                    .unwrap_or("Bot mail");
                let body = arguments.get("body").and_then(Value::as_str).unwrap_or("");
                let thread_id = arguments.get("thread_id").and_then(Value::as_str);
                let msg_id = mgr.runtime.send_bot_mail(
                    &reg,
                    &mgr.store,
                    &mgr.secrets,
                    &mgr.tool_relay,
                    &mgr.composio_base_url,
                    ctx.bot_id.as_str(),
                    to_raw,
                    subject,
                    body,
                    thread_id,
                )?;
                Ok(json!({ "message_id": msg_id }).to_string())
            }
            "group_list" => {
                let groups = mgr.store.list_groups().map_err(|e| e.to_string())?;
                serde_json::to_string(&groups).map_err(|e| e.to_string())
            }
            "group_send" => {
                let group_id = arguments
                    .get("group_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing required 'group_id'".to_owned())?;
                let group = mgr
                    .store
                    .get_group(group_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("group not found: {group_id}"))?;
                if !group.bot_ids.contains(&ctx.bot_id) {
                    return Err("permission denied: bot is not a member of target group".to_owned());
                }
                let text = arguments
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing required 'text'".to_owned())?;
                let thread_id = arguments.get("thread_id").and_then(Value::as_str);
                let reply_mode = match arguments.get("reply_mode").and_then(Value::as_str) {
                    Some("all") => krill_proto::bots::BotGroupReplyMode::All,
                    Some("selected") => krill_proto::bots::BotGroupReplyMode::Selected,
                    _ => krill_proto::bots::BotGroupReplyMode::Mentions,
                };
                let plan = mgr
                    .groups
                    .prepare_message_dispatch(
                        &mgr.store,
                        group_id,
                        thread_id,
                        ctx.bot_id.as_str(),
                        text,
                        Some(reply_mode),
                        ts,
                    )
                    .map_err(|e| e.to_string())?;
                let mut dispatched = Vec::new();
                let mut failed = Vec::new();
                for target in &plan.target_bots {
                    let registered = std::cell::Cell::new(None::<(SessionId, u64)>);
                    let mut on_bound = |sid: SessionId, turn_seq: u64| {
                        if registered.get().is_some() {
                            return;
                        }
                        mgr.groups
                            .record_turn_started_seq(
                                &plan.group.id,
                                &plan.thread.thread_id,
                                target,
                                sid,
                                Some(turn_seq),
                                ts,
                            )
                            .ok();
                        registered.set(Some((sid, turn_seq)));
                    };
                    let mut on_rollback = |sid: SessionId, turn_seq: u64| {
                        if registered.get() == Some((sid, turn_seq)) {
                            registered.set(None);
                            mgr.groups
                                .on_bot_turn_finished_seq(&target.id, sid, Some(turn_seq), "", ts)
                                .ok();
                        }
                    };
                    match mgr.runtime.deliver_group_turn_with_hooks(
                        &reg,
                        &mgr.store,
                        &mgr.secrets,
                        &mgr.tool_relay,
                        &mgr.composio_base_url,
                        &target.id,
                        &plan.group.id,
                        &plan.thread.thread_id,
                        &plan.prompt_for_bot,
                        ts,
                        Some(&mut on_bound),
                        Some(&mut on_rollback),
                    ) {
                        Ok((sid, turn_seq, _)) => {
                            if registered.get().is_none() {
                                on_bound(sid, turn_seq);
                            }
                            dispatched.push(target.id.clone());
                        }
                        Err(err) => {
                            if let Some((sid, turn_seq)) = registered.get() {
                                on_rollback(sid, turn_seq);
                            }
                            mgr.groups
                                .record_dispatch_failure(
                                    &plan.group.id,
                                    &plan.thread.thread_id,
                                    target,
                                    &err,
                                    ts,
                                )
                                .ok();
                            failed.push(json!({
                                "bot_id": target.id.as_str(),
                                "error": err,
                            }));
                        }
                    }
                }
                serde_json::to_string(&json!({
                    "message": plan.posted_message,
                    "dispatched_bots": dispatched,
                    "failed_bots": failed,
                }))
                .map_err(|e| e.to_string())
            }
            "access_list" => {
                let reqs = mgr
                    .store
                    .list_access_requests(Some(&ctx.bot_id))
                    .map_err(|e| e.to_string())?;
                let redacted = store::redact_access_requests_for_observer(reqs);
                serde_json::to_string(&redacted).map_err(|e| e.to_string())
            }
            "ask_owner" => {
                let question = arguments
                    .get("question")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "missing required 'question'".to_owned())?
                    .to_owned();
                let options: Vec<String> = arguments
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let recommended = arguments
                    .get("recommended")
                    .and_then(Value::as_u64)
                    .and_then(|v| usize::try_from(v).ok());
                let multi = arguments
                    .get("multi")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let decision = BotItemDecision {
                    question: question.clone(),
                    options,
                    recommended,
                    multi,
                };
                let existing_id = arguments
                    .get("item_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let uid = if let Some(raw_eid) = existing_id {
                    let eid = qualify_bot_item_id(&ctx.bot_id, raw_eid);
                    ensure_bot_owns_item(&mgr.items, &ctx.bot_id, &eid)?;
                    let cur = mgr
                        .items
                        .get_item(&eid)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| format!("bot item not found: {raw_eid}"))?;
                    if cur.status != BotItemStatus::NeedsYou {
                        mgr.items
                            .update_item(
                                &UpdateBotItemParams {
                                    item_id: cur.uid.clone(),
                                    title: None,
                                    description: None,
                                    done_means: None,
                                    status: Some(BotItemStatus::NeedsYou),
                                    comment: None,
                                    decisions: Vec::new(),
                                    close_note: None,
                                    snoozed_until_ms: FieldPatch::Unchanged,
                                },
                                &actor,
                                ts,
                            )
                            .map_err(|e| e.to_string())?;
                    }
                    cur.uid
                } else {
                    let title = arguments
                        .get("title")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map_or_else(
                            || {
                                let clamped = items::clamp_utf8_bytes_and_chars(
                                    &question,
                                    120,
                                    krill_proto::bots::BOT_ITEM_TITLE_MAX_BYTES,
                                );
                                if clamped.is_empty() {
                                    "Question".to_owned()
                                } else {
                                    clamped
                                }
                            },
                            items::clamp_item_title,
                        );
                    let created = mgr
                        .items
                        .create_item(
                            &CreateBotItemParams {
                                bot_id: ctx.bot_id.clone(),
                                title,
                                description: None,
                                done_means: arguments
                                    .get("done_means")
                                    .and_then(Value::as_str)
                                    .map(ToOwned::to_owned),
                                status: Some(BotItemStatus::NeedsYou),
                                links: Vec::new(),
                            },
                            &actor,
                            ts,
                        )
                        .map_err(|e| e.to_string())?;
                    created.uid
                };
                let asked = mgr
                    .items
                    .update_item(
                        &UpdateBotItemParams {
                            item_id: uid,
                            title: None,
                            description: None,
                            done_means: None,
                            status: None,
                            comment: Some(question),
                            decisions: vec![decision],
                            close_note: None,
                            snoozed_until_ms: FieldPatch::Unchanged,
                        },
                        &actor,
                        ts,
                    )
                    .map_err(|e| e.to_string())?;
                reg.notify_orch();
                serde_json::to_string(&asked).map_err(|e| e.to_string())
            }
            "reply" => {
                let text = arguments
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let files: Vec<String> = arguments
                    .get("files")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let reaction = arguments
                    .get("reaction")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let link_id = arguments
                    .get("link_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| ctx.link.clone());
                // Validate bot & link ownership BEFORE initializing lazy ConnectorManager (MB-05).
                validate_reply_target(
                    &mgr.store,
                    &mgr.runtime,
                    ctx.session_id,
                    &ctx.bot_id,
                    link_id.as_deref(),
                )?;
                let ext_ref = mgr.runtime.post_bot_reply_for_session(
                    &mgr.store,
                    mgr.connector_manager(),
                    ctx.session_id,
                    &OutboundBotReply {
                        bot_id: ctx.bot_id.clone(),
                        link_id,
                        text,
                        files,
                        reaction,
                    },
                )?;
                Ok(json!({ "external_ref": ext_ref }).to_string())
            }
            other => Err(format!("unknown built-in tool: {other}")),
        }
    }
}

fn validate_agent_profile(registry: &Registry, raw_agent_id: &str) -> Result<(), String> {
    let trimmed = raw_agent_id.trim();
    if trimmed.is_empty() {
        return Err("agent_id cannot be empty".to_owned());
    }
    let agents = registry
        .agent_control()
        .agents()
        .map_err(|err| err.to_string())?;
    if agents.resolve(trimmed).is_some() {
        Ok(())
    } else {
        Err(format!("unknown agent profile: {trimmed}"))
    }
}

fn classify_turn_error_refusal(
    agent_id: &str,
    err_text: &str,
) -> Option<krill_proto::bots::BotSessionRefusal> {
    let lower = err_text.to_ascii_lowercase();
    if lower.contains("quota")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("resource_exhausted")
        || lower.contains("resource exhausted")
        || lower.contains("usage limit")
        || lower.contains("too many requests")
        || lower.contains("429")
    {
        return Some(krill_proto::bots::BotSessionRefusal::quota_exceeded(
            agent_id,
            err_text.to_owned(),
        ));
    }
    if lower.contains("unauthorized")
        || lower.contains("sign in")
        || lower.contains("signed out")
        || lower.contains("not logged in")
        || lower.contains("authentication")
        || lower.contains("401")
    {
        return Some(krill_proto::bots::BotSessionRefusal::auth_required(
            agent_id,
        ));
    }
    None
}

fn parse_item_status(raw: &str) -> Option<BotItemStatus> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "planned" => Some(BotItemStatus::Planned),
        "working" => Some(BotItemStatus::Working),
        "needs_you" | "needs-you" => Some(BotItemStatus::NeedsYou),
        "done" => Some(BotItemStatus::Done),
        "canceled" | "cancelled" => Some(BotItemStatus::Canceled),
        _ => None,
    }
}

fn parse_optional_mcp_status(arguments: &Value) -> Result<Option<BotItemStatus>, String> {
    let Some(val) = arguments.get("status") else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    let raw = val
        .as_str()
        .ok_or_else(|| "invalid 'status': expected a string".to_owned())?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    parse_item_status(trimmed).map(Some).ok_or_else(|| {
        format!(
            "invalid 'status' value '{trimmed}': expected one of planned, working, needs_you, needs-you, done, canceled"
        )
    })
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

        assert!(reg.bots_if_initialized().is_none());
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));
        assert!(!mgr.items().is_open());
        assert!(!mgr.tool_relay().status().running);
        assert!(mgr.connectors.get().is_none());
        assert!(mgr.google_poller.get().is_none());
        assert!(mgr.tracker_poller.get().is_none());

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
        assert!(matches!(
            mgr.handle_rpc(&reg, BotRpcRequest::RunMemoryTidy { bot_id: created.id.clone() }),
            BotRpcResponse::MemoryTidyResult { changes, .. } if changes >= 1
        ));
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
        // PostBotReply to an unknown bot returns Error WITHOUT starting ConnectorManager!
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::PostBotReply {
                    bot_id: BotId::new("nonexistent-bot").unwrap(),
                    link_id: None,
                    text: "Should fail lazily".to_owned(),
                    files: Vec::new(),
                    reaction: None,
                }
            ),
            BotRpcResponse::Error(_)
        ));
        assert!(mgr.connectors.get().is_none());

        // PostBotReply must fail closed when no external conversation link matches (zero connectors started).
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
            BotRpcResponse::Error(ref e) if e.contains("no external conversation link")
        ));
        assert!(mgr.connectors.get().is_none());
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
            mgr.handle_rpc(&reg, BotRpcRequest::ListImportedSources),
            BotRpcResponse::ImportedSources(_)
        ));
        // Creating a copy from an external candidate dir records the source and allows a 2nd copy.
        let ext_dir = state.join("ext-copy-src");
        fs::create_dir_all(&ext_dir).unwrap();
        fs::write(ext_dir.join("SOUL.md"), "# Ext\n\nHelps out.\n").unwrap();
        let copy1 = match mgr.handle_rpc(
            &reg,
            BotRpcRequest::ImportExternalBotCopy {
                source: Some(krill_proto::bots::ExternalBotSourceApp::OpenClaw),
                path: ext_dir.to_string_lossy().into_owned(),
                params: Box::new(krill_proto::bots::CreateBotV2Params::from(
                    CreateBotParams {
                        name: "Ext Copy 1".to_owned(),
                        agent_id: "claude".to_owned(),
                        description: String::new(),
                        emoji: None,
                        avatar: Some("mascot:B1".to_owned()),
                        model: None,
                        effort: None,
                        permission_mode: None,
                        workspace: None,
                        template_id: None,
                        initial_soul: None,
                    },
                )),
            },
        ) {
            BotRpcResponse::Bot(b) => Some(b),
            _ => None,
        }
        .unwrap();
        assert_eq!(copy1.name, "Ext Copy 1");
        assert_eq!(copy1.avatar.as_deref(), Some("mascot:B1"));
        let imported_sources = match mgr.handle_rpc(&reg, BotRpcRequest::ListImportedSources) {
            BotRpcResponse::ImportedSources(s) => s,
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(imported_sources.len(), 1);
        assert_eq!(imported_sources[0].1, copy1.id);
        let copy2 = match mgr.handle_rpc(
            &reg,
            BotRpcRequest::ImportExternalBotCopy {
                source: Some(krill_proto::bots::ExternalBotSourceApp::OpenClaw),
                path: ext_dir.to_string_lossy().into_owned(),
                params: Box::new(krill_proto::bots::CreateBotV2Params::from(
                    CreateBotParams {
                        name: "Ext Copy 2".to_owned(),
                        agent_id: "codex".to_owned(),
                        description: String::new(),
                        emoji: None,
                        avatar: Some("mascot:B2".to_owned()),
                        model: None,
                        effort: None,
                        permission_mode: None,
                        workspace: None,
                        template_id: None,
                        initial_soul: None,
                    },
                )),
            },
        ) {
            BotRpcResponse::Bot(b) => Some(b),
            _ => None,
        }
        .unwrap();
        assert_eq!(copy2.name, "Ext Copy 2");
        assert_ne!(copy1.id, copy2.id);
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
    fn runtime_hook_heals_memory_and_respects_owner_turn_pending_for_first_run_completed() {
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

        // Empty no-prompt session (`owner_turn_pending = false`) does NOT complete onboarding.
        let sid_empty = SessionId(41);
        lock(mgr.runtime.sessions_for_test()).insert(
            sid_empty,
            BotSessionBinding {
                bot_id: created.id.clone(),
                link_id: None,
                memory_snapshot: MemorySnapshot::capture(initial_mem),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 1234,
                label: "Empty".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: false,
                last_prompt: None,
                status_message_id: None,
                rate_limit_pause: None,
            },
        );
        mgr.on_hook_event(&reg, sid_empty, &stop_event);
        assert!(
            !mgr.store()
                .get_bot(&created.id)
                .unwrap()
                .unwrap()
                .first_run_completed
        );

        // Actual owner/kickoff turn (`owner_turn_pending = true`) heals memory AND sets first_run_completed.
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
                pid: 1234,
                label: "Kickoff".to_owned(),
                created_at_ms: 1_500,
                owner_turn_pending: true,
                last_prompt: Some("Say hello".to_owned()),
                status_message_id: None,
                rate_limit_pause: None,
            },
        );
        let mem_path = mgr.store().bot_home(&created).join("MEMORY.md");
        fs::write(&mem_path, "# Memory\n\n").unwrap();

        mgr.on_hook_event(&reg, sid, &stop_event);

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

    #[test]
    fn bot_session_scope_denies_owner_rpcs_and_attributes_bot_item_actor() {
        use crate::bots::runtime::BotSessionBinding;
        use krill_proto::bots::{CreateBotItemParams, MemorySnapshot};

        let state = temp_state("bot-authz");
        let reg = Arc::new(Registry::new(None, Some(&state)));
        reg.bind_self_arc();
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));

        let bot_a = match mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: "Bot Alpha".to_owned(),
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
            BotRpcResponse::Bot(b) => *b,
            _ => panic!("create bot_a"),
        };
        let bot_b = match mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: "Bot Beta".to_owned(),
                agent_id: "codex".to_owned(),
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
            BotRpcResponse::Bot(b) => *b,
            _ => panic!("create bot_b"),
        };

        let bot_sid = SessionId(700);
        lock(mgr.runtime.sessions_for_test()).insert(
            bot_sid,
            BotSessionBinding {
                bot_id: bot_a.id.clone(),
                link_id: None,
                memory_snapshot: MemorySnapshot::capture(""),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 10,
                label: "Alpha".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: true,
                last_prompt: None,
                status_message_id: None,
                rate_limit_pause: None,
            },
        );

        // Non-bot session (SessionId(800)) keeps full owner permissions!
        assert!(matches!(
            mgr.handle_rpc_for_peer(
                &reg,
                Some(SessionId(800)),
                false,
                BotRpcRequest::SetSecret {
                    secret_ref: "nonbot-ref".to_owned(),
                    value: "v".to_owned(),
                }
            ),
            BotRpcResponse::Ok
        ));

        // Bot session denied owner-only RPCs and cross-bot mutations:
        assert!(matches!(
            mgr.handle_rpc_for_peer(
                &reg,
                Some(bot_sid),
                false,
                BotRpcRequest::SetSecret {
                    secret_ref: "bot-set".to_owned(),
                    value: "v".to_owned(),
                }
            ),
            BotRpcResponse::Error(ref e) if e.contains("permission denied")
        ));
        assert!(matches!(
            mgr.handle_rpc_for_peer(
                &reg,
                Some(bot_sid),
                false,
                BotRpcRequest::UpdateMemory {
                    bot_id: bot_a.id.clone(),
                    content: "# Memory\n".to_owned(),
                    allow_deletions: true,
                }
            ),
            BotRpcResponse::Error(ref e) if e.contains("permission denied")
        ));
        assert!(matches!(
            mgr.handle_rpc_for_peer(
                &reg,
                Some(bot_sid),
                false,
                BotRpcRequest::DeleteBot {
                    bot_id: bot_b.id.clone(),
                    delete_home: true,
                }
            ),
            BotRpcResponse::Error(ref e) if e.contains("permission denied")
        ));

        // Self-created item attributes actor to BotItemActor::Bot { bot_id, session_id }:
        let item = match mgr.handle_rpc_for_peer(
            &reg,
            Some(bot_sid),
            false,
            BotRpcRequest::CreateItem(Box::new(CreateBotItemParams {
                bot_id: bot_a.id.clone(),
                title: "Check deploy".to_owned(),
                description: None,
                done_means: None,
                status: None,
                links: Vec::new(),
            })),
        ) {
            BotRpcResponse::Item(i) => *i,
            other => panic!("expected item, got {other:?}"),
        };
        assert!(matches!(
            item.events.first().map(|e| &e.actor),
            Some(BotItemActor::Bot {
                bot_id,
                session_id: Some(sid)
            }) if bot_id == &bot_a.id && *sid == bot_sid
        ));

        // Cross-bot item create denied:
        assert!(matches!(
            mgr.handle_rpc_for_peer(
                &reg,
                Some(bot_sid),
                false,
                BotRpcRequest::CreateItem(Box::new(CreateBotItemParams {
                    bot_id: bot_b.id,
                    title: "Hijack".to_owned(),
                    description: None,
                    done_means: None,
                    status: None,
                    links: Vec::new(),
                }))
            ),
            BotRpcResponse::Error(ref e) if e.contains("permission denied")
        ));

        // Wave-3 TestConnector reports structured missing-credential result cleanly:
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::TestConnector {
                    bot_id: bot_a.id,
                    source: ExternalSource::Slack,
                }
            ),
            BotRpcResponse::ConnectorTest(ref res) if !res.ok && res.detail.contains("Missing Slack bot token")
        ));

        mgr.shutdown();
        fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn bot_session_manages_only_its_own_schedules() {
        use crate::bots::runtime::BotSessionBinding;
        use krill_proto::agent::Permission;
        use krill_proto::bots::{BotScheduleConfig, MemorySnapshot};

        let state = temp_state("bot-sched-authz");
        let reg = Arc::new(Registry::new(None, Some(&state)));
        reg.bind_self_arc();
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));
        let create = |name: &str| match mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: name.to_owned(),
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
            BotRpcResponse::Bot(b) => *b,
            other => panic!("create {name}: {other:?}"),
        };
        let bot_a = create("Sched Alpha");
        let bot_b = create("Sched Beta");
        let bot_sid = SessionId(710);
        lock(mgr.runtime.sessions_for_test()).insert(
            bot_sid,
            BotSessionBinding {
                bot_id: bot_a.id.clone(),
                link_id: None,
                memory_snapshot: MemorySnapshot::capture(""),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 10,
                label: "Alpha".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: false,
                last_prompt: None,
                status_message_id: None,
                rate_limit_pause: None,
            },
        );
        // Same shape `krill bot schedules add|rm|enable|disable` sends.
        let schedules_only = |bot_id: &BotId, schedules: Vec<BotScheduleConfig>| UpdateBotParams {
            bot_id: bot_id.clone(),
            name: None,
            description: None,
            emoji: FieldPatch::Unchanged,
            avatar: FieldPatch::Unchanged,
            agent_id: None,
            model: FieldPatch::Unchanged,
            effort: FieldPatch::Unchanged,
            permission_mode: None,
            workspace: FieldPatch::Unchanged,
            paused: None,
            first_run_completed: None,
            notifications: None,
            schedules: Some(schedules),
            look_around: None,
            memory_tidy: None,
            mcp_tools: None,
        };
        let call = |params: UpdateBotParams| {
            mgr.handle_rpc_for_peer(
                &reg,
                Some(bot_sid),
                false,
                BotRpcRequest::UpdateBot(Box::new(params)),
            )
        };
        let poke = BotScheduleConfig {
            id: "sched-poke".to_owned(),
            name: "Poke".to_owned(),
            cron: "*/30 * * * *".to_owned(),
            prompt: "Check the Poke thread".to_owned(),
            enabled: true,
            timezone: None,
        };

        // Add, list, and remove its own schedule.
        assert!(matches!(
            call(schedules_only(&bot_a.id, vec![poke.clone()])),
            BotRpcResponse::Bot(ref b) if b.schedules == vec![poke.clone()]
        ));
        assert!(matches!(
            mgr.handle_rpc_for_peer(
                &reg,
                Some(bot_sid),
                false,
                BotRpcRequest::GetBot {
                    bot_id: bot_a.id.clone()
                }
            ),
            BotRpcResponse::Bot(ref b) if b.schedules.len() == 1
        ));
        assert!(matches!(
            call(schedules_only(&bot_a.id, Vec::new())),
            BotRpcResponse::Bot(ref b) if b.schedules.is_empty()
        ));

        // Another bot's schedules stay out of reach.
        assert!(matches!(
            call(schedules_only(&bot_b.id, vec![poke.clone()])),
            BotRpcResponse::Error(ref e) if e.contains("another bot's schedules")
        ));
        // A schedule edit cannot carry other bot settings with it.
        let mut escalate = schedules_only(&bot_a.id, vec![poke.clone()]);
        escalate.permission_mode = Some(Permission::Manual);
        assert!(matches!(
            call(escalate),
            BotRpcResponse::Error(ref e) if e.contains("owner-only")
        ));
        let mut rename = schedules_only(&bot_a.id, Vec::new());
        rename.schedules = None;
        rename.name = Some("Renamed".to_owned());
        assert!(matches!(
            call(rename),
            BotRpcResponse::Error(ref e) if e.contains("owner-only")
        ));
        let get =
            |id: &BotId| match mgr.handle_rpc(&reg, BotRpcRequest::GetBot { bot_id: id.clone() }) {
                BotRpcResponse::Bot(b) => *b,
                other => panic!("get bot: {other:?}"),
            };
        assert!(get(&bot_b.id).schedules.is_empty());
        let a = get(&bot_a.id);
        assert!(a.schedules.is_empty());
        assert_eq!(a.name, "Sched Alpha");
        assert_eq!(a.permission_mode, bot_a.permission_mode);

        mgr.shutdown();
        fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn a_bot_session_proposes_bots_only_for_itself_and_cannot_answer_them() {
        use crate::bots::runtime::BotSessionBinding;
        use krill_proto::bots::MemorySnapshot;

        let state = temp_state("bot-offer-authz");
        let reg = Arc::new(Registry::new(None, Some(&state)));
        reg.bind_self_arc();
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));
        let create = |name: &str| match mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: name.to_owned(),
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
            BotRpcResponse::Bot(b) => *b,
            other => panic!("create {name}: {other:?}"),
        };
        let alpha = create("Offer Alpha");
        let beta = create("Offer Beta");
        let sid = SessionId(711);
        lock(mgr.runtime.sessions_for_test()).insert(
            sid,
            BotSessionBinding {
                bot_id: alpha.id.clone(),
                link_id: None,
                memory_snapshot: MemorySnapshot::capture(""),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 11,
                label: "Alpha".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: false,
                last_prompt: None,
                status_message_id: None,
                rate_limit_pause: None,
            },
        );
        let as_bot = |req| mgr.handle_rpc_for_peer(&reg, Some(sid), false, req);
        let offer = |bot_id: &BotId| BotRpcRequest::OfferBot {
            bot_id: bot_id.clone(),
            name: "Ledger".to_owned(),
            purpose: "Keeps the spend sheet.".to_owned(),
        };
        assert!(matches!(as_bot(offer(&alpha.id)), BotRpcResponse::Text(_)));
        assert!(matches!(as_bot(offer(&beta.id)), BotRpcResponse::Error(_)));
        let BotRpcResponse::BotOffers(offers) = as_bot(BotRpcRequest::ListBotOffers {
            bot_id: alpha.id.clone(),
        }) else {
            panic!("offers");
        };
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].session_id, Some(sid.0));
        let answer = BotRpcRequest::ResolveBotOffer {
            offer_id: offers[0].offer_id.clone(),
            created: None,
        };
        assert!(matches!(as_bot(answer.clone()), BotRpcResponse::Error(_)));
        assert!(matches!(mgr.handle_rpc(&reg, answer), BotRpcResponse::Ok));
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

        let (mut client, server) = UnixStream::pair().unwrap();
        let reg_srv = Arc::clone(&reg);
        let h_srv = hello.clone();
        let srv_thread = std::thread::spawn(move || reg_srv.serve(server, &h_srv));

        let _ = ipc::read_frame(&mut client).unwrap().unwrap();
        client.write_all(&client_hello.encode().unwrap()).unwrap();

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
        let created_bot = match reply {
            Reply::Bot(BotRpcResponse::Bot(b)) => *b,
            other => panic!("expected Bot reply, got {other:?}"),
        };

        // Add pending access request to test Observer redaction.
        reg.bots()
            .unwrap()
            .store()
            .check_or_request_access(
                &created_bot.id,
                ExternalSource::Slack,
                "U1",
                "Alice",
                "C1",
                1_000,
            )
            .unwrap();

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

        // Security contract: Observer cannot call ListAccessRequests (it is not read-only
        // because access requests carry pairing tokens), AND defense-in-depth redaction works:
        let raw_reqs = reg
            .bots()
            .unwrap()
            .store()
            .list_access_requests(Some(&created_bot.id))
            .unwrap();
        assert_eq!(raw_reqs.len(), 1);
        assert!(!raw_reqs[0].token.is_empty());
        let redacted = store::redact_access_requests_for_observer(raw_reqs);
        assert_eq!(redacted.len(), 1);
        assert!(redacted[0].token.is_empty());

        client
            .write_all(
                &Request::Bot(BotRpcRequest::ListAccessRequests {
                    bot_id: Some(created_bot.id.clone()),
                })
                .encode()
                .unwrap(),
            )
            .unwrap();
        let (k, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert!(matches!(
            Reply::decode(k, &body).unwrap(),
            Reply::Failed(krill_proto::control::Failure::Denied)
        ));

        client.write_all(&create_req.encode().unwrap()).unwrap();
        let (k, body) = ipc::read_frame(&mut client).unwrap().unwrap();
        assert!(matches!(
            Reply::decode(k, &body).unwrap(),
            Reply::Failed(krill_proto::control::Failure::Denied)
        ));
        drop(client);
        srv_thread.join().ok();

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

    #[test]
    fn wave3_builtin_mcp_ask_owner_oauth_and_composio_rpcs_work() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let state = temp_state("wave3-rpcs");
        let reg = Arc::new(Registry::new(None, Some(&state)));
        reg.bind_self_arc();

        // Loopback mock server supporting Composio v3.1 + GitHub test_connector.
        let mock = TcpListener::bind("127.0.0.1:0").unwrap();
        let mock_addr = mock.local_addr().unwrap();
        let srv = std::thread::spawn(move || {
            for _ in 0..5 {
                let Ok((mut sock, _)) = mock.accept() else {
                    break;
                };
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    continue;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let line = req.lines().next().unwrap_or("");
                let target = line.split_whitespace().nth(1).unwrap_or("/");
                let body = if target.contains("/connected_accounts") && line.starts_with("GET") {
                    r#"{"items":[{"id":"ca-99","toolkit":{"slug":"github","name":"GitHub"},"status":"ACTIVE"}]}"#.to_owned()
                } else if target.ends_with("/tool_router/session") {
                    format!(
                        r#"{{"session_id":"tr-w3","mcp":{{"url":"http://{mock_addr}/mcp/tr-w3"}}}}"#
                    )
                } else if target.contains("/link") {
                    r#"{"connected_account_id":"ca-link-1","redirect_url":"https://connect.composio.dev/go"}"#.to_owned()
                } else if line.starts_with("DELETE ") {
                    r#"{"deleted":true}"#.to_owned()
                } else if target == "/user" {
                    r#"{"login":"wave3_bot"}"#.to_owned()
                } else {
                    r#"{"items":[]}"#.to_owned()
                };
                let wire = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(wire.as_bytes()).unwrap();
            }
        });

        let trk_ep = trackers::TrackerEndpoints {
            github_api_base: format!("http://{mock_addr}"),
            linear_graphql_url: format!("http://{mock_addr}/graphql"),
        };
        let mgr = BotsManager::with_endpoints(
            state.clone(),
            Arc::downgrade(&reg),
            google::GoogleEndpoints::default(),
            trk_ep,
            format!("http://{mock_addr}/api/v3.1"),
        );

        let bot = match mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: "Wave3 Bot".to_owned(),
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
            BotRpcResponse::Bot(b) => *b,
            other => panic!("unexpected: {other:?}"),
        };

        // 1. Built-in /bot-tools/krill/mcp: ask_owner creates NeedsYou decision item.
        let grant = mgr
            .tool_relay()
            .prepare_launch_for_bot(&bot)
            .unwrap()
            .unwrap();
        let krill_mcp_url = format!("{}/bot-tools/krill/mcp", grant.base_url);
        let ask_resp = http_client::send(
            &http_client::HttpRequest::post_json(
                &krill_mcp_url,
                br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ask_owner","arguments":{"title":"Deploy approval","question":"Ship v2?","options":["Yes","No"],"recommended":0}}}"#.to_vec(),
            )
            .with_header("Authorization", format!("Bearer {}", *grant.token))
            .with_ssrf_policy(http_client::SsrfPolicy::AllowLoopback),
        )
        .unwrap();
        assert_eq!(ask_resp.status, 200);
        let listed = mgr
            .items()
            .list_items(Some(&bot.id), None, true, true, now_ms())
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, BotItemStatus::NeedsYou);
        assert_eq!(
            listed[0].events.last().unwrap().decisions[0].options,
            vec!["Yes", "No"]
        );

        // 2. TestConnector (GitHub), McpOAuthStatus/CancelMcpOAuth, and Composio v3.1 RPCs.
        mgr.secrets()
            .set_secret(
                &bot.id,
                "github_token",
                Zeroizing::new("ghp_mock".to_owned()),
            )
            .unwrap();
        mgr.secrets()
            .set_secret(
                &bot.id,
                "composio_api_key",
                Zeroizing::new("cmp_mock".to_owned()),
            )
            .unwrap();
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::TestConnector {
                    bot_id: bot.id.clone(),
                    source: ExternalSource::GitHub,
                }
            ),
            BotRpcResponse::ConnectorTest(ref t) if t.ok && t.account_label.as_deref() == Some("@wave3_bot")
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::McpOAuthStatus {
                    bot_id: bot.id.clone(),
                    tool_id: "notion".to_owned(),
                }
            ),
            BotRpcResponse::McpOAuth(ref s) if s.status == "idle"
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::CancelMcpOAuth {
                    bot_id: bot.id.clone(),
                    tool_id: "notion".to_owned(),
                }
            ),
            BotRpcResponse::McpOAuth(ref s) if s.status == "cancelled"
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::ListComposioConnections {
                    bot_id: bot.id.clone(),
                }
            ),
            BotRpcResponse::ComposioConnections(ref c) if c.len() == 1 && c[0].id == "ca-99"
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::StartComposioConnect {
                    bot_id: bot.id.clone(),
                    app_id: "github".to_owned(),
                }
            ),
            BotRpcResponse::ComposioConnectStarted { ref connection_id, .. } if connection_id == "ca-link-1"
        ));
        assert!(matches!(
            mgr.handle_rpc(
                &reg,
                BotRpcRequest::DisconnectComposioAccount {
                    bot_id: bot.id.clone(),
                    connection_id: "ca-99".to_owned(),
                }
            ),
            BotRpcResponse::Ok
        ));

        srv.join().ok();
        mgr.shutdown();
        fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn switching_bot_provider_clears_stale_model_sessions_cron_and_links() {
        let state =
            std::env::temp_dir().join(format!("krilld-bot-provider-switch-{}", std::process::id()));
        fs::remove_dir_all(&state).ok();
        fs::create_dir_all(&state).unwrap();

        let reg = Arc::new(Registry::new(None, Some(&state)));
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));

        let BotRpcResponse::Bot(created) = mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: "Switch Bot".to_owned(),
                agent_id: "claude".to_owned(),
                description: "provider switch test".to_owned(),
                emoji: None,
                avatar: None,
                model: Some("opus".to_owned()),
                effort: Some("high".to_owned()),
                permission_mode: None,
                workspace: None,
                template_id: None,
                initial_soul: None,
            })),
        ) else {
            panic!("expected Bot");
        };
        assert_eq!(created.agent_id, "claude");
        assert_eq!(created.model.as_deref(), Some("opus"));
        assert_eq!(created.effort.as_deref(), Some("high"));

        // Seed an old-provider session binding, active cron session, and external link session.
        let old_sid = SessionId(701);
        lock(mgr.runtime.sessions_for_test()).insert(
            old_sid,
            crate::bots::runtime::BotSessionBinding {
                bot_id: created.id.clone(),
                link_id: None,
                memory_snapshot: krill_proto::bots::MemorySnapshot::capture(""),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 0,
                label: "Switch Bot".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: false,
                last_prompt: None,
                status_message_id: None,
                rate_limit_pause: None,
            },
        );
        lock(&mgr.active_cron_sessions).insert((created.id.clone(), "daily".to_owned()), old_sid);
        let link = mgr
            .store
            .lookup_or_create_link(
                &created.id,
                ExternalSource::Slack,
                None,
                "C1",
                None,
                ActsFor::Owner,
                1_000,
            )
            .unwrap();
        mgr.store
            .bind_link_session(&link.id, old_sid, 1_100)
            .unwrap();

        // Switch provider from claude -> antigravity (canonicalized to agy) without setting model/effort.
        let BotRpcResponse::Bot(updated) = mgr.handle_rpc(
            &reg,
            BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                bot_id: created.id.clone(),
                name: None,
                description: None,
                emoji: FieldPatch::Unchanged,
                avatar: FieldPatch::Unchanged,
                agent_id: Some("antigravity".to_owned()),
                model: FieldPatch::Unchanged,
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
            })),
        ) else {
            panic!("expected updated Bot");
        };
        assert_eq!(updated.agent_id, "agy");
        assert_eq!(updated.model, None, "stale claude model must be cleared");
        assert_eq!(updated.effort, None, "stale claude effort must be cleared");
        assert!(
            !mgr.runtime.is_bot_session(old_sid),
            "old provider session binding must be cleared on provider switch"
        );
        assert!(
            lock(&mgr.active_cron_sessions).is_empty(),
            "old provider cron session binding must be cleared on provider switch"
        );
        assert_eq!(
            mgr.store
                .get_link(&link.id)
                .unwrap()
                .and_then(|l| l.session_id),
            None,
            "external link session binding must be cleared on provider switch"
        );

        // Verify persistence across a fresh BotsManager (simulated daemon restart) and bot.json.
        let mgr2 = BotsManager::new(state.clone(), Arc::downgrade(&reg));
        let reloaded = mgr2.store.get_bot(&created.id).unwrap().unwrap();
        assert_eq!(reloaded.agent_id, "agy");
        assert_eq!(reloaded.model, None);
        let bot_json = fs::read_to_string(mgr2.store.bot_home(&reloaded).join("bot.json")).unwrap();
        assert!(bot_json.contains("\"agent_id\": \"agy\""), "{bot_json}");

        mgr2.shutdown();
        mgr.shutdown();
        fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn bot_start_refusals_and_schedule_run_failures_surface_structured_reasons() {
        use krill_proto::bots::{BotRefusalCode, BotScheduleConfig};
        use krill_proto::hook::{EventId, HookEvent, Input};

        let state = std::env::temp_dir().join(format!(
            "krilld-bot-refusal-and-quota-{}",
            std::process::id()
        ));
        fs::remove_dir_all(&state).ok();
        fs::create_dir_all(&state).unwrap();

        let reg = Arc::new(Registry::new(None, Some(&state)));
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));

        let BotRpcResponse::Bot(created) = mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: "Refusal Bot".to_owned(),
                agent_id: "agy".to_owned(),
                description: "refusal test".to_owned(),
                emoji: None,
                avatar: None,
                model: None,
                effort: None,
                permission_mode: None,
                workspace: None,
                template_id: None,
                initial_soul: None,
            })),
        ) else {
            panic!("expected Bot");
        };

        // 1. Add a schedule and pause the bot -> StartBotSession and RunScheduleNow return BotPaused refusal.
        let BotRpcResponse::Bot(_) = mgr.handle_rpc(
            &reg,
            BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                bot_id: created.id.clone(),
                name: None,
                description: None,
                emoji: FieldPatch::Unchanged,
                avatar: FieldPatch::Unchanged,
                agent_id: None,
                model: FieldPatch::Unchanged,
                effort: FieldPatch::Unchanged,
                permission_mode: None,
                workspace: FieldPatch::Unchanged,
                paused: Some(true),
                first_run_completed: None,
                notifications: None,
                schedules: Some(vec![BotScheduleConfig {
                    id: "nightly".to_owned(),
                    name: "Nightly triage".to_owned(),
                    cron: "0 9 * * *".to_owned(),
                    prompt: "Check inbox".to_owned(),
                    enabled: true,
                    timezone: None,
                }]),
                look_around: None,
                memory_tidy: None,
                mcp_tools: None,
            })),
        ) else {
            panic!("expected updated Bot");
        };

        let BotRpcResponse::SessionRefused(refusal) = mgr.handle_rpc(
            &reg,
            BotRpcRequest::StartBotSession {
                bot_id: created.id.clone(),
                cwd: None,
                prompt: None,
            },
        ) else {
            panic!("expected SessionRefused for paused bot");
        };
        assert_eq!(refusal.code, BotRefusalCode::BotPaused);
        assert!(
            refusal
                .message_en(Some("this computer"))
                .contains("Refusal Bot")
        );

        // 2. RunScheduleNow on the paused bot returns SessionRefused AND records it in GetBotUpcoming.
        let BotRpcResponse::SessionRefused(sched_refusal) = mgr.handle_rpc(
            &reg,
            BotRpcRequest::RunScheduleNow {
                bot_id: created.id.clone(),
                schedule_id: "nightly".to_owned(),
            },
        ) else {
            panic!("expected SessionRefused from RunScheduleNow");
        };
        assert_eq!(sched_refusal.code, BotRefusalCode::BotPaused);

        let BotRpcResponse::Upcoming(up) = mgr.handle_rpc(
            &reg,
            BotRpcRequest::GetBotUpcoming {
                bot_id: created.id.clone(),
            },
        ) else {
            panic!("expected Upcoming");
        };
        let sched_up = up
            .schedules
            .iter()
            .find(|s| s.id == "nightly")
            .expect("nightly upcoming");
        assert_eq!(
            sched_up.last_refusal.as_ref().map(|r| r.code),
            Some(BotRefusalCode::BotPaused)
        );
        assert!(
            sched_up
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("Refusal Bot"))
        );

        // 3. Quota-stopped Antigravity Stop hook skips `first_run_completed`, clears pending turn,
        //    and records the quota error on the active schedule instead of stale progress text.
        let sid = SessionId(709);
        lock(mgr.runtime.sessions_for_test()).insert(
            sid,
            crate::bots::runtime::BotSessionBinding {
                bot_id: created.id.clone(),
                link_id: None,
                memory_snapshot: krill_proto::bots::MemorySnapshot::capture(""),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 0,
                label: "Refusal Bot".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: true,
                last_prompt: Some("Check inbox".to_owned()),
                status_message_id: None,
                rate_limit_pause: None,
            },
        );
        lock(&mgr.active_cron_sessions).insert((created.id.clone(), "nightly".to_owned()), sid);

        let quota_err =
            "generating and executing: RESOURCE_EXHAUSTED (code 429): Individual quota reached.";
        let quota_stop = HookEvent {
            id: EventId {
                unix_ns: 100,
                pid: 1,
            },
            agent: "agy".to_owned(),
            ppid: 1,
            input: Input::Complete,
            payload: format!(
                r#"{{"hook_event_name":"Stop","fullyIdle":true,"terminationReason":"ERROR","error":"{quota_err}","last_assistant_message":"Running the baseline benchmarks."}}"#
            )
            .into_bytes(),
        };
        mgr.on_hook_event(&reg, sid, &quota_stop);
        assert!(
            !mgr.runtime.turn_running(sid),
            "quota-stopped turn must clear owner_turn_pending"
        );
        assert!(
            !mgr.store
                .get_bot(&created.id)
                .unwrap()
                .unwrap()
                .first_run_completed,
            "quota-stopped turn must NOT mark first_run_completed"
        );
        let BotRpcResponse::Upcoming(up_after) = mgr.handle_rpc(
            &reg,
            BotRpcRequest::GetBotUpcoming {
                bot_id: created.id.clone(),
            },
        ) else {
            panic!("expected Upcoming");
        };
        let sched_after = up_after
            .schedules
            .iter()
            .find(|s| s.id == "nightly")
            .expect("nightly upcoming after quota stop");
        assert_eq!(sched_after.last_error.as_deref(), Some(quota_err));
        assert_eq!(
            sched_after.last_refusal.as_ref().map(|r| r.code),
            Some(BotRefusalCode::QuotaExceeded)
        );

        mgr.shutdown();
        fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn wave2_regressions_delete_memory_hooks_mail_suggestions_refusals_and_groups() {
        use krill_proto::bots::{
            BotGroupReplyMode, BotRefusalCode, CreateBotParams, FieldPatch, UpdateBotParams,
        };
        use krill_proto::hook::{EventId, HookEvent, Input};

        let state =
            std::env::temp_dir().join(format!("krilld-wave2-core-reg-{}", std::process::id()));
        fs::remove_dir_all(&state).ok();
        fs::create_dir_all(&state).unwrap();
        let reg = Arc::new(Registry::new(None, Some(&state)));
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));

        // S-02: CreateBot & UpdateBot reject unknown agent_id.
        let bad_create = mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: "Bad Agent".to_owned(),
                agent_id: "nonexistent-agent".to_owned(),
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
        );
        assert!(
            matches!(bad_create, BotRpcResponse::Error(ref msg) if msg.contains("unknown agent")),
            "{bad_create:?}"
        );

        let BotRpcResponse::Bot(bot) = mgr.handle_rpc(
            &reg,
            BotRpcRequest::CreateBot(Box::new(CreateBotParams {
                name: "Core Bot".to_owned(),
                agent_id: "agy".to_owned(),
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
        ) else {
            panic!("expected Bot");
        };

        let bad_update = mgr.handle_rpc(
            &reg,
            BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                bot_id: bot.id.clone(),
                name: None,
                description: None,
                emoji: FieldPatch::Unchanged,
                avatar: FieldPatch::Unchanged,
                agent_id: Some("nonexistent-agent".to_owned()),
                model: FieldPatch::Unchanged,
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
            })),
        );
        assert!(
            matches!(bad_update, BotRpcResponse::Error(ref msg) if msg.contains("unknown agent")),
            "{bad_update:?}"
        );

        // D-04: MCP memory_update with allow_deletions=true must be blocked.
        let mcp_handler = DaemonBuiltinMcpHandler {
            registry: Arc::downgrade(&reg),
            manager: Arc::downgrade(&mgr),
        };
        let mcp_ctx = BuiltinMcpCallContext {
            bot_id: bot.id.clone(),
            session_id: Some(SessionId(501)),
            link: None,
        };
        let mcp_del = mcp_handler.call_tool(
            &mcp_ctx,
            "memory_update",
            &json!({"content": "# Memory\n", "allow_deletions": true}),
        );
        assert!(mcp_del.is_err(), "MCP allow_deletions=true must fail");

        // D-05: Owner ForgetMemoryFact refreshes live session snapshot so Stop does not heal it back.
        mgr.store
            .update_memory(
                &bot.id,
                "# Memory\n\n- 2026-11-20 Secret fact\n- 2026-11-20 Keep fact\n",
                true,
            )
            .unwrap();
        let live_sid = SessionId(501);
        lock(mgr.runtime.sessions_for_test()).insert(
            live_sid,
            crate::bots::runtime::BotSessionBinding {
                bot_id: bot.id.clone(),
                link_id: None,
                memory_snapshot: krill_proto::bots::MemorySnapshot::capture(
                    &mgr.store.get_memory(&bot.id).unwrap(),
                ),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 0,
                label: "Core Bot".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: false,
                last_prompt: None,
                status_message_id: None,
                rate_limit_pause: None,
            },
        );
        let forgot = mgr.handle_rpc(
            &reg,
            BotRpcRequest::ForgetMemoryFact {
                bot_id: bot.id.clone(),
                fact: "Secret fact".to_owned(),
            },
        );
        assert!(matches!(forgot, BotRpcResponse::MemoryUpdated { .. }));

        // D-06: UserPromptSubmit hook marks owner_turn_pending=true and records last_prompt.
        let submit_ev = HookEvent {
            id: EventId {
                unix_ns: 200,
                pid: 1,
            },
            agent: "agy".to_owned(),
            ppid: 1,
            input: Input::Complete,
            payload: br#"{"hook_event_name":"UserPromptSubmit","prompt":"Hello from chat"}"#
                .to_vec(),
        };
        mgr.on_hook_event(&reg, live_sid, &submit_ev);
        assert!(mgr.runtime.turn_running(live_sid));
        let stop_ok = HookEvent {
            id: EventId {
                unix_ns: 300,
                pid: 1,
            },
            agent: "agy".to_owned(),
            ppid: 1,
            input: Input::Complete,
            payload: br#"{"hook_event_name":"Stop","fullyIdle":true}"#.to_vec(),
        };
        mgr.on_hook_event(&reg, live_sid, &stop_ok);
        assert!(!mgr.runtime.turn_running(live_sid));
        assert!(
            !mgr.store
                .get_memory(&bot.id)
                .unwrap()
                .contains("Secret fact"),
            "owner-forgotten fact must NOT be restored by system:heal"
        );
        assert!(
            mgr.store
                .get_bot(&bot.id)
                .unwrap()
                .unwrap()
                .first_run_completed,
            "clean UserPromptSubmit -> Stop must mark first_run_completed"
        );

        // D-08: SendBotMail sender spoofing denied for bot sessions.
        let spoof = mgr.handle_rpc_for_peer(
            &reg,
            Some(live_sid),
            false,
            BotRpcRequest::SendBotMail {
                from: "owner".to_owned(),
                to_bot: bot.id.as_str().to_owned(),
                subject: "Spoof".to_owned(),
                body: "No".to_owned(),
                thread_id: None,
            },
        );
        assert!(matches!(spoof, BotRpcResponse::Error(ref m) if m.contains("spoof")));

        // D-30: Generic non-quota Antigravity Stop error must NOT record QuotaExceeded refusal.
        assert_eq!(
            classify_turn_error_refusal("agy", "network timeout connecting to backend"),
            None
        );
        assert_eq!(
            classify_turn_error_refusal("agy", "RESOURCE_EXHAUSTED (code 429): quota reached")
                .map(|r| r.code),
            Some(BotRefusalCode::QuotaExceeded)
        );

        // D-13(c): StartSuggestion on paused bot must NOT dismiss the suggestion.
        mgr.store
            .update_bot(
                UpdateBotParams {
                    bot_id: bot.id.clone(),
                    name: None,
                    description: None,
                    emoji: FieldPatch::Unchanged,
                    avatar: FieldPatch::Unchanged,
                    agent_id: None,
                    model: FieldPatch::Unchanged,
                    effort: FieldPatch::Unchanged,
                    permission_mode: None,
                    workspace: FieldPatch::Unchanged,
                    paused: Some(true),
                    first_run_completed: None,
                    notifications: None,
                    schedules: None,
                    look_around: None,
                    memory_tidy: None,
                    mcp_tools: None,
                },
                2_000,
            )
            .unwrap();
        let sugg = krill_proto::bots::BotSuggestion {
            id: "sugg-keep-1".to_owned(),
            bot_id: bot.id.clone(),
            title: "Check deploy".to_owned(),
            why: "Pending PR".to_owned(),
            ask: "Review it".to_owned(),
            link: None,
            source: None,
            dismissed: false,
            created_at_ms: 2_000,
        };
        mgr.store.save_suggestions(&bot.id, &[sugg]).unwrap();
        let start_sugg = mgr.handle_rpc(
            &reg,
            BotRpcRequest::StartSuggestion {
                bot_id: bot.id.clone(),
                suggestion_id: "sugg-keep-1".to_owned(),
            },
        );
        assert!(matches!(start_sugg, BotRpcResponse::SessionRefused(_)));
        let remaining = look_around::list_suggestions(&mgr.store, Some(&bot.id), false).unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "failed StartSuggestion must keep suggestion active"
        );

        // D-02: DeleteBot removes live session bindings, cron sessions, offers, and 0-member groups.
        let solo_grp = mgr
            .store
            .create_group("Solo Group", vec![bot.id.clone()], String::new(), 3_000)
            .unwrap();
        let _ = mgr
            .groups
            .prepare_message_dispatch(
                &mgr.store,
                &solo_grp.id,
                None,
                "owner",
                "hi",
                Some(BotGroupReplyMode::All),
                3_010,
            )
            .unwrap();
        lock(&mgr.active_cron_sessions).insert((bot.id.clone(), "cron-1".to_owned()), live_sid);
        let del = mgr.handle_rpc(
            &reg,
            BotRpcRequest::DeleteBot {
                bot_id: bot.id.clone(),
                delete_home: true,
            },
        );
        assert!(matches!(del, BotRpcResponse::Deleted { .. }));
        assert!(!mgr.runtime.is_bot_session(live_sid));
        assert!(lock(&mgr.active_cron_sessions).is_empty());
        assert!(
            mgr.store.list_groups().unwrap().is_empty(),
            "0-member zombie group must be pruned when its last bot is deleted"
        );

        mgr.shutdown();
        fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn bot_security_mcp_and_item_qualification_regressions() {
        let state = temp_state("mb04-05-06-07-09-34-38");
        let reg = Arc::new(Registry::new(None, Some(&state)));
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));
        let mk_bot = |name: &str| {
            mgr.store
                .create_bot(
                    CreateBotParams {
                        name: name.to_owned(),
                        agent_id: "claude".to_owned(),
                        description: String::new(),
                        emoji: None,
                        avatar: None,
                        model: None,
                        effort: Some("max".to_owned()),
                        permission_mode: None,
                        workspace: None,
                        template_id: None,
                        initial_soul: None,
                    },
                    1_000,
                )
                .unwrap()
        };
        let bot_a = mk_bot("Bot Alpha");
        let bot_b = mk_bot("Bot Beta");

        let sid_a = SessionId(9101);
        lock(mgr.runtime.sessions_for_test()).insert(
            sid_a,
            runtime::BotSessionBinding {
                bot_id: bot_a.id.clone(),
                link_id: None,
                memory_snapshot: krill_proto::bots::MemorySnapshot::capture(""),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 1,
                label: "alpha".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: false,
                last_prompt: None,
                status_message_id: None,
                rate_limit_pause: None,
            },
        );

        let handler = DaemonBuiltinMcpHandler {
            registry: Arc::downgrade(&reg),
            manager: Arc::downgrade(&mgr),
        };
        let ctx_a = tool_relay::BuiltinMcpCallContext {
            bot_id: bot_a.id.clone(),
            session_id: Some(sid_a),
            link: None,
        };

        // MB-04: bot cannot post to a group it isn't a member of, nor with empty sender.
        let group_b_only = mgr
            .store
            .create_group("Beta Only", vec![bot_b.id.clone()], String::new(), 1_100)
            .unwrap();
        let group_both = mgr
            .store
            .create_group(
                "Shared",
                vec![bot_a.id.clone(), bot_b.id.clone()],
                String::new(),
                1_110,
            )
            .unwrap();
        let resp_nonmember = mgr.handle_rpc_for_peer(
            &reg,
            Some(sid_a),
            false,
            BotRpcRequest::SendGroupMessage {
                group_id: group_b_only.id.clone(),
                thread_id: None,
                sender: bot_a.id.as_str().to_owned(),
                text: "intrude".to_owned(),
                reply_mode: None,
            },
        );
        assert!(
            matches!(resp_nonmember, BotRpcResponse::Error(ref e) if e.contains("not a member"))
        );
        let resp_empty_sender = mgr.handle_rpc_for_peer(
            &reg,
            Some(sid_a),
            false,
            BotRpcRequest::SendGroupMessage {
                group_id: group_both.id.clone(),
                thread_id: None,
                sender: String::new(),
                text: "spoof owner".to_owned(),
                reply_mode: None,
            },
        );
        assert!(
            matches!(resp_empty_sender, BotRpcResponse::Error(ref e) if e.contains("cannot be empty"))
        );
        let mcp_nonmember = handler
            .call_tool(
                &ctx_a,
                "group_send",
                &json!({ "group_id": group_b_only.id, "text": "intrude" }),
            )
            .unwrap_err();
        assert!(mcp_nonmember.contains("not a member"), "{mcp_nonmember}");

        // MB-05: MCP `reply` rejects cross-bot link_id WITHOUT initializing lazy ConnectorManager.
        let link_b = mgr
            .store
            .lookup_or_create_link(
                &bot_b.id,
                krill_proto::bots::ExternalSource::Telegram,
                None,
                "tg-chat-b",
                None,
                krill_proto::bots::ActsFor::Owner,
                1_200,
            )
            .unwrap();
        let reply_err = handler
            .call_tool(
                &ctx_a,
                "reply",
                &json!({ "link_id": link_b.id, "text": "cross-bot" }),
            )
            .unwrap_err();
        assert!(reply_err.contains("does not belong to bot"), "{reply_err}");
        assert!(
            mgr.connectors.get().is_none(),
            "rejected MCP reply must not wake lazy ConnectorManager"
        );

        // MB-06: MCP `access_list` must redact access request approval tokens.
        let req = match mgr
            .store
            .check_or_request_access(
                &bot_a.id,
                krill_proto::bots::ExternalSource::Telegram,
                "ext-user-1",
                "Stranger",
                "chat-1",
                1_300,
            )
            .unwrap()
        {
            connectors::AccessDecision::Pending(req) => req,
            other => panic!("expected pending access request, got {other:?}"),
        };
        assert!(!req.token.is_empty());
        let access_json = handler
            .call_tool(&ctx_a, "access_list", &json!({}))
            .unwrap();
        assert!(
            !access_json.contains(&req.token),
            "access_list leaked raw token: {access_json}"
        );

        // MB-07: MCP `ask_owner` with >200B CJK question safely clamps UTF-8 title without failing.
        let long_korean_q = "한국어질문입니다".repeat(25); // 600 bytes
        let ask_res = handler
            .call_tool(
                &ctx_a,
                "ask_owner",
                &json!({ "question": long_korean_q, "options": ["네", "아니오"] }),
            )
            .unwrap();
        let asked_item: krill_proto::bots::BotItem = serde_json::from_str(&ask_res).unwrap();
        assert!(asked_item.title.len() <= krill_proto::bots::BOT_ITEM_TITLE_MAX_BYTES);
        assert_eq!(asked_item.status, BotItemStatus::NeedsYou);

        // MB-09: `needs-you` accepted; unrecognized status errors cleanly.
        let created_json = handler
            .call_tool(
                &ctx_a,
                "items_create",
                &json!({ "title": "Hyphen status", "status": "needs-you" }),
            )
            .unwrap();
        let created_item: krill_proto::bots::BotItem = serde_json::from_str(&created_json).unwrap();
        assert_eq!(created_item.status, BotItemStatus::NeedsYou);
        let bad_status_err = handler
            .call_tool(
                &ctx_a,
                "items_create",
                &json!({ "title": "Typo status", "status": "in_progress" }),
            )
            .unwrap_err();
        assert!(
            bad_status_err.contains("invalid 'status'"),
            "{bad_status_err}"
        );
        let bad_upd_err = handler
            .call_tool(
                &ctx_a,
                "items_update",
                &json!({ "item_id": created_item.uid, "status": "blocked" }),
            )
            .unwrap_err();
        assert!(bad_upd_err.contains("invalid 'status'"), "{bad_upd_err}");

        // MB-34: Bot-scoped bare "#1" / "1" resolves caller's own item even when another bot also has #1.
        let b_item1 = mgr
            .items
            .create_item(
                &CreateBotItemParams {
                    bot_id: bot_b.id.clone(),
                    title: "Beta item 1".to_owned(),
                    description: None,
                    done_means: None,
                    status: Some(BotItemStatus::Planned),
                    links: Vec::new(),
                },
                &BotItemActor::Owner { device: None },
                1_400,
            )
            .unwrap();
        assert_eq!(b_item1.number, 1);
        let upd_bare = handler
            .call_tool(
                &ctx_a,
                "items_update",
                &json!({ "item_id": "#1", "comment": "Self update via bare #1" }),
            )
            .unwrap();
        let upd_item: krill_proto::bots::BotItem = serde_json::from_str(&upd_bare).unwrap();
        assert_eq!(upd_item.owner_bot_id, bot_a.id);
        assert_eq!(upd_item.number, 1);
        // Cross-bot qualified ID still rejected.
        let cross_item_err = handler
            .call_tool(
                &ctx_a,
                "items_update",
                &json!({ "item_id": format!("{}#1", bot_b.id.as_str()), "comment": "nope" }),
            )
            .unwrap_err();
        assert!(
            cross_item_err.contains("owned by another bot"),
            "{cross_item_err}"
        );

        // MB-38: Switching provider from claude (effort="max") to agy with model="gemini-4-carbon"
        // and effort=Unchanged must NOT fold the previous provider's stale "max" effort.
        let switched = mgr.handle_rpc(
            &reg,
            BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                bot_id: bot_a.id.clone(),
                name: None,
                description: None,
                emoji: FieldPatch::Unchanged,
                avatar: FieldPatch::Unchanged,
                agent_id: Some("agy".to_owned()),
                model: FieldPatch::Set("gemini-4-carbon".to_owned()),
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
            })),
        );
        if let BotRpcResponse::Bot(updated_bot) = switched {
            assert_ne!(
                updated_bot.effort.as_deref(),
                Some("max"),
                "stale claude effort must not carry into agy fold"
            );
        }

        // R2M-23: Verify UpdateBot on existing `agy` bot with effort="high" and model=Set("gemini-4-argon-low")
        // + effort=Unchanged passes None (not stale prev_bot.effort) to `antigravity_listed_family_selection`.
        let listed = vec![krill_proto::agent::AgyListedModel {
            id: "gemini-4-argon".to_owned(),
            efforts: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
        }];
        let model_patch = "gemini-4-argon-low";
        let req_effort = if krill_proto::agent::strip_agy_effort_suffix(model_patch)
            .1
            .is_some()
        {
            None
        } else {
            Some("high")
        };
        assert_eq!(
            krill_proto::agent::antigravity_listed_family_selection(
                model_patch,
                req_effort,
                &listed
            ),
            Some(("gemini-4-argon".to_owned(), "low".to_owned()))
        );

        // R2M-11: DeleteBot cancels pending MCP/Google OAuth listeners cleanly and shutdown cancels all.
        let del_resp = mgr.handle_rpc(
            &reg,
            BotRpcRequest::DeleteBot {
                bot_id: bot_b.id.clone(),
                delete_home: true,
            },
        );
        assert!(matches!(del_resp, BotRpcResponse::Deleted { .. }));

        mgr.shutdown();
        fs::remove_dir_all(&state).ok();
    }

    #[test]
    fn s1_07_10_shared_secrets_and_forget_actor() {
        use krill_proto::bots::CreateBotParams;

        let state =
            std::env::temp_dir().join(format!("krill-bots-swarm-s1-{}", std::process::id()));
        fs::remove_dir_all(&state).ok();
        fs::create_dir_all(&state).unwrap();
        let reg = Arc::new(Registry::new(None, Some(&state)));
        let mgr = BotsManager::new(state.clone(), Arc::downgrade(&reg));

        let create_params = |name: &str| CreateBotParams {
            name: name.to_owned(),
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
        };
        let bot_a = mgr.store.create_bot(create_params("Alpha"), 1_000).unwrap();
        let bot_b = mgr.store.create_bot(create_params("Beta"), 1_001).unwrap();

        // S1-07: DeleteBot must NOT wipe a shared secret_ref still referenced by another bot.
        mgr.secrets
            .set_ref("shared:org-token", Zeroizing::new("secret-val".to_owned()))
            .unwrap();
        let mut slack_cfg = BotConnectorConfig::default();
        slack_cfg.slack.bot_token_secret_ref = Some("shared:org-token".to_owned());
        mgr.store
            .set_connector_config(&bot_a.id, slack_cfg.clone(), 1_010)
            .unwrap();
        mgr.store
            .set_connector_config(&bot_b.id, slack_cfg, 1_011)
            .unwrap();

        // S1-10: ForgetMemoryFact from a bot session records `bot:<id>`, not `"owner"`.
        mgr.store
            .append_memory_fact_line(
                &bot_b.id,
                "Release target is v0.3.26",
                Some("2026-11-24"),
                None,
            )
            .unwrap();
        let beta_sid = SessionId(777);
        lock(mgr.runtime.sessions_for_test()).insert(
            beta_sid,
            crate::bots::runtime::BotSessionBinding {
                bot_id: bot_b.id.clone(),
                link_id: None,
                memory_snapshot: krill_proto::bots::MemorySnapshot::capture(
                    &mgr.store.get_memory(&bot_b.id).unwrap(),
                ),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 0,
                label: "Beta".to_owned(),
                created_at_ms: 1_020,
                owner_turn_pending: false,
                last_prompt: None,
                status_message_id: None,
                rate_limit_pause: None,
            },
        );
        let forget_res = mgr.handle_rpc_for_peer(
            &reg,
            Some(beta_sid),
            false,
            BotRpcRequest::ForgetMemoryFact {
                bot_id: bot_b.id.clone(),
                fact: "Release target is v0.3.26".to_owned(),
            },
        );
        assert!(matches!(
            forget_res,
            BotRpcResponse::MemoryUpdated {
                removed_count: 1,
                ..
            }
        ));
        let hist = mgr.store.get_memory_history(&bot_b.id, Some(10)).unwrap();
        let forget_entry = hist
            .iter()
            .find(|e| e.action == "forget")
            .expect("forget history");
        assert_eq!(forget_entry.actor, format!("bot:{}", bot_b.id.as_str()));

        // Now delete bot_a and confirm bot_b's shared secret survives.
        let del = mgr.handle_rpc(
            &reg,
            BotRpcRequest::DeleteBot {
                bot_id: bot_a.id.clone(),
                delete_home: true,
            },
        );
        assert!(matches!(del, BotRpcResponse::Deleted { .. }));
        assert_eq!(
            mgr.secrets
                .get_ref("shared:org-token")
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("secret-val"),
            "shared secret referenced by remaining bot must survive DeleteBot"
        );

        mgr.shutdown();
        fs::remove_dir_all(&state).ok();
    }
}
