// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Bot session launch, durable runtime bindings (`runtime-bindings.json`), per-session preamble
//! serialization, hook integration (`is_turn_stop` & tool activity), reply extraction with
//! bounded JSONL flush retries, `MEMORY.md` post-turn silent-deletion recovery, group relays, and
//! inter-bot mail routing.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, DirBuilder};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::SessionId;
use krill_proto::agent_history::ChatItemKind;
use krill_proto::bots::{
    BOT_QUIET_MARKER, BotConfig, BotId, BotSessionSummary, MemorySnapshot, build_launch_preamble,
    find_bot_template, init_bot_home,
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
}

/// Runtime coordinator tracking live bot sessions and handling hooks/mail/replies.
#[derive(Debug, Default)]
pub struct BotRuntime {
    bots_root: Option<PathBuf>,
    bindings_io_lock: Mutex<()>,
    sessions: Mutex<HashMap<SessionId, BotSessionBinding>>,
    turn_controls: Mutex<HashMap<SessionId, Arc<SessionTurnControl>>>,
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
            bots_root: Some(bots_root),
            bindings_io_lock: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
            turn_controls: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn sessions_for_test(&self) -> &Mutex<HashMap<SessionId, BotSessionBinding>> {
        &self.sessions
    }

    /// Returns `true` if `session_id` belongs to a bot session.
    #[must_use]
    pub fn is_bot_session(&self, session_id: SessionId) -> bool {
        lock(&self.sessions).contains_key(&session_id)
    }

    /// Returns the [`BotId`] currently owning `session_id`, if any.
    #[must_use]
    pub fn bot_for_session(&self, session_id: SessionId) -> Option<BotId> {
        lock(&self.sessions)
            .get(&session_id)
            .map(|b| b.bot_id.clone())
    }

    /// Rehydrates live bindings from `<bots_root>/runtime-bindings.json` and prunes dead sessions.
    pub fn rehydrate_from_disk(&self, store: &BotStore, registry: &Registry) -> usize {
        let Some(ref root) = self.bots_root else {
            return 0;
        };
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

        let mut kept = 0_usize;
        let mut pruned_any = false;
        {
            let mut sessions = lock(&self.sessions);
            let mut controls = lock(&self.turn_controls);
            for rec in records {
                if registry.is_session_live(rec.session_id) {
                    let pid = registry.session_pid(rec.session_id).unwrap_or(rec.pid);
                    let memory = store.get_memory(&rec.bot_id).unwrap_or_default();
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
                    kept += 1;
                } else {
                    pruned_any = true;
                }
            }
        }
        if pruned_any {
            self.persist_bindings();
        }
        kept
    }

    /// Lists tracked bot sessions (pruning stale exited entries if desired, returning live state).
    #[must_use]
    pub fn list_sessions(
        &self,
        registry: &Registry,
        bot_id: Option<&BotId>,
    ) -> Vec<BotSessionSummary> {
        let snapshot: Vec<(SessionId, BotSessionBinding)> = lock(&self.sessions)
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

    /// Returns a live session ID currently bound to `bot_id`, if any.
    #[must_use]
    pub fn live_session_for_bot(&self, registry: &Registry, bot_id: &BotId) -> Option<SessionId> {
        let candidates: Vec<SessionId> = lock(&self.sessions)
            .iter()
            .filter(|(_, b)| &b.bot_id == bot_id)
            .map(|(&sid, _)| sid)
            .collect();
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
    ) -> Result<(SessionId, u32), String> {
        let bot = store
            .get_bot(bot_id)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("bot not found: {}", bot_id.as_str()))?;
        if bot.paused {
            return Err(format!("bot '{}' is paused", bot.name));
        }

        let home = init_bot_home(store.root(), &bot, None).map_err(|err| err.to_string())?;
        let soul = store.get_soul(bot_id).unwrap_or_default();
        let memory = store.get_memory(bot_id).unwrap_or_default();
        let preamble = build_launch_preamble(&bot, Some(&soul), Some(&memory));

        let launch_cwd = resolve_launch_cwd(&bot, &home, cwd_override);

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
                    .map_err(|err| format!("failed to prepare bot tool relay: {err}"))?,
            ),
            Err(err) => return Err(format!("failed to prepare bot tool relay: {err}")),
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
        let binding = BotSessionBinding {
            bot_id: bot.id.clone(),
            link_id: link_id.clone(),
            memory_snapshot: MemorySnapshot::capture(&memory),
            chat_offset: 0,
            explicit_reply_posted: false,
            launch_token: grant.map(|g| g.token),
            pid,
            label: link_id.map_or_else(|| bot.name.clone(), |l| format!("{} ({l})", bot.name)),
            created_at_ms: now_ms(),
            owner_turn_pending: has_prompt,
            last_prompt: trimmed_prompt.clone(),
        };
        {
            lock(&self.turn_controls).insert(session_id, Arc::clone(&ctrl));
            lock(&self.sessions).insert(session_id, binding);
        }
        self.persist_bindings();

