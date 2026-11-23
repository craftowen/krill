// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Durable multi-bot group chat threads, messages, `@mention` + [`BotGroupReplyMode`] routing,
//! and active turn completion relay.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder};
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use krill_proto::SessionId;
use krill_proto::bots::{
    BotConfig, BotGroupChat, BotGroupMessage, BotGroupReplyAudience, BotGroupReplyMode,
    BotGroupRoomView, BotGroupSummary, BotGroupThread, BotGroupTurnInfo, BotGroupView, BotId,
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
const META_FILE: &str = "group-meta.json";
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
    #[serde(default)]
    pub turn_seq: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GroupRuntimeData {
    #[serde(default)]
    threads: Vec<BotGroupThread>,
    #[serde(default)]
    messages: Vec<BotGroupMessage>,
    #[serde(default)]
    turns: Vec<ActiveGroupTurn>,
    #[serde(default)]
    audiences: BTreeMap<String, BotGroupReplyAudience>,
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
            data.audiences.remove(group_id);
            self.persist_locked(data)?;
        }
        Ok(())
    }

    pub(crate) fn prune_audience_for_updated_group(&self, group: &BotGroupChat) {
        let _io = lock(&self.io_lock);
        if self.ensure_loaded_locked().is_err() {
            return;
        }
        let mut guard = lock(&self.state);
        if let Some(data) = guard.as_mut()
            && let Some(BotGroupReplyAudience::Bot { bot_id }) = data.audiences.get(&group.id)
            && !group.bot_ids.contains(bot_id)
        {
            data.audiences
                .insert(group.id.clone(), BotGroupReplyAudience::Mentions);
            self.persist_locked(data).ok();
        }
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

    pub fn list_group_summaries(&self, store: &BotStore) -> io::Result<Vec<BotGroupSummary>> {
        let groups = store.list_groups()?;
        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let guard = lock(&self.state);
        let data = guard
            .as_ref()
            .ok_or_else(|| io::Error::other("missing state"))?;

        Ok(groups
            .into_iter()
            .map(|group| {
                let mut threads: Vec<BotGroupThread> = data
                    .threads
                    .iter()
                    .filter(|t| t.group_id == group.id)
                    .cloned()
                    .collect();
                threads.sort_by_key(|a| std::cmp::Reverse(a.updated_at_ms));
                let latest_message = data
                    .messages
                    .iter()
                    .rev()
                    .find(|m| {
                        m.group_id == group.id && (!m.body.trim().is_empty() || m.in_progress)
                    })
                    .cloned();
                let active_turns = data
                    .turns
                    .iter()
                    .filter(|t| t.group_id == group.id)
                    .map(|t| BotGroupTurnInfo {
                        group_id: t.group_id.clone(),
                        thread_id: t.thread_id.clone(),
                        bot_id: t.bot_id.clone(),
                        session_id: t.session_id,
                        message_id: t.placeholder_message_id.clone(),
                        started_at_ms: t.started_at_ms,
                    })
                    .collect();
                let reply_audience =
                    effective_audience(data.audiences.get(&group.id), &group.bot_ids);
                BotGroupSummary {
                    group,
                    reply_audience,
                    threads,
                    latest_message,
                    active_turns,
                }
            })
            .collect())
    }

    pub fn set_reply_audience(
        &self,
        store: &BotStore,
        group_id: &str,
        audience: BotGroupReplyAudience,
    ) -> io::Result<BotGroupReplyAudience> {
        let group = store.get_group(group_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("group not found: {group_id}"),
            )
        })?;
        if let BotGroupReplyAudience::Bot { ref bot_id } = audience
            && !group.bot_ids.contains(bot_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "bot {} is not a member of group {group_id}",
                    bot_id.as_str()
                ),
            ));
        }
        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        let data = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;
        data.audiences.insert(group.id, audience.clone());
        self.persist_locked(data)?;
        Ok(audience)
    }

    pub fn read_group_room(
        &self,
        store: &BotStore,
        group_id: &str,
        thread_id: Option<&str>,
        before_message_id: Option<&str>,
        limit: Option<u32>,
    ) -> io::Result<BotGroupRoomView> {
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

        let mut thread_msgs: Vec<BotGroupMessage> = if let Some(ref tid) = selected_thread_id {
            data.messages
                .iter()
                .filter(|m| m.group_id == group.id && &m.thread_id == tid)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        let end = before_message_id
            .and_then(|anchor| thread_msgs.iter().position(|m| m.message_id == anchor))
            .unwrap_or(thread_msgs.len());
        let start = end.saturating_sub(cap);
        let has_earlier = start > 0;
        let messages = thread_msgs.drain(start..end).collect();

        let active_turns = data
            .turns
            .iter()
            .filter(|t| {
                t.group_id == group.id
                    && selected_thread_id
                        .as_ref()
                        .is_none_or(|tid| &t.thread_id == tid)
            })
            .map(|t| BotGroupTurnInfo {
                group_id: t.group_id.clone(),
                thread_id: t.thread_id.clone(),
                bot_id: t.bot_id.clone(),
                session_id: t.session_id,
                message_id: t.placeholder_message_id.clone(),
                started_at_ms: t.started_at_ms,
            })
            .collect();
        let reply_audience = effective_audience(data.audiences.get(&group.id), &group.bot_ids);

        Ok(BotGroupRoomView {
            group,
            reply_audience,
            threads,
            active_thread_id: selected_thread_id,
            messages,
            has_earlier,
            active_turns,
        })
    }

    pub fn read_group(
        &self,
        store: &BotStore,
        group_id: &str,
        thread_id: Option<&str>,
        limit: Option<u32>,
    ) -> io::Result<BotGroupView> {
        let room = self.read_group_room(store, group_id, thread_id, None, limit)?;
        let active_run =
            !room.active_turns.is_empty() || room.messages.iter().any(|m| m.in_progress);
        Ok(BotGroupView {
            group: room.group,
            threads: room.threads,
            messages: room.messages,
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
        let audience = reply_mode.map(|mode| match mode {
            BotGroupReplyMode::All | BotGroupReplyMode::Selected => BotGroupReplyAudience::All,
            BotGroupReplyMode::Mentions => BotGroupReplyAudience::Mentions,
        });
        let legacy_selected = reply_mode == Some(BotGroupReplyMode::Selected);
        self.prepare_message_dispatch_inner(
            store,
            group_id,
            thread_id,
            sender,
            text,
            audience,
            legacy_selected,
            now_ms,
        )
    }

    #[expect(clippy::too_many_arguments, reason = "group message dispatch inputs")]
    pub(crate) fn prepare_message_dispatch_with_audience(
        &self,
        store: &BotStore,
        group_id: &str,
        thread_id: Option<&str>,
        sender: &str,
        text: &str,
        audience: BotGroupReplyAudience,
        now_ms: u64,
    ) -> io::Result<GroupDispatchPlan> {
        self.prepare_message_dispatch_inner(
            store,
            group_id,
            thread_id,
            sender,
            text,
            Some(audience),
            false,
            now_ms,
        )
    }

    #[expect(clippy::too_many_arguments, reason = "group message dispatch inputs")]
    fn prepare_message_dispatch_inner(
        &self,
        store: &BotStore,
        group_id: &str,
        thread_id: Option<&str>,
        sender: &str,
        text: &str,
        audience: Option<BotGroupReplyAudience>,
        legacy_selected: bool,
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

        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        let data = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;

        let mentioned_ids = extract_mentions(trimmed_text, &member_bots);
        let resolved_audience = if legacy_selected {
            if mentioned_ids.is_empty() {
                member_bots
                    .first()
                    .map_or(BotGroupReplyAudience::Mentions, |b| {
                        BotGroupReplyAudience::Bot {
                            bot_id: b.id.clone(),
                        }
                    })
            } else {
                BotGroupReplyAudience::Mentions
            }
        } else {
            match audience {
                Some(explicit) => explicit,
                None => effective_audience(data.audiences.get(&group.id), &group.bot_ids),
            }
        };
        let sender_bot_id = member_bots
            .iter()
            .find(|b| {
                sender_name == b.id.as_str()
                    || sender_name == format!("bot:{}", b.id.as_str())
                    || sender_name == format!("bot:{}", b.slug.as_str())
            })
            .map(|b| b.id.clone());
        let target_ids: Vec<BotId> = match resolved_audience {
            BotGroupReplyAudience::All => member_bots
                .iter()
                .filter(|b| sender_bot_id.as_ref() != Some(&b.id))
                .map(|b| b.id.clone())
                .collect(),
            BotGroupReplyAudience::Mentions => mentioned_ids.clone(),
            BotGroupReplyAudience::Bot { bot_id } => {
                if mentioned_ids.is_empty() {
                    vec![bot_id]
                } else {
                    mentioned_ids.clone()
                }
            }
        };

        let target_bots: Vec<BotConfig> = member_bots
            .into_iter()
            .filter(|b| target_ids.contains(&b.id) && !b.paused)
            .collect();

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

    #[cfg(test)]
    pub(crate) fn record_turn_started(
        &self,
        group_id: &str,
        thread_id: &str,
        bot: &BotConfig,
        session_id: SessionId,
        now_ms: u64,
    ) -> io::Result<()> {
        self.record_turn_started_seq(group_id, thread_id, bot, session_id, None, now_ms)
    }

    pub(crate) fn record_turn_started_seq(
        &self,
        group_id: &str,
        thread_id: &str,
        bot: &BotConfig,
        session_id: SessionId,
        turn_seq: Option<u64>,
        now_ms: u64,
    ) -> io::Result<()> {
        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        let data = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;

        let mut replaced_placeholders = Vec::new();
        data.turns.retain(|t| {
            let replacing =
                t.group_id == group_id && t.thread_id == thread_id && t.bot_id == bot.id;
            if replacing {
                replaced_placeholders.push(t.placeholder_message_id.clone());
            }
            !replacing
        });
        if !replaced_placeholders.is_empty() {
            data.messages.retain(|m| {
                !(replaced_placeholders.contains(&m.message_id)
                    && m.in_progress
                    && m.body.trim().is_empty())
            });
            for msg in &mut data.messages {
                if replaced_placeholders.contains(&msg.message_id) && msg.in_progress {
                    msg.in_progress = false;
                }
            }
        }

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
        data.turns.push(ActiveGroupTurn {
            group_id: group_id.to_owned(),
            thread_id: thread_id.to_owned(),
            bot_id: bot.id.clone(),
            session_id,
            placeholder_message_id: placeholder_id,
            started_at_ms: now_ms,
            turn_seq,
        });
        prune_thread_messages(&mut data.messages, group_id, thread_id);
        self.persist_locked(data)?;
        Ok(())
    }

    /// Records a visible dispatch failure note in a group thread when a targeted bot could not
    /// be started or woken (e.g., missing CLI, unknown agent, or spawn error).
    pub(crate) fn record_dispatch_failure(
        &self,
        group_id: &str,
        thread_id: &str,
        bot: &BotConfig,
        error: &str,
        now_ms: u64,
    ) -> io::Result<BotGroupMessage> {
        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        let data = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;

        let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
        let msg = BotGroupMessage {
            message_id: format!("gmsg-{now_ms}-{seq}"),
            group_id: group_id.to_owned(),
            thread_id: thread_id.to_owned(),
            sender_id: bot.id.as_str().to_owned(),
            sender_name: bot.name.clone(),
            body: format!("[dispatch failed: {}]", error.trim()),
            mentions: Vec::new(),
            created_at_ms: now_ms,
            in_progress: false,
        };
        data.messages.push(msg.clone());
        if let Some(t) = data
            .threads
            .iter_mut()
            .find(|t| t.group_id == group_id && t.thread_id == thread_id)
        {
            t.updated_at_ms = now_ms;
        }
        prune_thread_messages(&mut data.messages, group_id, thread_id);
        self.persist_locked(data)?;
        Ok(msg)
    }

    /// Relays a finished bot turn's output into any matching active group chat turn.
    #[cfg(test)]
    pub(crate) fn on_bot_turn_finished(
        &self,
        bot_id: &BotId,
        session_id: SessionId,
        reply_text: &str,
        now_ms: u64,
    ) -> io::Result<Option<BotGroupMessage>> {
        self.on_bot_turn_finished_seq(bot_id, session_id, None, reply_text, now_ms)
    }

    /// Relays a finished bot turn's output into a matching active group chat turn, enforcing
    /// `turn_seq` correlation when both sides supply a sequence number so an earlier non-group
    /// turn cannot hijack or leak into a later group placeholder.
    pub(crate) fn on_bot_turn_finished_seq(
        &self,
        bot_id: &BotId,
        session_id: SessionId,
        turn_seq: Option<u64>,
        reply_text: &str,
        now_ms: u64,
    ) -> io::Result<Option<BotGroupMessage>> {
        let _io = lock(&self.io_lock);
        self.ensure_loaded_locked()?;
        let mut guard = lock(&self.state);
        let data = guard
            .as_mut()
            .ok_or_else(|| io::Error::other("missing state"))?;

        let Some(turn_idx) = data.turns.iter().position(|t| {
            &t.bot_id == bot_id
                && t.session_id == session_id
                && match (t.turn_seq, turn_seq) {
                    (Some(expected), Some(actual)) => expected == actual,
                    _ => true,
                }
        }) else {
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
        self.stop_group_member(store, group_id, thread_id, None)
    }

    pub(crate) fn stop_group_member(
        &self,
        store: &BotStore,
        group_id: &str,
        thread_id: Option<&str>,
        bot_id: Option<&BotId>,
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
            let matches_bot = bot_id.is_none_or(|bid| &turn.bot_id == bid);
            if matches_group && matches_thread && matches_bot {
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
            let matches_bot = bot_id.is_none_or(|bid| msg.sender_id == bid.as_str());
            if msg.group_id == group_id && matches_thread && matches_bot && msg.in_progress {
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
            let mut removed_placeholders = Vec::new();
            data.turns.retain(|t| {
                if t.session_id == session_id {
                    removed_placeholders.push(t.placeholder_message_id.clone());
                    false
                } else {
                    true
                }
            });
            if !removed_placeholders.is_empty() {
                data.messages.retain(|m| {
                    !(removed_placeholders.contains(&m.message_id)
                        && m.in_progress
                        && m.body.trim().is_empty())
                });
                for msg in &mut data.messages {
                    if removed_placeholders.contains(&msg.message_id) && msg.in_progress {
                        msg.in_progress = false;
                    }
                }
                self.persist_locked(data).ok();
            }
        }
    }

    /// Cleans up active turns, placeholders, audiences, and pruned 0-member groups when a bot is deleted.
    pub(crate) fn on_bot_deleted(&self, bot_id: &BotId, removed_group_ids: &[String]) {
        let _io = lock(&self.io_lock);
        if self.ensure_loaded_locked().is_err() {
            return;
        }
        let mut guard = lock(&self.state);
        if let Some(data) = guard.as_mut() {
            let mut changed = false;
            let mut removed_placeholders = Vec::new();
            data.turns.retain(|t| {
                let drop_turn =
                    &t.bot_id == bot_id || removed_group_ids.iter().any(|gid| gid == &t.group_id);
                if drop_turn {
                    removed_placeholders.push(t.placeholder_message_id.clone());
                    changed = true;
                }
                !drop_turn
            });
            if !removed_placeholders.is_empty() {
                data.messages.retain(|m| {
                    !(removed_placeholders.contains(&m.message_id)
                        && m.in_progress
                        && m.body.trim().is_empty())
                });
                for msg in &mut data.messages {
                    if removed_placeholders.contains(&msg.message_id) && msg.in_progress {
                        msg.in_progress = false;
                    }
                }
            }
            if !removed_group_ids.is_empty() {
                let prev_t = data.threads.len();
                let prev_m = data.messages.len();
                data.threads
                    .retain(|t| !removed_group_ids.iter().any(|gid| gid == &t.group_id));
                data.messages
                    .retain(|m| !removed_group_ids.iter().any(|gid| gid == &m.group_id));
                for gid in removed_group_ids {
                    if data.audiences.remove(gid).is_some() {
                        changed = true;
                    }
                }
                if data.threads.len() != prev_t || data.messages.len() != prev_m {
                    changed = true;
                }
            }
            for audience in data.audiences.values_mut() {
                if matches!(audience, BotGroupReplyAudience::Bot { bot_id: b } if b == bot_id) {
                    *audience = BotGroupReplyAudience::Mentions;
                    changed = true;
                }
            }
            if changed {
                self.persist_locked(data).ok();
            }
        }
    }

    fn ensure_loaded_locked(&self) -> io::Result<()> {
        if lock(&self.state).is_some() {
            return Ok(());
        }
        ensure_no_symlink_path(&self.bots_root)?;
        let threads: Vec<BotGroupThread> = self.read_json_or_default(THREADS_FILE)?;
        let messages: Vec<BotGroupMessage> = self.read_json_or_default(MESSAGES_FILE)?;
        let turns: Vec<ActiveGroupTurn> = self.read_json_or_default(TURNS_FILE)?;
        let audiences: BTreeMap<String, BotGroupReplyAudience> =
            self.read_json_or_default(META_FILE)?;
        *lock(&self.state) = Some(GroupRuntimeData {
            threads,
            messages,
            turns,
            audiences,
        });
        Ok(())
    }

    fn persist_locked(&self, data: &GroupRuntimeData) -> io::Result<()> {
        ensure_no_symlink_path(&self.bots_root)?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.bots_root)?;
        self.write_json(THREADS_FILE, &data.threads)?;
        self.write_json(MESSAGES_FILE, &data.messages)?;
        self.write_json(TURNS_FILE, &data.turns)?;
        self.write_json(META_FILE, &data.audiences)?;
        Ok(())
    }

    fn read_json_or_default<T: Default + for<'de> Deserialize<'de>>(
        &self,
        filename: &str,
    ) -> io::Result<T> {
        let path = self.bots_root.join(filename);
        ensure_no_symlink_path(&path)?;
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
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

    fn write_json<T: Serialize + ?Sized>(&self, filename: &str, value: &T) -> io::Result<()> {
        let path = self.bots_root.join(filename);
        ensure_no_symlink_path(&path)?;
        let mut bytes = serde_json::to_vec_pretty(value)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        bytes.push(b'\n');
        write_atomic_0600(&path, &bytes)
    }
}

fn effective_audience(
    saved: Option<&BotGroupReplyAudience>,
    member_ids: &[BotId],
) -> BotGroupReplyAudience {
    match saved {
        Some(BotGroupReplyAudience::Bot { bot_id }) if member_ids.contains(bot_id) => {
            BotGroupReplyAudience::Bot {
                bot_id: bot_id.clone(),
            }
        }
        Some(BotGroupReplyAudience::All) => BotGroupReplyAudience::All,
        _ => BotGroupReplyAudience::Mentions,
    }
}

fn extract_mentions(text: &str, members: &[BotConfig]) -> Vec<BotId> {
    let refs: Vec<(&BotId, &str, &str)> = members
        .iter()
        .map(|b| (&b.id, b.slug.as_str(), b.name.as_str()))
        .collect();
    krill_proto::bots::extract_group_mention_ids(text, &refs)
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

        // V2 room pagination, reply audience persistence & pruning, @everyone, and per-member stop.
        let grp3 = store
            .create_group(
                "Incident Room",
                vec![alice.id.clone(), bob.id.clone()],
                String::new(),
                2_000,
            )
            .unwrap();
        let saved = groups
            .set_reply_audience(
                &store,
                &grp3.id,
                BotGroupReplyAudience::Bot {
                    bot_id: bob.id.clone(),
                },
            )
            .unwrap();
        assert_eq!(
            saved,
            BotGroupReplyAudience::Bot {
                bot_id: bob.id.clone()
            }
        );

        let plan_default = groups
            .prepare_message_dispatch_with_audience(
                &store,
                &grp3.id,
                None,
                "owner",
                "first message",
                saved.clone(),
                2_100,
            )
            .unwrap();
        assert_eq!(plan_default.target_bots.len(), 1);
        assert_eq!(plan_default.target_bots[0].id, bob.id);

        let plan_everyone = groups
            .prepare_message_dispatch_with_audience(
                &store,
                &grp3.id,
                Some(&plan_default.thread.thread_id),
                "owner",
                "@everyone status?",
                BotGroupReplyAudience::Mentions,
                2_200,
            )
            .unwrap();
        assert_eq!(plan_everyone.target_bots.len(), 2);

        groups
            .record_turn_started(
                &grp3.id,
                &plan_default.thread.thread_id,
                &alice,
                SessionId(91),
                2_210,
            )
            .unwrap();
        groups
            .record_turn_started(
                &grp3.id,
                &plan_default.thread.thread_id,
                &bob,
                SessionId(92),
                2_220,
            )
            .unwrap();
        let stopped_one = groups
            .stop_group_member(
                &store,
                &grp3.id,
                Some(&plan_default.thread.thread_id),
                Some(&alice.id),
            )
            .unwrap();
        assert_eq!(stopped_one, vec![(alice.id.clone(), SessionId(91))]);
        let room_mid = groups
            .read_group_room(&store, &grp3.id, None, None, Some(1))
            .unwrap();
        assert!(room_mid.has_earlier);
        assert_eq!(room_mid.messages.len(), 1);
        assert_eq!(room_mid.active_turns.len(), 1);
        assert_eq!(room_mid.active_turns[0].bot_id, bob.id);

        let updated = GroupCoordinator::update_group(
            &store,
            &grp3.id,
            None,
            None,
            Some(vec![alice.id.clone()]),
            2_300,
        )
        .unwrap();
        assert_eq!(updated.bot_ids, vec![alice.id.clone()]);

        // D-07(a): on_session_removed only cleans placeholders for that exact session_id.
        groups
            .record_turn_started_seq(
                &grp3.id,
                &plan_default.thread.thread_id,
                &alice,
                SessionId(201),
                Some(1),
                2_400,
            )
            .unwrap();
        groups
            .record_turn_started_seq(
                &grp3.id,
                &plan_default.thread.thread_id,
                &bob,
                SessionId(202),
                Some(1),
                2_410,
            )
            .unwrap();
        groups.on_session_removed(SessionId(201));
        let room_after_remove = groups
            .read_group_room(
                &store,
                &grp3.id,
                Some(&plan_default.thread.thread_id),
                None,
                Some(50),
            )
            .unwrap();
        assert_eq!(room_after_remove.active_turns.len(), 1);
        assert_eq!(room_after_remove.active_turns[0].bot_id, bob.id);
        assert!(
            room_after_remove
                .messages
                .iter()
                .any(|m| m.sender_id == bob.id.as_str() && m.in_progress),
            "bob's in_progress placeholder must remain untouched when alice's session is removed"
        );

        // S-01: turn_seq mismatch must NOT consume bob's active group placeholder.
        let wrong_seq = groups
            .on_bot_turn_finished_seq(
                &bob.id,
                SessionId(202),
                Some(999),
                "Private reply leak",
                2_420,
            )
            .unwrap();
        assert!(wrong_seq.is_none());
        let right_seq = groups
            .on_bot_turn_finished_seq(&bob.id, SessionId(202), Some(1), "Real group reply", 2_430)
            .unwrap()
            .unwrap();
        assert_eq!(right_seq.body, "Real group reply");

        // D-07(d): All reply mode excludes the sending bot itself.
        let grp_both = store
            .create_group(
                "Both",
                vec![alice.id.clone(), bob.id.clone()],
                String::new(),
                2_500,
            )
            .unwrap();
        let self_plan = groups
            .prepare_message_dispatch(
                &store,
                &grp_both.id,
                None,
                alice.id.as_str(),
                "Broadcast from alice",
                Some(BotGroupReplyMode::All),
                2_510,
            )
            .unwrap();
        assert_eq!(self_plan.target_bots.len(), 1);
        assert_eq!(self_plan.target_bots[0].id, bob.id);

        // D-07(c): replacing an active turn removes the old empty placeholder.
        groups
            .record_turn_started_seq(
                &grp_both.id,
                &self_plan.thread.thread_id,
                &bob,
                SessionId(301),
                Some(1),
                2_520,
            )
            .unwrap();
        groups
            .record_turn_started_seq(
                &grp_both.id,
                &self_plan.thread.thread_id,
                &bob,
                SessionId(301),
                Some(2),
                2_530,
            )
            .unwrap();
        let room_replaced = groups
            .read_group_room(
                &store,
                &grp_both.id,
                Some(&self_plan.thread.thread_id),
                None,
                Some(50),
            )
            .unwrap();
        let in_prog_count = room_replaced
            .messages
            .iter()
            .filter(|m| m.in_progress)
            .count();
        assert_eq!(in_prog_count, 1);

        // D-07(b): empty reply cleans active turn and placeholder.
        groups
            .on_bot_turn_finished_seq(&bob.id, SessionId(301), Some(2), "", 2_540)
            .unwrap();
        let view_clean = groups
            .read_group(
                &store,
                &grp_both.id,
                Some(&self_plan.thread.thread_id),
                Some(50),
            )
            .unwrap();
        assert!(!view_clean.active_run);
        groups.prune_audience_for_updated_group(&updated);
        let summaries = groups.list_group_summaries(&store).unwrap();
        let summary3 = summaries.iter().find(|s| s.group.id == grp3.id).unwrap();
        assert_eq!(summary3.reply_audience, BotGroupReplyAudience::Mentions);

        fs::remove_dir_all(&root).ok();
    }
}
