// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Durable multi-bot group chat threads, messages, `@mention` + [`BotGroupReplyMode`] routing,
//! and active turn completion relay.

use std::collections::BTreeSet;
use std::fs::{self, DirBuilder};
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::SessionId;
use krill_proto::bots::{
    BotConfig, BotGroupChat, BotGroupMessage, BotGroupReplyMode, BotGroupThread, BotGroupView,
    BotId,
};
use krill_proto::os::fs::DirBuilderExt as _;
use krill_proto::os::write_atomic_0600;
use serde::{Deserialize, Serialize};

use crate::bots::secrets::ensure_no_symlink_path;
use crate::bots::store::BotStore;
use crate::session::lock;

const THREADS_FILE: &str = "group-threads.json";
const MESSAGES_FILE: &str = "group-messages.json";
const TURNS_FILE: &str = "group-turns.json";
const MAX_GROUP_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_MESSAGES_PER_THREAD: usize = 200;
const MAX_MESSAGE_BYTES: usize = 32 * 1024;
const DEFAULT_READ_LIMIT: usize = 100;

static MSG_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActiveGroupTurn {
    pub group_id: String,
    pub thread_id: String,
    pub bot_id: BotId,
    pub session_id: SessionId,
    pub placeholder_message_id: String,
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GroupRuntimeData {
    #[serde(default)]
    threads: Vec<BotGroupThread>,
    #[serde(default)]
    messages: Vec<BotGroupMessage>,
    #[serde(default)]
    turns: Vec<ActiveGroupTurn>,
}

/// Dispatch plan produced when posting a message to a group thread.
#[derive(Debug, Clone)]
pub(crate) struct GroupDispatchPlan {
    pub group: BotGroupChat,
    pub thread: BotGroupThread,
    pub posted_message: BotGroupMessage,
    pub target_bots: Vec<BotConfig>,
    pub prompt_for_bot: String,
}

/// Strictly-lazy durable coordinator for multi-bot group conversations.
#[derive(Debug)]
pub struct GroupCoordinator {
    bots_root: PathBuf,
    io_lock: Mutex<()>,
    state: Mutex<Option<GroupRuntimeData>>,
}

impl GroupCoordinator {
    /// Creates a lazy [`GroupCoordinator`] doing zero disk I/O until first group operation.
    #[must_use]
    pub fn new(bots_root: PathBuf) -> Self {
        Self {
            bots_root,
            io_lock: Mutex::new(()),
            state: Mutex::new(None),
        }
    }

    pub fn update_group(
        store: &BotStore,
        group_id: &str,
        name: Option<&str>,
        description: Option<String>,
        bot_ids: Option<Vec<BotId>>,
        now_ms: u64,
    ) -> io::Result<BotGroupChat> {
        store.update_group(group_id, name, description, bot_ids, now_ms)
    }

    pub fn delete_group(&self, store: &BotStore, group_id: &str) -> io::Result<()> {
        store.delete_group(group_id)?;
        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        if let Some(data) = guard.as_mut() {
            data.threads.retain(|t| t.group_id != group_id);
            data.messages.retain(|m| m.group_id != group_id);
            data.turns.retain(|t| t.group_id != group_id);
            self.persist_locked(data)?;
        }
        Ok(())
    }

    pub fn reorder_groups(store: &BotStore, group_ids: &[String]) -> io::Result<Vec<BotGroupChat>> {
        store.reorder_groups(group_ids)
    }

    pub fn new_thread(
        &self,
        store: &BotStore,
        group_id: &str,
        title: Option<&str>,
        now_ms: u64,
    ) -> io::Result<BotGroupThread> {
        let group = store.get_group(group_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("group not found: {group_id}"),
            )
        })?;
        let cleaned_title = title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("New thread")
            .to_owned();

        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        let data = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;
        let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
        let thread = BotGroupThread {
            thread_id: format!("thr-{now_ms}-{seq}"),
            group_id: group.id,
            title: cleaned_title,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        data.threads.push(thread.clone());
        self.persist_locked(data)?;
        Ok(thread)
    }

    pub fn read_group(
        &self,
        store: &BotStore,
        group_id: &str,
        thread_id: Option<&str>,
        limit: Option<u32>,
    ) -> io::Result<BotGroupView> {
        let group = store.get_group(group_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("group not found: {group_id}"),
            )
        })?;

        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let guard = lock(&self.state);
        let data = guard
            .as_ref()
            .ok_or_else(|| io::Error::other("missing state"))?;

        let mut threads: Vec<BotGroupThread> = data
            .threads
            .iter()
            .filter(|t| t.group_id == group.id)
            .cloned()
            .collect();
        threads.sort_by_key(|a| std::cmp::Reverse(a.updated_at_ms));

        let selected_thread_id = match thread_id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(explicit) => {
                if !threads.iter().any(|t| t.thread_id == explicit) {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("group thread not found: {explicit}"),
                    ));
                }
                Some(explicit.to_owned())
            }
            None => threads.first().map(|t| t.thread_id.clone()),
        };

        let cap = limit
            .and_then(|n| usize::try_from(n).ok())
            .map_or(DEFAULT_READ_LIMIT, |n| n.clamp(1, MAX_MESSAGES_PER_THREAD));

        let mut messages: Vec<BotGroupMessage> = if let Some(ref tid) = selected_thread_id {
            data.messages
                .iter()
                .filter(|m| m.group_id == group.id && &m.thread_id == tid)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if messages.len() > cap {
            let start = messages.len().saturating_sub(cap);
            messages.drain(..start);
        }

        let active_run = data.turns.iter().any(|t| {
            t.group_id == group.id
                && selected_thread_id
                    .as_ref()
                    .is_none_or(|tid| &t.thread_id == tid)
        }) || messages.iter().any(|m| m.in_progress);

        Ok(BotGroupView {
            group,
            threads,
            messages,
            active_run,
        })
    }

    #[expect(clippy::too_many_arguments, reason = "group message dispatch inputs")]
    pub(crate) fn prepare_message_dispatch(
        &self,
        store: &BotStore,
        group_id: &str,
        thread_id: Option<&str>,
        sender: &str,
        text: &str,
        reply_mode: Option<BotGroupReplyMode>,
        now_ms: u64,
    ) -> io::Result<GroupDispatchPlan> {
        let trimmed_text = text.trim();
        if trimmed_text.is_empty() || trimmed_text.len() > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "group message text must be between 1 and 32768 bytes",
            ));
        }
        let sender_clean = sender.trim();
        let sender_name = if sender_clean.is_empty() {
            "owner".to_owned()
        } else {
            sender_clean.to_owned()
        };

        let group = store.get_group(group_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("group not found: {group_id}"),
            )
        })?;

        let all_bots = store.list_bots()?;
        let mut member_bots = Vec::new();
        for bid in &group.bot_ids {
            if let Some(bot) = all_bots.iter().find(|b| &b.id == bid) {
                member_bots.push(bot.clone());
            }
        }

        let mentioned_ids = extract_mentions(trimmed_text, &member_bots);
        let mode = reply_mode.unwrap_or_default();
        let target_ids: Vec<BotId> = match mode {
            BotGroupReplyMode::All => member_bots.iter().map(|b| b.id.clone()).collect(),
            BotGroupReplyMode::Mentions => mentioned_ids.clone(),
            BotGroupReplyMode::Selected => {
                if mentioned_ids.is_empty() {
                    member_bots
                        .first()
                        .map(|b| vec![b.id.clone()])
                        .unwrap_or_default()
                } else {
                    mentioned_ids.clone()
                }
            }
        };

        let target_bots: Vec<BotConfig> = member_bots
            .into_iter()
            .filter(|b| target_ids.contains(&b.id) && !b.paused)
            .collect();

        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        let data = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;

        let thread = if let Some(explicit) = thread_id.map(str::trim).filter(|s| !s.is_empty()) {
            let pos = data
                .threads
                .iter()
                .position(|t| t.group_id == group.id && t.thread_id == explicit)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("group thread not found: {explicit}"),
                    )
                })?;
            let Some(t) = data.threads.get_mut(pos) else {
                return Err(io::Error::other("group thread vanished"));
            };
            t.updated_at_ms = now_ms;
            t.clone()
        } else if let Some(pos) = data
            .threads
            .iter()
            .enumerate()
            .filter(|(_, t)| t.group_id == group.id)
            .max_by_key(|(_, t)| t.updated_at_ms)
            .map(|(i, _)| i)
        {
            let Some(t) = data.threads.get_mut(pos) else {
                return Err(io::Error::other("group thread vanished"));
            };
            t.updated_at_ms = now_ms;
            t.clone()
        } else {
            let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
            let created = BotGroupThread {
                thread_id: format!("thr-{now_ms}-{seq}"),
                group_id: group.id.clone(),
                title: "Main thread".to_owned(),
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            };
            data.threads.push(created.clone());
            created
        };

        let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
        let posted_message = BotGroupMessage {
            message_id: format!("gmsg-{now_ms}-{seq}"),
            group_id: group.id.clone(),
            thread_id: thread.thread_id.clone(),
            sender_id: sender_name.clone(),
            sender_name: sender_name.clone(),
            body: trimmed_text.to_owned(),
            mentions: mentioned_ids,
            created_at_ms: now_ms,
            in_progress: false,
        };
        data.messages.push(posted_message.clone());
        prune_thread_messages(&mut data.messages, &group.id, &thread.thread_id);

        let recent_context: Vec<String> = data
            .messages
            .iter()
            .filter(|m| m.group_id == group.id && m.thread_id == thread.thread_id && !m.in_progress)
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|m| format!("{}: {}", m.sender_name, m.body))
            .collect();

        let member_names: Vec<String> = group
            .bot_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect();
        let prompt_for_bot = format!(
            "[Group Chat '{}' ({}) | Thread: '{}' ({}) | Members: {}]\n\
             Recent messages:\n{}\n\n\
             Reply concisely as yourself in this group thread. Your final reply will be posted back to the group.",
            group.name,
            group.id,
            thread.title,
            thread.thread_id,
            member_names.join(", "),
            recent_context.join("\n"),
        );

        self.persist_locked(data)?;
        Ok(GroupDispatchPlan {
            group,
            thread,
            posted_message,
            target_bots,
            prompt_for_bot,
        })
    }

    pub(crate) fn record_turn_started(
        &self,
        group_id: &str,
        thread_id: &str,
        bot: &BotConfig,
        session_id: SessionId,
        now_ms: u64,
    ) -> io::Result<()> {
        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        let data = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;

        let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
        let placeholder_id = format!("gmsg-{now_ms}-{seq}");
        data.messages.push(BotGroupMessage {
            message_id: placeholder_id.clone(),
            group_id: group_id.to_owned(),
            thread_id: thread_id.to_owned(),
            sender_id: bot.id.as_str().to_owned(),
            sender_name: bot.name.clone(),
            body: String::new(),
            mentions: Vec::new(),
            created_at_ms: now_ms,
            in_progress: true,
        });
        data.turns.retain(|t| {
            !(t.group_id == group_id && t.thread_id == thread_id && t.bot_id == bot.id)
        });
        data.turns.push(ActiveGroupTurn {
            group_id: group_id.to_owned(),
            thread_id: thread_id.to_owned(),
            bot_id: bot.id.clone(),
            session_id,
            placeholder_message_id: placeholder_id,
            started_at_ms: now_ms,
        });
        prune_thread_messages(&mut data.messages, group_id, thread_id);
        self.persist_locked(data)?;
        Ok(())
    }

    /// Relays a finished bot turn's output into any matching active group chat turn.
    pub(crate) fn on_bot_turn_finished(
        &self,
        bot_id: &BotId,
        session_id: SessionId,
        reply_text: &str,
        now_ms: u64,
    ) -> io::Result<Option<BotGroupMessage>> {
        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        let data = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;

        let Some(turn_idx) = data
            .turns
            .iter()
            .position(|t| &t.bot_id == bot_id && t.session_id == session_id)
        else {
            return Ok(None);
        };
        let turn = data.turns.remove(turn_idx);
        let trimmed = reply_text.trim();
        let mut completed_msg = None;

        if trimmed.is_empty() {
            data.messages
                .retain(|m| m.message_id != turn.placeholder_message_id);
        } else if let Some(msg) = data
            .messages
            .iter_mut()
            .find(|m| m.message_id == turn.placeholder_message_id)
        {
            trimmed.clone_into(&mut msg.body);
            msg.in_progress = false;
            msg.created_at_ms = now_ms;
            completed_msg = Some(msg.clone());
        } else {
            let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
            let msg = BotGroupMessage {
                message_id: format!("gmsg-{now_ms}-{seq}"),
                group_id: turn.group_id.clone(),
                thread_id: turn.thread_id.clone(),
                sender_id: bot_id.as_str().to_owned(),
                sender_name: bot_id.as_str().to_owned(),
                body: trimmed.to_owned(),
                mentions: Vec::new(),
                created_at_ms: now_ms,
                in_progress: false,
            };
            data.messages.push(msg.clone());
            completed_msg = Some(msg);
        }
        if let Some(t) = data
            .threads
            .iter_mut()
            .find(|t| t.group_id == turn.group_id && t.thread_id == turn.thread_id)
        {
            t.updated_at_ms = now_ms;
        }
        self.persist_locked(data)?;
        Ok(completed_msg)
    }

    /// Cancels all active turns for `group_id` (optionally scoped to `thread_id`) and returns
    /// `(bot_id, session_id)` pairs to interrupt in `BotRuntime`.
    pub(crate) fn stop_group(
        &self,
        store: &BotStore,
        group_id: &str,
        thread_id: Option<&str>,
    ) -> io::Result<Vec<(BotId, SessionId)>> {
        if store.get_group(group_id)?.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("group not found: {group_id}"),
            ));
        }

        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        let data = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;

        let mut stopped = Vec::new();
        let mut keep_turns = Vec::with_capacity(data.turns.len());
        for turn in std::mem::take(&mut data.turns) {
            let matches_group = turn.group_id == group_id;
            let matches_thread = thread_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none_or(|tid| turn.thread_id == tid);
            if matches_group && matches_thread {
                stopped.push((turn.bot_id, turn.session_id));
            } else {
                keep_turns.push(turn);
            }
        }
        data.turns = keep_turns;

        for msg in &mut data.messages {
            let matches_thread = thread_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_none_or(|tid| msg.thread_id == tid);
            if msg.group_id == group_id && matches_thread && msg.in_progress {
                msg.in_progress = false;
                if msg.body.trim().is_empty() {
                    "[stopped]".clone_into(&mut msg.body);
                }
            }
        }
        data.messages.retain(|m| {
            !(m.group_id == group_id
                && m.body == "[stopped]"
                && !m.in_progress
                && m.sender_id.is_empty())
        });

        self.persist_locked(data)?;
        Ok(stopped)
    }

    pub(crate) fn on_session_removed(&self, session_id: SessionId) {
        let _io = lock(&self.io_lock);
        if self.ensure_loaded_locked().is_err() {
            return;
        }
        let mut guard = lock(&self.state);
        if let Some(data) = guard.as_mut() {
            let before = data.turns.len();
            data.turns.retain(|t| t.session_id != session_id);
            if data.turns.len() != before {
                for msg in &mut data.messages {
                    if msg.in_progress && msg.body.trim().is_empty() {
                        msg.in_progress = false;
                    }
                }
                data.messages
                    .retain(|m| !m.body.is_empty() || m.in_progress);
                self.persist_locked(data).ok();
            }
        }
    }

    fn ensure_loaded_locked(&self) -> io::Result<()> {
        if lock(&self.state).is_some() {
            return Ok(());
        }
        ensure_no_symlink_path(&self.bots_root)?;
        let threads: Vec<BotGroupThread> = self.read_vec_or_default(THREADS_FILE)?;
        let messages: Vec<BotGroupMessage> = self.read_vec_or_default(MESSAGES_FILE)?;
        let turns: Vec<ActiveGroupTurn> = self.read_vec_or_default(TURNS_FILE)?;
        *lock(&self.state) = Some(GroupRuntimeData {
            threads,
            messages,
            turns,
        });
        Ok(())
    }

    fn persist_locked(&self, data: &GroupRuntimeData) -> io::Result<()> {
        ensure_no_symlink_path(&self.bots_root)?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.bots_root)?;
        self.write_vec(THREADS_FILE, &data.threads)?;
        self.write_vec(MESSAGES_FILE, &data.messages)?;
        self.write_vec(TURNS_FILE, &data.turns)?;
        Ok(())
    }

    fn read_vec_or_default<T: for<'de> Deserialize<'de>>(
        &self,
        filename: &str,
    ) -> io::Result<Vec<T>> {
        let path = self.bots_root.join(filename);
        ensure_no_symlink_path(&path)?;
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        if bytes.len() > MAX_GROUP_JSON_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "group metadata file exceeds size limit",
            ));
        }
        Ok(serde_json::from_slice(&bytes).unwrap_or_default())
    }

    fn write_vec<T: Serialize>(&self, filename: &str, items: &[T]) -> io::Result<()> {
        let path = self.bots_root.join(filename);
        ensure_no_symlink_path(&path)?;
        let mut bytes = serde_json::to_vec_pretty(items)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        bytes.push(b'\n');
        write_atomic_0600(&path, &bytes)
    }
}

