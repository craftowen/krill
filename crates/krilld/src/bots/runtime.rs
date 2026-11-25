// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Bot session launch, durable runtime bindings (`runtime-bindings.json`), per-session preamble
//! serialization, hook integration (`is_turn_stop` & tool activity), reply extraction with
//! bounded JSONL flush retries, `MEMORY.md` post-turn silent-deletion recovery, group relays, and
//! inter-bot mail routing.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::{self, DirBuilder};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::SessionId;
use krill_proto::agent_history::ChatItemKind;
use krill_proto::bots::{
    BOT_QUIET_MARKER, BotConfig, BotId, BotRpcResponse, BotSessionRefusal, BotSessionSummary,
    MemorySnapshot, build_launch_preamble_in, find_bot_template, init_bot_home,
    session_belongs_to_bot,
};
use krill_proto::hook::HookEvent;
use krill_proto::orchestration::{MessageType, OrchestrationStore, SendParams};
use krill_proto::os::fs::DirBuilderExt as _;
use krill_proto::os::write_atomic_0600;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::agent::{BotMcpHttpServer, BotMcpLaunchConfig, BotMcpStdioServer};
use crate::bots::composio;
use crate::bots::connectors::{ConnectorManager, InboundExternalMessage, OutboundBotReply};
use crate::bots::groups::GroupCoordinator;
use crate::bots::secrets::{SecretVault, ensure_no_symlink_path};
use crate::bots::store::BotStore;
use crate::bots::tool_relay::BotToolRelay;
use crate::registry::Registry;
use crate::session::lock;
use crate::status;

const STACK_SIZE: usize = 128 * 1024;
const PREAMBLE_READY_WAIT: Duration = Duration::from_secs(15);
const PREAMBLE_SERIAL_WAIT: Duration = Duration::from_secs(17);
const JSONL_FLUSH_RETRIES: usize = 5;
const JSONL_FLUSH_SLEEP: Duration = Duration::from_millis(45);
const RUNTIME_BINDINGS_FILE: &str = "runtime-bindings.json";
const MAX_BINDINGS_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug)]
struct SessionTurnControl {
    preamble_gate: (Mutex<bool>, Condvar),
    delivery_lock: Mutex<()>,
}

impl SessionTurnControl {
    fn new(ready: bool) -> Arc<Self> {
        Arc::new(Self {
            preamble_gate: (Mutex::new(ready), Condvar::new()),
            delivery_lock: Mutex::new(()),
        })
    }

    fn mark_ready(&self) {
        let (mtx, cvar) = &self.preamble_gate;
        let mut guard = lock(mtx);
        *guard = true;
        drop(guard);
        cvar.notify_all();
    }