        // Deliver launch preamble (+ optional initial prompt) once the agent PTY is ready, holding
        // `ctrl.delivery_lock` until `ctrl.preamble_gate` flips ready so concurrent dispatches wait cleanly.
        // Agents that took the preamble as their system prompt get only the owner's prompt (or nothing:
        // the bot speaks first when its owner does, as in Orca). Others still get it as their first input.
        let initial_input = match (instructions_given, trimmed_prompt) {
            (true, prompt) => prompt,
            (false, Some(user_prompt)) => Some(format!("{preamble}\n\n---\n\n{user_prompt}")),
            (false, None) => Some(preamble),
        };
        if let Some(tap) = registry.session_tap(session_id) {
            let bytes = initial_input.map(String::into_bytes);
            let ctrl_thread = Arc::clone(&ctrl);
            let spawn_res = std::thread::Builder::new()
                .name(format!("bot-preamble-{}", session_id.0))
                .stack_size(STACK_SIZE)
                .spawn(move || {
                    let _ready_guard = ReadyDropGuard(Arc::clone(&ctrl_thread));
                    let _serial = lock(&ctrl_thread.delivery_lock);
                    let deadline = Instant::now() + PREAMBLE_READY_WAIT;
                    while Instant::now() < deadline {
                        let p = tap.pulse();
                        if p.closed || (p.end > 0 && tap.quiet_ms() >= 150) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    if let Some(bytes) = bytes
                        && !tap.pulse().closed
                    {
                        drop(Registry::deliver_input_to_tap(&tap, &bytes, true));
                    }
                });
            if spawn_res.is_err() {
                ctrl.mark_ready();
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

        // Reuse bound session if still alive (reconstructing missing in-memory binding if needed).
        if let Some(sid) = link.session_id
            && registry.is_session_live(sid)
        {
            self.ensure_binding_for_live_session(
                registry,
                store,
                &msg.bot_id,
                sid,
                Some(link.id.clone()),
                now_ms,
            );
            {
                let mut sessions = lock(&self.sessions);
                if let Some(binding) = sessions.get_mut(&sid) {
                    binding.explicit_reply_posted = false;
                    binding.owner_turn_pending = true;
                    binding.link_id = Some(link.id.clone());
                    binding.last_prompt = Some(formatted.clone());
                    if let Ok(mem) = store.get_memory(&msg.bot_id) {
                        binding.memory_snapshot = MemorySnapshot::capture(&mem);
                    }
                }
            }
            relay.update_session_link(&msg.bot_id, sid, &link.id);
            self.persist_bindings();
            self.deliver_serialized_input(registry, sid, formatted.as_bytes(), true)?;
            return Ok((link, sid, false));
        }

        let (sid, _pid) = self
            .start_bot_session(
                registry,
                store,
                secrets,
                relay,
                composio_base_url,
                &msg.bot_id,
                None,
                Some(&formatted),
                Some(link.id.clone()),
            )
            .map_err(io::Error::other)?;
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
        if let Some(sid) = self.live_session_for_bot(registry, bot_id) {
            {
                let mut sessions = lock(&self.sessions);
                if let Some(binding) = sessions.get_mut(&sid) {
                    binding.explicit_reply_posted = false;
                    binding.owner_turn_pending = true;
                    binding.last_prompt = Some(prompt.to_owned());
                    if let Ok(mem) = store.get_memory(bot_id) {
                        binding.memory_snapshot = MemorySnapshot::capture(&mem);
                    }
                }
            }
            self.persist_bindings();
            self.deliver_serialized_input(registry, sid, prompt.as_bytes(), true)
                .map_err(|e| e.to_string())?;
            let _ = now_ms;
            return Ok((sid, false));
        }
        let (sid, _pid) = self.start_bot_session(
            registry,
            store,
            secrets,
            relay,
            composio_base_url,
            bot_id,
            None,
            Some(prompt),
            None,
        )?;
        Ok((sid, true))
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
        let targets: Vec<SessionId> = lock(&self.sessions)
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
        let owns = lock(&self.sessions)
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
        registry.kill_bot_session(session_id)?;
        self.on_session_removed(relay, session_id);
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
    ) -> Result<(SessionId, u32), String> {
        let bot = store
            .get_bot(bot_id)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("bot not found: {}", bot_id.as_str()))?;
        self.reset_chat(registry, store, relay, bot_id, None, now_ms)
            .map_err(|err| err.to_string())?;
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
            let mut sessions = lock(&self.sessions);
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
            sessions.remove(&session_id)
        };
        lock(&self.turn_controls).remove(&session_id);
        if let Some(b) = binding
            && let Some(token) = b.launch_token
        {
            relay.revoke_token(&token);
        }
        self.persist_bindings();
        Ok(())
    }