fn extract_mentions(text: &str, members: &[BotConfig]) -> Vec<BotId> {
    let mut mentioned = Vec::new();
    let mut seen = BTreeSet::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars.get(i).copied() == Some('@') {
            let prev_ok = i == 0
                || chars
                    .get(i.saturating_sub(1))
                    .is_some_and(|c| !c.is_alphanumeric() && *c != '_');
            if prev_ok {
                let start = i + 1;
                let mut end = start;
                while end < chars.len()
                    && chars
                        .get(end)
                        .is_some_and(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                {
                    end += 1;
                }
                let token: String = chars.get(start..end).unwrap_or_default().iter().collect();
                if !token.is_empty() {
                    for bot in members {
                        let clean_name = bot.name.replace(' ', "-");
                        let matched = bot.id.as_str().eq_ignore_ascii_case(&token)
                            || bot.slug.as_str().eq_ignore_ascii_case(&token)
                            || bot.name.eq_ignore_ascii_case(&token)
                            || clean_name.eq_ignore_ascii_case(&token);
                        if matched && seen.insert(bot.id.clone()) {
                            mentioned.push(bot.id.clone());
                        }
                    }
                }
            }
        }
        i += 1;
    }
    // Also support exact `@<Full Name With Spaces>` mentions if present.
    let lower = text.to_ascii_lowercase();
    for bot in members {
        let needle = format!("@{}", bot.name.to_ascii_lowercase());
        if lower.contains(&needle) && seen.insert(bot.id.clone()) {
            mentioned.push(bot.id.clone());
        }
    }
    mentioned
}