    fn wait_ready(&self) {
        let (mtx, cvar) = &self.preamble_gate;
        let deadline = Instant::now() + PREAMBLE_SERIAL_WAIT;
        let mut guard = lock(mtx);
        while !*guard {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            guard = cvar
                .wait_timeout(guard, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

struct ReadyDropGuard(Arc<SessionTurnControl>);

impl Drop for ReadyDropGuard {
    fn drop(&mut self) {
        self.0.mark_ready();
    }
}

/// Bot-specific launch inputs prepared when resuming a live bot session under a new account.
pub(crate) struct BotAccountSwitchLaunch {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub extra_env: Vec<(Vec<u8>, Vec<u8>)>,
    pub mcp_cfg: BotMcpLaunchConfig,
    pub grant_token: Option<Zeroizing<String>>,
    pub instructions: String,
}

/// Outcome of rebinding a live bot session from `old` to `new`, holding `old`'s prior relay
/// launch token so the caller can revoke it after retirement or roll back on error.
pub(crate) struct ReboundBotSession {
    pub previous_token: Option<Zeroizing<String>>,
}

/// Active mapping between a live Krill PTY session and the bot running inside it.
#[derive(Clone)]
pub struct BotSessionBinding {
    pub bot_id: BotId,
    pub link_id: Option<String>,
    pub memory_snapshot: MemorySnapshot,
    pub chat_offset: u64,
    pub explicit_reply_posted: bool,
    pub launch_token: Option<Zeroizing<String>>,
    pub pid: u32,
    pub label: String,
    pub created_at_ms: u64,
    pub owner_turn_pending: bool,
    pub last_prompt: Option<String>,
}

impl fmt::Debug for BotSessionBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotSessionBinding")
            .field("bot_id", &self.bot_id)
            .field("link_id", &self.link_id)
            .field("pid", &self.pid)
            .field("chat_offset", &self.chat_offset)
            .field("explicit_reply_posted", &self.explicit_reply_posted)
            .field("owner_turn_pending", &self.owner_turn_pending)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
struct QueuedBotTurn {
    prompt: String,
    turn_seq: u64,
    link_id: Option<String>,
}

#[derive(Debug, Default, Clone)]
struct SessionTurnMeta {
    group_key: Option<(String, String)>,
    turn_seq: u64,
    active_turn_seq: Option<u64>,
    pending_turns: VecDeque<QueuedBotTurn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedBotBinding {
    session_id: SessionId,
    bot_id: BotId,
    #[serde(default)]
    link_id: Option<String>,
    #[serde(default)]
    chat_offset: u64,
    #[serde(default)]
    pid: u32,
    #[serde(default)]
    label: String,
    #[serde(default)]
    created_at_ms: u64,
    #[serde(default)]
    owner_turn_pending: bool,
    #[serde(default)]
    last_prompt: Option<String>,
    #[serde(default)]
    group_key: Option<(String, String)>,
    #[serde(default)]
    turn_seq: u64,
    #[serde(default)]
    active_turn_seq: Option<u64>,
}

/// Structured error from starting a bot session (`b10792b6`, `e1779f39`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotStartError {
    Refused(BotSessionRefusal),
    Other(String),
}

impl BotStartError {
    #[must_use]
    pub fn refusal(&self) -> Option<&BotSessionRefusal> {
        match self {
            Self::Refused(r) => Some(r),
            Self::Other(_) => None,
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Refused(r) => r.message_en(None),
            Self::Other(msg) => msg.clone(),
        }
    }

    #[must_use]
    pub fn into_rpc_response(self) -> BotRpcResponse {
        match self {
            Self::Refused(r) => BotRpcResponse::SessionRefused(Box::new(r)),
            Self::Other(msg) => BotRpcResponse::Error(msg),
        }
    }
}

impl std::fmt::Display for BotStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

struct RuntimeCallbacks {
    registry: Weak<Registry>,
    store: Weak<BotStore>,
    groups: Weak<GroupCoordinator>,
    connectors: Mutex<Weak<ConnectorManager>>,
}

/// Shared interior state of [`BotRuntime`].
///
/// Canonical lock acquisition order (MUST NEVER be inverted, and NO disk/store/registry I/O while
/// holding any of these locks):
/// `bindings_io_lock -> sessions -> turn_controls -> turn_meta`.
struct RuntimeInner {
    bots_root: Option<PathBuf>,
    bindings_io_lock: Mutex<()>,
    sessions: Mutex<HashMap<SessionId, BotSessionBinding>>,
    turn_controls: Mutex<HashMap<SessionId, Arc<SessionTurnControl>>>,
    turn_meta: Mutex<HashMap<SessionId, SessionTurnMeta>>,
    taken_over_sessions: Mutex<HashSet<SessionId>>,
    callbacks: Mutex<Option<RuntimeCallbacks>>,
}

impl Default for RuntimeInner {
    fn default() -> Self {
        Self {
            bots_root: None,
            bindings_io_lock: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
            turn_controls: Mutex::new(HashMap::new()),
            turn_meta: Mutex::new(HashMap::new()),
            taken_over_sessions: Mutex::new(HashSet::new()),
            callbacks: Mutex::new(None),
        }
    }
}

impl fmt::Debug for RuntimeInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeInner")
            .field("bots_root", &self.bots_root)
            .finish_non_exhaustive()
    }
}

/// Runtime coordinator tracking live bot sessions and handling hooks/mail/replies.
#[derive(Debug, Default)]
pub struct BotRuntime {
    inner: Arc<RuntimeInner>,
}

impl BotRuntime {
    /// Creates an in-memory runtime (zero disk I/O).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a lazy runtime backed by `<bots_root>/runtime-bindings.json` (zero I/O until used).
    #[must_use]
    pub fn with_root(bots_root: PathBuf) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                bots_root: Some(bots_root),
                ..RuntimeInner::default()
            }),
        }
    }

    /// Wires weak daemon callbacks used to propagate async startup delivery failures (`MB-08`).
    pub(crate) fn attach_callbacks(
        &self,
        registry: Weak<Registry>,
        store: Weak<BotStore>,
        groups: Weak<GroupCoordinator>,
    ) {
        *lock(&self.inner.callbacks) = Some(RuntimeCallbacks {
            registry,
            store,
            groups,
            connectors: Mutex::new(Weak::new()),
        });
    }

    /// Wires the lazily-initialized [`ConnectorManager`] weak reference into runtime callbacks.
    pub(crate) fn attach_connectors(&self, connectors: Weak<ConnectorManager>) {
        if let Some(cb) = lock(&self.inner.callbacks).as_ref() {
            *lock(&cb.connectors) = connectors;
        }
    }

    /// Returns the currently bound `link_id` for `session_id` if owned by `bot_id`.
    #[must_use]
    pub(crate) fn session_link_for_bot(
        &self,
        session_id: SessionId,
        bot_id: &BotId,
    ) -> Option<String> {
        lock(&self.inner.sessions)
            .get(&session_id)
            .filter(|b| &b.bot_id == bot_id)
            .and_then(|b| b.link_id.clone())
    }

    #[cfg(test)]
    pub(crate) fn sessions_for_test(&self) -> &Mutex<HashMap<SessionId, BotSessionBinding>> {
        &self.inner.sessions
    }

    /// Returns `true` if `session_id` belongs to a bot session.
    #[must_use]
    pub fn is_bot_session(&self, session_id: SessionId) -> bool {
        lock(&self.inner.sessions).contains_key(&session_id)
    }

    /// Returns the [`BotId`] currently owning `session_id`, if any.
    #[must_use]
    pub fn bot_for_session(&self, session_id: SessionId) -> Option<BotId> {
        lock(&self.inner.sessions)
            .get(&session_id)
            .map(|b| b.bot_id.clone())
    }

    /// Rehydrates live bindings from `<bots_root>/runtime-bindings.json` and prunes dead,
    /// orphaned (deleted bot), or stale pre-creation bindings.
    pub fn rehydrate_from_disk(&self, store: &BotStore, registry: &Registry) -> usize {
        let Some(ref root) = self.inner.bots_root else {
            return 0;
        };
        let records = {
            let _io = lock(&self.inner.bindings_io_lock);
            let path = root.join(RUNTIME_BINDINGS_FILE);
            if ensure_no_symlink_path(root).is_err() || ensure_no_symlink_path(&path).is_err() {
                return 0;
            }
            let Ok(bytes) = fs::read(&path) else {
                return 0;
            };
            if bytes.len() > MAX_BINDINGS_BYTES {
                return 0;
            }
            let Ok(records) = serde_json::from_slice::<Vec<PersistedBotBinding>>(&bytes) else {
                return 0;
            };
            records
        };

        // Pre-read active bots, MEMORY.md, and query registry outside all runtime mutexes (MB-01).
        let active_bots: Option<HashMap<BotId, BotConfig>> = store
            .list_bots()
            .ok()
            .map(|list| list.into_iter().map(|b| (b.id.clone(), b)).collect());
        let mut live_records = Vec::new();
        let mut pruned_any = false;
        for rec in records {
            let bot_valid = active_bots.as_ref().is_none_or(|map| {
                map.get(&rec.bot_id)
                    .is_some_and(|bot| session_belongs_to_bot(bot.created_at_ms, rec.created_at_ms))
            });
            if registry.is_session_live(rec.session_id) && bot_valid {
                let pid = registry.session_pid(rec.session_id).unwrap_or(rec.pid);
                let memory = store.get_memory(&rec.bot_id).unwrap_or_default();
                live_records.push((rec, pid, memory));
            } else {
                pruned_any = true;
            }
        }

        let mut kept = 0_usize;
        {
            let mut sessions = lock(&self.inner.sessions);
            let mut controls = lock(&self.inner.turn_controls);
            let mut meta_map = lock(&self.inner.turn_meta);
            for (rec, pid, memory) in live_records {
                sessions.entry(rec.session_id).or_insert(BotSessionBinding {
                    bot_id: rec.bot_id,
                    link_id: rec.link_id,
                    memory_snapshot: MemorySnapshot::capture(&memory),
                    chat_offset: rec.chat_offset,
                    explicit_reply_posted: false,
                    launch_token: None,
                    pid,
                    label: rec.label,
                    created_at_ms: rec.created_at_ms,
                    owner_turn_pending: rec.owner_turn_pending,
                    last_prompt: rec.last_prompt,
                });
                controls
                    .entry(rec.session_id)
                    .or_insert_with(|| SessionTurnControl::new(true));
                meta_map
                    .entry(rec.session_id)
                    .or_insert_with(|| SessionTurnMeta {
                        group_key: rec.group_key,
                        turn_seq: rec.turn_seq,
                        active_turn_seq: rec.active_turn_seq,
                        pending_turns: VecDeque::new(),
                    });
                kept += 1;
            }
        }
        if pruned_any {
            self.inner.persist_bindings();
        }
        kept
    }

    /// Lists tracked bot sessions validated against active bots in `store`, excluding any
    /// orphaned bindings from deleted bots or stale sessions predating a bot's creation time.
    #[must_use]
    pub fn list_sessions_for_store(
        &self,
        store: &BotStore,
        registry: &Registry,
        bot_id: Option<&BotId>,
    ) -> Vec<BotSessionSummary> {
        let mut sessions = self.list_sessions(registry, bot_id);
        if let Ok(bots) = store.list_bots() {
            let by_id: HashMap<BotId, u64> =
                bots.into_iter().map(|b| (b.id, b.created_at_ms)).collect();
            sessions.retain(|s| {
                by_id.get(&s.bot_id).is_some_and(|&bot_created| {
                    session_belongs_to_bot(bot_created, s.created_at_ms)
                })
            });
        }
        sessions
    }

    /// Lists tracked bot sessions (pruning stale exited entries if desired, returning live state).
    #[must_use]
    pub fn list_sessions(
        &self,
        registry: &Registry,
        bot_id: Option<&BotId>,
    ) -> Vec<BotSessionSummary> {
        let snapshot: Vec<(SessionId, BotSessionBinding)> = lock(&self.inner.sessions)
            .iter()
            .filter(|(_, b)| bot_id.is_none_or(|id| &b.bot_id == id))
            .map(|(&sid, b)| (sid, b.clone()))
            .collect();
        let mut out: Vec<BotSessionSummary> = snapshot
            .into_iter()
            .map(|(session_id, b)| {
                let live = registry.is_session_live(session_id);
                let pid = registry.session_pid(session_id).unwrap_or(b.pid);
                let working = live && b.owner_turn_pending;
                BotSessionSummary {
                    session_id,
                    bot_id: b.bot_id,
                    link_id: b.link_id,
                    pid,
                    live,
                    working,
                    label: b.label,
                    created_at_ms: b.created_at_ms,
                }
            })
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.created_at_ms));
        out
    }

    /// Returns a live home session ID currently bound to `bot_id` (excluding external links and
    /// dedicated group chat sessions so private turns never collide with group or channel threads).
    #[must_use]
    pub fn live_session_for_bot(&self, registry: &Registry, bot_id: &BotId) -> Option<SessionId> {
        self.live_session_for_bot_since(registry, bot_id, 0)
    }

    fn live_session_for_bot_since(
        &self,
        registry: &Registry,
        bot_id: &BotId,
        bot_created_at_ms: u64,
    ) -> Option<SessionId> {
        // Canonical lock order: sessions -> turn_meta (MB-01).
        let mut candidates: Vec<(u64, SessionId)> = {
            let sessions = lock(&self.inner.sessions);
            let meta_map = lock(&self.inner.turn_meta);
            sessions
                .iter()
                .filter(|(sid, b)| {
                    &b.bot_id == bot_id
                        && b.link_id.is_none()
                        && session_belongs_to_bot(bot_created_at_ms, b.created_at_ms)
                        && meta_map.get(sid).is_none_or(|m| m.group_key.is_none())
                })
                .map(|(&sid, b)| (b.created_at_ms, sid))
                .collect()
        };
        candidates.sort_by_key(|(created, _)| std::cmp::Reverse(*created));
        candidates
            .into_iter()
            .map(|(_, sid)| sid)
            .find(|&sid| registry.is_session_live(sid))
    }

    fn live_group_session_for_bot(
        &self,
        registry: &Registry,
        bot_id: &BotId,
        group_id: &str,
        thread_id: &str,
    ) -> Option<SessionId> {
        // Canonical lock order: sessions -> turn_meta (MB-01).
        let candidates: Vec<SessionId> = {
            let sessions = lock(&self.inner.sessions);
            let meta_map = lock(&self.inner.turn_meta);
            sessions
                .iter()
                .filter(|(sid, b)| {
                    &b.bot_id == bot_id
                        && meta_map
                            .get(sid)
                            .and_then(|m| m.group_key.as_ref())
                            .is_some_and(|(gid, tid)| gid == group_id && tid == thread_id)
                })
                .map(|(&sid, _)| sid)
                .collect()
        };
        candidates
            .into_iter()
            .find(|&sid| registry.is_session_live(sid))
    }

    /// Starts a new bot session via normal `Registry` + `holder::Spawn`, injects bot env vars,
    /// `BotMcpLaunchConfig` (`/bot-tools/krill/mcp`, relayed HTTP & enabled stdio MCP tools,
    /// bot home directory access, `NO_PROXY`/`no_proxy`), and delivers the launch preamble + optional prompt.
    #[expect(clippy::too_many_arguments, reason = "bot launch orchestration inputs")]
    pub fn start_bot_session(
        &self,
        registry: &Registry,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        bot_id: &BotId,
        cwd_override: Option<&str>,
        prompt: Option<&str>,
        link_id: Option<String>,
    ) -> Result<(SessionId, u32), BotStartError> {
        self.start_bot_session_with_group(
            registry,
            store,
            secrets,
            relay,
            composio_base_url,
            bot_id,
            cwd_override,
            prompt,
            link_id,
            None,
        )
    }

    #[expect(clippy::too_many_arguments, reason = "bot launch orchestration inputs")]
    fn start_bot_session_with_group(
        &self,
        registry: &Registry,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        bot_id: &BotId,
        cwd_override: Option<&str>,
        prompt: Option<&str>,
        link_id: Option<String>,
        group_key: Option<(String, String)>,
    ) -> Result<(SessionId, u32), BotStartError> {
        self.start_bot_session_inner(
            registry,
            store,
            secrets,
            relay,
            composio_base_url,
            bot_id,
            cwd_override,
            prompt,
            link_id,
            group_key,
            None,
        )
    }

    #[expect(clippy::too_many_arguments, reason = "bot launch orchestration inputs")]
    fn start_bot_session_inner(
        &self,
        registry: &Registry,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        bot_id: &BotId,
        cwd_override: Option<&str>,
        prompt: Option<&str>,
        link_id: Option<String>,
        group_key: Option<(String, String)>,
        on_bound: Option<&mut dyn FnMut(SessionId, u64)>,
    ) -> Result<(SessionId, u32), BotStartError> {
        let bot = store
            .get_bot(bot_id)
            .map_err(|err| BotStartError::Other(err.to_string()))?
            .ok_or_else(|| BotStartError::Other(format!("bot not found: {}", bot_id.as_str())))?;
        if bot.paused {
            return Err(BotStartError::Refused(BotSessionRefusal::bot_paused(
                &bot.name,
            )));
        }

        let home = init_bot_home(store.root(), &bot, None)
            .map_err(|err| BotStartError::Other(err.to_string()))?;
        let soul = store.get_soul(bot_id).unwrap_or_default();
        let memory = store.get_memory(bot_id).unwrap_or_default();
        let launch_cwd = resolve_launch_cwd(&bot, &home, cwd_override);
        let preamble = build_launch_preamble_in(
            &bot,
            Some(&soul),
            Some(&memory),
            Some(&home),
            Some(&launch_cwd),
        );

        // Register Composio MCP route lazily if enabled on this bot.
        if bot.connectors.composio.enabled {
            composio::resolve_and_register_composio_with_root(
                &bot,
                secrets,
                relay,
                Some(store.root()),
                composio_base_url,
                now_ms(),
            )
            .ok();
        }

        let grant = match relay.prepare_launch_for_bot(&bot) {
            Ok(Some(g)) => Some(g),
            Ok(None) => Some(
                relay
                    .mint_grant_for_creds(&bot.id, vec!["krill".to_owned()])
                    .map_err(|err| {
                        BotStartError::Other(format!("failed to prepare bot tool relay: {err}"))
                    })?,
            ),
            Err(err) => {
                return Err(BotStartError::Other(format!(
                    "failed to prepare bot tool relay: {err}"
                )));
            }
        };

        let mut extra_env: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (
                b"KRILL_BOT_ID".to_vec(),
                bot.id.as_str().as_bytes().to_vec(),
            ),
            (
                b"KRILL_BOT_HOME".to_vec(),
                home.to_string_lossy().as_bytes().to_vec(),
            ),
        ];
        if let Some(ref g) = grant {
            extra_env.push((
                b"KRILL_BOT_TOOLS_URL".to_vec(),
                g.base_url.as_bytes().to_vec(),
            ));
            extra_env.push((
                b"KRILL_BOT_TOOLS_TOKEN".to_vec(),
                g.token.as_bytes().to_vec(),
            ));
        }

        let mut mcp_cfg = build_bot_mcp_launch_config(&bot, &home, grant.as_ref(), secrets);
        mcp_cfg.instructions = Some(preamble.clone());

        let spawn_res = registry.spawn_bot_agent_session(
            &bot.agent_id,
            bot.model.as_deref(),
            bot.effort.as_deref(),
            bot.permission_mode,
            &launch_cwd,
            extra_env,
            Some(&mcp_cfg),
        );
        let (session_id, pid, instructions_given) = match spawn_res {
            Ok(ok) => ok,
            Err(err) => {
                if let Some(ref g) = grant {
                    relay.revoke_token(&g.token);
                }
                return Err(err);
            }
        };
        if let Some(ref g) = grant {
            relay.bind_grant_session(&g.token, session_id, link_id.as_deref());
        }

        let trimmed_prompt = prompt
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let has_prompt = trimmed_prompt.is_some();

        let ctrl = SessionTurnControl::new(false);
        let initial_seq = u64::from(has_prompt);
        let label = if let Some(ref l) = link_id {
            format!("{} ({l})", bot.name)
        } else if let Some((ref gid, _)) = group_key {
            format!("{} (group:{gid})", bot.name)
        } else {
            bot.name.clone()
        };
        let binding = BotSessionBinding {
            bot_id: bot.id.clone(),
            link_id,
            memory_snapshot: MemorySnapshot::capture(&memory),
            chat_offset: 0,
            explicit_reply_posted: false,
            launch_token: grant.map(|g| g.token),
            pid,
            label,
            created_at_ms: now_ms(),
            owner_turn_pending: has_prompt,
            last_prompt: trimmed_prompt.clone(),
        };
        {
            // Canonical lock order: sessions -> turn_controls -> turn_meta (MB-01).
            lock(&self.inner.sessions).insert(session_id, binding);
            lock(&self.inner.turn_controls).insert(session_id, Arc::clone(&ctrl));
            lock(&self.inner.turn_meta).insert(
                session_id,
                SessionTurnMeta {
                    group_key,
                    turn_seq: initial_seq,
                    active_turn_seq: has_prompt.then_some(initial_seq),
                    pending_turns: VecDeque::new(),
                },
            );
        }
        self.inner.persist_bindings();

        // Deliver launch preamble (+ optional initial prompt) once the agent PTY is ready, holding
        // `ctrl.delivery_lock` until `ctrl.preamble_gate` flips ready so concurrent dispatches wait cleanly.
        // Agents that took the preamble as their system prompt get only the owner's prompt (or nothing:
        // the bot speaks first when its owner does, as in Orca). Others still get it as their first input.
        let initial_input = match (instructions_given, trimmed_prompt) {
            (true, prompt) => prompt,
            (false, Some(user_prompt)) => Some(format!("{preamble}\n\n---\n\n{user_prompt}")),
            (false, None) => Some(preamble),
        };
        let tap_opt = registry.session_tap(session_id);
        if has_prompt && tap_opt.is_none() {
            ctrl.mark_ready();
            registry.kill_bot_session(session_id).ok();
            self.on_session_removed(relay, session_id);
            return Err(BotStartError::Other(
                "session PTY tap unavailable for initial prompt delivery".to_owned(),
            ));
        }

        // Notify caller (group coordinator / connector router) BEFORE spawning `bot-preamble`
        // so an early asynchronous `fail_initial_prompt_delivery` can never beat registration.
        if let Some(cb) = on_bound {
            cb(session_id, initial_seq);
        }

        if let Some(tap) = tap_opt {
            let bytes = initial_input.map(String::into_bytes);
            let ctrl_thread = Arc::clone(&ctrl);
            let inner_thread = Arc::clone(&self.inner);
            let bot_id_thread = bot.id.clone();
            let is_agy = krill_proto::agent::canonical_id(&bot.agent_id) == "agy";
            let spawn_res = std::thread::Builder::new()
                .name(format!("bot-preamble-{}", session_id.0))
                .stack_size(STACK_SIZE)
                .spawn(move || {
                    let failure_reason: Option<String> = {
                        let _ready_guard = ReadyDropGuard(Arc::clone(&ctrl_thread));
                        let _serial = lock(&ctrl_thread.delivery_lock);
                        let deadline = Instant::now() + PREAMBLE_READY_WAIT;
                        let mut ready_seen = false;
                        while Instant::now() < deadline {
                            let p = tap.pulse();
                            if p.closed {
                                break;
                            }
                            if is_agy {
                                // Antigravity CLI pauses >150ms during "Signing in..." before rendering
                                // its interactive `> ` composer; wait for the composer to appear.
                                if p.end > 0
                                    && tap.quiet_ms() >= 150
                                    && crate::mobile::agy_composer_ready(&tap.capture().0)
                                {
                                    ready_seen = true;
                                    break;
                                }
                            } else if p.end > 0 && tap.quiet_ms() >= 150 {
                                ready_seen = true;
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        if let Some(bytes) = bytes {
                            if tap.pulse().closed {
                                Some("pty closed before initial prompt delivery".to_owned())
                            } else if is_agy && !ready_seen {
                                Some("agy composer did not become ready within timeout".to_owned())
                            } else if let Err(err) =
                                Registry::deliver_input_to_tap(&tap, &bytes, true)
                            {
                                Some(format!("deliver_input_to_tap failed: {err}"))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    };
                    // `_serial` and `_ready_guard` are dropped BEFORE propagating failure /
                    // draining queued turns so `deliver_serialized_input` never deadlocks (MB-08).
                    if let Some(reason) = failure_reason {
                        if has_prompt {
                            inner_thread.fail_initial_prompt_delivery(
                                &bot_id_thread,
                                session_id,
                                &reason,
                            );
                        } else {
                            tracing::warn!(
                                session = session_id.0,
                                reason = %reason,
                                "bot launch preamble delivery failed"
                            );
                        }
                    }
                });
            if let Err(err) = spawn_res {
                ctrl.mark_ready();
                if has_prompt {
                    registry.kill_bot_session(session_id).ok();
                    self.on_session_removed(relay, session_id);
                    return Err(BotStartError::Other(format!(
                        "failed to spawn initial prompt delivery thread: {err}"
                    )));
                }
            }
        } else {
            ctrl.mark_ready();
        }

        Ok((session_id, pid))
    }

    /// Dispatches an inbound external message into an existing or newly spawned bot session.
    #[expect(
        clippy::too_many_arguments,
        reason = "external message dispatch context"
    )]
    pub fn dispatch_external_message(
        &self,
        registry: &Registry,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        msg: &InboundExternalMessage,
        now_ms: u64,
    ) -> io::Result<(krill_proto::bots::ExternalConversationLink, SessionId, bool)> {
        self.dispatch_external_message_with_hooks(
            registry,
            store,
            secrets,
            relay,
            composio_base_url,
            msg,
            now_ms,
            None,
            None,
        )
    }

    /// Dispatches an inbound external message with optional pre-delivery registration and rollback hooks.
    #[expect(
        clippy::too_many_arguments,
        reason = "external message dispatch context"
    )]
    pub fn dispatch_external_message_with_hooks(
        &self,
        registry: &Registry,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        msg: &InboundExternalMessage,
        now_ms: u64,
        on_bound: Option<&mut dyn FnMut(SessionId, Option<u64>)>,
        on_rollback: Option<&mut dyn FnMut(SessionId, Option<u64>)>,
    ) -> io::Result<(krill_proto::bots::ExternalConversationLink, SessionId, bool)> {
        let mut link = store.lookup_or_create_link(
            &msg.bot_id,
            msg.source,
            msg.workspace_id.as_deref(),
            &msg.channel_id,
            msg.thread_id.as_deref(),
            msg.acts_for.clone(),
            now_ms,
        )?;

        let formatted = format_external_prompt(msg);

        // Reuse bound session if still alive and NOT taken over by the human owner, queueing
        // behind any in-flight turn so active turn state is never clobbered (MB-10 / R2M-04).
        if let Some(sid) = link.session_id
            && registry.is_session_live(sid)
            && !self.is_taken_over(sid)
        {
            self.ensure_binding_for_live_session(
                registry,
                store,
                &msg.bot_id,
                sid,
                Some(link.id.clone()),
                now_ms,
            );
            relay.update_session_link(&msg.bot_id, sid, &link.id);
            let mut bound_bridge =
                on_bound.map(|cb| move |s: SessionId, seq: u64| cb(s, Some(seq)));
            let mut rollback_bridge =
                on_rollback.map(|cb| move |s: SessionId, seq: u64| cb(s, Some(seq)));
            self.deliver_or_queue_turn_with_link_hooks(
                registry,
                store,
                sid,
                &msg.bot_id,
                &formatted,
                Some(link.id.clone()),
                bound_bridge
                    .as_mut()
                    .map(|f| f as &mut dyn FnMut(SessionId, u64)),
                rollback_bridge
                    .as_mut()
                    .map(|f| f as &mut dyn FnMut(SessionId, u64)),
            )
            .map_err(io::Error::other)?;
            return Ok((link, sid, false));
        }

        let mut bound_bridge = on_bound.map(|cb| move |s: SessionId, seq: u64| cb(s, Some(seq)));
        let (sid, _pid) = self
            .start_bot_session_inner(
                registry,
                store,
                secrets,
                relay,
                composio_base_url,
                &msg.bot_id,
                None,
                Some(&formatted),
                Some(link.id.clone()),
                None,
                bound_bridge
                    .as_mut()
                    .map(|f| f as &mut dyn FnMut(SessionId, u64)),
            )
            .map_err(|err| io::Error::other(err.message()))?;
        if let Some(updated) = store.bind_link_session(&link.id, sid, now_ms)? {
            link = updated;
        }
        Ok((link, sid, true))
    }

    /// Delivers a prompt into a bot's live session (or wakes/spawns a session if offline).
    #[expect(clippy::too_many_arguments, reason = "bot wake context")]
    pub fn deliver_or_wake_bot(
        &self,
        registry: &Registry,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        bot_id: &BotId,
        prompt: &str,
        now_ms: u64,
    ) -> Result<(SessionId, bool), String> {
        let bot_created_at_ms = store
            .get_bot(bot_id)
            .ok()
            .flatten()
            .map_or(0, |b| b.created_at_ms);
        if let Some(sid) = self.live_session_for_bot_since(registry, bot_id, bot_created_at_ms) {
            self.deliver_turn(registry, store, sid, bot_id, prompt)?;
            let _ = now_ms;
            return Ok((sid, false));
        }
        let (sid, _pid) = self
            .start_bot_session(
                registry,
                store,
                secrets,
                relay,
                composio_base_url,
                bot_id,
                None,
                Some(prompt),
                None,
            )
            .map_err(|err| err.message())?;
        Ok((sid, true))
    }

    /// Delivers a group-chat turn into a dedicated `(group_id, thread_id)` session for `bot_id`
    /// (isolated from private home sessions) and returns `(session_id, turn_seq, started_new)`.
    /// If that group session already has a turn in flight, queues the prompt so it runs serially
    /// after the active turn finishes instead of interleaving into the PTY.
    #[expect(clippy::too_many_arguments, reason = "group bot turn dispatch context")]
    pub fn deliver_group_turn(
        &self,
        registry: &Registry,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        bot_id: &BotId,
        group_id: &str,
        thread_id: &str,
        prompt: &str,
        now_ms: u64,
    ) -> Result<(SessionId, u64, bool), String> {
        self.deliver_group_turn_with_hooks(
            registry,
            store,
            secrets,
            relay,
            composio_base_url,
            bot_id,
            group_id,
            thread_id,
            prompt,
            now_ms,
            None,
            None,
        )
    }

    #[expect(clippy::too_many_arguments, reason = "group bot turn dispatch context")]
    pub(crate) fn deliver_group_turn_with_hooks(
        &self,
        registry: &Registry,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        bot_id: &BotId,
        group_id: &str,
        thread_id: &str,
        prompt: &str,
        now_ms: u64,
        on_bound: Option<&mut dyn FnMut(SessionId, u64)>,
        on_rollback: Option<&mut dyn FnMut(SessionId, u64)>,
    ) -> Result<(SessionId, u64, bool), String> {
        let _ = now_ms;
        if let Some(sid) = self.live_group_session_for_bot(registry, bot_id, group_id, thread_id) {
            let seq = self.deliver_or_queue_turn_with_link_hooks(
                registry,
                store,
                sid,
                bot_id,
                prompt,
                None,
                on_bound,
                on_rollback,
            )?;
            return Ok((sid, seq, false));
        }
        let mut recorded_seq: Option<u64> = None;
        let mut on_bound = on_bound;
        let mut bound_wrapper = |sid: SessionId, seq: u64| {
            recorded_seq = Some(seq);
            if let Some(cb) = on_bound.as_deref_mut() {
                cb(sid, seq);
            }
        };
        let (sid, _pid) = self
            .start_bot_session_inner(
                registry,
                store,
                secrets,
                relay,
                composio_base_url,
                bot_id,
                None,
                Some(prompt),
                None,
                Some((group_id.to_owned(), thread_id.to_owned())),
                Some(&mut bound_wrapper),
            )
            .map_err(|err| err.message())?;
        let seq = recorded_seq.unwrap_or_else(|| {
            lock(&self.inner.turn_meta)
                .get(&sid)
                .and_then(|m| m.active_turn_seq)
                .unwrap_or(1)
        });
        Ok((sid, seq, true))
    }

    /// Delivers `prompt` into the live bot session `sid` as the bot's next turn.
    pub fn deliver_turn(
        &self,
        registry: &Registry,
        store: &BotStore,
        sid: SessionId,
        bot_id: &BotId,
        prompt: &str,
    ) -> Result<(), String> {
        self.deliver_or_queue_turn(registry, store, sid, bot_id, prompt)
            .map(|_| ())
    }

    /// Delivers `prompt` into live session `sid` immediately if idle, or queues it behind any
    /// in-flight turn so earlier turns never collide with or hijack later turns. Returns the
    /// assigned `turn_seq`.
    pub fn deliver_or_queue_turn(
        &self,
        registry: &Registry,
        store: &BotStore,
        sid: SessionId,
        bot_id: &BotId,
        prompt: &str,
    ) -> Result<u64, String> {
        self.deliver_or_queue_turn_with_link(registry, store, sid, bot_id, prompt, None)
    }

    fn deliver_or_queue_turn_with_link(
        &self,
        registry: &Registry,
        store: &BotStore,
        sid: SessionId,
        bot_id: &BotId,
        prompt: &str,
        link_id: Option<String>,
    ) -> Result<u64, String> {
        self.deliver_or_queue_turn_with_link_hooks(
            registry, store, sid, bot_id, prompt, link_id, None, None,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "turn delivery with lifecycle hooks"
    )]
    fn deliver_or_queue_turn_with_link_hooks(
        &self,
        registry: &Registry,
        store: &BotStore,
        sid: SessionId,
        bot_id: &BotId,
        prompt: &str,
        link_id: Option<String>,
        on_bound: Option<&mut dyn FnMut(SessionId, u64)>,
        on_rollback: Option<&mut dyn FnMut(SessionId, u64)>,
    ) -> Result<u64, String> {
        type PrevTurnState = (bool, bool, Option<String>, Option<String>, Option<u64>);
        enum Action {
            Queued(u64),
            Deliver { seq: u64, prev: PrevTurnState },
        }
        // Pre-read MEMORY.md outside all runtime locks (MB-01).
        let fresh_memory = store.get_memory(bot_id).ok();
        let action = {
            // Canonical lock order: sessions -> turn_meta (MB-01).
            let mut sessions = lock(&self.inner.sessions);
            let Some(binding) = sessions.get_mut(&sid) else {
                return Err(format!(
                    "session {} is not bound to bot {}",
                    sid.0,
                    bot_id.as_str()
                ));
            };
            if &binding.bot_id != bot_id {
                return Err(format!("session {} belongs to another bot", sid.0));
            }
            let mut meta_map = lock(&self.inner.turn_meta);
            let meta = meta_map.entry(sid).or_default();
            meta.turn_seq = meta.turn_seq.saturating_add(1);
            let seq = meta.turn_seq;
            if binding.owner_turn_pending {
                meta.pending_turns.push_back(QueuedBotTurn {
                    prompt: prompt.to_owned(),
                    turn_seq: seq,
                    link_id,
                });
                Action::Queued(seq)
            } else {
                let prev = (
                    binding.explicit_reply_posted,
                    binding.owner_turn_pending,
                    binding.link_id.clone(),
                    binding.last_prompt.clone(),
                    meta.active_turn_seq,
                );
                binding.explicit_reply_posted = false;
                binding.owner_turn_pending = true;
                if let Some(ref lid) = link_id {
                    binding.link_id = Some(lid.clone());
                }
                binding.last_prompt = Some(prompt.to_owned());
                if let Some(ref mem) = fresh_memory {
                    binding.memory_snapshot = MemorySnapshot::capture(mem);
                }
                meta.active_turn_seq = Some(seq);
                Action::Deliver { seq, prev }
            }
        };

        match action {
            Action::Queued(seq) => {
                if let Some(cb) = on_bound {
                    cb(sid, seq);
                }
                self.inner.persist_bindings();
                Ok(seq)
            }
            Action::Deliver { seq, prev } => {
                if let Some(cb) = on_bound {
                    cb(sid, seq);
                }
                self.inner.persist_bindings();
                if let Err(err) =
                    self.inner
                        .deliver_serialized_input(registry, sid, prompt.as_bytes(), true)
                {
                    let (prev_explicit, prev_pending, prev_link, prev_prompt, prev_seq) = prev;
                    let has_queued = {
                        let mut sessions = lock(&self.inner.sessions);
                        let mut meta_map = lock(&self.inner.turn_meta);
                        if let Some(binding) = sessions.get_mut(&sid) {
                            binding.explicit_reply_posted = prev_explicit;
                            binding.owner_turn_pending = prev_pending;
                            binding.link_id = prev_link;
                            binding.last_prompt = prev_prompt;
                        }
                        if let Some(meta) = meta_map.get_mut(&sid) {
                            if meta.active_turn_seq == Some(seq) {
                                meta.active_turn_seq = prev_seq;
                            }
                            !meta.pending_turns.is_empty()
                        } else {
                            false
                        }
                    };
                    self.inner.persist_bindings();
                    if let Some(rb) = on_rollback {
                        rb(sid, seq);
                    }
                    if has_queued {
                        let (groups, connectors) = {
                            let cb = lock(&self.inner.callbacks);
                            match cb.as_ref() {
                                Some(c) => (c.groups.upgrade(), lock(&c.connectors).upgrade()),
                                None => (None, None),
                            }
                        };
                        self.inner.drain_next_queued_turn(
                            registry,
                            store,
                            groups.as_deref(),
                            connectors.as_deref(),
                            bot_id,
                            sid,
                            now_ms(),
                        );
                    }
                    return Err(err.to_string());
                }
                Ok(seq)
            }
        }
    }

    /// True while the bot session `sid` is still working on a delivered turn.
    pub fn turn_running(&self, sid: SessionId) -> bool {
        lock(&self.inner.sessions)
            .get(&sid)
            .is_some_and(|b| b.owner_turn_pending)
    }

    /// Refreshes the in-memory `MEMORY.md` baseline snapshot for all live sessions of `bot_id`
    /// after an owner-authorized memory edit/forget/undo/tidy so the next `Stop` hook does not
    /// misclassify intentional changes as silent bot deletions (`system:heal`).
    pub fn refresh_memory_snapshot(&self, bot_id: &BotId, content: &str) {
        let mut sessions = lock(&self.inner.sessions);
        for binding in sessions.values_mut() {
            if &binding.bot_id == bot_id {
                binding.memory_snapshot = MemorySnapshot::capture(content);
            }
        }
    }

    /// Records that the human owner directly submitted a prompt into a live bot session (via
    /// desktop `SendInput` or mobile `Action::Prompt`), setting `owner_turn_pending` and
    /// `last_prompt` so `working`, `first_run_completed`, and `SendBotRunAgain` stay accurate.
    ///
    /// When a turn is already in flight (`owner_turn_pending == true`), owner steering prompts
    /// must not advance `active_turn_seq`, reset `explicit_reply_posted`, or clobber
    /// `last_prompt` so the active connector/group turn still completes with its original
    /// `turn_seq` (R4D-01).
    pub fn on_owner_prompt_submitted(&self, session_id: SessionId, prompt: &str) {
        let mut changed = false;
        {
            let mut sessions = lock(&self.inner.sessions);
            if let Some(binding) = sessions.get_mut(&session_id)
                && !binding.owner_turn_pending
            {
                let trimmed = prompt.trim();
                binding.owner_turn_pending = true;
                binding.explicit_reply_posted = false;
                if !trimmed.is_empty() {
                    binding.last_prompt = Some(trimmed.to_owned());
                }
                let mut meta_map = lock(&self.inner.turn_meta);
                let meta = meta_map.entry(session_id).or_default();
                meta.turn_seq = meta.turn_seq.saturating_add(1);
                meta.active_turn_seq = Some(meta.turn_seq);
                changed = true;
            }
        }
        if changed {
            self.inner.persist_bindings();
        }
    }

    /// Stops and unbinds ALL live sessions for `bot_id` without requiring `bot_id` to still exist
    /// in `BotStore` (used during `DeleteBot`). Also inspects and purges `runtime-bindings.json`
    /// directly so a delete racing ahead of `rehydrate_from_disk` still kills and removes every
    /// persisted session for `bot_id`.
    pub fn stop_all_for_bot(
        &self,
        registry: &Registry,
        relay: &BotToolRelay,
        bot_id: &BotId,
    ) -> usize {
        let mut targets: Vec<SessionId> = lock(&self.inner.sessions)
            .iter()
            .filter(|(_, b)| &b.bot_id == bot_id)
            .map(|(&sid, _)| sid)
            .collect();
        let disk_targets = self.purge_persisted_bindings_for_bot(bot_id);
        for sid in disk_targets {
            if !targets.contains(&sid) {
                targets.push(sid);
            }
        }
        let count = targets.len();
        for sid in targets {
            registry.kill_bot_session(sid).ok();
            self.on_session_removed(relay, sid);
        }
        count
    }

    fn purge_persisted_bindings_for_bot(&self, bot_id: &BotId) -> Vec<SessionId> {
        let Some(ref root) = self.inner.bots_root else {
            return Vec::new();
        };
        let _io = lock(&self.inner.bindings_io_lock);
        let path = root.join(RUNTIME_BINDINGS_FILE);
        if ensure_no_symlink_path(root).is_err() || ensure_no_symlink_path(&path).is_err() {
            return Vec::new();
        }
        let Ok(bytes) = fs::read(&path) else {
            return Vec::new();
        };
        if bytes.len() > MAX_BINDINGS_BYTES {
            return Vec::new();
        }
        let Ok(mut records) = serde_json::from_slice::<Vec<PersistedBotBinding>>(&bytes) else {
            return Vec::new();
        };
        let mut removed_sids = Vec::new();
        records.retain(|rec| {
            if &rec.bot_id == bot_id {
                removed_sids.push(rec.session_id);
                false
            } else {
                true
            }
        });
        if !removed_sids.is_empty()
            && let Ok(mut out) = serde_json::to_vec_pretty(&records)
        {
            out.push(b'\n');
            write_atomic_0600(&path, &out).ok();
        }
        removed_sids
    }

    /// Stops all live sessions for `bot_id` (or a specific `session_id`) so the next run starts clean.
    pub fn reset_chat(
        &self,
        registry: &Registry,
        store: &BotStore,
        relay: &BotToolRelay,
        bot_id: &BotId,
        session_id: Option<SessionId>,
        now_ms: u64,
    ) -> io::Result<()> {
        if store.get_bot(bot_id)?.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("bot not found: {}", bot_id.as_str()),
            ));
        }
        let targets: Vec<SessionId> = lock(&self.inner.sessions)
            .iter()
            .filter(|(sid, b)| &b.bot_id == bot_id && session_id.is_none_or(|want| want == **sid))
            .map(|(sid, _)| *sid)
            .collect();
        if let Some(want) = session_id
            && targets.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("session {} is not bound to bot {}", want.0, bot_id.as_str()),
            ));
        }
        for sid in targets {
            registry.kill_bot_session(sid).ok();
            self.on_session_removed(relay, sid);
        }
        let _ = now_ms;
        Ok(())
    }

    /// Stops a single bot PTY session.
    pub fn stop_session(
        &self,
        registry: &Registry,
        relay: &BotToolRelay,
        bot_id: &BotId,
        session_id: SessionId,
    ) -> io::Result<()> {
        let owns = lock(&self.inner.sessions)
            .get(&session_id)
            .is_some_and(|b| &b.bot_id == bot_id);
        if !owns {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "session {} is not bound to bot {}",
                    session_id.0,
                    bot_id.as_str()
                ),
            ));
        }
        let kill_res = registry.kill_bot_session(session_id);
        self.on_session_removed(relay, session_id);
        if let Err(err) = kill_res
            && err.kind() != io::ErrorKind::NotFound
        {
            return Err(err);
        }
        Ok(())
    }

    /// Retries the bot's onboarding / kickoff turn cleanly.
    #[expect(clippy::too_many_arguments, reason = "bot kickoff retry context")]
    pub fn retry_kickoff(
        &self,
        registry: &Registry,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        bot_id: &BotId,
        now_ms: u64,
    ) -> Result<(SessionId, u32), BotStartError> {
        let bot = store
            .get_bot(bot_id)
            .map_err(|err| BotStartError::Other(err.to_string()))?
            .ok_or_else(|| BotStartError::Other(format!("bot not found: {}", bot_id.as_str())))?;
        if bot.paused {
            return Err(BotStartError::Refused(BotSessionRefusal::bot_paused(
                &bot.name,
            )));
        }
        self.reset_chat(registry, store, relay, bot_id, None, now_ms)
            .map_err(|err| BotStartError::Other(err.to_string()))?;
        let kickoff_prompt = bot
            .template_id
            .as_deref()
            .and_then(find_bot_template)
            .map(|t| t.first_chat.to_owned())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "Owner requested your kickoff turn for {}. Introduce yourself concisely, summarize what you can watch or help with from SOUL.md, and note any setup still needed.",
                    bot.name
                )
            });
        self.start_bot_session(
            registry,
            store,
            secrets,
            relay,
            composio_base_url,
            bot_id,
            None,
            Some(&kickoff_prompt),
            None,
        )
    }

    /// Unbinds a live PTY session from autonomous bot turn handling so the human owner can
    /// directly operate inside that terminal workspace without autonomous hook replies interfering.
    pub fn take_over_workspace(
        &self,
        registry: &Registry,
        relay: &BotToolRelay,
        bot_id: &BotId,
        session_id: SessionId,
    ) -> io::Result<()> {
        let binding = {
            let mut sessions = lock(&self.inner.sessions);
            match sessions.get(&session_id) {
                Some(b) if &b.bot_id == bot_id => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "session {} is not bound to bot {}",
                            session_id.0,
                            bot_id.as_str()
                        ),
                    ));
                }
            }
            if !registry.is_session_live(session_id) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "session is no longer live",
                ));
            }
            lock(&self.inner.taken_over_sessions).insert(session_id);
            sessions.remove(&session_id)
        };
        lock(&self.inner.turn_controls).remove(&session_id);
        lock(&self.inner.turn_meta).remove(&session_id);
        if let Some(b) = binding
            && let Some(token) = b.launch_token
        {
            relay.revoke_token(&token);
        }
        self.inner.persist_bindings();
        registry.release_bot_session(session_id);
        Ok(())
    }

    fn is_taken_over(&self, session_id: SessionId) -> bool {
        lock(&self.inner.taken_over_sessions).contains(&session_id)
    }

    /// Re-sends the most recent prompt on a live bot session (`SendBotRunAgain`).
    /// Routes through [`Self::deliver_or_queue_turn_with_link_hooks`] so an in-flight turn is
    /// never interleaved or clobbered.
    pub fn send_run_again(
        &self,
        registry: &Registry,
        store: &BotStore,
        bot_id: &BotId,
        session_id: SessionId,
    ) -> io::Result<()> {
        if !registry.is_session_live(session_id) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "bot session is not live",
            ));
        }
        let prompt = {
            let sessions = lock(&self.inner.sessions);
            let Some(binding) = sessions.get(&session_id) else {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "bot session binding not found",
                ));
            };
            if &binding.bot_id != bot_id {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "session belongs to another bot",
                ));
            }
            let Some(last) = binding
                .last_prompt
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
            else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "no previous prompt recorded for this session",
                ));
            };
            last
        };
        self.deliver_or_queue_turn_with_link_hooks(
            registry, store, session_id, bot_id, &prompt, None, None, None,
        )
        .map(|_| ())
        .map_err(io::Error::other)
    }

    /// Marks that `bot_id` explicitly called `PostBotReply` during its current turn.
    pub fn mark_explicit_reply(&self, bot_id: &BotId, link_id: Option<&str>) {
        self.mark_explicit_reply_for_session(bot_id, None, link_id);
    }

    /// Marks `explicit_reply_posted` scoped to `caller_session` (if known) or `link_id` so
    /// concurrent unrelated chats of the same bot are never suppressed.
    pub fn mark_explicit_reply_for_session(
        &self,
        bot_id: &BotId,
        caller_session: Option<SessionId>,
        link_id: Option<&str>,
    ) {
        let mut map = lock(&self.inner.sessions);
        if let Some(sid) = caller_session {
            if let Some(binding) = map.get_mut(&sid)
                && &binding.bot_id == bot_id
            {
                binding.explicit_reply_posted = true;
            }
            return;
        }
        if let Some(lid) = link_id {
            for binding in map.values_mut() {
                if &binding.bot_id == bot_id && binding.link_id.as_deref() == Some(lid) {
                    binding.explicit_reply_posted = true;
                }
            }
            return;
        }
        // Without session or link scoping, only mark if this bot has a single active session.
        let matching: Vec<SessionId> = map
            .iter()
            .filter(|(_, b)| &b.bot_id == bot_id)
            .map(|(&sid, _)| sid)
            .collect();
        if matching.len() == 1
            && let Some(sid) = matching.first()
            && let Some(binding) = map.get_mut(sid)
        {
            binding.explicit_reply_posted = true;
        }
    }

    /// Hook callback invoked by `Registry` on every agent hook event (socket or spool).
    pub fn on_hook_event(
        &self,
        registry: &Registry,
        store: &BotStore,
        connectors: Option<&ConnectorManager>,
        session_id: SessionId,
        event: &HookEvent,
    ) {
        self.on_hook_event_with_groups(registry, store, connectors, None, session_id, event);
    }

    /// Hook callback including optional [`GroupCoordinator`] for group-chat reply completion.
    /// Returns `Some(error_text)` when an idle Antigravity `Stop` ended with `terminationReason: "ERROR"`.
    pub fn on_hook_event_with_groups(
        &self,
        registry: &Registry,
        store: &BotStore,
        connectors: Option<&ConnectorManager>,
        groups: Option<&GroupCoordinator>,
        session_id: SessionId,
        event: &HookEvent,
    ) -> Option<String> {
        let binding_opt = lock(&self.inner.sessions).get(&session_id).cloned();
        let binding = binding_opt?;

        // 1. Track direct interactive prompts (`UserPromptSubmit` / `BeforeAgent` / `beforeSubmitPrompt`)
        // so bot chats opened without an initial prompt still report `working: true`, mark
        // `first_run_completed`, and support `SendBotRunAgain`.
        if let PromptSubmitEvent::Submitted(prompt_opt) = extract_prompt_submit(&event.payload) {
            let mut changed = false;
            {
                let mut sessions = lock(&self.inner.sessions);
                if let Some(entry) = sessions.get_mut(&session_id) {
                    if !entry.owner_turn_pending {
                        entry.owner_turn_pending = true;
                        entry.explicit_reply_posted = false;
                        let mut meta_map = lock(&self.inner.turn_meta);
                        let meta = meta_map.entry(session_id).or_default();
                        meta.turn_seq = meta.turn_seq.saturating_add(1);
                        meta.active_turn_seq = Some(meta.turn_seq);
                        changed = true;
                    }
                    if let Some(prompt) = prompt_opt
                        && !prompt.trim().is_empty()
                    {
                        entry.last_prompt = Some(prompt.trim().to_owned());
                        changed = true;
                    }
                }
            }
            if changed {
                self.inner.persist_bindings();
            }
        }

        // 2. Report tool/turn activity to messaging connectors.
        if let Some(activity) = extract_tool_activity(&event.payload)
            && let Some(mgr) = connectors
        {
            let active_seq = lock(&self.inner.turn_meta)
                .get(&session_id)
                .and_then(|m| m.active_turn_seq);
            mgr.on_turn_activity_seq(&binding.bot_id, session_id, active_seq, &activity);
        }

        // 3. On turn stop: heal silent MEMORY.md deletions, mark first_run_completed, extract reply.
        if !status::is_turn_stop(&event.payload) {
            return None;
        }

        let ts = now_ms();
        let completed_seq = lock(&self.inner.turn_meta)
            .get(&session_id)
            .and_then(|m| m.active_turn_seq);
        let agy_failed = status::is_agy_stop_failed(&event.payload);
        if agy_failed {
            // Quota/provider error Stop: skip transcript read and `first_run_completed`, and report
            // the provider error text instead of the last progress update (Orca `b342cee0` / `#46999`).
            let err_text = status::agy_stop_error_message(&event.payload)
                .unwrap_or_else(|| "Turn failed".to_owned());
            {
                let mut sessions = lock(&self.inner.sessions);
                let mut meta_map = lock(&self.inner.turn_meta);
                if let Some(entry) = sessions.get_mut(&session_id) {
                    entry.explicit_reply_posted = false;
                    entry.owner_turn_pending = false;
                }
                if let Some(meta) = meta_map.get_mut(&session_id) {
                    meta.active_turn_seq = None;
                }
            }
            self.inner.persist_bindings();
            if let Some(g) = groups {
                let fail_note = format!("[turn failed: {err_text}]");
                g.on_bot_turn_finished_seq(
                    &binding.bot_id,
                    session_id,
                    completed_seq,
                    &fail_note,
                    ts,
                )
                .ok();
            }
            if let Some(mgr) = connectors {
                mgr.on_turn_complete_seq(
                    &binding.bot_id,
                    session_id,
                    completed_seq,
                    Some(err_text.as_str()),
                );
            }
            self.inner.drain_next_queued_turn(
                registry,
                store,
                groups,
                connectors,
                &binding.bot_id,
                session_id,
                ts,
            );
            return Some(err_text);
        }

        // Heal any silent fact deletions against pre-turn snapshot.
        if let Ok(current_mem) = store.get_memory(&binding.bot_id) {
            let (restored_content, restored_facts) =
                binding.memory_snapshot.restore_missing(&current_mem);
            // What this turn changed itself, said in its chat. Read before the heal is recorded.
            let before = binding.memory_snapshot.content();
            if before != restored_content {
                store
                    .record_turn_memory(
                        &binding.bot_id,
                        session_id.0,
                        before,
                        &restored_content,
                        ts,
                    )
                    .ok();
            }
            if !restored_facts.is_empty() {
                store
                    .apply_memory_with_history(
                        &binding.bot_id,
                        &restored_content,
                        true,
                        "system:heal",
                        "restore_deleted_facts",
                        ts,
                    )
                    .ok();
                tracing::info!(
                    session = session_id.0,
                    restored = restored_facts.len(),
                    "restored silently deleted MEMORY.md facts after bot turn"
                );
            }
            if let Some(entry) = lock(&self.inner.sessions).get_mut(&session_id) {
                entry.memory_snapshot = MemorySnapshot::capture(&restored_content);
            }
        }

        // Mark `first_run_completed = true` ONLY when an owner/kickoff turn completes cleanly.
        if binding.owner_turn_pending {
            store.mark_first_run_completed(&binding.bot_id, ts).ok();
        }

        // Extract final reply (always advancing `chat_offset`, even when `explicit_reply_posted` is true).
        let (extracted_reply, next_offset) =
            extract_final_reply(registry, session_id, binding.chat_offset);
        let cleaned_reply = extracted_reply
            .map(|text| strip_echoed_prompt(&text, binding.last_prompt.as_deref()))
            .filter(|text| !text.trim().is_empty() && !is_quiet_reply(text));

        let final_reply = {
            let mut sessions = lock(&self.inner.sessions);
            let mut meta_map = lock(&self.inner.turn_meta);
            let explicit_posted = if let Some(entry) = sessions.get_mut(&session_id) {
                let posted = entry.explicit_reply_posted;
                entry.explicit_reply_posted = false;
                entry.owner_turn_pending = false;
                entry.chat_offset = next_offset;
                posted
            } else {
                binding.explicit_reply_posted
            };
            if let Some(meta) = meta_map.get_mut(&session_id) {
                meta.active_turn_seq = None;
            }
            if explicit_posted { None } else { cleaned_reply }
        };
        self.inner.persist_bindings();

        if let Some(g) = groups {
            // Always notify GroupCoordinator so empty/quiet/explicit-reply turns clear their
            // `ActiveGroupTurn` and placeholder instead of leaving `in_progress: true` stuck.
            g.on_bot_turn_finished_seq(
                &binding.bot_id,
                session_id,
                completed_seq,
                final_reply.as_deref().unwrap_or(""),
                ts,
            )
            .ok();
        }
        if let Some(mgr) = connectors {
            mgr.on_turn_complete_seq(
                &binding.bot_id,
                session_id,
                completed_seq,
                final_reply.as_deref(),
            );
        }
        self.inner.drain_next_queued_turn(
            registry,
            store,
            groups,
            connectors,
            &binding.bot_id,
            session_id,
            ts,
        );
        None
    }

    /// Prepares bot overrides, MCP config, relay grant, env vars, and instructions when `old`
    /// is a live bot session switching accounts into replacement session `new`.
    #[expect(clippy::too_many_arguments, reason = "bot switch launch preparation")]
    pub(crate) fn prepare_account_switch_launch(
        &self,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        old: SessionId,
        new: SessionId,
        launch_cwd: &std::path::Path,
    ) -> Result<Option<BotAccountSwitchLaunch>, String> {
        let Some((bot_id, link_id)) = lock(&self.inner.sessions)
            .get(&old)
            .map(|b| (b.bot_id.clone(), b.link_id.clone()))
        else {
            return Ok(None);
        };
        let Some(bot) = store.get_bot(&bot_id).map_err(|err| err.to_string())? else {
            return Ok(None);
        };
        let home = init_bot_home(store.root(), &bot, None).map_err(|err| err.to_string())?;
        let soul = store.get_soul(&bot_id).unwrap_or_default();
        let memory = store.get_memory(&bot_id).unwrap_or_default();
        let preamble = build_launch_preamble_in(
            &bot,
            Some(&soul),
            Some(&memory),
            Some(&home),
            Some(launch_cwd),
        );

        if bot.connectors.composio.enabled {
            composio::resolve_and_register_composio_with_root(
                &bot,
                secrets,
                relay,
                Some(store.root()),
                composio_base_url,
                now_ms(),
            )
            .ok();
        }

        let grant = match relay.prepare_launch_for_bot(&bot) {
            Ok(Some(g)) => Some(g),
            Ok(None) => Some(
                relay
                    .mint_grant_for_creds(&bot.id, vec!["krill".to_owned()])
                    .map_err(|err| format!("failed to prepare bot tool relay: {err}"))?,
            ),
            Err(err) => return Err(format!("failed to prepare bot tool relay: {err}")),
        };
        if let Some(ref g) = grant {
            relay.bind_grant_session(&g.token, new, link_id.as_deref());
        }

        let mut extra_env: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (
                b"KRILL_BOT_ID".to_vec(),
                bot.id.as_str().as_bytes().to_vec(),
            ),
            (
                b"KRILL_BOT_HOME".to_vec(),
                home.to_string_lossy().as_bytes().to_vec(),
            ),
        ];
        if let Some(ref g) = grant {
            extra_env.push((
                b"KRILL_BOT_TOOLS_URL".to_vec(),
                g.base_url.as_bytes().to_vec(),
            ));
            extra_env.push((
                b"KRILL_BOT_TOOLS_TOKEN".to_vec(),
                g.token.as_bytes().to_vec(),
            ));
        }

        let mut mcp_cfg = build_bot_mcp_launch_config(&bot, &home, grant.as_ref(), secrets);
        mcp_cfg.instructions = Some(preamble.clone());

        Ok(Some(BotAccountSwitchLaunch {
            model: bot.model,
            effort: bot.effort,
            extra_env,
            mcp_cfg,
            grant_token: grant.map(|g| g.token),
            instructions: preamble,
        }))
    }

    /// Rebinds an active bot session from `old` to `new`, returning the previous launch token
    /// so the caller can revoke it once `old` has retired (or roll back if retirement fails).
    pub(crate) fn rebind_session(
        &self,
        old: SessionId,
        new: SessionId,
        new_pid: u32,
        new_token: Option<Zeroizing<String>>,
    ) -> Option<ReboundBotSession> {
        let previous_token = {
            let mut sessions = lock(&self.inner.sessions);
            let mut binding = sessions.remove(&old)?;
            let prev_token = std::mem::replace(&mut binding.launch_token, new_token);
            binding.pid = new_pid;
            sessions.insert(new, binding);
            prev_token
        };
        {
            let mut controls = lock(&self.inner.turn_controls);
            let ctrl = controls
                .remove(&old)
                .unwrap_or_else(|| SessionTurnControl::new(true));
            ctrl.mark_ready();
            controls.insert(new, ctrl);
        }
        {
            let mut meta = lock(&self.inner.turn_meta);
            if let Some(entry) = meta.remove(&old) {
                meta.insert(new, entry);
            }
        }
        self.inner.persist_bindings();
        Some(ReboundBotSession { previous_token })
    }
    /// Cleans up session state and revokes the session's ephemeral `BotToolRelay` token.
    pub fn on_session_removed(&self, relay: &BotToolRelay, session_id: SessionId) {
        let removed = {
            let mut sessions = lock(&self.inner.sessions);
            let mut controls = lock(&self.inner.turn_controls);
            let mut meta = lock(&self.inner.turn_meta);
            controls.remove(&session_id);
            meta.remove(&session_id);
            lock(&self.inner.taken_over_sessions).remove(&session_id);
            sessions.remove(&session_id)
        };
        if let Some(binding) = removed {
            if let Some(token) = binding.launch_token {
                relay.revoke_token(&token);
            }
            self.inner.persist_bindings();
        }
    }

    /// Sends inter-bot mail (`bot:<name>`, `<bot_id>`, or a [`BotGroupChat`] name/ID) via
    /// [`OrchestrationStore`] and notifies or wakes target bot sessions.
    #[expect(clippy::too_many_arguments, reason = "inter-bot mail routing context")]
    pub fn send_bot_mail(
        &self,
        registry: &Registry,
        store: &BotStore,
        secrets: &SecretVault,
        relay: &BotToolRelay,
        composio_base_url: &str,
        from: &str,
        to_bot: &str,
        subject: &str,
        body: &str,
        thread_id: Option<&str>,
    ) -> Result<String, String> {
        let dir = registry
            .state_dir()
            .ok_or_else(|| "daemon state directory unavailable".to_owned())?;

        let targets: Vec<BotConfig> =
            if let Some(group) = store.find_group(to_bot).map_err(|e| e.to_string())? {
                let mut members = Vec::new();
                for bid in &group.bot_ids {
                    if let Some(b) = store.get_bot(bid).map_err(|e| e.to_string())? {
                        members.push(b);
                    }
                }
                if members.is_empty() {
                    return Err(format!("group '{}' has no active member bots", group.name));
                }
                members
            } else if let Some(bot) = store.resolve_bot(to_bot).map_err(|e| e.to_string())? {
                vec![bot]
            } else {
                return Err(format!("recipient bot or group not found: {to_bot}"));
            };

        let mut first_msg_id = None;
        let ts = now_ms();
        for target in &targets {
            let recipient = format!("bot:{}", target.slug.as_str());
            let params = SendParams {
                from: from.to_owned(),
                to: recipient,
                msg_type: MessageType::Status,
                summary: subject.to_owned(),
                body: body.to_owned(),
                run_id: None,
                task_id: None,
                dispatch_id: None,
                outcome: None,
                reply_to: thread_id.map(ToOwned::to_owned),
            };
            let sent = OrchestrationStore::mutate(&dir, |s| s.send_message(&params))
                .map_err(|err| format!("failed to write orchestration mail: {err}"))?;
            if first_msg_id.is_none() {
                first_msg_id = sent.first().map(|m| m.id.clone());
            }

            let notice = format!("[Krill bot mail from {from}] Subject: {subject}\n{body}");
            if !target.paused {
                self.deliver_or_wake_bot(
                    registry,
                    store,
                    secrets,
                    relay,
                    composio_base_url,
                    &target.id,
                    &notice,
                    ts,
                )
                .ok();
            }
        }

        registry.notify_orch();
        Ok(first_msg_id.unwrap_or_else(|| format!("mail-{ts}")))
    }

    /// Posts an explicit bot reply via [`ConnectorManager`] using caller session `link_id`
    /// fallback and marks `explicit_reply_posted` only on substantive (non-reaction-only) replies.
    pub fn post_bot_reply_for_session(
        &self,
        store: &BotStore,
        connectors: &ConnectorManager,
        caller_session: Option<SessionId>,
        reply: &OutboundBotReply,
    ) -> Result<Option<String>, String> {
        let session_link_id = if reply.link_id.is_none() {
            caller_session.and_then(|sid| self.session_link_for_bot(sid, &reply.bot_id))
        } else {
            None
        };
        let resolved_link_id = reply.link_id.as_deref().or(session_link_id.as_deref());
        let link = match resolved_link_id {
            Some(lid) => match store.get_link(lid).map_err(|e| e.to_string())? {
                Some(l) if l.bot_id == reply.bot_id => Some(l),
                Some(_) => {
                    return Err(format!(
                        "link '{lid}' does not belong to bot {}",
                        reply.bot_id.as_str()
                    ));
                }
                None => return Err(format!("link not found: {lid}")),
            },
            None => store
                .latest_link_for_bot(&reply.bot_id)
                .map_err(|e| e.to_string())?,
        };
        let ext_ref = connectors
            .post_reply(link.as_ref(), reply)
            .map_err(|err| err.to_string())?;
        let substantive = !reply.text.trim().is_empty() || !reply.files.is_empty();
        if substantive {
            self.mark_explicit_reply_for_session(&reply.bot_id, caller_session, resolved_link_id);
        }
        Ok(ext_ref)
    }

    /// Posts an explicit bot reply via [`ConnectorManager`] and marks the current turn replied.
    pub fn post_bot_reply(
        &self,
        store: &BotStore,
        connectors: &ConnectorManager,
        reply: &OutboundBotReply,
    ) -> Result<Option<String>, String> {
        self.post_bot_reply_for_session(store, connectors, None, reply)
    }

    fn ensure_binding_for_live_session(
        &self,
        registry: &Registry,
        store: &BotStore,
        bot_id: &BotId,
        sid: SessionId,
        link_id: Option<String>,
        now_ms: u64,
    ) {
        if self.is_taken_over(sid) {
            return;
        }
        let missing = !lock(&self.inner.sessions).contains_key(&sid);
        if !missing {
            return;
        }
        let memory = store.get_memory(bot_id).unwrap_or_default();
        let pid = registry.session_pid(sid).unwrap_or(0);
        let label = link_id
            .clone()
            .unwrap_or_else(|| bot_id.as_str().to_owned());
        {
            // Canonical lock order: sessions -> turn_controls (MB-01).
            lock(&self.inner.sessions)
                .entry(sid)
                .or_insert(BotSessionBinding {
                    bot_id: bot_id.clone(),
                    link_id,
                    memory_snapshot: MemorySnapshot::capture(&memory),
                    chat_offset: 0,
                    explicit_reply_posted: false,
                    launch_token: None,
                    pid,
                    label,
                    created_at_ms: now_ms,
                    owner_turn_pending: false,
                    last_prompt: None,
                });
            lock(&self.inner.turn_controls)
                .entry(sid)
                .or_insert_with(|| SessionTurnControl::new(true));
        }
        self.inner.persist_bindings();
    }
}

