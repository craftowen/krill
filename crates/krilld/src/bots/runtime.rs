// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Bot session launch, hook integration (`is_turn_stop` & tool activity), reply extraction,
//! `MEMORY.md` post-turn silent-deletion recovery, and inter-bot mail routing.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::SessionId;
use krill_proto::agent_history::ChatItemKind;
use krill_proto::bots::{
    BOT_QUIET_MARKER, BotConfig, BotId, MemorySnapshot, build_launch_preamble, init_bot_home,
};
use krill_proto::hook::HookEvent;
use krill_proto::orchestration::{MessageType, OrchestrationStore, SendParams};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::bots::composio;
use crate::bots::connectors::{ConnectorManager, InboundExternalMessage, OutboundBotReply};
use crate::bots::secrets::SecretVault;
use crate::bots::store::BotStore;
use crate::bots::tool_relay::BotToolRelay;
use crate::registry::Registry;
use crate::session::lock;
use crate::status;

const STACK_SIZE: usize = 128 * 1024;
const PREAMBLE_READY_WAIT: Duration = Duration::from_secs(15);

/// Active mapping between a live Krill PTY session and the bot running inside it.
#[derive(Clone)]
pub struct BotSessionBinding {
    pub bot_id: BotId,
    pub link_id: Option<String>,
    pub memory_snapshot: MemorySnapshot,
    pub chat_offset: u64,
    pub explicit_reply_posted: bool,
    pub launch_token: Option<Zeroizing<String>>,
}

impl fmt::Debug for BotSessionBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotSessionBinding")
            .field("bot_id", &self.bot_id)
            .field("link_id", &self.link_id)
            .field("chat_offset", &self.chat_offset)
            .field("explicit_reply_posted", &self.explicit_reply_posted)
            .finish_non_exhaustive()
    }
}

/// Runtime coordinator tracking live bot sessions and handling hooks/mail/replies.
#[derive(Debug, Default)]
pub struct BotRuntime {
    sessions: Mutex<HashMap<SessionId, BotSessionBinding>>,
}