    /// Re-sends the most recent prompt on a live bot session (`SendBotRunAgain`).
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
            let mut sessions = lock(&self.sessions);
            let Some(binding) = sessions.get_mut(&session_id) else {
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
            binding.explicit_reply_posted = false;
            binding.owner_turn_pending = true;
            if let Ok(mem) = store.get_memory(bot_id) {
                binding.memory_snapshot = MemorySnapshot::capture(&mem);
            }
            last
        };
        self.persist_bindings();
        self.deliver_serialized_input(registry, session_id, prompt.as_bytes(), true)
    }

    /// Marks that `bot_id` explicitly called `PostBotReply` during its current turn.
    pub fn mark_explicit_reply(&self, bot_id: &BotId, link_id: Option<&str>) {
        let mut map = lock(&self.sessions);
        for binding in map.values_mut() {
            if &binding.bot_id == bot_id
                && (link_id.is_none() || binding.link_id.as_deref() == link_id)
            {
                binding.explicit_reply_posted = true;
            }
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
    pub fn on_hook_event_with_groups(
        &self,
        registry: &Registry,
        store: &BotStore,
        connectors: Option<&ConnectorManager>,
        groups: Option<&GroupCoordinator>,
        session_id: SessionId,
        event: &HookEvent,
    ) {
        let binding_opt = lock(&self.sessions).get(&session_id).cloned();
        let Some(binding) = binding_opt else {
            return;
        };

        // 1. Report tool/turn activity to messaging connectors.
        if let Some(activity) = extract_tool_activity(&event.payload)
            && let Some(mgr) = connectors
        {
            mgr.on_turn_activity(&binding.bot_id, session_id, &activity);
        }

        // 2. On turn stop: heal silent MEMORY.md deletions, mark first_run_completed, extract reply.
        if !status::is_turn_stop(&event.payload) {
            return;
        }

        let ts = now_ms();

        // Heal any silent fact deletions against pre-turn snapshot.
        if let Ok(current_mem) = store.get_memory(&binding.bot_id) {
            let (restored_content, restored_facts) =
                binding.memory_snapshot.restore_missing(&current_mem);
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
            if let Some(entry) = lock(&self.sessions).get_mut(&session_id) {
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

        let final_reply = if binding.explicit_reply_posted {
            if let Some(entry) = lock(&self.sessions).get_mut(&session_id) {
                entry.explicit_reply_posted = false;
                entry.owner_turn_pending = false;
                entry.chat_offset = next_offset;
            }
            None
        } else {
            if let Some(entry) = lock(&self.sessions).get_mut(&session_id) {
                entry.owner_turn_pending = false;
                entry.chat_offset = next_offset;
            }
            cleaned_reply
        };
        self.persist_bindings();

        if let Some(g) = groups
            && let Some(ref reply_text) = final_reply
        {
            g.on_bot_turn_finished(&binding.bot_id, session_id, reply_text, ts)
                .ok();
        }
        if let Some(mgr) = connectors {
            mgr.on_turn_complete(&binding.bot_id, session_id, final_reply.as_deref());
        }
    }

    /// Cleans up session state and revokes the session's ephemeral `BotToolRelay` token.
    pub fn on_session_removed(&self, relay: &BotToolRelay, session_id: SessionId) {
        let removed = lock(&self.sessions).remove(&session_id);
        lock(&self.turn_controls).remove(&session_id);
        if let Some(binding) = removed {
            if let Some(token) = binding.launch_token {
                relay.revoke_token(&token);
            }
            self.persist_bindings();
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
            caller_session.and_then(|sid| {
                lock(&self.sessions)
                    .get(&sid)
                    .filter(|b| b.bot_id == reply.bot_id)
                    .and_then(|b| b.link_id.clone())
            })
        } else {
            None
        };
        let resolved_link_id = reply.link_id.as_deref().or(session_link_id.as_deref());
        let link = match resolved_link_id {
            Some(lid) => store.get_link(lid).map_err(|e| e.to_string())?,
            None => store
                .latest_link_for_bot(&reply.bot_id)
                .map_err(|e| e.to_string())?,
        };
        let ext_ref = connectors
            .post_reply(link.as_ref(), reply)
            .map_err(|err| err.to_string())?;
        let substantive = !reply.text.trim().is_empty() || !reply.files.is_empty();
        if substantive {
            self.mark_explicit_reply(&reply.bot_id, resolved_link_id);
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

    fn ensure_binding_for_live_session(
        &self,
        registry: &Registry,
        store: &BotStore,
        bot_id: &BotId,
        sid: SessionId,
        link_id: Option<String>,
        now_ms: u64,
    ) {
        let missing = !lock(&self.sessions).contains_key(&sid);
        if !missing {
            return;
        }
        let memory = store.get_memory(bot_id).unwrap_or_default();
        let pid = registry.session_pid(sid).unwrap_or(0);
        let label = link_id
            .clone()
            .unwrap_or_else(|| bot_id.as_str().to_owned());
        lock(&self.turn_controls)
            .entry(sid)
            .or_insert_with(|| SessionTurnControl::new(true));
        lock(&self.sessions)
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
        self.persist_bindings();
    }

    fn persist_bindings(&self) {
        let Some(ref root) = self.bots_root else {
            return;
        };
        let _io = lock(&self.bindings_io_lock);
        let records: Vec<PersistedBotBinding> = lock(&self.sessions)
            .iter()
            .map(|(&session_id, b)| PersistedBotBinding {
                session_id,
                bot_id: b.bot_id.clone(),
                link_id: b.link_id.clone(),
                chat_offset: b.chat_offset,
                pid: b.pid,
                label: b.label.clone(),
                created_at_ms: b.created_at_ms,
                owner_turn_pending: b.owner_turn_pending,
                last_prompt: b.last_prompt.clone(),
            })
            .collect();
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
            let poll_from = if snap.reset { 0 } else { prev_offset };
            let next_snap = registry.chat_poll(session_id, poll_from);
            if !next_snap.bound {
                snap = next_snap;
                break;
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
    (pty_tail, snap.next_offset.max(prev_offset))
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
        if let Some(first_line) = prompt.lines().next().map(str::trim)
            && !first_line.is_empty()
            && let Some(stripped) = text.strip_prefix(first_line)
        {
            text = stripped.trim().to_owned();
        }
        if let Some(stripped) = text.strip_prefix(prompt) {
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
        lock(&rt1.sessions).insert(
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
        rt1.persist_bindings();
        assert!(dir.join(RUNTIME_BINDINGS_FILE).is_file());

        let rt2 = BotRuntime::with_root(dir.clone());
        let store = BotStore::new(dir.clone());
        assert_eq!(rt2.rehydrate_from_disk(&store, &reg), 0);
        assert!(!rt2.is_bot_session(SessionId(999)));

        fs::remove_dir_all(&dir).ok();
    }
}