impl RuntimeInner {
    fn deliver_serialized_input(
        &self,
        registry: &Registry,
        session_id: SessionId,
        bytes: &[u8],
        submit: bool,
    ) -> io::Result<()> {
        let ctrl = lock(&self.turn_controls).get(&session_id).cloned();
        if let Some(ctrl) = ctrl {
            ctrl.wait_ready();
            let _serial = lock(&ctrl.delivery_lock);
            return registry.deliver_bot_input(session_id, bytes, submit);
        }
        registry.deliver_bot_input(session_id, bytes, submit)
    }

    fn fail_initial_prompt_delivery(&self, bot_id: &BotId, session_id: SessionId, reason: &str) {
        tracing::warn!(
            session = session_id.0,
            reason,
            "bot initial prompt delivery failed; rolling back pending turn"
        );
        let completed_seq = {
            let mut sessions = lock(&self.sessions);
            let mut meta_map = lock(&self.turn_meta);
            if let Some(b) = sessions.get_mut(&session_id) {
                b.owner_turn_pending = false;
            }
            meta_map
                .get_mut(&session_id)
                .and_then(|m| m.active_turn_seq.take())
        };
        self.persist_bindings();

        let (reg, store, groups, connectors) = {
            let cb = lock(&self.callbacks);
            match cb.as_ref() {
                Some(c) => (
                    c.registry.upgrade(),
                    c.store.upgrade(),
                    c.groups.upgrade(),
                    lock(&c.connectors).upgrade(),
                ),
                None => (None, None, None, None),
            }
        };
        let ts = now_ms();
        let fail_note = format!("[dispatch failed: {reason}]");
        if let Some(ref g) = groups {
            g.on_bot_turn_finished_seq(bot_id, session_id, completed_seq, &fail_note, ts)
                .ok();
        }
        if let Some(ref c) = connectors {
            c.on_turn_complete_seq(bot_id, session_id, completed_seq, Some(fail_note.as_str()));
        }
        if let (Some(reg), Some(store)) = (reg, store) {
            self.drain_next_queued_turn(
                &reg,
                &store,
                groups.as_deref(),
                connectors.as_deref(),
                bot_id,
                session_id,
                ts,
            );
        }
    }