impl BotRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    /// Starts a new bot session via normal `Registry` + `holder::Spawn`, injects bot env vars and
    /// ephemeral `BotToolRelay` credentials, and delivers the launch preamble + optional prompt.
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
            composio::resolve_and_register_composio(&bot, secrets, relay, composio_base_url).ok();
        }

        let grant = relay
            .prepare_launch_for_bot(&bot)
            .map_err(|err| format!("failed to prepare bot tool relay: {err}"))?;

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

        let (session_id, pid) = registry.spawn_bot_agent_session(
            &bot.agent_id,
            bot.model.as_deref(),
            bot.effort.as_deref(),
            bot.permission_mode,
            &launch_cwd,
            extra_env,
        )?;

        let binding = BotSessionBinding {
            bot_id: bot.id.clone(),
            link_id,
            memory_snapshot: MemorySnapshot::capture(&memory),
            chat_offset: 0,
            explicit_reply_posted: false,
            launch_token: grant.map(|g| g.token),
        };
        lock(&self.sessions).insert(session_id, binding);

        // Deliver launch preamble (+ optional initial prompt) once the agent PTY is ready.
        let initial_input = match prompt.map(str::trim).filter(|s| !s.is_empty()) {
            Some(user_prompt) => format!("{preamble}\n\n---\n\n{user_prompt}"),
            None => preamble,
        };
        if let Some(tap) = registry.session_tap(session_id) {
            let bytes = initial_input.into_bytes();
            std::thread::Builder::new()
                .name(format!("bot-preamble-{}", session_id.0))
                .stack_size(STACK_SIZE)
                .spawn(move || {
                    let deadline = Instant::now() + PREAMBLE_READY_WAIT;
                    while Instant::now() < deadline {
                        let p = tap.pulse();
                        if p.closed || (p.end > 0 && tap.quiet_ms() >= 150) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    if !tap.pulse().closed {
                        drop(Registry::deliver_input_to_tap(&tap, &bytes, true));
                    }
                })
                .ok();
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

        // Reuse bound session if still alive.
        if let Some(sid) = link.session_id
            && registry.is_session_live(sid)
        {
            if let Some(binding) = lock(&self.sessions).get_mut(&sid) {
                binding.explicit_reply_posted = false;
                if let Ok(mem) = store.get_memory(&msg.bot_id) {
                    binding.memory_snapshot = MemorySnapshot::capture(&mem);
                }
            }
            registry.deliver_bot_input(sid, formatted.as_bytes(), true)?;
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
    /// Fast-paths out immediately if `session_id` is not a bot session.
    pub fn on_hook_event(
        &self,
        registry: &Registry,
        store: &BotStore,
        connectors: Option<&ConnectorManager>,
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
                    .update_memory(&binding.bot_id, &restored_content, true)
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

        // Mark first_run_completed = true after the bot completes its first turn.
        store.mark_first_run_completed(&binding.bot_id, ts).ok();

        // Extract final reply (unless already explicitly posted via PostBotReply).
        let final_reply = if binding.explicit_reply_posted {
            if let Some(entry) = lock(&self.sessions).get_mut(&session_id) {
                entry.explicit_reply_posted = false;
            }
            None
        } else {
            let (reply, next_offset) =
                extract_final_reply(registry, session_id, binding.chat_offset);
            if let Some(entry) = lock(&self.sessions).get_mut(&session_id) {
                entry.chat_offset = next_offset;
            }
            reply.filter(|text| !is_quiet_reply(text))
        };

        if let Some(mgr) = connectors {
            mgr.on_turn_complete(&binding.bot_id, session_id, final_reply.as_deref());
        }
    }

    /// Cleans up session state and revokes the session's ephemeral `BotToolRelay` token.
    pub fn on_session_removed(&self, relay: &BotToolRelay, session_id: SessionId) {
        let removed = lock(&self.sessions).remove(&session_id);
        if let Some(binding) = removed
            && let Some(token) = binding.launch_token
        {
            relay.revoke_token(&token);
        }
    }

    /// Sends inter-bot mail (`bot:<name>`, `<bot_id>`, or a [`BotGroupChat`] name/ID) via
    /// [`OrchestrationStore`] and notifies live bot sessions.
    #[expect(clippy::too_many_arguments, reason = "inter-bot mail routing context")]
    pub fn send_bot_mail(
        &self,
        registry: &Registry,
        store: &BotStore,
        from: &str,
        to_bot: &str,
        subject: &str,
        body: &str,
        thread_id: Option<&str>,
    ) -> Result<String, String> {
        let dir = registry
            .state_dir()
            .ok_or_else(|| "daemon state directory unavailable".to_owned())?;

        // Resolve target: either a group chat (fan-out to member bots) or a single bot.
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

            // If target bot has a live session, deliver a brief wake prompt directly.
            if let Some(sid) = self.live_session_for_bot(registry, &target.id) {
                let notice = format!("[Krill bot mail from {from}] Subject: {subject}\n{body}");
                registry
                    .deliver_bot_input(sid, notice.as_bytes(), true)
                    .ok();
            }
        }

        registry.notify_orch();
        Ok(first_msg_id.unwrap_or_else(|| format!("mail-{}", now_ms())))
    }

    /// Posts an explicit bot reply via [`ConnectorManager`] and marks the current turn replied.
    pub fn post_bot_reply(
        &self,
        store: &BotStore,
        connectors: &ConnectorManager,
        reply: &OutboundBotReply,
    ) -> Result<Option<String>, String> {
        let link = match reply.link_id.as_deref() {
            Some(lid) => store.get_link(lid).map_err(|e| e.to_string())?,
            None => store
                .latest_link_for_bot(&reply.bot_id)
                .map_err(|e| e.to_string())?,
        };
        let ext_ref = connectors
            .post_reply(link.as_ref(), reply)
            .map_err(|err| err.to_string())?;
        self.mark_explicit_reply(&reply.bot_id, reply.link_id.as_deref());
        Ok(ext_ref)
    }
}

/// Extracts the latest assistant reply via `agent_history/chat.rs` (`claude`/`codex`/`agy`/`opencode`)
/// with automatic fallback to latest plain PTY screen capture (`grok`/`omp`/`pi`/`cursor`).
pub fn extract_final_reply(
    registry: &Registry,
    session_id: SessionId,
    prev_offset: u64,
) -> (Option<String>, u64) {
    let snap = registry.chat_poll(session_id, prev_offset);
    if snap.bound {
        let assistant_text = snap
            .items
            .iter()
            .rev()
            .find(|item| item.kind == ChatItemKind::Assistant && !item.text.trim().is_empty())
            .map(|item| item.text.trim().to_owned());
        if assistant_text.is_some() {
            return (assistant_text, snap.next_offset);
        }
        // Retry from offset 0 if incremental poll saw no assistant item yet (e.g. reset).
        if prev_offset > 0 {
            let full = registry.chat_poll(session_id, 0);
            let fallback_text = full
                .items
                .iter()
                .rev()
                .find(|item| item.kind == ChatItemKind::Assistant && !item.text.trim().is_empty())
                .map(|item| item.text.trim().to_owned());
            if fallback_text.is_some() {
                return (fallback_text, full.next_offset);
            }
        }
    }

    // Fallback for agents without structured transcript normalization (`grok`/`omp`/`pi`/`cursor`).
    let pty_tail = registry.capture_session_plain_tail(session_id);
    (pty_tail, snap.next_offset.max(prev_offset))
}

/// Returns `true` when a bot reply ends with [`BOT_QUIET_MARKER`] (`[[KRILL_QUIET]]`).
#[must_use]
pub fn is_quiet_reply(text: &str) -> bool {
    text.trim_end().ends_with(BOT_QUIET_MARKER)
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
}