fn prune_thread_messages(messages: &mut Vec<BotGroupMessage>, group_id: &str, thread_id: &str) {
    let count = messages
        .iter()
        .filter(|m| m.group_id == group_id && m.thread_id == thread_id)
        .count();
    if count <= MAX_MESSAGES_PER_THREAD {
        return;
    }
    let mut to_drop = count.saturating_sub(MAX_MESSAGES_PER_THREAD);
    messages.retain(|m| {
        if to_drop > 0 && m.group_id == group_id && m.thread_id == thread_id && !m.in_progress {
            to_drop -= 1;
            false
        } else {
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::CreateBotParams;
    use std::path::PathBuf;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-bot-groups-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn group_threads_mentions_turn_completion_and_stop() {
        let root = temp_dir("flow");
        let store = BotStore::new(root.clone());
        let groups = GroupCoordinator::new(root.clone());

        let alice = store
            .create_bot(
                CreateBotParams {
                    name: "Alice Bot".to_owned(),
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
        let bob = store
            .create_bot(
                CreateBotParams {
                    name: "Bob Bot".to_owned(),
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
                },
                1_100,
            )
            .unwrap();

        let grp = store
            .create_group(
                "Triage Room",
                vec![alice.id.clone(), bob.id.clone()],
                "Joint triage".to_owned(),
                1_200,
            )
            .unwrap();

        let thr = groups
            .new_thread(&store, &grp.id, Some("Release v2"), 1_300)
            .unwrap();

        let plan_mention = groups
            .prepare_message_dispatch(
                &store,
                &grp.id,
                Some(&thr.thread_id),
                "owner",
                "@alice-bot please check CI",
                Some(BotGroupReplyMode::Mentions),
                1_400,
            )
            .unwrap();
        assert_eq!(plan_mention.target_bots.len(), 1);
        assert_eq!(plan_mention.target_bots.first().unwrap().id, alice.id);

        groups
            .record_turn_started(&grp.id, &thr.thread_id, &alice, SessionId(77), 1_450)
            .unwrap();
        let view_active = groups
            .read_group(&store, &grp.id, Some(&thr.thread_id), None)
            .unwrap();
        assert!(view_active.active_run);

        let reply = groups
            .on_bot_turn_finished(&alice.id, SessionId(77), "CI is green!", 1_500)
            .unwrap()
            .unwrap();
        assert_eq!(reply.body, "CI is green!");
        assert!(!reply.in_progress);

        let view_done = groups
            .read_group(&store, &grp.id, Some(&thr.thread_id), None)
            .unwrap();
        assert!(!view_done.active_run);
        assert_eq!(view_done.messages.len(), 2);

        // `All` dispatches both bots, `stop_group` cancels both active turns cleanly.
        let plan_all = groups
            .prepare_message_dispatch(
                &store,
                &grp.id,
                Some(&thr.thread_id),
                "owner",
                "Everyone check this",
                Some(BotGroupReplyMode::All),
                1_600,
            )
            .unwrap();
        assert_eq!(plan_all.target_bots.len(), 2);
        groups
            .record_turn_started(&grp.id, &thr.thread_id, &alice, SessionId(78), 1_610)
            .unwrap();
        groups
            .record_turn_started(&grp.id, &thr.thread_id, &bob, SessionId(79), 1_620)
            .unwrap();
        let stopped = groups
            .stop_group(&store, &grp.id, Some(&thr.thread_id))
            .unwrap();
        assert_eq!(stopped.len(), 2);
        let view_stopped = groups
            .read_group(&store, &grp.id, Some(&thr.thread_id), None)
            .unwrap();
        assert!(!view_stopped.active_run);

        // Reorder + update + delete group.
        let grp2 = store
            .create_group("Ops Room", vec![bob.id.clone()], String::new(), 1_700)
            .unwrap();
        let reordered =
            GroupCoordinator::reorder_groups(&store, &[grp2.id.clone(), grp.id.clone()]).unwrap();
        assert_eq!(reordered.first().unwrap().id, grp2.id);

        groups.delete_group(&store, &grp.id).unwrap();
        assert!(store.get_group(&grp.id).unwrap().is_none());

        fs::remove_dir_all(&root).ok();
    }
}