    #[expect(clippy::too_many_arguments, reason = "queued turn drain context")]
    fn drain_next_queued_turn(
        &self,
        registry: &Registry,
        store: &BotStore,
        groups: Option<&GroupCoordinator>,
        connectors: Option<&ConnectorManager>,
        bot_id: &BotId,
        session_id: SessionId,
        ts: u64,
    ) {
        loop {
            // Pre-read MEMORY.md outside all runtime mutexes (MB-01).
            let fresh_memory = store.get_memory(bot_id).ok();
            let next = {
                // Canonical lock order: sessions -> turn_meta (MB-01).
                let mut sessions = lock(&self.sessions);
                let mut meta_map = lock(&self.turn_meta);
                if sessions
                    .get(&session_id)
                    .is_some_and(|entry| entry.owner_turn_pending)
                {
                    return;
                }
                let Some(queued) = meta_map
                    .get_mut(&session_id)
                    .and_then(|meta| meta.pending_turns.pop_front())
                else {
                    return;
                };
                if let Some(entry) = sessions.get_mut(&session_id) {
                    entry.explicit_reply_posted = false;
                    entry.owner_turn_pending = true;
                    if let Some(ref lid) = queued.link_id {
                        entry.link_id = Some(lid.clone());
                    }
                    entry.last_prompt = Some(queued.prompt.clone());
                    if let Some(ref mem) = fresh_memory {
                        entry.memory_snapshot = MemorySnapshot::capture(mem);
                    }
                }
                if let Some(meta) = meta_map.get_mut(&session_id) {
                    meta.active_turn_seq = Some(queued.turn_seq);
                }
                queued
            };
            self.persist_bindings();
            match self.deliver_serialized_input(registry, session_id, next.prompt.as_bytes(), true)
            {
                Ok(()) => return,
                Err(err) => {
                    {
                        let mut sessions = lock(&self.sessions);
                        let mut meta_map = lock(&self.turn_meta);
                        if let Some(entry) = sessions.get_mut(&session_id) {
                            entry.owner_turn_pending = false;
                        }
                        if let Some(meta) = meta_map.get_mut(&session_id) {
                            meta.active_turn_seq = None;
                        }
                    }
                    self.persist_bindings();
                    let fail_note = format!("[dispatch failed: {err}]");
                    if let Some(g) = groups {
                        g.on_bot_turn_finished_seq(
                            bot_id,
                            session_id,
                            Some(next.turn_seq),
                            &fail_note,
                            ts,
                        )
                        .ok();
                    }
                    if let Some(c) = connectors {
                        c.on_turn_complete_seq(
                            bot_id,
                            session_id,
                            Some(next.turn_seq),
                            Some(fail_note.as_str()),
                        );
                    }
                }
            }
        }
    }

    fn persist_bindings(&self) {
        let Some(ref root) = self.bots_root else {
            return;
        };
        // Canonical lock order: bindings_io_lock -> sessions -> turn_meta (MB-01).
        let _io = lock(&self.bindings_io_lock);
        let records: Vec<PersistedBotBinding> = {
            let sessions = lock(&self.sessions);
            let meta_map = lock(&self.turn_meta);
            sessions
                .iter()
                .map(|(&session_id, b)| {
                    let meta = meta_map.get(&session_id);
                    PersistedBotBinding {
                        session_id,
                        bot_id: b.bot_id.clone(),
                        link_id: b.link_id.clone(),
                        chat_offset: b.chat_offset,
                        pid: b.pid,
                        label: b.label.clone(),
                        created_at_ms: b.created_at_ms,
                        owner_turn_pending: b.owner_turn_pending,
                        last_prompt: b.last_prompt.clone(),
                        group_key: meta.and_then(|m| m.group_key.clone()),
                        turn_seq: meta.map_or(0, |m| m.turn_seq),
                        active_turn_seq: meta.and_then(|m| m.active_turn_seq),
                    }
                })
                .collect()
        };
        if ensure_no_symlink_path(root).is_err() {
            return;
        }
        if DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .is_err()
        {
            return;
        }
        let path = root.join(RUNTIME_BINDINGS_FILE);
        if ensure_no_symlink_path(&path).is_err() {
            return;
        }
        if let Ok(mut bytes) = serde_json::to_vec_pretty(&records) {
            bytes.push(b'\n');
            write_atomic_0600(&path, &bytes).ok();
        }
    }
}

/// Extracts the latest assistant reply via `agent_history/chat.rs` (`claude`/`codex`/`agy`/`opencode`)
/// with bounded JSONL flush wait, safe `snap.reset` handling (never re-reading `0` when
/// `prev_offset > 0` unless `snap.reset == true`), and clean fallback to PTY screen tail.
pub fn extract_final_reply(
    registry: &Registry,
    session_id: SessionId,
    prev_offset: u64,
) -> (Option<String>, u64) {
    let mut snap = registry.chat_poll(session_id, prev_offset);
    let mut saw_reset = snap.reset;
    if snap.bound {
        if snap.reset && prev_offset > 0 {
            let full = registry.chat_poll(session_id, 0);
            let reset_text = last_assistant_text(&full.items);
            if reset_text.is_some() {
                return (reset_text, full.next_offset);
            }
            snap = full;
        } else if let Some(text) = last_assistant_text(&snap.items) {
            return (Some(text), snap.next_offset);
        }

        // Bounded JSONL flush retry (up to ~225ms) so `Stop` hook races do not drop fresh output.
        for _ in 0..JSONL_FLUSH_RETRIES {
            std::thread::sleep(JSONL_FLUSH_SLEEP);
            let poll_from = if saw_reset || snap.reset {
                0
            } else {
                prev_offset
            };
            let next_snap = registry.chat_poll(session_id, poll_from);
            if !next_snap.bound {
                snap = next_snap;
                break;
            }
            if next_snap.reset {
                saw_reset = true;
            }
            if next_snap.reset && poll_from > 0 {
                let full = registry.chat_poll(session_id, 0);
                if let Some(text) = last_assistant_text(&full.items) {
                    return (Some(text), full.next_offset);
                }
                snap = full;
            } else {
                snap = next_snap;
                if let Some(text) = last_assistant_text(&snap.items) {
                    return (Some(text), snap.next_offset);
                }
            }
        }
    }

    // Fallback for agents without structured transcript normalization (`grok`/`omp`/`pi`/`cursor`)
    // or if no JSONL assistant record flushed within budget.
    let pty_tail = registry.capture_session_plain_tail(session_id);
    let next_offset = if saw_reset || snap.reset {
        snap.next_offset
    } else {
        snap.next_offset.max(prev_offset)
    };
    (pty_tail, next_offset)
}

fn last_assistant_text(items: &[krill_proto::agent_history::ChatItem]) -> Option<String> {
    items
        .iter()
        .rev()
        .find(|item| item.kind == ChatItemKind::Assistant && !item.text.trim().is_empty())
        .map(|item| item.text.trim().to_owned())
}

fn strip_echoed_prompt(raw: &str, last_prompt: Option<&str>) -> String {
    let mut text = raw.trim().to_owned();
    if let Some(prompt) = last_prompt.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(stripped) = text.strip_prefix(prompt) {
            text = stripped.trim().to_owned();
        } else if let Some(first_line) = prompt.lines().next().map(str::trim)
            && !first_line.is_empty()
            && let Some(stripped) = text.strip_prefix(first_line)
        {
            text = stripped.trim().to_owned();
        }
    }
    text
}

/// Returns `true` when a bot reply ends with [`BOT_QUIET_MARKER`] (`[[KRILL_QUIET]]`).
#[must_use]
pub fn is_quiet_reply(text: &str) -> bool {
    text.trim_end().ends_with(BOT_QUIET_MARKER)
}

fn build_bot_mcp_launch_config(
    bot: &BotConfig,
    home: &std::path::Path,
    grant: Option<&crate::bots::tool_relay::BotToolLaunchGrant>,
    secrets: &SecretVault,
) -> BotMcpLaunchConfig {
    let mut http_servers = Vec::new();
    if let Some(g) = grant {
        http_servers.push(BotMcpHttpServer {
            id: "krill".to_owned(),
            url: format!("{}/bot-tools/krill/mcp", g.base_url),
            bearer_token: (*g.token).clone(),
        });
        for cred_id in &g.cred_ids {
            if cred_id == "krill" {
                continue;
            }
            http_servers.push(BotMcpHttpServer {
                id: cred_id.clone(),
                url: format!("{}/bot-tools/{cred_id}/mcp", g.base_url),
                bearer_token: (*g.token).clone(),
            });
        }
    }

    let mut stdio_servers = Vec::new();
    if let Some(g) = grant
        && !g.stdio_tools.is_empty()
    {
        for st in &g.stdio_tools {
            stdio_servers.push(BotMcpStdioServer {
                id: st.id.clone(),
                command: st.command.clone(),
                env: st
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), (**v).clone()))
                    .collect(),
            });
        }
    } else {
        for tool in &bot.mcp_tools {
            if !tool.enabled || tool.command.is_empty() {
                continue;
            }
            let mut env = Vec::new();
            if let Ok(Some(secret_val)) = secrets.resolve_secret(
                &bot.id,
                tool.secret_ref.as_deref(),
                &format!("mcp:{}", tool.id),
            ) {
                if tool.header_names.is_empty() {
                    let fallback_var = format!(
                        "KRILL_MCP_{}_TOKEN",
                        tool.id
                            .chars()
                            .map(|c| {
                                if c.is_ascii_alphanumeric() {
                                    c.to_ascii_uppercase()
                                } else {
                                    '_'
                                }
                            })
                            .collect::<String>()
                    );
                    env.push((fallback_var, (*secret_val).clone()));
                } else {
                    for env_name in &tool.header_names {
                        let trimmed = env_name.trim();
                        if !trimmed.is_empty() {
                            env.push((trimmed.to_owned(), (*secret_val).clone()));
                        }
                    }
                }
            }
            stdio_servers.push(BotMcpStdioServer {
                id: tool.id.clone(),
                command: tool.command.clone(),
                env,
            });
        }
    }

    BotMcpLaunchConfig {
        bot_home: Some(home.to_path_buf()),
        http_servers,
        stdio_servers,
        instructions: None,
    }
}

fn resolve_launch_cwd(
    bot: &BotConfig,
    home: &std::path::Path,
    override_cwd: Option<&str>,
) -> PathBuf {
    if let Some(raw) = override_cwd.map(str::trim).filter(|s| !s.is_empty()) {
        let p = PathBuf::from(raw);
        if p.is_dir() {
            return p;
        }
    }
    if let Some(raw) = bot
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let p = PathBuf::from(raw);
        if p.is_dir() {
            return p;
        }
    }
    home.to_path_buf()
}

fn format_external_prompt(msg: &InboundExternalMessage) -> String {
    let mut out = format!(
        "[Krill external message via {} | channel: {} | from: {} ({})]\n{}",
        msg.source.as_str(),
        msg.channel_id,
        msg.sender_name,
        msg.sender_id,
        krill_proto::bots::fence_quoted_text(&msg.text)
    );
    if !msg.attachments.is_empty() {
        out.push_str("\nAttachments:\n");
        for path in &msg.attachments {
            out.push_str("- ");
            out.push_str(path);
            out.push('\n');
        }
    }
    out
}

#[derive(Deserialize)]
struct HookActivityFields {
    #[serde(alias = "hookEventName", alias = "event")]
    hook_event_name: Option<String>,
    #[serde(alias = "toolName")]
    tool_name: Option<String>,
    #[serde(alias = "userPrompt", alias = "user_prompt", alias = "message")]
    prompt: Option<String>,
    #[serde(default, alias = "isSubagent")]
    is_subagent: bool,
    #[serde(alias = "agentId", alias = "subagent_id", alias = "subagentId")]
    agent_id: Option<String>,
    #[serde(alias = "parentSessionId")]
    parent_session_id: Option<String>,
    #[serde(alias = "teammateId")]
    teammate_id: Option<String>,
}

enum PromptSubmitEvent {
    None,
    Submitted(Option<String>),
}

fn extract_prompt_submit(payload: &[u8]) -> PromptSubmitEvent {
    let Ok(f) = serde_json::from_slice::<HookActivityFields>(payload) else {
        return PromptSubmitEvent::None;
    };
    let is_subagent = f.is_subagent
        || f.agent_id.as_deref().is_some_and(|s| !s.is_empty())
        || f.parent_session_id
            .as_deref()
            .is_some_and(|s| !s.is_empty())
        || f.teammate_id.as_deref().is_some_and(|s| !s.is_empty());
    if is_subagent {
        return PromptSubmitEvent::None;
    }
    match f.hook_event_name.as_deref() {
        Some("UserPromptSubmit" | "BeforeAgent" | "beforeSubmitPrompt") => {
            PromptSubmitEvent::Submitted(f.prompt)
        }
        _ => PromptSubmitEvent::None,
    }
}

fn extract_tool_activity(payload: &[u8]) -> Option<String> {
    let f: HookActivityFields = serde_json::from_slice(payload).ok()?;
    let event = f.hook_event_name.as_deref()?;
    match event {
        "PreToolUse" | "BeforeTool" | "PreInvocation" => Some(match f.tool_name.as_deref() {
            Some(tool) if !tool.trim().is_empty() => format!("Using {tool}..."),
            _ => "Working...".to_owned(),
        }),
        "PostToolUse" | "AfterTool" | "PostInvocation" => Some("Thinking...".to_owned()),
        _ => None,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::{BotMcpAuthKind, BotMcpToolConfig, CreateBotParams};

    #[test]
    fn quiet_marker_and_tool_activity_extraction_work() {
        assert!(is_quiet_reply("Nothing to report.\n[[KRILL_QUIET]]"));
        assert!(!is_quiet_reply("Found 2 risky PR changes."));
        assert_eq!(
            extract_tool_activity(br#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#),
            Some("Using Bash...".to_owned())
        );
        assert_eq!(
            extract_tool_activity(br#"{"hook_event_name":"Stop"}"#),
            None
        );
    }

    #[test]
    fn build_mcp_config_includes_krill_builtin_relayed_and_stdio_vault_env() {
        let dir = std::env::temp_dir().join(format!("krilld-rt-mcp-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let secrets = SecretVault::with_static_key(dir.clone(), [0x22; 32]);
        let store = BotStore::new(dir.clone());
        let mut bot = store
            .create_bot(
                CreateBotParams {
                    name: "MCP Bot".to_owned(),
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
        bot.mcp_tools.push(BotMcpToolConfig {
            id: "gh-stdio".to_owned(),
            name: "GitHub Stdio".to_owned(),
            endpoint_url: None,
            command: vec![
                "npx".to_owned(),
                "@modelcontextprotocol/server-github".to_owned(),
            ],
            auth_kind: BotMcpAuthKind::BearerHeader,
            header_names: vec!["GITHUB_PERSONAL_ACCESS_TOKEN".to_owned()],
            secret_ref: Some(SecretVault::scoped_ref(&bot.id, "mcp:gh-stdio")),
            enabled: true,
        });
        secrets
            .set_secret(
                &bot.id,
                "mcp:gh-stdio",
                Zeroizing::new("ghp_test123".to_owned()),
            )
            .unwrap();

        let grant = crate::bots::tool_relay::BotToolLaunchGrant {
            base_url: "http://127.0.0.1:41000".to_owned(),
            token: Zeroizing::new("tok_xyz".to_owned()),
            cred_ids: vec!["krill".to_owned(), "linear-c1".to_owned()],
            stdio_tools: Vec::new(),
        };
        let cfg = build_bot_mcp_launch_config(&bot, &store.bot_home(&bot), Some(&grant), &secrets);
        assert_eq!(cfg.http_servers.len(), 2);
        assert_eq!(cfg.http_servers.first().unwrap().id, "krill");
        assert_eq!(
            cfg.http_servers.first().unwrap().url,
            "http://127.0.0.1:41000/bot-tools/krill/mcp"
        );
        assert_eq!(cfg.http_servers.get(1).unwrap().id, "linear-c1");
        assert_eq!(cfg.stdio_servers.len(), 1);
        assert_eq!(
            cfg.stdio_servers.first().unwrap().env,
            vec![(
                "GITHUB_PERSONAL_ACCESS_TOKEN".to_owned(),
                "ghp_test123".to_owned()
            )]
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bindings_persistence_rehydrates_live_sessions_and_prunes_dead_ones() {
        let dir = std::env::temp_dir().join(format!("krilld-rt-rehydrate-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let rt1 = BotRuntime::with_root(dir.clone());
        let bot_id = BotId::new("bot-rehydrate").unwrap();
        lock(&rt1.inner.sessions).insert(
            SessionId(999),
            BotSessionBinding {
                bot_id,
                link_id: None,
                memory_snapshot: MemorySnapshot::capture(""),
                chat_offset: 12,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 424_242,
                label: "Dead".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: false,
                last_prompt: None,
            },
        );
        rt1.inner.persist_bindings();
        assert!(dir.join(RUNTIME_BINDINGS_FILE).is_file());

        let rt2 = BotRuntime::with_root(dir.clone());
        let store = BotStore::new(dir.clone());
        assert_eq!(rt2.rehydrate_from_disk(&store, &reg), 0);
        assert!(!rt2.is_bot_session(SessionId(999)));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn strip_echoed_prompt_prefers_full_multiline_match() {
        let prompt = "[Krill external message via slack]\nCheck PR #42\nPlease reply";
        let echoed = format!("{prompt}\n\nLooks good to merge.");
        assert_eq!(
            strip_echoed_prompt(&echoed, Some(prompt)),
            "Looks good to merge."
        );
    }

    #[test]
    fn mark_explicit_reply_scoped_to_caller_session() {
        let rt = BotRuntime::new();
        let bid = BotId::new("bot-multi").unwrap();
        let mk = || BotSessionBinding {
            bot_id: bid.clone(),
            link_id: None,
            memory_snapshot: MemorySnapshot::capture(""),
            chat_offset: 0,
            explicit_reply_posted: false,
            launch_token: None,
            pid: 0,
            label: "Multi".to_owned(),
            created_at_ms: 1_000,
            owner_turn_pending: true,
            last_prompt: None,
        };
        lock(&rt.inner.sessions).insert(SessionId(10), mk());
        lock(&rt.inner.sessions).insert(SessionId(11), mk());

        rt.mark_explicit_reply_for_session(&bid, Some(SessionId(10)), None);
        let guard = lock(&rt.inner.sessions);
        assert!(guard.get(&SessionId(10)).unwrap().explicit_reply_posted);
        assert!(!guard.get(&SessionId(11)).unwrap().explicit_reply_posted);
    }

    #[test]
    fn stale_pre_creation_and_orphaned_bindings_are_rejected_and_delete_purges_disk() {
        let dir =
            std::env::temp_dir().join(format!("krilld-rt-stale-filter-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let store = BotStore::new(dir.clone());
        let bot_created_ms = 1_795_438_259_549_u64;
        let stale_session_ms = 1_795_340_556_317_u64; // 27 hours earlier
        let fresh_session_ms = 1_795_438_259_692_u64;

        let bot = store
            .create_bot(
                CreateBotParams {
                    name: "크릴 에러 수정봇".to_owned(),
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
                },
                bot_created_ms,
            )
            .unwrap();
        let deleted_bot_id = BotId::new("bot-deleted-old").unwrap();

        let rt = BotRuntime::with_root(dir.clone());
        {
            let mut sessions = lock(&rt.inner.sessions);
            sessions.insert(
                SessionId(41764),
                BotSessionBinding {
                    bot_id: bot.id.clone(),
                    link_id: None,
                    memory_snapshot: MemorySnapshot::capture(""),
                    chat_offset: 0,
                    explicit_reply_posted: false,
                    launch_token: None,
                    pid: 36865,
                    label: "리서치봇".to_owned(),
                    created_at_ms: stale_session_ms,
                    owner_turn_pending: false,
                    last_prompt: None,
                },
            );
            sessions.insert(
                SessionId(41801),
                BotSessionBinding {
                    bot_id: bot.id.clone(),
                    link_id: None,
                    memory_snapshot: MemorySnapshot::capture(""),
                    chat_offset: 0,
                    explicit_reply_posted: false,
                    launch_token: None,
                    pid: 78853,
                    label: "크릴 에러 수정봇".to_owned(),
                    created_at_ms: fresh_session_ms,
                    owner_turn_pending: false,
                    last_prompt: None,
                },
            );
            sessions.insert(
                SessionId(41700),
                BotSessionBinding {
                    bot_id: deleted_bot_id.clone(),
                    link_id: None,
                    memory_snapshot: MemorySnapshot::capture(""),
                    chat_offset: 0,
                    explicit_reply_posted: false,
                    launch_token: None,
                    pid: 11111,
                    label: "삭제된 봇".to_owned(),
                    created_at_ms: fresh_session_ms,
                    owner_turn_pending: false,
                    last_prompt: None,
                },
            );
        }
        rt.inner.persist_bindings();

        // Validated listing drops both the 27h-old pre-creation binding and the deleted bot's binding.
        let validated = rt.list_sessions_for_store(&store, &reg, None);
        assert_eq!(validated.len(), 1);
        assert_eq!(validated[0].session_id, SessionId(41801));
        assert_eq!(validated[0].bot_id, bot.id);

        // Cold runtime (before `rehydrate_from_disk`) still purges persisted bindings on DeleteBot.
        let cold_rt = BotRuntime::with_root(dir.clone());
        let relay = BotToolRelay::new(Arc::new(SecretVault::with_static_key(
            dir.clone(),
            [0x11; 32],
        )));
        let stopped = cold_rt.stop_all_for_bot(&reg, &relay, &bot.id);
        assert_eq!(stopped, 2);
        let remaining: Vec<PersistedBotBinding> =
            serde_json::from_slice(&fs::read(dir.join(RUNTIME_BINDINGS_FILE)).unwrap()).unwrap();
        assert!(
            remaining.iter().all(|r| r.bot_id != bot.id),
            "deleted bot's persisted bindings must be purged from disk even before rehydration"
        );
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].bot_id, deleted_bot_id);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn runtime_mb01_mb08_mb10_lock_order_failure_propagation_and_queueing() {
        struct NoopBridge;
        impl crate::bots::connectors::ConnectorBridge for NoopBridge {
            fn dispatch_external_message(
                &self,
                _msg: InboundExternalMessage,
            ) -> io::Result<crate::bots::connectors::DispatchOutcome> {
                Err(io::Error::other("noop"))
            }
            fn check_or_request_access(
                &self,
                _bot_id: &BotId,
                _source: krill_proto::bots::ExternalSource,
                _sender_id: &str,
                _sender_name: &str,
                _channel_id: &str,
            ) -> io::Result<crate::bots::connectors::AccessDecision> {
                Ok(crate::bots::connectors::AccessDecision::Denied(
                    "noop".to_owned(),
                ))
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
                _source: krill_proto::bots::ExternalSource,
                _workspace_id: Option<&str>,
                _channel_id: &str,
                _thread_id: Option<&str>,
                _acts_for: krill_proto::bots::ActsFor,
            ) -> io::Result<krill_proto::bots::ExternalConversationLink> {
                Err(io::Error::other("noop"))
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
            fn update_connector_status(&self, _status: krill_proto::bots::BotConnectorStatus) {}
            fn get_bot_config(&self, _bot_id: &BotId) -> Option<BotConfig> {
                None
            }
            fn list_bots(&self) -> Vec<BotConfig> {
                Vec::new()
            }
        }

        let dir = std::env::temp_dir().join(format!("krilld-rt-mb01-08-10-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let store = Arc::new(BotStore::new(dir.clone()));
        let groups = Arc::new(GroupCoordinator::new(dir.clone()));
        let bot = store
            .create_bot(
                krill_proto::bots::CreateBotParams {
                    name: "Runtime Bot".to_owned(),
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
        let grp = store
            .create_group("Ops", vec![bot.id.clone()], String::new(), 1_000)
            .unwrap();
        let plan = groups
            .prepare_message_dispatch(
                &store,
                &grp.id,
                None,
                "owner",
                "Check status",
                Some(krill_proto::bots::BotGroupReplyMode::All),
                1_010,
            )
            .unwrap();

        let rt = Arc::new(BotRuntime::with_root(dir.clone()));
        rt.attach_callbacks(
            Arc::downgrade(&reg),
            Arc::downgrade(&store),
            Arc::downgrade(&groups),
        );

        let sid = SessionId(777);
        {
            lock(&rt.inner.sessions).insert(
                sid,
                BotSessionBinding {
                    bot_id: bot.id.clone(),
                    link_id: Some("link-ext-1".to_owned()),
                    memory_snapshot: MemorySnapshot::capture(""),
                    chat_offset: 0,
                    explicit_reply_posted: true,
                    launch_token: None,
                    pid: 1234,
                    label: "Test".to_owned(),
                    created_at_ms: 1_000,
                    owner_turn_pending: true,
                    last_prompt: Some("Turn 1".to_owned()),
                },
            );
            lock(&rt.inner.turn_controls).insert(sid, SessionTurnControl::new(true));
            lock(&rt.inner.turn_meta).insert(
                sid,
                SessionTurnMeta {
                    group_key: Some((grp.id.clone(), plan.thread.thread_id.clone())),
                    turn_seq: 1,
                    active_turn_seq: Some(1),
                    pending_turns: VecDeque::new(),
                },
            );
        }
        groups
            .record_turn_started_seq(&grp.id, &plan.thread.thread_id, &bot, sid, Some(1), 1_020)
            .unwrap();

        // MB-10: queueing while `owner_turn_pending == true` must NOT clobber active turn state.
        let seq2 = rt
            .deliver_or_queue_turn_with_link(
                &reg,
                &store,
                sid,
                &bot.id,
                "Turn 2",
                Some("link-ext-1".to_owned()),
            )
            .unwrap();
        assert_eq!(seq2, 2);
        {
            let sessions = lock(&rt.inner.sessions);
            let b = sessions.get(&sid).unwrap();
            assert!(
                b.explicit_reply_posted,
                "active turn explicit flag must stay intact"
            );
            assert_eq!(b.last_prompt.as_deref(), Some("Turn 1"));
            let meta = lock(&rt.inner.turn_meta);
            let m = meta.get(&sid).unwrap();
            assert_eq!(m.active_turn_seq, Some(1));
            assert_eq!(m.pending_turns.len(), 1);
        }

        // MB-01: stress concurrent `live_session_for_bot` + `persist_bindings` (no AB/BA deadlock).
        let rt_a = Arc::clone(&rt);
        let reg_a = Arc::clone(&reg);
        let bid_a = bot.id.clone();
        let t1 = std::thread::spawn(move || {
            for _ in 0..100 {
                assert!(rt_a.live_session_for_bot(&reg_a, &bid_a).is_none());
            }
        });
        let rt_b = Arc::clone(&rt);
        let t2 = std::thread::spawn(move || {
            for _ in 0..100 {
                rt_b.inner.persist_bindings();
            }
        });
        t1.join().unwrap();
        t2.join().unwrap();

        // MB-08: initial prompt delivery failure clears pending turn, notifies GroupCoordinator
        // (finishing Turn 1 with a dispatch failure note), persists bindings, and attempts to
        // drain Turn 2 cleanly without deadlocking.
        groups
            .record_turn_started_seq(&grp.id, &plan.thread.thread_id, &bot, sid, Some(2), 1_025)
            .unwrap();
        rt.inner.fail_initial_prompt_delivery(
            &bot.id,
            sid,
            "pty closed before initial prompt delivery",
        );
        assert!(!rt.turn_running(sid));
        assert!(
            lock(&rt.inner.turn_meta)
                .get(&sid)
                .unwrap()
                .pending_turns
                .is_empty()
        );
        let view = groups
            .read_group(&store, &grp.id, Some(&plan.thread.thread_id), Some(20))
            .unwrap();
        assert!(!view.active_run);
        assert!(
            view.messages
                .iter()
                .any(|m| m.body.contains("dispatch failed") && !m.in_progress),
            "expected dispatch failure note in group thread: {:?}",
            view.messages
        );

        // MB-05: cross-bot link_id must be rejected in `post_bot_reply_for_session`.
        let other = store
            .create_bot(
                krill_proto::bots::CreateBotParams {
                    name: "Other Bot".to_owned(),
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
                1_100,
            )
            .unwrap();
        let other_link = store
            .lookup_or_create_link(
                &other.id,
                krill_proto::bots::ExternalSource::Telegram,
                None,
                "chat-99",
                None,
                krill_proto::bots::ActsFor::Owner,
                1_110,
            )
            .unwrap();
        let cm = ConnectorManager::new(Arc::new(NoopBridge));
        let cross_err = rt
            .post_bot_reply_for_session(
                &store,
                &cm,
                None,
                &OutboundBotReply {
                    bot_id: bot.id.clone(),
                    link_id: Some(other_link.id),
                    text: "spoof".to_owned(),
                    files: Vec::new(),
                    reaction: None,
                },
            )
            .unwrap_err();
        assert!(cross_err.contains("does not belong to bot"), "{cross_err}");

        // R2M-04: deliver_or_queue_turn rejects unbound/taken-over session IDs instead of writing to PTY.
        let unbound_err = rt
            .deliver_or_queue_turn(&reg, &store, SessionId(999_999), &bot.id, "orphan")
            .unwrap_err();
        assert!(unbound_err.contains("not bound"), "{unbound_err}");
        lock(&rt.inner.taken_over_sessions).insert(SessionId(888_888));
        rt.ensure_binding_for_live_session(
            &reg,
            &store,
            &bot.id,
            SessionId(888_888),
            Some("link-stale".to_owned()),
            1_200,
        );
        assert!(
            !lock(&rt.inner.sessions).contains_key(&SessionId(888_888)),
            "taken-over session must never be re-bound by ensure_binding_for_live_session"
        );

        // R2M-09: Rollback on failed deliver_serialized_input drains queued pending_turns instead of stranding them.
        let sid_q = SessionId(707_707);
        lock(&rt.inner.sessions).insert(
            sid_q,
            BotSessionBinding {
                bot_id: bot.id.clone(),
                link_id: None,
                memory_snapshot: MemorySnapshot::capture(""),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 4243,
                label: "queue-test".to_owned(),
                created_at_ms: 1_200,
                owner_turn_pending: false,
                last_prompt: Some("prev".to_owned()),
            },
        );
        lock(&rt.inner.turn_meta).insert(
            sid_q,
            SessionTurnMeta {
                group_key: None,
                turn_seq: 1,
                active_turn_seq: None,
                pending_turns: VecDeque::from([QueuedBotTurn {
                    prompt: "Turn 2".to_owned(),
                    turn_seq: 2,
                    link_id: None,
                }]),
            },
        );
        // Delivery of Turn 1 fails (session not in registry) -> rollback MUST drain Turn 2 from pending_turns!
        assert!(
            rt.deliver_or_queue_turn(&reg, &store, sid_q, &bot.id, "Turn 1")
                .is_err()
        );
        assert!(
            lock(&rt.inner.turn_meta)
                .get(&sid_q)
                .unwrap()
                .pending_turns
                .is_empty(),
            "R2M-09: rollback must drain pending_turns instead of stranding Turn 2"
        );

        // R2M-21: on_hook_event_with_groups reads & resets live `entry.explicit_reply_posted` under lock.
        let sid_exp = SessionId(708_708);
        lock(&rt.inner.sessions).insert(
            sid_exp,
            BotSessionBinding {
                bot_id: bot.id.clone(),
                link_id: None,
                memory_snapshot: MemorySnapshot::capture(""),
                chat_offset: 0,
                explicit_reply_posted: true,
                launch_token: None,
                pid: 4244,
                label: "exp-test".to_owned(),
                created_at_ms: 1_300,
                owner_turn_pending: true,
                last_prompt: Some("hi".to_owned()),
            },
        );
        let stop_ev = HookEvent {
            id: krill_proto::hook::EventId {
                pid: 4244,
                unix_ns: 1_400_000_000,
            },
            agent: "claude".to_owned(),
            ppid: 4244,
            input: krill_proto::hook::Input::Complete,
            payload: serde_json::json!({"hook_event_name": "Stop"})
                .to_string()
                .into_bytes(),
        };
        rt.on_hook_event_with_groups(&reg, &store, None, None, sid_exp, &stop_ev);
        {
            let sessions = lock(&rt.inner.sessions);
            let b = sessions.get(&sid_exp).unwrap();
            assert!(
                !b.explicit_reply_posted,
                "R2M-21: explicit_reply_posted reset"
            );
            assert!(!b.owner_turn_pending, "R2M-21: owner_turn_pending cleared");
        }

        // R3M-03: non-connector Turn 2 activity & stop completion on a bound session must NOT
        // leak into a queued external connector Turn 3 (seq correlation across BotRuntime -> ConnectorManager).
        {
            use crate::bots::connectors::router::{DeliveryTarget, PlatformSender};
            use crate::bots::{AccessDecision, ConnectorBridge, DispatchOutcome};
            use krill_proto::bots::ExternalSource;

            struct RecordingSender {
                sent: Mutex<Vec<String>>,
            }
            impl PlatformSender for RecordingSender {
                fn source(&self) -> ExternalSource {
                    ExternalSource::Telegram
                }
                fn max_chunk_chars(&self) -> usize {
                    4000
                }
                fn send_message(&self, _target: &DeliveryTarget, text: &str) -> io::Result<String> {
                    lock(&self.sent).push(text.to_owned());
                    Ok(format!("msg-{}", lock(&self.sent).len()))
                }
                fn edit_message(
                    &self,
                    _target: &DeliveryTarget,
                    _message_id: &str,
                    text: &str,
                ) -> io::Result<()> {
                    lock(&self.sent).push(format!("edit:{text}"));
                    Ok(())
                }
            }
            struct QueuedSeqBridge {
                bot: BotConfig,
                rt: Arc<BotRuntime>,
                reg: Arc<Registry>,
                store: Arc<BotStore>,
                sid: SessionId,
            }
            impl ConnectorBridge for QueuedSeqBridge {
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
                        msg.acts_for,
                        1_500,
                    )?;
                    Ok(DispatchOutcome {
                        link,
                        session_id: self.sid,
                        started_new_session: false,
                    })
                }
                fn dispatch_external_message_with_hooks(
                    &self,
                    msg: InboundExternalMessage,
                    on_bound: &mut dyn FnMut(SessionId, Option<u64>),
                    on_rollback: &mut dyn FnMut(SessionId, Option<u64>),
                ) -> io::Result<DispatchOutcome> {
                    let mut bound = |s: SessionId, seq: u64| on_bound(s, Some(seq));
                    let mut rb = |s: SessionId, seq: u64| on_rollback(s, Some(seq));
                    self.rt
                        .deliver_or_queue_turn_with_link_hooks(
                            &self.reg,
                            &self.store,
                            self.sid,
                            &msg.bot_id,
                            &msg.text,
                            None,
                            Some(&mut bound),
                            Some(&mut rb),
                        )
                        .map_err(io::Error::other)?;
                    self.dispatch_external_message(msg)
                }
                fn check_or_request_access(
                    &self,
                    _bot_id: &BotId,
                    _source: ExternalSource,
                    _sender_id: &str,
                    _sender_name: &str,
                    _channel_id: &str,
                ) -> io::Result<AccessDecision> {
                    Ok(AccessDecision::Allowed)
                }
                fn resolve_secret(
                    &self,
                    _bot_id: &BotId,
                    _explicit_ref: Option<&str>,
                    _fallback_key: &str,
                ) -> io::Result<Option<zeroize::Zeroizing<String>>> {
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
                    self.store.lookup_or_create_link(
                        bot_id,
                        source,
                        workspace_id,
                        channel_id,
                        thread_id,
                        acts_for,
                        1_500,
                    )
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
                fn update_connector_status(&self, _status: krill_proto::bots::BotConnectorStatus) {}
                fn get_bot_config(&self, bot_id: &BotId) -> Option<BotConfig> {
                    (bot_id == &self.bot.id).then(|| self.bot.clone())
                }
                fn list_bots(&self) -> Vec<BotConfig> {
                    vec![self.bot.clone()]
                }
            }

            let mut tg_bot = bot.clone();
            tg_bot.connectors.telegram.enabled = true;
            tg_bot.connectors.telegram.acts_for = krill_proto::bots::ActsFor::Anyone;

            let sid_seq = SessionId(709_709);
            lock(&rt.inner.sessions).insert(
                sid_seq,
                BotSessionBinding {
                    bot_id: tg_bot.id.clone(),
                    link_id: None,
                    memory_snapshot: MemorySnapshot::capture(""),
                    chat_offset: 0,
                    explicit_reply_posted: false,
                    launch_token: None,
                    pid: 4245,
                    label: "seq-test".to_owned(),
                    created_at_ms: 1_500,
                    owner_turn_pending: true,
                    last_prompt: Some("owner secret".to_owned()),
                },
            );
            lock(&rt.inner.turn_meta).insert(
                sid_seq,
                SessionTurnMeta {
                    group_key: None,
                    turn_seq: 2,
                    active_turn_seq: Some(2),
                    pending_turns: VecDeque::new(),
                },
            );

            let sender = Arc::new(RecordingSender {
                sent: Mutex::new(Vec::new()),
            });
            let cm_seq = ConnectorManager::new(Arc::new(QueuedSeqBridge {
                bot: tg_bot.clone(),
                rt: Arc::clone(&rt),
                reg: Arc::clone(&reg),
                store: Arc::clone(&store),
                sid: sid_seq,
            }));
            cm_seq.router().register_sender(
                tg_bot.id.clone(),
                ExternalSource::Telegram,
                sender.clone(),
            );

            // Alice's message queues as Turn 3 while owner Turn 2 is active.
            let target = DeliveryTarget {
                source: ExternalSource::Telegram,
                workspace_id: None,
                channel_id: "alice-chan".to_owned(),
                thread_id: None,
                reply_to_message_id: Some("m-alice".to_owned()),
            };
            let outcome = cm_seq.router().handle_inbound_message(
                InboundExternalMessage {
                    bot_id: tg_bot.id.clone(),
                    source: ExternalSource::Telegram,
                    workspace_id: None,
                    channel_id: "alice-chan".to_owned(),
                    thread_id: None,
                    sender_id: "alice".to_owned(),
                    sender_name: "Alice".to_owned(),
                    text: "ping".to_owned(),
                    attachments: Vec::new(),
                    external_message_id: Some("m-alice".to_owned()),
                    acts_for: krill_proto::bots::ActsFor::Owner,
                },
                target,
                sender.as_ref(),
            );
            assert!(matches!(
                outcome,
                crate::bots::connectors::router::InboundRouteOutcome::Dispatched(_)
            ));

            // Turn 2 tool activity must NOT be sent to Alice.
            let act_ev = HookEvent {
                id: krill_proto::hook::EventId {
                    pid: 4245,
                    unix_ns: 1_510_000_000,
                },
                agent: "claude".to_owned(),
                ppid: 4245,
                input: krill_proto::hook::Input::Complete,
                payload: serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash"})
                    .to_string()
                    .into_bytes(),
            };
            rt.on_hook_event_with_groups(&reg, &store, Some(&cm_seq), None, sid_seq, &act_ev);
            assert!(
                lock(&sender.sent).is_empty(),
                "R3M-03: Turn 2 tool activity must not leak into Alice's queued Turn 3 chat"
            );

            // Turn 2 agy-failed Stop must NOT leak error text into Alice's chat (only drain Turn 3,
            // which fails delivery because fake session is not in Registry -> delivers Turn 3 dispatch failure).
            let stop_fail_ev = HookEvent {
                id: krill_proto::hook::EventId {
                    pid: 4245,
                    unix_ns: 1_520_000_000,
                },
                agent: "agy".to_owned(),
                ppid: 4245,
                input: krill_proto::hook::Input::Complete,
                payload: serde_json::json!({
                    "hook_event_name": "Stop",
                    "terminationReason": "ERROR",
                    "error": "SECRET_OWNER_TURN2_ERROR"
                })
                .to_string()
                .into_bytes(),
            };
            rt.on_hook_event_with_groups(&reg, &store, Some(&cm_seq), None, sid_seq, &stop_fail_ev);
            let sent_msgs = lock(&sender.sent).clone();
            assert!(
                sent_msgs
                    .iter()
                    .all(|m| !m.contains("SECRET_OWNER_TURN2_ERROR")),
                "R3M-03: Turn 2 error must never leak to Alice: {sent_msgs:?}"
            );
            assert!(
                sent_msgs.iter().any(|m| m.contains("[dispatch failed:")),
                "R3M-03: queued Turn 3 drain failure must still notify Alice: {sent_msgs:?}"
            );
        }

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn r4d_01_owner_steering_prompt_preserves_in_flight_turn_seq() {
        use crate::bots::connectors::router::{DeliveryTarget, PlatformSender};
        use crate::bots::{AccessDecision, ConnectorBridge, DispatchOutcome};
        use krill_proto::bots::ExternalSource;

        struct RecordingSender {
            sent: Mutex<Vec<String>>,
        }
        impl PlatformSender for RecordingSender {
            fn source(&self) -> ExternalSource {
                ExternalSource::Telegram
            }
            fn max_chunk_chars(&self) -> usize {
                4000
            }
            fn send_message(&self, _target: &DeliveryTarget, text: &str) -> io::Result<String> {
                lock(&self.sent).push(text.to_owned());
                Ok(format!("msg-{}", lock(&self.sent).len()))
            }
            fn edit_message(
                &self,
                _target: &DeliveryTarget,
                _message_id: &str,
                text: &str,
            ) -> io::Result<()> {
                lock(&self.sent).push(format!("edit:{text}"));
                Ok(())
            }
        }

        struct ActiveSeqBridge {
            bot: BotConfig,
            store: Arc<BotStore>,
            sid: SessionId,
            seq: u64,
        }
        impl ConnectorBridge for ActiveSeqBridge {
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
                    msg.acts_for,
                    1_000,
                )?;
                Ok(DispatchOutcome {
                    link,
                    session_id: self.sid,
                    started_new_session: false,
                })
            }
            fn dispatch_external_message_with_hooks(
                &self,
                msg: InboundExternalMessage,
                on_bound: &mut dyn FnMut(SessionId, Option<u64>),
                _on_rollback: &mut dyn FnMut(SessionId, Option<u64>),
            ) -> io::Result<DispatchOutcome> {
                on_bound(self.sid, Some(self.seq));
                self.dispatch_external_message(msg)
            }
            fn check_or_request_access(
                &self,
                _bot_id: &BotId,
                _source: ExternalSource,
                _sender_id: &str,
                _sender_name: &str,
                _channel_id: &str,
            ) -> io::Result<AccessDecision> {
                Ok(AccessDecision::Allowed)
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
                acts_for: krill_proto::bots::ActsFor,
            ) -> io::Result<krill_proto::bots::ExternalConversationLink> {
                self.store.lookup_or_create_link(
                    bot_id,
                    source,
                    workspace_id,
                    channel_id,
                    thread_id,
                    acts_for,
                    1_000,
                )
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
            fn update_connector_status(&self, _status: krill_proto::bots::BotConnectorStatus) {}
            fn get_bot_config(&self, bot_id: &BotId) -> Option<BotConfig> {
                (bot_id == &self.bot.id).then(|| self.bot.clone())
            }
            fn list_bots(&self) -> Vec<BotConfig> {
                vec![self.bot.clone()]
            }
        }

        let dir = std::env::temp_dir().join(format!("krilld-rt-r4d01-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        let reg = Arc::new(Registry::new(None, Some(&dir)));
        let store = Arc::new(BotStore::new(dir.clone()));
        let groups = Arc::new(GroupCoordinator::new(dir.clone()));
        let mut bot = store
            .create_bot(
                krill_proto::bots::CreateBotParams {
                    name: "Steer Guard Bot".to_owned(),
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
                },
                1_000,
            )
            .unwrap();
        bot.connectors.telegram.enabled = true;
        bot.connectors.telegram.acts_for = krill_proto::bots::ActsFor::Anyone;

        let grp = store
            .create_group("Steer Ops", vec![bot.id.clone()], String::new(), 1_000)
            .unwrap();
        let plan = groups
            .prepare_message_dispatch(
                &store,
                &grp.id,
                None,
                "owner",
                "Initial group prompt",
                Some(krill_proto::bots::BotGroupReplyMode::All),
                1_010,
            )
            .unwrap();

        let rt = BotRuntime::with_root(dir.clone());
        let sid = SessionId(801_801);
        lock(&rt.inner.sessions).insert(
            sid,
            BotSessionBinding {
                bot_id: bot.id.clone(),
                link_id: None,
                memory_snapshot: MemorySnapshot::capture(""),
                chat_offset: 0,
                explicit_reply_posted: false,
                launch_token: None,
                pid: 4301,
                label: "steer-turn1".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: true,
                last_prompt: Some("External turn 1 prompt".to_owned()),
            },
        );
        lock(&rt.inner.turn_meta).insert(
            sid,
            SessionTurnMeta {
                group_key: Some((grp.id.clone(), plan.thread.thread_id.clone())),
                turn_seq: 1,
                active_turn_seq: Some(1),
                pending_turns: VecDeque::new(),
            },
        );
        groups
            .record_turn_started_seq(&grp.id, &plan.thread.thread_id, &bot, sid, Some(1), 1_020)
            .unwrap();

        let sender = Arc::new(RecordingSender {
            sent: Mutex::new(Vec::new()),
        });
        let cm = ConnectorManager::new(Arc::new(ActiveSeqBridge {
            bot: bot.clone(),
            store: Arc::clone(&store),
            sid,
            seq: 1,
        }));
        cm.router()
            .register_sender(bot.id.clone(), ExternalSource::Telegram, sender.clone());

        let target = DeliveryTarget {
            source: ExternalSource::Telegram,
            workspace_id: None,
            channel_id: "tg-steer".to_owned(),
            thread_id: None,
            reply_to_message_id: Some("m-1".to_owned()),
        };
        let outcome = cm.router().handle_inbound_message(
            InboundExternalMessage {
                bot_id: bot.id.clone(),
                source: ExternalSource::Telegram,
                workspace_id: None,
                channel_id: "tg-steer".to_owned(),
                thread_id: None,
                sender_id: "user-1".to_owned(),
                sender_name: "User".to_owned(),
                text: "External turn 1 prompt".to_owned(),
                attachments: Vec::new(),
                external_message_id: Some("m-1".to_owned()),
                acts_for: krill_proto::bots::ActsFor::Owner,
            },
            target,
            sender.as_ref(),
        );
        assert!(matches!(
            outcome,
            crate::bots::connectors::router::InboundRouteOutcome::Dispatched(_)
        ));

        // Overlapping owner steering prompt while Turn 1 is still active must NOT overwrite
        // `active_turn_seq`, advance `turn_seq`, reset `explicit_reply_posted`, or clobber
        // `last_prompt`.
        rt.on_owner_prompt_submitted(sid, "Owner steering follow-up");
        {
            let sessions = lock(&rt.inner.sessions);
            let b = sessions.get(&sid).unwrap();
            assert!(b.owner_turn_pending);
            assert_eq!(b.last_prompt.as_deref(), Some("External turn 1 prompt"));
            let meta = lock(&rt.inner.turn_meta);
            let m = meta.get(&sid).unwrap();
            assert_eq!(m.turn_seq, 1);
            assert_eq!(m.active_turn_seq, Some(1));
        }

        // Also verify `explicit_reply_posted` stays intact during an overlapping steering prompt.
        let sid_exp = SessionId(801_802);
        lock(&rt.inner.sessions).insert(
            sid_exp,
            BotSessionBinding {
                bot_id: bot.id.clone(),
                link_id: None,
                memory_snapshot: MemorySnapshot::capture(""),
                chat_offset: 0,
                explicit_reply_posted: true,
                launch_token: None,
                pid: 4302,
                label: "steer-explicit".to_owned(),
                created_at_ms: 1_000,
                owner_turn_pending: true,
                last_prompt: Some("Explicit prompt".to_owned()),
            },
        );
        lock(&rt.inner.turn_meta).insert(
            sid_exp,
            SessionTurnMeta {
                group_key: None,
                turn_seq: 3,
                active_turn_seq: Some(3),
                pending_turns: VecDeque::new(),
            },
        );
        rt.on_owner_prompt_submitted(sid_exp, "Steering over explicit reply");
        {
            let sessions = lock(&rt.inner.sessions);
            let b = sessions.get(&sid_exp).unwrap();
            assert!(b.explicit_reply_posted);
            assert_eq!(b.last_prompt.as_deref(), Some("Explicit prompt"));
            let meta = lock(&rt.inner.turn_meta);
            let m = meta.get(&sid_exp).unwrap();
            assert_eq!(m.turn_seq, 3);
            assert_eq!(m.active_turn_seq, Some(3));
        }

        // Turn 1 live activity still correlates with original `turn_seq = Some(1)`.
        let act_ev = HookEvent {
            id: krill_proto::hook::EventId {
                pid: 4301,
                unix_ns: 1_030_000_000,
            },
            agent: "agy".to_owned(),
            ppid: 4301,
            input: krill_proto::hook::Input::Complete,
            payload: serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash"})
                .to_string()
                .into_bytes(),
        };
        rt.on_hook_event_with_groups(&reg, &store, Some(&cm), Some(&groups), sid, &act_ev);
        assert!(
            !lock(&sender.sent).is_empty(),
            "R4D-01: Turn 1 activity must still correlate with active connector turn_seq=1"
        );

        // Turn 1 Stop completes both the connector turn and the group placeholder with `Some(1)`.
        let stop_ev = HookEvent {
            id: krill_proto::hook::EventId {
                pid: 4301,
                unix_ns: 1_040_000_000,
            },
            agent: "agy".to_owned(),
            ppid: 4301,
            input: krill_proto::hook::Input::Complete,
            payload: serde_json::json!({
                "hook_event_name": "Stop",
                "terminationReason": "ERROR",
                "error": "TURN_1_RESULT"
            })
            .to_string()
            .into_bytes(),
        };
        rt.on_hook_event_with_groups(&reg, &store, Some(&cm), Some(&groups), sid, &stop_ev);

        let sent_msgs = lock(&sender.sent).clone();
        assert!(
            sent_msgs.iter().any(|m| m.contains("TURN_1_RESULT")),
            "R4D-01: Turn 1 completion must still deliver to connector turn_seq=1: {sent_msgs:?}"
        );
        let view = groups
            .read_group(&store, &grp.id, Some(&plan.thread.thread_id), Some(20))
            .unwrap();
        assert!(
            !view.active_run,
            "R4D-01: group placeholder for turn_seq=1 must complete instead of sticking active"
        );
        assert!(
            view.messages
                .iter()
                .any(|m| m.body.contains("TURN_1_RESULT") && !m.in_progress),
            "R4D-01: group thread must receive Turn 1 completion: {:?}",
            view.messages
        );

        // Once Turn 1 is idle (`owner_turn_pending == false`), a subsequent owner prompt starts
        // Turn 2 cleanly with `turn_seq = 2`.
        rt.on_owner_prompt_submitted(sid, "Fresh turn 2 prompt");
        {
            let sessions = lock(&rt.inner.sessions);
            let b = sessions.get(&sid).unwrap();
            assert!(b.owner_turn_pending);
            assert!(!b.explicit_reply_posted);
            assert_eq!(b.last_prompt.as_deref(), Some("Fresh turn 2 prompt"));
            let meta = lock(&rt.inner.turn_meta);
            let m = meta.get(&sid).unwrap();
            assert_eq!(m.turn_seq, 2);
            assert_eq!(m.active_turn_seq, Some(2));
        }

        fs::remove_dir_all(&dir).ok();
    }
}
