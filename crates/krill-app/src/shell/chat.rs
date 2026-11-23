// UI interaction patterns referenced from Orca native-chat (MIT).
// Copyright (c) 2026 Lovecast Inc.
//! OWE-946 / MK-243: 데스크톱 `[Chat | Term]` 하이브리드 뷰 확장.
//!
//! 활성 세션의 PTY 연결(`Live`)을 끊지 않은 채 `Request::AgentHistory(Action::ChatPoll)`로
//! 정규화된 `ChatItem` 카드 스트림·인터랙티브 질문 카드·권한 승인 카드·하단 Live Activity Strip을
//! 렌더링하고 1-클릭으로 `[Chat | Term]` 양방향 전환을 제공한다.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Subscription, Task, Window, div,
};
use gpui_component::Disableable as _;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use krill_app::i18n::catalog::chat as ctext;
use krill_app::ui;
use krill_proto::SessionId;
use krill_proto::agent_history::{Action, ChatItem, ChatItemKind, ChatPollSnapshot, Response};
use krill_proto::control::{Reply, Request};
use krill_proto::screen::Cell;

use super::{PaneId, Shell};

/// 채팅 상태의 키. 세션 ID는 데몬마다 따로 세므로 로컬과 원격 세션 번호가 겹친다.
pub(super) type ChatKey = (krill_app::hosts::HostId, SessionId);
pub(super) type BotKey = (krill_app::hosts::HostId, krill_proto::bots::BotId);

#[cfg(test)]
pub(super) fn local_key(session: SessionId) -> ChatKey {
    (krill_app::hosts::HostId::Local, session)
}

mod actions;
mod body;
mod bot_rows;
mod catalog;
mod code_view;
mod composer;
mod images;
mod prompt;
#[cfg(test)]
mod prompt_tests;
mod rate_limit;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod ui_tests;
mod view;
mod work_run;
use actions::{RequestIdentity, Submission};
#[cfg(test)]
mod action_tests;
#[cfg(test)]
mod poll_tests;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ComposerModelItem {
    pub id: String,
    pub label: String,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Tracks independent provider capability, loading, and rate-limit flags on ComposerMeta."
)]
pub(crate) struct ComposerMeta {
    pub model: Option<String>,
    pub model_label: Option<String>,
    pub current_model_id: Option<String>,
    pub effort: Option<String>,
    pub context_percent: Option<u8>,
    pub models: Vec<ComposerModelItem>,
    pub efforts: Vec<String>,
    pub supported: bool,
    pub effort_supported: bool,
    pub reason: Option<String>,
    pub pending: bool,
    pub rate_limited: bool,
    pub live_prompt: Option<krill_proto::screen::prompt::PromptKind>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftAttachment {
    pub path: std::path::PathBuf,
    pub display_name: String,
    pub byte_len: u64,
    pub is_image: bool,
}

impl DraftAttachment {
    pub(super) fn from_path(path: std::path::PathBuf) -> Option<Self> {
        use krill_proto::os::ffi::OsStrExt as _;
        let raw_bytes = path.as_os_str().as_bytes();
        if raw_bytes.is_empty() || raw_bytes.iter().any(|&b| b < 0x20 || b == 0x7f) {
            return None;
        }
        let byte_len = match std::fs::metadata(&path) {
            Ok(meta) => {
                if meta.is_dir() {
                    return None;
                }
                meta.len()
            }
            Err(_) => 0,
        };
        let display_name = path
            .file_name()
            .map_or_else(
                || path.to_string_lossy().into_owned(),
                |name| name.to_string_lossy().into_owned(),
            )
            .chars()
            .filter(|ch| !ch.is_control())
            .collect::<String>();
        if display_name.is_empty() {
            return None;
        }
        let is_image = krill_term::paste::is_image_path(raw_bytes);
        Some(Self {
            path,
            display_name,
            byte_len,
            is_image,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PendingStatus {
    Sending,
    Queued,
    ReviewRequired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingChatPrompt {
    pub id: u64,
    pub origin_item: usize,
    pub submitted_payload: String,
    pub display_text: String,
    pub attachments: Vec<DraftAttachment>,
    pub status: PendingStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueuedChatPrompt {
    pub id: u64,
    pub display_text: String,
    pub submitted_payload: String,
    pub attachments: Vec<DraftAttachment>,
}

#[derive(Default)]
pub(crate) struct SessionChatLog {
    pub agent: String,
    pub offset: u64,
    pub bound: bool,
    pub base_index: usize,
    pub items: Vec<Arc<ChatItem>>,
    pub revision: u64,
    pub meta: ComposerMeta,
    expanded_runs: BTreeSet<usize>,
    expanded_steps: BTreeSet<usize>,
    expanded_user_messages: BTreeSet<usize>,
    wrapped_code: BTreeSet<(usize, usize)>,
    timing: work_run::WorkRunTimingState,
    cached_feed: Option<Arc<work_run::DisplayFeed>>,
    pub answered: BTreeSet<String>,
    binding: (String, String),
    host: Option<krill_app::hosts::HostId>,
    epoch: u64,
    /// The first read for this log came from the end of the transcript.
    seeded: bool,
    /// Byte boundary of the oldest loaded row, while older rows remain on disk.
    earlier: Option<u64>,
    submissions: BTreeMap<String, Submission>,
    pub multi_selected: BTreeMap<String, BTreeSet<usize>>,
    consumed_user_indices: BTreeSet<usize>,
    pub queued_prompts: VecDeque<QueuedChatPrompt>,
    pub unread_from: Option<usize>,
    /// A bot chat's item changes and its newest owner turns' times (newest first).
    item_notes: (Vec<Arc<work_run::ItemNote>>, Vec<u64>),
    /// A bot chat folds each settled turn behind its answer (Orca); `None` elsewhere.
    turn_fold: Option<work_run::TurnFold>,
}

impl SessionChatLog {
    fn display_feed(&mut self) -> Arc<work_run::DisplayFeed> {
        Arc::clone(self.cached_feed.get_or_insert_with(|| {
            if self.items.is_empty() && self.item_notes.0.is_empty() {
                work_run::DisplayFeed::empty()
            } else {
                Arc::new(work_run::DisplayFeed::project_with_notes(
                    self.base_index,
                    &self.items,
                    self.unread_from,
                    &self.item_notes.0,
                    &self.item_notes.1,
                    self.turn_fold.as_ref(),
                ))
            }
        }))
    }

    /// Shows a bot's item changes between the turns of its chat (Orca's item note rows).
    pub(super) fn set_item_notes(&mut self, notes: Vec<Arc<work_run::ItemNote>>, turns: Vec<u64>) {
        if self.item_notes.0 != notes || self.item_notes.1 != turns {
            self.item_notes = (notes, turns);
            self.cached_feed = None;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Turns the bot chat turn fold on (`Some(live)`: whether the last turn still runs) or off.
    pub(super) fn set_turn_fold(&mut self, live: Option<bool>) {
        let next = live.map(|live| work_run::TurnFold {
            live,
            expanded: self
                .turn_fold
                .as_ref()
                .map(|f| f.expanded.clone())
                .unwrap_or_default(),
        });
        if self.turn_fold != next {
            self.turn_fold = next;
            self.cached_feed = None;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Opens or closes the work folded behind the turn the prompt at `user_abs_idx` started.
    pub(super) fn toggle_turn(&mut self, user_abs_idx: usize) {
        if let Some(fold) = self.turn_fold.as_mut() {
            if !fold.expanded.remove(&user_abs_idx) {
                fold.expanded.insert(user_abs_idx);
            }
            self.cached_feed = None;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub(super) fn sync_timing(
        &mut self,
        agent_state: krill_proto::control::AgentState,
        now: std::time::Instant,
    ) {
        let feed = self.display_feed();
        self.timing.sync(&feed, agent_state, now);
    }

    pub(super) fn run_elapsed_secs(
        &self,
        run_start: usize,
        now: std::time::Instant,
    ) -> Option<u64> {
        self.timing.elapsed_secs(run_start, now)
    }

    fn is_run_expanded(&self, span: work_run::RawSpan) -> bool {
        self.expanded_runs.range(span.range()).next().is_some()
    }

    fn toggle_run(&mut self, span: work_run::RawSpan) {
        let range = span.range();
        let before = self.expanded_runs.len();
        self.expanded_runs.retain(|idx| !range.contains(idx));
        if self.expanded_runs.len() == before {
            self.expanded_runs.insert(span.start());
        }
        self.revision = self.revision.wrapping_add(1);
    }

    fn is_step_expanded(&self, abs_idx: usize) -> bool {
        self.expanded_steps.contains(&abs_idx)
    }

    fn toggle_step(&mut self, abs_idx: usize) {
        if !self.expanded_steps.remove(&abs_idx) {
            self.expanded_steps.insert(abs_idx);
        }
        self.revision = self.revision.wrapping_add(1);
    }

    pub(super) fn is_user_message_expanded(&self, abs_idx: usize) -> bool {
        self.expanded_user_messages.contains(&abs_idx)
    }

    pub(super) fn toggle_user_message(&mut self, abs_idx: usize) {
        if !self.expanded_user_messages.remove(&abs_idx) {
            self.expanded_user_messages.insert(abs_idx);
        }
        self.revision = self.revision.wrapping_add(1);
    }

    pub(super) fn is_code_wrapped(&self, abs_idx: usize, code_idx: usize) -> bool {
        self.wrapped_code.contains(&(abs_idx, code_idx))
    }

    pub(super) fn toggle_code_wrap(&mut self, abs_idx: usize, code_idx: usize) {
        let key = (abs_idx, code_idx);
        if !self.wrapped_code.remove(&key) {
            self.wrapped_code.insert(key);
        }
        self.revision = self.revision.wrapping_add(1);
    }

    pub(super) fn reset_transcript(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.items.clear();
        self.base_index = 0;
        self.seeded = false;
        self.earlier = None;
        self.meta = ComposerMeta::default();
        self.answered.clear();
        self.expanded_runs.clear();
        self.expanded_steps.clear();
        self.expanded_user_messages.clear();
        self.wrapped_code.clear();
        self.timing.clear();
        self.cached_feed = None;
        self.multi_selected.clear();
        self.submissions.clear();
        self.consumed_user_indices.clear();
        self.queued_prompts.clear();
        self.unread_from = None;
    }

    pub(super) fn prepend_earlier(&mut self, older: Vec<ChatItem>, next_earlier: Option<u64>) {
        let count = older.len();
        self.items.splice(0..0, older.into_iter().map(Arc::new));
        self.base_index = self.base_index.saturating_sub(count);
        self.earlier = next_earlier;
        self.cached_feed = None;
        self.revision = self.revision.wrapping_add(1);
    }

    pub(super) fn append_and_trim(&mut self, incoming: Vec<ChatItem>, max_kept: usize) {
        if incoming.is_empty() {
            return;
        }
        self.items.extend(incoming.into_iter().map(Arc::new));
        self.cached_feed = None;
        if self.items.len() > max_kept {
            let excess = self.items.len() - max_kept;
            let old_base = self.base_index;
            let base = old_base.saturating_add(excess);
            let old_run_abs_start = work_run::straddling_run_rel_start(&self.items, excess)
                .map(|run_rel| old_base.saturating_add(run_rel));
            if let Some(old_start) = old_run_abs_start
                && self.expanded_runs.range(old_start..base).next().is_some()
            {
                self.expanded_runs.insert(base);
            }
            self.items.drain(0..excess);
            self.base_index = base;
            self.earlier = None;
            let retained: BTreeSet<_> = self
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| format!("{}:{}", item.tool_id, base + index))
                .collect();
            self.expanded_runs.retain(|index| *index >= base);
            self.expanded_steps.retain(|index| *index >= base);
            self.expanded_user_messages.retain(|index| *index >= base);
            self.wrapped_code.retain(|(index, _)| *index >= base);
            self.consumed_user_indices.retain(|index| *index >= base);
            self.timing.migrate_head_drain(old_run_abs_start, base);
            self.answered.retain(|key| retained.contains(key));
            self.multi_selected.retain(|key, _| retained.contains(key));
            self.submissions.retain(|key, _| retained.contains(key));
        }
    }
}

pub(super) fn is_user_prompt_foldable(text: &str) -> bool {
    text.chars().count() > 600 || text.lines().count() > 8
}

struct ChatList {
    host: krill_app::hosts::HostId,
    feed: Arc<work_run::DisplayFeed>,
    pending_len: usize,
    pending_rev: u64,
    revision: u64,
    epoch: u64,
    state: gpui::ListState,
}

impl ChatList {
    fn new(
        host: krill_app::hosts::HostId,
        feed: &Arc<work_run::DisplayFeed>,
        pending_len: usize,
        pending_rev: u64,
        revision: u64,
        epoch: u64,
        owner: gpui::WeakEntity<Shell>,
    ) -> Self {
        let state = gpui::ListState::new(
            feed.len() + pending_len,
            gpui::ListAlignment::Bottom,
            ui::SPACE_4,
        );
        state.set_follow_mode(gpui::FollowMode::Tail);
        state.set_scroll_handler(move |_, _, cx| {
            owner.update(cx, |_, cx| cx.notify()).ok();
        });
        Self {
            host,
            feed: Arc::clone(feed),
            pending_len,
            pending_rev,
            revision,
            epoch,
            state,
        }
    }

    fn near_top(&self, base_index: usize, raw_threshold: usize) -> bool {
        let top_row = self.state.logical_scroll_top().item_ix;
        match self.feed.get(top_row) {
            Some(row) => row.span().start().saturating_sub(base_index) < raw_threshold,
            None => self.feed.is_empty(),
        }
    }

    // Straddling runs have end > cutoff, so partition_point leaves them in place to remeasure.
    fn sync(
        &mut self,
        feed: &Arc<work_run::DisplayFeed>,
        pending_len: usize,
        pending_rev: u64,
        revision: u64,
        epoch: u64,
    ) -> gpui::ListState {
        if epoch == self.epoch && Arc::ptr_eq(&self.feed, feed) {
            let old_total = self.feed.len() + self.pending_len;
            let new_total = feed.len() + pending_len;
            if new_total > old_total {
                self.state
                    .splice(old_total..old_total, new_total - old_total);
            } else if new_total < old_total {
                self.state.splice(new_total..old_total, 0);
            }
            if revision != self.revision || pending_rev != self.pending_rev {
                self.state.remeasure();
                self.revision = revision;
                self.pending_rev = pending_rev;
            }
            self.pending_len = pending_len;
            return self.state.clone();
        }
        let prev = self.feed.rows();
        let next = feed.rows();
        match (prev.first().zip(prev.last()), next.first().zip(next.last())) {
            (Some((prev_first, prev_last)), Some((next_first, next_last)))
                if epoch == self.epoch
                    && next_first.span().start() < prev_last.span().end()
                    && prev_first.span().start() < next_last.span().end() =>
            {
                if self.pending_len != 0 {
                    self.state
                        .splice(prev.len()..(prev.len() + self.pending_len), 0);
                }
                let old_base = prev_first.span().start();
                let new_base = next_first.span().start();
                let removed = if new_base > old_base {
                    prev.partition_point(|r| r.span().end() <= new_base)
                } else {
                    0
                };
                let added = if new_base < old_base {
                    next.partition_point(|r| r.span().end() <= old_base)
                } else {
                    0
                };
                if removed != 0 {
                    self.state.splice(0..removed, 0);
                }
                if added != 0 {
                    self.state.splice(0..0, added);
                }
                let kept = prev.len() - removed + added;
                if next.len() > kept {
                    self.state.splice(kept..kept, next.len() - kept);
                } else if next.len() < kept {
                    self.state.splice(next.len()..kept, 0);
                }
                if pending_len != 0 {
                    self.state.splice(next.len()..next.len(), pending_len);
                }
                if revision != self.revision || pending_rev != self.pending_rev {
                    self.state.remeasure();
                }
            }
            _ => {
                self.state.reset(next.len() + pending_len);
                self.state.set_follow_mode(gpui::FollowMode::Tail);
            }
        }
        self.feed = Arc::clone(feed);
        self.pending_len = pending_len;
        self.pending_rev = pending_rev;
        self.revision = revision;
        self.epoch = epoch;
        self.state.clone()
    }
}

enum ChatFocus {
    Composer(ChatKey),
    Terminal(ChatKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CopiedCodeTarget {
    pub abs_idx: usize,
    pub block_idx: usize,
}

pub(super) struct ChatFindState {
    pub query: String,
    pub input: gpui::Entity<gpui_component::input::InputState>,
    _sub: Subscription,
    pub matches: Vec<usize>,
    pub active: usize,
}

#[derive(Default)]
pub(crate) struct ChatState {
    /// Each bot's item changes, as chat lines, and when they were read (Orca's item note rows).
    bot_notes: BTreeMap<BotKey, (std::time::Instant, Vec<BotNote>)>,
    bot_notes_tasks: BTreeMap<BotKey, Task<()>>,
    /// Each bot's SOUL.md first paragraph, the persona line its empty chat says (Orca).
    pub(super) bot_souls: BTreeMap<BotKey, String>,
    /// Offer IDs currently creating a bot, to prevent duplicate creation on double click.
    pub(super) creating_offers: BTreeSet<String>,
    /// Last known bot metadata for open bot chats so host switches don't strip open chat chrome.
    cached_bots: BTreeMap<ChatKey, krill_app::bots::SidebarBot>,
    /// Memory receipts the reader opened, by their time (Orca's memory note disclosure).
    pub(super) open_memory_notes: BTreeSet<u64>,
    /// Images whose chip was put in each composer's text (Orca's inline image reference).
    pub(super) image_tokens: std::collections::HashMap<ChatKey, BTreeSet<std::path::PathBuf>>,
    /// When a bot's remote host was first seen down (Orca's outage card counts from there).
    pub(super) outage_since: BTreeMap<String, std::time::Instant>,
    poll_timer: Option<Task<()>>,
    poll_failed: BTreeSet<ChatKey>,
    lists: BTreeMap<(PaneId, SessionId), ChatList>,
    focus_pending: Option<ChatFocus>,
    pub logs: BTreeMap<ChatKey, SessionChatLog>,
    pub inputs: BTreeMap<ChatKey, (gpui::Entity<TextareaState>, Subscription)>,
    pub last_poll: Option<std::time::Instant>,
    pub poll_tasks: BTreeMap<ChatKey, Task<()>>,
    poll_generations: BTreeMap<ChatKey, u64>,
    next_poll_generation: u64,
    pub copied_item: Option<(ChatKey, usize, std::time::Instant)>,
    copied_item_timer: Option<Task<()>>,
    pub(super) copied_code: Option<(ChatKey, CopiedCodeTarget)>,
    copied_code_timer: Option<Task<()>>,
    pub quick_menu: BTreeSet<ChatKey>,
    pub model_picker: BTreeSet<ChatKey>,
    pub model_picker_selected: BTreeMap<ChatKey, usize>,
    composer_models_in_flight: BTreeSet<ChatKey>,
    pub(super) finds: BTreeMap<ChatKey, ChatFindState>,
    composer_tasks: BTreeMap<ChatKey, Task<()>>,
    earlier_tasks: BTreeMap<ChatKey, Task<()>>,
    earlier_retry_after: BTreeMap<ChatKey, Task<()>>,
    last_composer_poll: BTreeMap<ChatKey, std::time::Instant>,
    #[cfg(test)]
    pub next_poll_io: Option<tokio::sync::oneshot::Receiver<()>>,
    #[cfg(test)]
    pub poll_call: Option<krill_app::agent_history::Call>,
    #[cfg(test)]
    pub workflow_call: Option<krill_app::agent_history::WorkflowCall>,
    request_tasks: BTreeMap<ChatKey, Task<()>>,
    prompt_failed: BTreeSet<ChatKey>,
    prompt_tasks: BTreeMap<ChatKey, Task<()>>,
    prompt_review: BTreeSet<ChatKey>,
    multiline_blocked: BTreeSet<ChatKey>,
    pub(super) interrupt_latched: std::collections::HashSet<ChatKey>,
    interrupt_turn_since: std::collections::HashMap<ChatKey, u64>,
    pub(super) stop_cooldown_since: BTreeMap<ChatKey, u64>,
    stop_cooldown_timers: BTreeMap<ChatKey, Task<()>>,
    pub(super) question_other_inputs:
        BTreeMap<(ChatKey, String), gpui::Entity<gpui_component::input::InputState>>,
    #[cfg(test)]
    send_input: Option<Box<dyn FnMut(Vec<u8>) -> bool>>,
    #[cfg(test)]
    prompt_delivery: Option<krill_proto::input::Delivery>,
    #[cfg(test)]
    prompt_confirmed: Option<bool>,
    pub pending_prompts: std::collections::HashMap<ChatKey, Vec<PendingChatPrompt>>,
    pub draft_attachments: std::collections::HashMap<ChatKey, Vec<DraftAttachment>>,
    pub(super) images: images::ChatImages,
    pub pending_revision: u64,
    next_pending_id: u64,
    pending_epochs: std::collections::HashMap<(ChatKey, u64), u64>,
    turn_tasks: BTreeMap<(ChatKey, u64), Task<()>>,
    pub copied_pending: Option<(ChatKey, u64, std::time::Instant)>,
    copied_pending_timer: Option<Task<()>>,
    pub autocomplete: BTreeMap<ChatKey, catalog::AutocompleteState>,
    autocomplete_missed_at: BTreeMap<ChatKey, std::time::Instant>,
    attach_tasks: BTreeMap<(ChatKey, u64), Task<()>>,
    next_attach_id: u64,
    sent_history: BTreeMap<ChatKey, Vec<String>>,
    history_cursor: BTreeMap<ChatKey, usize>,
    history_saved_draft: BTreeMap<ChatKey, String>,
    auto_continue: HashMap<ChatKey, rate_limit::AutoContinue>,
    auto_continue_declined: HashMap<ChatKey, rate_limit::Declined>,
    #[cfg(test)]
    pub now_override: Option<u64>,
}

impl ChatState {
    pub(super) fn review_pending_prompt(&mut self, key: &ChatKey) {
        if self.prompt_tasks.contains_key(key) {
            self.prompt_review.insert(key.clone());
        } else if let Some(entries) = self.pending_prompts.get_mut(key) {
            let mut changed = false;
            for entry in entries.iter_mut() {
                if entry.status == PendingStatus::Sending {
                    entry.status = PendingStatus::ReviewRequired;
                    self.turn_tasks.remove(&(key.clone(), entry.id));
                    changed = true;
                }
            }
            if changed {
                self.pending_revision = self.pending_revision.wrapping_add(1);
            }
        }
    }
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_valid_token_string(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/@[]".contains(&b))
}

/// Absolute row indices stay stable when older rows are prepended, so a log starts high.
const CHAT_BASE_ORIGIN: usize = 1 << 31;
// ponytail: fixed row cap to hold the app memory budget; page rows out if deep history is wanted.
const MAX_CHAT_KEPT: usize = 1_000;
const CHAT_PRELOAD_ITEMS: usize = 256;

struct ChatPage {
    snapshot: ChatPollSnapshot,
    start: u64,
    has_earlier: bool,
}

fn chat_tail_request(session: SessionId, before: Option<u64>) -> String {
    serde_json::json!({
        "action": "chat_poll",
        "session": session.0,
        "offset": 0,
        "latest": true,
        "end_offset": before,
    })
    .to_string()
}

/// `None` also covers an older daemon, which has no tail page and is read from the start instead.
fn parse_chat_page(raw: &str) -> Option<ChatPage> {
    let root: serde_json::Value = serde_json::from_str(raw).ok()?;
    if root.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let data = root.get("data")?;
    let mut snapshot: ChatPollSnapshot = serde_json::from_value(data.clone()).ok()?;
    if let Some(raw_items) = data.get("items").and_then(serde_json::Value::as_array) {
        for (item, raw_item) in snapshot.items.iter_mut().zip(raw_items.iter()) {
            if let Some(at) = raw_item
                .get("at")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                at.clone_into(&mut item.at);
            }
        }
    }
    Some(ChatPage {
        start: data.get("page_start")?.as_u64()?,
        has_earlier: data.get("has_earlier")?.as_bool()?,
        snapshot,
    })
}

fn ratio_percent(used: u64, limit: u64) -> Option<u8> {
    if limit == 0 {
        return None;
    }
    let pct = ((u128::from(used) * 100 + u128::from(limit) / 2) / u128::from(limit)).min(100);
    u8::try_from(pct).ok()
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0.0..=100.0"
)]
fn float_percent(percent: f64) -> Option<u8> {
    (percent.is_finite() && (0.0..=100.0).contains(&percent))
        .then(|| percent.round().clamp(0.0, 100.0) as u8)
}

pub(crate) fn parse_transcript_record(record: &serde_json::Value, meta: &mut ComposerMeta) {
    let payload = record.get("payload").unwrap_or(record);
    let message = record.get("message").unwrap_or(record);

    if record.get("type").and_then(serde_json::Value::as_str) == Some("turn_context") {
        if let Some(model) = payload
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|v| is_valid_token_string(v))
        {
            meta.model = Some(model.to_owned());
        }
        for field in ["effort", "reasoning_effort"] {
            if let Some(effort) = payload
                .get(field)
                .and_then(serde_json::Value::as_str)
                .filter(|v| is_valid_token_string(v))
            {
                meta.effort = Some(effort.to_owned());
            }
        }
    }

    if let Some(model) = message
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|v| is_valid_token_string(v) && !v.starts_with('<'))
    {
        meta.model = Some(model.to_owned());
    }

    if let Some(info) = payload.get("info") {
        let used = info
            .pointer("/last_token_usage/total_tokens")
            .and_then(serde_json::Value::as_u64);
        let limit = info
            .get("model_context_window")
            .and_then(serde_json::Value::as_u64);
        if let (Some(used), Some(limit)) = (used, limit)
            && let Some(pct) = ratio_percent(used, limit)
        {
            meta.context_percent = Some(pct);
        }
    }
}

const TRANSCRIPT_META_TTL: Duration = Duration::from_secs(300);
const TRANSCRIPT_META_CACHE_CAP: usize = 64;

type TranscriptFileStamp = Option<(Option<SystemTime>, u64)>;

#[derive(Clone)]
struct CachedTranscriptMeta {
    stamp: TranscriptFileStamp,
    at: Instant,
    meta: ComposerMeta,
}

type TranscriptMetaCache = Mutex<HashMap<String, CachedTranscriptMeta>>;

fn transcript_meta_cache() -> &'static TranscriptMetaCache {
    static CACHE: OnceLock<TranscriptMetaCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn observe_transcript_meta_cached(transcript_path: &str) -> ComposerMeta {
    if transcript_path.is_empty() {
        return ComposerMeta::default();
    }
    let Ok(mut file) = std::fs::File::open(transcript_path) else {
        return ComposerMeta::default();
    };
    let Ok(metadata) = file.metadata() else {
        return ComposerMeta::default();
    };
    let len = metadata.len();
    let stamp = Some((metadata.modified().ok(), len));
    if let Ok(guard) = transcript_meta_cache().lock()
        && let Some(cached) = guard.get(transcript_path)
        && cached.at.elapsed() < TRANSCRIPT_META_TTL
        && cached.stamp == stamp
    {
        return cached.meta.clone();
    }

    let mut meta = ComposerMeta::default();
    let tail_bytes = 64 * 1024;
    let start = len.saturating_sub(tail_bytes);
    if std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(start)).is_ok() {
        let mut buf = Vec::new();
        if std::io::Read::read_to_end(&mut std::io::Read::take(file, tail_bytes), &mut buf).is_ok()
        {
            let text = String::from_utf8_lossy(&buf);
            for line in text.lines().skip(usize::from(start > 0)) {
                if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
                    parse_transcript_record(&record, &mut meta);
                }
            }
        }
    }
    if let Ok(mut guard) = transcript_meta_cache().lock() {
        if guard.len() >= TRANSCRIPT_META_CACHE_CAP {
            guard.retain(|_, v| v.at.elapsed() < TRANSCRIPT_META_TTL);
            if guard.len() >= TRANSCRIPT_META_CACHE_CAP
                && let Some(oldest) = guard
                    .iter()
                    .min_by_key(|(_, v)| v.at)
                    .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest);
            }
        }
        guard.insert(
            transcript_path.to_owned(),
            CachedTranscriptMeta {
                stamp,
                at: Instant::now(),
                meta: meta.clone(),
            },
        );
    }
    meta
}

pub(crate) fn observe_composer_meta(
    state_dir: Option<&std::path::Path>,
    session_id: &str,
    transcript_path: &str,
) -> ComposerMeta {
    let mut meta = observe_transcript_meta_cached(transcript_path);

    if let Some(state_dir) = state_dir {
        // The statusline record is the agent's own report, so it beats the transcript. Session ids
        // are unique, and the profile scope directory belongs to whichever account ran the session.
        let context_dir = state_dir.join("session-context");
        for entry in std::fs::read_dir(context_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let Some(json) =
                std::fs::read_to_string(entry.path().join(format!("{session_id}.json")))
                    .ok()
                    .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
            else {
                continue;
            };
            if let Some(model) = json.get("model").and_then(serde_json::Value::as_str) {
                meta.model = Some(model.to_owned());
            }
            if let Some(effort) = json.get("effort").and_then(serde_json::Value::as_str) {
                meta.effort = Some(effort.to_owned());
            }
            if let Some(percent) = json
                .get("percent")
                .and_then(serde_json::Value::as_f64)
                .and_then(float_percent)
            {
                meta.context_percent = Some(percent);
            }
        }
    }
    meta
}

/// Reads composer meta off the UI thread when the poll may have changed it. `settled` is the
/// binding whose meta is already known; `None` means it must be read.
fn poll_composer_meta(
    snap: &ChatPollSnapshot,
    local: bool,
    settled: Option<&(String, String)>,
) -> Option<ComposerMeta> {
    let fresh = snap.reset
        || !snap.items.is_empty()
        || settled.is_none_or(|(id, path)| *id != snap.session_id || *path != snap.transcript_path);
    (local && snap.bound && fresh).then(|| {
        observe_composer_meta(
            krill_proto::daemon::state_dir().as_deref(),
            &snap.session_id,
            &snap.transcript_path,
        )
    })
}

pub(crate) fn parse_composer_items(items: &[serde_json::Value]) -> Vec<ComposerModelItem> {
    items
        .iter()
        .filter_map(|item| {
            let id = item
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())?;
            let label = item
                .get("label")
                .or_else(|| item.get("name"))
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(id);
            let efforts = item
                .get("efforts")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect();
            let default_effort = item
                .get("default_effort")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            Some(ComposerModelItem {
                id: id.to_owned(),
                label: label.to_owned(),
                efforts,
                default_effort,
            })
        })
        .collect()
}

pub(crate) fn apply_composer_status_json(raw: &str, meta: &mut ComposerMeta) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(raw).ok()?;
    if root.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let data = root.get("data")?;
    let agent = data
        .get("agent")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if let Some(model) = data
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        meta.model = Some(model.to_owned());
    }
    if let Some(label) = data
        .get("model_label")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        meta.model_label = Some(label.to_owned());
    }
    match data.get("effort") {
        Some(serde_json::Value::String(effort)) if !effort.is_empty() => {
            meta.effort = Some(effort.to_owned());
        }
        Some(serde_json::Value::Null) => {
            meta.effort = None;
        }
        _ => {}
    }
    if let Some(pct) = data
        .pointer("/context/percent")
        .and_then(serde_json::Value::as_f64)
        .and_then(float_percent)
    {
        meta.context_percent = Some(pct);
    }
    if let Some(supported) = data.get("supported").and_then(serde_json::Value::as_bool) {
        meta.supported = supported;
    }
    if let Some(effort_supported) = data
        .get("effort_supported")
        .and_then(serde_json::Value::as_bool)
    {
        meta.effort_supported = effort_supported;
    }
    if let Some(models) = data.get("models").and_then(serde_json::Value::as_array) {
        let parsed = parse_composer_items(models);
        if !parsed.is_empty() {
            meta.models = parsed;
        }
    }
    if let Some(efforts) = data.get("efforts").and_then(serde_json::Value::as_array) {
        let parsed: Vec<String> = efforts
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect();
        if !parsed.is_empty() {
            meta.efforts = parsed;
        }
    }
    if let Some(rate_limited) = data
        .get("rate_limited")
        .and_then(serde_json::Value::as_bool)
    {
        meta.rate_limited = rate_limited;
    }
    if let Some(live_prompt) = data.get("live_prompt") {
        meta.live_prompt = if live_prompt.is_null() {
            None
        } else {
            serde_json::from_value(live_prompt.clone()).ok()
        };
    }
    agent
}

pub(crate) fn apply_composer_models_json(raw: &str, meta: &mut ComposerMeta) {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    if root.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return;
    }
    let Some(data) = root.get("data") else {
        return;
    };
    if let Some(supported) = data.get("supported").and_then(serde_json::Value::as_bool) {
        meta.supported = supported;
    }
    if let Some(current) = data
        .get("current")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        meta.current_model_id = Some(current.to_owned());
    }
    if let Some(items) = data.get("items").and_then(serde_json::Value::as_array) {
        let parsed = parse_composer_items(items);
        if !parsed.is_empty() {
            meta.models = parsed;
        }
    }
    meta.reason = data
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
}

pub(crate) fn apply_composer_set_json(raw: &str, meta: &mut ComposerMeta) -> bool {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    if root.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        meta.reason = root
            .get("error")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        return false;
    }
    let Some(data) = root.get("data") else {
        return false;
    };
    let applied = data
        .get("applied")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if applied {
        if let Some(model) = data
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            meta.model = Some(model.to_owned());
            meta.current_model_id = Some(model.to_owned());
        }
        if let Some(label) = data
            .get("label")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            meta.model_label = Some(label.to_owned());
        }
        match data.get("effort") {
            Some(serde_json::Value::String(effort)) if !effort.is_empty() => {
                meta.effort = Some(effort.to_owned());
            }
            Some(serde_json::Value::Null) => {
                meta.effort = None;
            }
            _ => {}
        }
        meta.reason = None;
        true
    } else {
        meta.reason = data
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        false
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct QuestionOptionRow {
    cursor: bool,
    checked: Option<bool>,
}

fn strip_option_number(text: &str) -> Option<&str> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let suffix = text.get(digits..)?;
    [". ", ") ", ": "]
        .iter()
        .find_map(|prefix| suffix.strip_prefix(prefix))
        .map(str::trim_start)
}

fn strip_option_checkbox(text: &str) -> Option<(&str, bool)> {
    [
        ("[✓]", true),
        ("[x]", true),
        ("☑", true),
        ("[ ]", false),
        ("☐", false),
    ]
    .into_iter()
    .find_map(|(prefix, checked)| {
        text.strip_prefix(prefix)
            .map(|rest| (rest.trim_start(), checked))
    })
}

fn option_label_matches(text: &str, label: &str) -> bool {
    text.strip_prefix(label).is_some_and(|suffix| {
        suffix.is_empty()
            || [" — ", " – ", " - ", ": "]
                .iter()
                .any(|sep| suffix.starts_with(sep))
            || (suffix.starts_with(" (") && suffix.ends_with(')'))
    })
}

/// Match whole labels after known TUI decorations, with optional delimited
/// descriptions. A row must identify one option, and an option at most one row.
/// Keep both numbered and literal interpretations so ambiguous labels fail shut.
fn question_option_rows(
    rows: &[Vec<Cell>],
    labels: &[String],
) -> Option<Vec<Option<QuestionOptionRow>>> {
    let labels: Vec<_> = labels.iter().map(|label| normalized(label)).collect();
    let unique: BTreeSet<_> = labels.iter().collect();
    if labels.is_empty() || labels.iter().any(String::is_empty) || unique.len() != labels.len() {
        return None;
    }
    let mut matched = vec![None; labels.len()];
    for row in rows {
        let line = normalized(&row.iter().map(|cell| cell.ch).collect::<String>());
        let body = line.strip_prefix(['❯', '>', '›', '●', '*', '→']);
        let cursor = body.is_some();
        let body = body.unwrap_or(&line).trim_start();
        let mut variants = vec![(body, None)];
        if let Some(numbered) = strip_option_number(body) {
            variants.push((numbered, None));
        }
        for index in 0..variants.len() {
            if let Some((text, checked)) = strip_option_checkbox(variants[index].0) {
                variants.push((text, Some(checked)));
                if let Some(numbered) = strip_option_number(text) {
                    variants.push((numbered, Some(checked)));
                }
            }
        }
        let mut candidate = None;
        for (index, label) in labels.iter().enumerate() {
            for &(text, checked) in &variants {
                if option_label_matches(text, label) {
                    let found = (index, QuestionOptionRow { cursor, checked });
                    if candidate.is_some_and(|previous| previous != found) {
                        return None;
                    }
                    candidate = Some(found);
                }
            }
        }
        if let Some((index, row)) = candidate {
            if matched[index].is_some() {
                return None;
            }
            matched[index] = Some(row);
        }
    }
    Some(matched)
}

/// Return the unique option with a visible TUI cursor; ambiguous prompts require Term.
pub(crate) fn detect_question_cursor_index(
    rows: &[Vec<Cell>],
    options: &[String],
) -> Option<usize> {
    let matched = question_option_rows(rows, options)?;
    let mut cursors = matched
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.is_some_and(|row| row.cursor).then_some(index));
    let cursor = cursors.next()?;
    cursors.next().is_none().then_some(cursor)
}

fn step_cursor(steps: &mut Vec<Vec<u8>>, cursor: &mut usize, target: usize) {
    while *cursor < target {
        steps.push(b"\x1b[B".to_vec());
        *cursor += 1;
    }
    while *cursor > target {
        steps.push(b"\x1b[A".to_vec());
        *cursor -= 1;
    }
}

use body::{ChatBodySegment, parse_chat_segments};

impl Shell {
    pub(super) fn is_session_chat_view(&self, session: SessionId, cx: &gpui::App) -> bool {
        let pane = self
            .layout
            .focused()
            .filter(|pane| self.layout.session(*pane) == Some(session));
        pane.is_some_and(|pane| self.is_pane_chat_view(pane, cx))
    }

    pub(super) fn is_pane_chat_view(&self, pane: PaneId, cx: &gpui::App) -> bool {
        if self.layout.session(pane).is_none() {
            return false;
        }
        if let Some(chat) = self.layout.chat_view(pane) {
            return chat;
        }
        krill_app::settings::default_view_mode(cx) == krill_app::settings::DefaultViewMode::Chat
    }

    pub(super) fn is_active_chat_view_in(&self, cx: &gpui::App) -> bool {
        self.active_session_id()
            .is_some_and(|id| self.is_session_chat_view(id, cx))
    }

    /// 현재 포커스된 칸의 세션이 명시적 Chat 뷰 모드인지 반환한다(테스트/내부 호환용).
    #[cfg(test)]
    pub(super) fn is_active_chat_view(&self) -> bool {
        self.layout
            .focused()
            .and_then(|pane| self.layout.chat_view(pane))
            .unwrap_or(false)
    }

    /// Keeps a bot chat's item lines current: the Home Chat shows every change since it began,
    /// another chat the changes it made. The bot's items are read again every few seconds.
    pub(super) fn sync_bot_item_notes(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        const REFRESH: Duration = Duration::from_secs(5);
        if key.0 == self.launch_host
            && let Some(live_bot) = self
                .sidebar_bots
                .iter()
                .find(|b| b.sessions.iter().any(|s| s.session_id == key.1))
                .cloned()
        {
            self.chat.cached_bots.insert(key.clone(), live_bot);
        }
        let Some(bot) = self.chat_bot(key).cloned() else {
            return;
        };
        let bot_id = bot.id.clone();
        let bot_key: BotKey = (key.0.clone(), bot_id.clone());
        let home = bot.home_session().is_some_and(|h| h.session_id == key.1);
        let since = bot
            .sessions
            .iter()
            .find(|s| s.session_id == key.1)
            .map_or(0, |s| s.created_at_ms);
        let stale = self
            .chat
            .bot_notes
            .get(&bot_key)
            .is_none_or(|(at, _)| at.elapsed() >= REFRESH);
        if stale
            && !self.chat.bot_notes_tasks.contains_key(&bot_key)
            && let Some(rpc) = self.bot_rpc_on(&key.0)
        {
            let id = bot_id;
            let task_bot_key = bot_key.clone();
            let task = cx.spawn(async move |this, cx| {
                use krill_proto::bots::{BotRpcRequest, BotRpcResponse};
                let bot = id;
                let (items, soul, memory, offers) = cx
                    .background_executor()
                    .spawn(async move {
                        (
                            rpc(&BotRpcRequest::ListItems {
                                bot_id: Some(bot.clone()),
                                include_closed: true,
                            }),
                            rpc(&BotRpcRequest::GetSoul {
                                bot_id: bot.clone(),
                            }),
                            rpc(&BotRpcRequest::GetMemoryHistory {
                                bot_id: bot.clone(),
                                limit: Some(50),
                            }),
                            rpc(&BotRpcRequest::ListBotOffers { bot_id: bot }),
                        )
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.chat.bot_notes_tasks.remove(&task_bot_key);
                    if let Ok(BotRpcResponse::Text(soul)) = soul {
                        match krill_proto::bots::soul_description(&soul) {
                            Some(line) => this.chat.bot_souls.insert(task_bot_key.clone(), line),
                            None => this.chat.bot_souls.remove(&task_bot_key),
                        };
                    }
                    let previous = this.chat.bot_notes.remove(&task_bot_key).map(|(_, n)| n);
                    let notes = match (items, memory) {
                        (Ok(BotRpcResponse::Items(items)), memory) => {
                            let mut notes = bot_notes(&items);
                            if let Ok(BotRpcResponse::MemoryHistory(entries)) = memory {
                                notes.extend(memory_notes(&entries));
                            }
                            if let Ok(BotRpcResponse::BotOffers(offers)) = offers {
                                notes.extend(offer_notes(offers));
                            }
                            notes.sort_by_key(|n| n.note.at_ms);
                            notes
                        }
                        _ => previous.unwrap_or_default(),
                    };
                    this.chat
                        .bot_notes
                        .insert(task_bot_key, (std::time::Instant::now(), notes));
                    cx.notify();
                })
                .ok();
            });
            self.chat.bot_notes_tasks.insert(bot_key.clone(), task);
        }
        let notes: Vec<Arc<work_run::ItemNote>> = self
            .chat
            .bot_notes
            .get(&bot_key)
            .map(|(_, notes)| {
                notes
                    .iter()
                    .filter(|n| n.note.at_ms >= since && (home || n.session == Some(key.1)))
                    .map(|n| Arc::clone(&n.note))
                    .collect()
            })
            .unwrap_or_default();
        let turns: Vec<u64> = self
            .sidebar
            .read(cx)
            .session_prompt_history_on(&key.0, key.1)
            .map(|h| h.turns.iter().map(|t| t.unix_ns / 1_000_000).collect())
            .unwrap_or_default();
        if let Some(log) = self.chat.logs.get_mut(key) {
            log.set_item_notes(notes, turns);
        }
    }

    /// The bot whose chat this is (Orca's bot chat: persona, permission chip, its folder).
    pub(super) fn chat_bot(&self, key: &ChatKey) -> Option<&krill_app::bots::SidebarBot> {
        if key.0 == self.launch_host {
            return self
                .sidebar_bots
                .iter()
                .find(|b| b.sessions.iter().any(|s| s.session_id == key.1));
        }
        self.chat.cached_bots.get(key)
    }

    pub(super) fn chat_host_label(
        &self,
        host: &krill_app::hosts::HostId,
        cx: &gpui::App,
    ) -> SharedString {
        match host {
            krill_app::hosts::HostId::Local => krill_app::i18n::catalog::bots::HOST_LOCAL
                .mac_or(krill_app::i18n::catalog::other_os::HOST_LOCAL)
                .get()
                .into(),
            krill_app::hosts::HostId::Remote(id) => self.remote_host_name(id, cx).into(),
        }
    }

    pub(super) fn chat_key(&self, pane: PaneId) -> Option<ChatKey> {
        Some((self.layout.host(pane).clone(), self.layout.session(pane)?))
    }

    /// 이 호스트·세션을 보여 주는 칸. 포커스 칸을 먼저 본다.
    fn chat_pane(&self, key: &ChatKey) -> Option<PaneId> {
        let shows = |pane: &PaneId| self.chat_key(*pane).as_ref() == Some(key);
        self.layout
            .focused()
            .filter(shows)
            .or_else(|| self.layout.visible().into_iter().find(shows))
    }

    pub(super) fn active_session_id(&self) -> Option<SessionId> {
        let pane = self.layout.focused()?;
        if self.layout.file(pane).is_some() {
            return None;
        }
        self.layout.session(pane)
    }

    // Term and background tabs keep their lists. Only a closed pane or a
    // reassigned terminal invalidates its cached reader anchor/follow intent.
    fn prune_chat_lists(&mut self) {
        let layout = &self.layout;
        self.chat.lists.retain(|(pane, session), list| {
            layout.pane_exists(*pane)
                && layout.file(*pane).is_none()
                && layout.session(*pane) == Some(*session)
                && layout.host(*pane) == &list.host
        });
    }

    /// After a layout change, drops per-session chat state no pane shows any more.
    /// An in-flight prompt send is left to finish.
    pub(super) fn prune_chat_sessions(&mut self) {
        let layout = &self.layout;
        let shown: BTreeSet<ChatKey> = (0..layout.tabs().len())
            .flat_map(|index| layout.tab_panes(index))
            .filter_map(|pane| self.chat_key(pane))
            .collect();
        let chat = &mut self.chat;
        let keep = |key: &ChatKey| shown.contains(key);
        chat.poll_failed.retain(keep);
        chat.poll_tasks.retain(|key, _| keep(key));
        chat.poll_generations.retain(|key, _| keep(key));
        chat.cached_bots.retain(|key, _| keep(key));
        chat.logs.retain(|key, _| keep(key));
        chat.inputs.retain(|key, _| keep(key));
        chat.quick_menu.retain(keep);
        chat.model_picker.retain(keep);
        chat.model_picker_selected.retain(|key, _| keep(key));
        chat.composer_models_in_flight.retain(keep);
        chat.finds.retain(|key, _| keep(key));
        chat.composer_tasks.retain(|key, _| keep(key));
        chat.earlier_tasks.retain(|key, _| keep(key));
        chat.earlier_retry_after.retain(|key, _| keep(key));
        chat.last_composer_poll.retain(|key, _| keep(key));
        chat.request_tasks.retain(|key, _| keep(key));
        chat.prompt_failed.retain(keep);
        chat.multiline_blocked.retain(keep);
        chat.interrupt_latched.retain(keep);
        chat.interrupt_turn_since.retain(|key, _| keep(key));
        chat.stop_cooldown_since.retain(|key, _| keep(key));
        chat.stop_cooldown_timers.retain(|key, _| keep(key));
        chat.sent_history.retain(|key, _| keep(key));
        chat.history_cursor.retain(|key, _| keep(key));
        chat.history_saved_draft.retain(|key, _| keep(key));
        chat.image_tokens.retain(|key, _| keep(key));
        chat.question_other_inputs.retain(|(key, _), _| keep(key));
        chat.auto_continue.retain(|key, _| keep(key));
        chat.auto_continue_declined.retain(|key, _| keep(key));
        let sending = &chat.prompt_tasks;
        let turn_tasks = &chat.turn_tasks;
        let keep_flight = |key: &ChatKey| {
            keep(key) || sending.contains_key(key) || turn_tasks.keys().any(|(k, _)| k == key)
        };
        chat.prompt_review.retain(keep_flight);
        chat.draft_attachments.retain(|key, _| keep(key));
        chat.autocomplete.retain(|key, _| keep(key));
        chat.autocomplete_missed_at.retain(|key, _| keep(key));
        chat.attach_tasks.retain(|(key, _), _| keep(key));
        chat.pending_prompts.retain(|key, _| keep_flight(key));
        chat.pending_epochs.retain(|(key, _), _| keep_flight(key));
        if chat
            .copied_item
            .as_ref()
            .is_some_and(|(key, ..)| !keep(key))
        {
            chat.copied_item = None;
            chat.copied_item_timer = None;
        }
        if chat.copied_code.as_ref().is_some_and(|(key, _)| !keep(key)) {
            chat.copied_code = None;
            chat.copied_code_timer = None;
        }
        if chat
            .copied_pending
            .as_ref()
            .is_some_and(|(key, ..)| !keep(key))
        {
            chat.copied_pending = None;
            chat.copied_pending_timer = None;
        }
    }

    pub(super) fn session_agent_state(
        &self,
        host: &krill_app::hosts::HostId,
        session: SessionId,
        cx: &gpui::App,
    ) -> krill_proto::control::AgentState {
        self.sidebar
            .read(cx)
            .session_on(host, session)
            .map_or(krill_proto::control::AgentState::Unknown, |s| {
                s.status.state
            })
    }

    pub(super) fn mark_chat_item_copied(
        &mut self,
        key: ChatKey,
        abs_idx: usize,
        cx: &mut Context<Self>,
    ) {
        let stamped = std::time::Instant::now();
        self.chat.copied_item = Some((key.clone(), abs_idx, stamped));
        self.chat.copied_item_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            this.update(cx, |this, cx| {
                if this
                    .chat
                    .copied_item
                    .as_ref()
                    .is_some_and(|(k, idx, at)| *k == key && *idx == abs_idx && *at == stamped)
                {
                    this.chat.copied_item = None;
                    this.chat.copied_item_timer = None;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    pub(super) fn copy_chat_code_block(
        &mut self,
        key: ChatKey,
        abs_idx: usize,
        block_idx: usize,
        code: &str,
        cx: &mut Context<Self>,
    ) {
        let target = CopiedCodeTarget { abs_idx, block_idx };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(code.to_owned()));
        self.chat.copied_code = Some((key.clone(), target));
        self.chat.copied_code_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            this.update(cx, |this, cx| {
                if this
                    .chat
                    .copied_code
                    .as_ref()
                    .is_some_and(|(k, t)| *k == key && *t == target)
                {
                    this.chat.copied_code = None;
                    this.chat.copied_code_timer = None;
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn set_pane_chat_view(&mut self, pane: PaneId, chat: bool, cx: &mut Context<Self>) {
        if self.layout.session(pane).is_none() {
            return;
        }
        self.layout.set_chat_view(pane, chat);
        self.save();
        cx.notify();
    }

    pub(super) fn switch_pane_to_terminal(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_pane_chat_view(pane, false, cx);
        self.chat.focus_pending = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn switch_pane_to_terminal_opt(
        &mut self,
        pane: PaneId,
        key: &ChatKey,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if let Some(window) = window {
            self.switch_pane_to_terminal(pane, window, cx);
        } else {
            self.set_pane_chat_view(pane, false, cx);
            self.chat.focus_pending = Some(ChatFocus::Terminal(key.clone()));
            cx.notify();
        }
    }

    /// `[Chat | Term]` 모드를 토글한다(OWE-946 / MK-243).
    pub(super) fn toggle_chat_view(&mut self, cx: &mut Context<Self>) {
        self.diff
            .update(cx, krill_app::diff::Panel::clear_selection);
        let Some(session) = self.active_session_id() else {
            return;
        };
        let chat = !self.is_session_chat_view(session, cx);
        let Some(pane) = self.layout.focused() else {
            return;
        };
        self.set_pane_chat_view(pane, chat, cx);
        let key = (self.layout.host(pane).clone(), session);
        self.chat.focus_pending = Some(if chat {
            ChatFocus::Composer(key)
        } else {
            ChatFocus::Terminal(key)
        });
        if chat {
            self.chat.last_poll = None;
            self.poll_visible_chats(cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_chat_view_with_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_chat_view(cx);
        if let Some(key) = self
            .active_session_id()
            .filter(|id| self.is_session_chat_view(*id, cx))
            .and_then(|_| self.layout.focused())
            .and_then(|pane| self.chat_key(pane))
        {
            let input = self.ensure_chat_input(key, window, cx);
            input.update(cx, |input, cx| input.focus(window, cx));
        } else {
            window.focus(&self.focus, cx);
        }
    }

    fn visible_chat_panes(&self, cx: &gpui::App) -> Vec<(PaneId, SessionId)> {
        let mut seen: BTreeSet<ChatKey> = BTreeSet::new();
        let mut out = Vec::new();
        let focused = self.layout.focused();
        for pane in focused.into_iter().chain(
            self.layout
                .visible()
                .into_iter()
                .filter(|p| Some(*p) != focused),
        ) {
            if !self.layout.pane_exists(pane)
                || self.layout.file(pane).is_some()
                || !self.is_pane_chat_view(pane, cx)
            {
                continue;
            }
            let Some(session) = self.layout.session(pane) else {
                continue;
            };
            let key = (self.layout.host(pane).clone(), session);
            if seen.insert(key) {
                out.push((pane, session));
            }
        }
        out
    }

    fn has_visible_chat_view_in(&self, cx: &gpui::App) -> bool {
        !self.visible_chat_panes(cx).is_empty()
    }

    fn pane_chat_poll_interval(
        &self,
        pane: PaneId,
        session: SessionId,
        cx: &gpui::App,
    ) -> Duration {
        use krill_proto::control::AgentState;
        let key = (self.layout.host(pane).clone(), session);
        match self.session_status(&key, cx).map(|(state, _)| state) {
            Some(AgentState::Working) => Duration::from_millis(250),
            Some(
                AgentState::Done | AgentState::Waiting | AgentState::Blocked | AgentState::Exited,
            ) if !self.chat.poll_failed.contains(&key) => Duration::from_millis(1500),
            _ => Duration::from_millis(800),
        }
    }

    fn active_chat_poll_interval(&self, cx: &gpui::App) -> Duration {
        let visible = self.visible_chat_panes(cx);
        if let Some(min_interval) = visible
            .into_iter()
            .map(|(pane, session)| self.pane_chat_poll_interval(pane, session, cx))
            .min()
        {
            return min_interval;
        }
        let Some(pane) = self.layout.focused() else {
            return Duration::from_millis(800);
        };
        let Some(session) = self.layout.session(pane) else {
            return Duration::from_millis(800);
        };
        self.pane_chat_poll_interval(pane, session, cx)
    }

    pub(super) fn maybe_poll_active_chat(&mut self, cx: &mut Context<Self>) {
        self.prune_chat_lists();
        if !self.has_visible_chat_view_in(cx) {
            return;
        }
        let interval = self.active_chat_poll_interval(cx);
        let now = std::time::Instant::now();
        if self
            .chat
            .last_poll
            .is_some_and(|last| now.duration_since(last) < interval)
        {
            return;
        }
        self.poll_visible_chats(cx);
    }

    // Transcript flushes and hook binding can happen after the final PTY redraw.
    // Keep retrying while any Chat pane is visible, with at most one request in flight per session.
    fn ensure_chat_poll_timer(&mut self, cx: &mut Context<Self>) {
        if self.chat.poll_timer.is_some() {
            return;
        }
        let mut sleep_for = self.active_chat_poll_interval(cx);
        self.chat.poll_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(sleep_for).await;
                let next_interval = this
                    .update(cx, |this, cx| {
                        if this.has_blocking_overlay()
                            || this.dashboard.is_some()
                            || !this.has_visible_chat_view_in(cx)
                        {
                            this.chat.poll_timer = None;
                            return None;
                        }
                        this.poll_visible_chats(cx);
                        Some(this.active_chat_poll_interval(cx))
                    })
                    .ok()
                    .flatten();
                match next_interval {
                    Some(interval) => sleep_for = interval,
                    None => break,
                }
            }
        }));
    }

    /// 데몬에 `Action::ChatPoll`을 요청해 보이는 모든 Chat 패인의 증분 트랜스크립트를 가져온다.
    pub(super) fn poll_visible_chats(&mut self, cx: &mut Context<Self>) {
        let panes = self.visible_chat_panes(cx);
        for (pane, session) in panes {
            self.poll_session_chat(pane, session, cx);
        }
    }

    /// 데몬에 `Action::ChatPoll`을 요청해 활성 세션의 증분 트랜스크립트를 가져온다.
    pub(super) fn poll_active_chat(&mut self, cx: &mut Context<Self>) {
        self.poll_visible_chats(cx);
    }

    fn poll_session_chat(
        &mut self,
        pane: super::PaneId,
        session: SessionId,
        cx: &mut Context<Self>,
    ) {
        if !self.is_pane_chat_view(pane, cx) {
            return;
        }
        let host = self.layout.host(pane).clone();
        let key = (host.clone(), session);
        if self.chat.poll_tasks.contains_key(&key) {
            return;
        }
        self.ensure_chat_poll_timer(cx);
        let Some(target) = self.target_for_host(&host) else {
            if self.chat.poll_failed.insert(key) {
                cx.notify();
            }
            return;
        };
        let call = krill_app::agent_history::daemon_call_for(Some(target));
        #[cfg(test)]
        let call = self.chat.poll_call.clone().unwrap_or(call);
        #[cfg(test)]
        let gate = self.chat.next_poll_io.take();
        self.chat.last_poll = Some(std::time::Instant::now());
        self.chat.next_poll_generation = self.chat.next_poll_generation.wrapping_add(1);
        let generation = self.chat.next_poll_generation;
        self.chat.poll_generations.insert(key.clone(), generation);
        let offset = self.chat.logs.get(&key).map_or(0, |log| log.offset);
        let seed = self
            .chat
            .logs
            .get(&key)
            .is_none_or(|log| !log.seeded)
            .then(|| self.workflow_call_for_key(&key))
            .flatten();
        #[cfg(test)]
        let seed = seed.filter(|_| self.chat.workflow_call.is_some());
        let local = host.is_local();
        let settled = self
            .chat
            .logs
            .get(&key)
            .filter(|log| log.bound && log.meta.model.is_some())
            .map(|log| log.binding.clone());
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let seeding = seed.is_some();
            let (page, reply, meta) = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    if let Some(gate) = gate {
                        gate.await.ok();
                    }
                    let page = seed.and_then(|workflow| {
                        parse_chat_page(&workflow(&chat_tail_request(session, None)).ok()?)
                    });
                    let reply = page.is_none().then(|| {
                        call(&Request::AgentHistory(Action::ChatPoll { session, offset }))
                    });
                    let meta = match (&page, &reply) {
                        (Some(page), _) => poll_composer_meta(&page.snapshot, local, None),
                        (None, Some(Ok(Reply::AgentHistory(Response::ChatPolled(snap))))) => {
                            poll_composer_meta(snap, local, settled.as_ref())
                        }
                        _ => None,
                    };
                    (page, reply, meta)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.chat.poll_generations.get(&task_key).copied() != Some(generation) {
                    return;
                }
                this.chat.poll_tasks.remove(&task_key);
                this.sync_chat_interrupt_latches(cx);
                if this.layout.pane_exists(pane)
                    && this.layout.host(pane) == &host
                    && this.layout.session(pane) == Some(session)
                    && this.is_pane_chat_view(pane, cx)
                    && this.chat.logs.get(&task_key).map_or(0, |log| log.offset) == offset
                {
                    if seeding {
                        this.chat.logs.entry(task_key.clone()).or_default().seeded = true;
                    }
                    let agent_state = this.session_agent_state(&host, session, cx);
                    let now = std::time::Instant::now();
                    if let Some(page) = page {
                        this.chat.poll_failed.remove(&task_key);
                        this.apply_chat_tail(session, host, page, meta, agent_state, now);
                        this.refresh_chat_composer(&task_key, false, cx);
                        this.flush_queued_chat_prompt_if_ready(&task_key, cx);
                    } else if let Some(Ok(Reply::AgentHistory(Response::ChatPolled(snapshot)))) =
                        reply
                    {
                        let composer_interval = if this
                            .chat
                            .logs
                            .get(&task_key)
                            .is_none_or(|log| log.meta.model.is_none() || !log.meta.supported)
                        {
                            Duration::from_millis(1500)
                        } else {
                            Duration::from_secs(5)
                        };
                        let refresh_composer = snapshot.reset
                            || !snapshot.items.is_empty()
                            || this
                                .chat
                                .last_composer_poll
                                .get(&task_key)
                                .is_none_or(|t| t.elapsed() >= composer_interval);
                        this.chat.poll_failed.remove(&task_key);
                        this.apply_chat_poll_for_host_with_state(
                            session,
                            host,
                            snapshot,
                            meta,
                            agent_state,
                            now,
                        );
                        if refresh_composer {
                            this.refresh_chat_composer(&task_key, false, cx);
                        }
                        this.flush_queued_chat_prompt_if_ready(&task_key, cx);
                    } else {
                        this.chat.poll_failed.insert(task_key.clone());
                    }
                    this.load_earlier_chat(pane, &task_key, cx);
                    cx.notify();
                }
            })
            .ok();
        });
        self.chat.poll_tasks.insert(key, task);
    }

    /// Shows the newest rows at once. Older rows arrive above the viewport through
    /// `load_earlier_chat`, so nothing visible moves while they load.
    fn apply_chat_tail(
        &mut self,
        session: SessionId,
        host: krill_app::hosts::HostId,
        page: ChatPage,
        meta: Option<ComposerMeta>,
        agent_state: krill_proto::control::AgentState,
        now: std::time::Instant,
    ) {
        let key = (host.clone(), session);
        self.chat.logs.entry(key.clone()).or_default().binding = Default::default();
        self.apply_chat_poll_for_host_with_state(
            session,
            host,
            page.snapshot,
            meta,
            agent_state,
            now,
        );
        let log = self.chat.logs.entry(key.clone()).or_default();
        let old_base = log.base_index;
        log.base_index = CHAT_BASE_ORIGIN;
        log.seeded = true;
        log.earlier = page.has_earlier.then_some(page.start);
        log.cached_feed = None;
        log.sync_timing(agent_state, now);
        if old_base < CHAT_BASE_ORIGIN {
            let delta = CHAT_BASE_ORIGIN - old_base;
            let epoch = log.epoch;
            log.consumed_user_indices = std::mem::take(&mut log.consumed_user_indices)
                .into_iter()
                .map(|idx| idx.saturating_add(delta))
                .collect();
            if let Some(pending) = self.chat.pending_prompts.get_mut(&key) {
                for entry in pending.iter_mut() {
                    if self.chat.pending_epochs.get(&(key.clone(), entry.id)) == Some(&epoch)
                        && entry.origin_item < CHAT_BASE_ORIGIN
                    {
                        entry.origin_item = entry.origin_item.saturating_add(delta);
                    }
                }
            }
        }
    }

    fn load_earlier_chat(&mut self, pane: super::PaneId, key: &ChatKey, cx: &mut Context<Self>) {
        let Some(log) = self.chat.logs.get(key) else {
            return;
        };
        let Some(before) = log.earlier else {
            return;
        };
        if self.chat.earlier_retry_after.contains_key(key) {
            return;
        }
        let near_top = log.items.len() < CHAT_PRELOAD_ITEMS
            || self
                .chat
                .lists
                .get(&(pane, key.1))
                .is_some_and(|list| list.near_top(log.base_index, CHAT_PRELOAD_ITEMS / 2));
        if !near_top
            || log.items.len() >= MAX_CHAT_KEPT
            || self.chat.earlier_tasks.contains_key(key)
        {
            return;
        }
        let Some(call) = self.workflow_call_for_key(key) else {
            return;
        };
        let task_key = key.clone();
        let session = key.1;
        let task = cx.spawn(async move |this, cx| {
            let page = cx
                .background_executor()
                .spawn(async move {
                    parse_chat_page(&call(&chat_tail_request(session, Some(before))).ok()?)
                })
                .await;
            this.update(cx, |this, cx| {
                this.chat.earlier_tasks.remove(&task_key);
                let Some(log) = this.chat.logs.get_mut(&task_key) else {
                    return;
                };
                if log.earlier != Some(before) {
                    return;
                }
                let Some(page) = page else {
                    let retry_key = task_key.clone();
                    let cooldown = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_secs(2)).await;
                        this.update(cx, |this, cx| {
                            if this.chat.earlier_retry_after.remove(&retry_key).is_some()
                                && let Some(retry_pane) = this.chat_pane(&retry_key)
                            {
                                this.load_earlier_chat(retry_pane, &retry_key, cx);
                                cx.notify();
                            }
                        })
                        .ok();
                    });
                    this.chat
                        .earlier_retry_after
                        .insert(task_key.clone(), cooldown);
                    cx.notify();
                    return;
                };
                this.chat.earlier_retry_after.remove(&task_key);
                let snapshot = page.snapshot;
                if log.binding != (snapshot.session_id, snapshot.transcript_path) {
                    return;
                }
                log.prepend_earlier(
                    snapshot.items,
                    (page.has_earlier && page.start < before).then_some(page.start),
                );
                this.recompute_chat_find(&task_key);
                cx.notify();
            })
            .ok();
        });
        self.chat.earlier_tasks.insert(key.clone(), task);
    }

    pub(super) fn trigger_load_earlier_if_near_top(
        &mut self,
        pane: super::PaneId,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) {
        self.load_earlier_chat(pane, key, cx);
    }

    #[cfg(test)]
    pub(super) fn apply_chat_poll(&mut self, session: SessionId, snap: ChatPollSnapshot) {
        let host = self
            .layout
            .visible()
            .into_iter()
            .find(|&pane| self.layout.session(pane) == Some(session))
            .map(|pane| self.layout.host(pane).clone());
        let Some(host) = host else {
            return;
        };
        self.apply_chat_poll_for_host(session, host, snap, None);
    }

    #[cfg(test)]
    pub(super) fn apply_chat_poll_for_host(
        &mut self,
        session: SessionId,
        host: krill_app::hosts::HostId,
        snap: ChatPollSnapshot,
        observed: Option<ComposerMeta>,
    ) {
        self.apply_chat_poll_for_host_with_state(
            session,
            host,
            snap,
            observed,
            krill_proto::control::AgentState::Unknown,
            std::time::Instant::now(),
        );
    }

    pub(super) fn apply_chat_poll_for_host_with_state(
        &mut self,
        session: SessionId,
        host: krill_app::hosts::HostId,
        snap: ChatPollSnapshot,
        observed: Option<ComposerMeta>,
        agent_state: krill_proto::control::AgentState,
        now: std::time::Instant,
    ) {
        let key = (host.clone(), session);
        let host = Some(host);
        let log = self.chat.logs.entry(key.clone()).or_default();
        let binding = (snap.session_id.clone(), snap.transcript_path.clone());
        let same_host = log.host.is_none() || log.host == host;
        let initial_bind = !log.bound && snap.bound && !snap.reset && same_host;
        let stay_unbound = !log.bound && !snap.bound && !snap.reset && same_host;
        let reset = !initial_bind
            && !stay_unbound
            && (snap.reset || log.binding != binding || !same_host || !snap.bound);
        if !snap.agent.is_empty() {
            log.agent = snap.agent;
        }
        log.bound = snap.bound;
        if initial_bind || reset || !snap.items.is_empty() {
            log.revision = log.revision.wrapping_add(1);
        }
        if reset {
            log.reset_transcript();
            self.chat.request_tasks.remove(&key);
            if self
                .chat
                .copied_code
                .as_ref()
                .is_some_and(|(copied_key, _)| *copied_key == key)
            {
                self.chat.copied_code = None;
                self.chat.copied_code_timer = None;
            }
            if self
                .chat
                .copied_item
                .as_ref()
                .is_some_and(|(copied_key, ..)| *copied_key == key)
            {
                self.chat.copied_item = None;
                self.chat.copied_item_timer = None;
            }
        }
        log.binding = binding;
        let is_local = host
            .as_ref()
            .is_some_and(krill_app::hosts::HostId::is_local);
        log.host = host;
        if is_local
            && snap.bound
            && (initial_bind || reset || !snap.items.is_empty() || log.meta.model.is_none())
            && let Some(observed) = observed
        {
            if observed.model.is_some() {
                log.meta.model = observed.model;
            }
            if observed.effort.is_some() {
                log.meta.effort = observed.effort;
            }
            if observed.context_percent.is_some() {
                log.meta.context_percent = observed.context_percent;
            }
        }
        let is_scrolled_up = self.chat.lists.iter().any(|((_, sid), list)| {
            *sid == session && list.host == key.0 && !list.state.is_following_tail()
        });
        let has_list_following = self.chat.lists.iter().any(|((_, sid), list)| {
            *sid == session && list.host == key.0 && list.state.is_following_tail()
        });
        let log = self.chat.logs.entry(key.clone()).or_default();
        if !snap.items.is_empty() {
            let old_end_abs = log.base_index.saturating_add(log.items.len());
            if !initial_bind && !reset && is_scrolled_up {
                log.unread_from.get_or_insert(old_end_abs);
            } else if has_list_following {
                log.unread_from = None;
            }
            log.append_and_trim(snap.items, MAX_CHAT_KEPT);
        } else if has_list_following && log.unread_from.take().is_some() {
            log.cached_feed = None;
        }
        log.sync_timing(agent_state, now);
        log.offset = snap.next_offset;
        // A matching transcript result is evidence of a response; queue delivery is not.
        let resolved: Vec<_> = log
            .items
            .iter()
            .enumerate()
            .filter(|(index, item)| {
                matches!(item.kind, ChatItemKind::Question | ChatItemKind::Permission)
                    && !item.tool_id.is_empty()
                    && log.items.iter().skip(index + 1).any(|later| {
                        later.kind == ChatItemKind::ToolResult && later.tool_id == item.tool_id
                    })
            })
            .map(|(index, item)| format!("{}:{}", item.tool_id, log.base_index + index))
            .collect();
        for resolved_key in resolved {
            log.answered.insert(resolved_key.clone());
            log.submissions.insert(resolved_key, Submission::Confirmed);
        }
        self.reconcile_pending_prompts_on_poll(&key);
        self.recompute_chat_find(&key);
    }

    fn send_to_session(&mut self, key: &ChatKey, bytes: &[u8], cx: &mut Context<Self>) -> bool {
        self.chat_pane(key)
            .is_some_and(|pane| self.send_to_chat_pane(pane, key.1, bytes, cx))
    }

    fn workflow_call_for_key(
        &self,
        key: &ChatKey,
    ) -> Option<krill_app::agent_history::WorkflowCall> {
        #[cfg(test)]
        if let Some(call) = &self.chat.workflow_call {
            return Some(call.clone());
        }
        let target = self.target_for_host(&key.0)?;
        Some(krill_app::agent_history::workflow_call_for(Some(target)))
    }

    pub(super) fn refresh_chat_composer(
        &mut self,
        key: &ChatKey,
        include_models: bool,
        cx: &mut Context<Self>,
    ) {
        if self.chat.logs.get(key).is_some_and(|log| log.meta.pending)
            || (!include_models && self.chat.composer_models_in_flight.contains(key))
        {
            return;
        }
        let Some(call) = self.workflow_call_for_key(key) else {
            return;
        };
        let task_key = key.clone();
        let session = key.1.0;
        if include_models {
            self.chat.composer_models_in_flight.insert(key.clone());
        }
        self.chat
            .last_composer_poll
            .insert(key.clone(), std::time::Instant::now());
        let task = cx.spawn(async move |this, cx| {
            let (status_raw, models_raw) = cx
                .background_executor()
                .spawn(async move {
                    let status_req =
                        serde_json::json!({"action": "chat_composer", "session": session})
                            .to_string();
                    let status_res = call(&status_req).ok();
                    let models_res = if include_models {
                        let models_req =
                            serde_json::json!({"action": "composer_models", "session": session})
                                .to_string();
                        call(&models_req).ok()
                    } else {
                        None
                    };
                    (status_res, models_res)
                })
                .await;
            this.update(cx, |this, cx| {
                this.chat.composer_tasks.remove(&task_key);
                if include_models {
                    this.chat.composer_models_in_flight.remove(&task_key);
                }
                let log = this.chat.logs.entry(task_key.clone()).or_default();
                if let Some(raw) = status_raw
                    && let Some(agent) = apply_composer_status_json(&raw, &mut log.meta)
                    && log.agent.is_empty()
                {
                    log.agent = agent;
                }
                if let Some(raw) = models_raw {
                    apply_composer_models_json(&raw, &mut log.meta);
                }
                this.sync_auto_continue(&task_key, cx);
                cx.notify();
            })
            .ok();
        });
        self.chat.composer_tasks.insert(key.clone(), task);
    }

    pub(super) fn toggle_chat_model_picker(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        self.chat.quick_menu.remove(key);
        if let Some(ac) = self.chat.autocomplete.get_mut(key) {
            ac.dismiss_current();
        }
        if self.chat.model_picker.contains(key) {
            self.chat.model_picker.remove(key);
            self.chat.model_picker_selected.remove(key);
        } else {
            if let Some(log) = self.chat.logs.get_mut(key) {
                log.meta.reason = None;
            }
            self.chat.model_picker.insert(key.clone());
            self.chat.model_picker_selected.insert(key.clone(), 0);
            self.refresh_chat_composer(key, true, cx);
        }
        cx.notify();
    }

    pub(super) fn chat_agent_id<'a>(&'a self, key: &ChatKey) -> &'a str {
        if let Some(agent) = self
            .chat
            .logs
            .get(key)
            .map(|l| l.agent.as_str())
            .filter(|a| !a.is_empty())
        {
            return agent;
        }
        if let Some(bot) = self.chat_bot(key) {
            let canon = krill_proto::agent::canonical_id(&bot.agent_id);
            if !canon.is_empty() {
                return canon;
            }
        }
        "claude"
    }

    pub(super) fn step_chat_model_picker(&mut self, key: &ChatKey, delta: isize) -> bool {
        if !self.chat.model_picker.contains(key) {
            return false;
        }
        let agent = self.chat_agent_id(key);
        let meta = self.chat.logs.get(key).map(|l| &l.meta);
        let (models, _) = composer::resolved_composer_choices(agent, meta);
        let len = models.len();
        if len == 0 {
            return false;
        }
        let cur = self
            .chat
            .model_picker_selected
            .get(key)
            .copied()
            .unwrap_or(0)
            .min(len - 1);
        let next = if delta >= 0 {
            (cur + 1) % len
        } else if cur == 0 {
            len - 1
        } else {
            cur - 1
        };
        self.chat.model_picker_selected.insert(key.clone(), next);
        true
    }

    pub(super) fn accept_chat_model_picker(
        &mut self,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.chat.model_picker.contains(key) {
            return false;
        }
        let meta = self.chat.logs.get(key).map(|l| &l.meta);
        if !meta.is_some_and(|m| m.supported && !m.pending) {
            return false;
        }
        let agent = self.chat_agent_id(key);
        let (models, _) = composer::resolved_composer_choices(agent, meta);
        if models.is_empty() {
            return false;
        }
        let idx = self
            .chat
            .model_picker_selected
            .get(key)
            .copied()
            .unwrap_or(0)
            .min(models.len() - 1);
        let model_id = models[idx].id.clone();
        self.select_chat_model(key, &model_id, cx);
        self.chat.model_picker.remove(key);
        self.chat.model_picker_selected.remove(key);
        cx.notify();
        true
    }

    pub(super) fn select_chat_model(
        &mut self,
        key: &ChatKey,
        model_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.chat.quick_menu.remove(key);
        let Some(call) = self.workflow_call_for_key(key) else {
            return;
        };
        let log = self.chat.logs.entry(key.clone()).or_default();
        if log.meta.pending {
            return;
        }
        let effort = log
            .meta
            .models
            .iter()
            .find(|item| item.id == model_id)
            .and_then(|item| {
                log.meta
                    .effort
                    .as_ref()
                    .filter(|eff| item.efforts.contains(eff))
                    .cloned()
                    .or_else(|| item.default_effort.clone())
            });
        self.chat.composer_models_in_flight.remove(key);
        log.meta.pending = true;
        log.meta.reason = None;
        let task_key = key.clone();
        let session = key.1.0;
        let model = model_id.to_owned();
        let req_json = serde_json::json!({
            "action": "set_model",
            "session": session,
            "model": model,
            "effort": effort,
        })
        .to_string();
        let task = cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move { call(&req_json) })
                .await;
            this.update(cx, |this, cx| {
                this.chat.composer_tasks.remove(&task_key);
                let applied = {
                    let log = this.chat.logs.entry(task_key.clone()).or_default();
                    log.meta.pending = false;
                    match reply {
                        Ok(raw) => apply_composer_set_json(&raw, &mut log.meta),
                        Err(err) => {
                            log.meta.reason = Some(err.to_string());
                            false
                        }
                    }
                };
                if applied {
                    this.chat.model_picker.remove(&task_key);
                    this.refresh_chat_composer(&task_key, false, cx);
                }
                cx.notify();
            })
            .ok();
        });
        self.chat.composer_tasks.insert(key.clone(), task);
        cx.notify();
    }

    pub(super) fn select_chat_effort(
        &mut self,
        key: &ChatKey,
        effort: &str,
        cx: &mut Context<Self>,
    ) {
        self.chat.quick_menu.remove(key);
        let Some(call) = self.workflow_call_for_key(key) else {
            return;
        };
        let log = self.chat.logs.entry(key.clone()).or_default();
        if log.meta.pending {
            return;
        }
        self.chat.composer_models_in_flight.remove(key);
        log.meta.pending = true;
        log.meta.reason = None;
        let task_key = key.clone();
        let session = key.1.0;
        let req_json = serde_json::json!({
            "action": "set_effort",
            "session": session,
            "effort": effort,
        })
        .to_string();
        let task = cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move { call(&req_json) })
                .await;
            this.update(cx, |this, cx| {
                this.chat.composer_tasks.remove(&task_key);
                let applied = {
                    let log = this.chat.logs.entry(task_key.clone()).or_default();
                    log.meta.pending = false;
                    match reply {
                        Ok(raw) => apply_composer_set_json(&raw, &mut log.meta),
                        Err(err) => {
                            log.meta.reason = Some(err.to_string());
                            false
                        }
                    }
                };
                if applied {
                    this.chat.model_picker.remove(&task_key);
                    this.refresh_chat_composer(&task_key, false, cx);
                }
                cx.notify();
            })
            .ok();
        });
        self.chat.composer_tasks.insert(key.clone(), task);
        cx.notify();
    }

    fn send_to_chat_pane(
        &mut self,
        pane: super::PaneId,
        session: SessionId,
        bytes: &[u8],
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .chat
            .prompt_tasks
            .contains_key(&(self.layout.host(pane).clone(), session))
        {
            return false;
        }
        self.send_chat_prompt_input(pane, session, bytes, cx)
    }

    fn send_chat_prompt_input(
        &mut self,
        pane: super::PaneId,
        session: SessionId,
        bytes: &[u8],
        cx: &mut Context<Self>,
    ) -> bool {
        // Pane::input also returns None when disconnected: preflight the exact
        // pane before interpreting its queue result. This is not an agent ACK.
        if self.paste_queue.has_pending_for(pane)
            || self.layout.file(pane).is_some()
            || self.layout.session(pane) != Some(session)
            || self.live.get(&pane).is_none_or(|live| {
                live.target.as_ref().is_none_or(|target| {
                    target.host() != self.layout.host(pane) || !target.is_current()
                }) || live.pane.session() != Some(session)
                    || !live.pane.is_linked()
                    || live.pane.is_closed()
                    || live.pane.is_cold()
                    || live.pane.account_switching()
                    || live.suspended
            })
        {
            return false;
        }
        #[cfg(test)]
        if let Some(send) = self.chat.send_input.as_mut() {
            return send(bytes.to_vec());
        }
        let Some(live) = self.live.get_mut(&pane) else {
            return false;
        };
        live.clear_selection();
        live.echo.key_sent(std::time::Instant::now());
        let notice = live.pane.input(bytes, super::Now::get());
        self.show(notice.as_ref(), cx);
        cx.notify();
        notice.is_none()
    }

    pub(super) fn ensure_chat_input(
        &mut self,
        key: ChatKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<TextareaState> {
        if let Some((input, _)) = self.chat.inputs.get(&key) {
            let input = input.clone();
            let placeholder = ctext::PROMPT_PLACEHOLDER.get();
            if input.read(cx).presentation().placeholder().as_ref() != placeholder {
                input.update(cx, |state, cx| {
                    state.set_placeholder(placeholder, window, cx);
                });
            }
            return input;
        }
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder(ctext::PROMPT_PLACEHOLDER.get())
        });
        let focus_key = key.clone();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, _state, ev: &InputEvent, window, cx| match ev {
                InputEvent::Focus => {
                    if let Some(pane) = this.chat_pane(&focus_key)
                        && this.layout.focused() != Some(pane)
                    {
                        this.change(false, cx, |l| l.focus(pane));
                        this.files.refocus = false;
                    } else {
                        cx.notify();
                    }
                }
                InputEvent::Change => {
                    this.chat.prompt_review.remove(&focus_key);
                    this.chat.prompt_failed.remove(&focus_key);
                    this.chat.multiline_blocked.remove(&focus_key);
                    this.sync_chat_autocomplete(&focus_key, window, cx);
                    cx.notify();
                }
                InputEvent::Blur => {
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {}
            },
        );
        let active =
            self.layout.focused().and_then(|pane| self.chat_key(pane)) == Some(key.clone());
        self.chat.inputs.insert(key, (input.clone(), sub));
        if active {
            input.update(cx, |state, cx| state.focus(window, cx));
        }
        input
    }

    pub(super) fn chat_pane_has_focus(
        &self,
        pane: PaneId,
        window: &Window,
        cx: &gpui::App,
    ) -> bool {
        use gpui::Focusable as _;
        let Some(key) = self.chat_key(pane) else {
            return false;
        };
        self.chat
            .inputs
            .get(&key)
            .is_some_and(|(input, _)| input.read(cx).focus_handle(cx).is_focused(window))
    }

    pub(super) fn flush_chat_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(focused) = self.layout.focused() else {
            return;
        };
        let Some(pane_key) = self.chat_key(focused) else {
            return;
        };
        match &self.chat.focus_pending {
            Some(ChatFocus::Terminal(id)) if *id == pane_key => {
                self.chat.focus_pending = None;
                window.focus(&self.focus, cx);
            }
            Some(ChatFocus::Composer(id))
                if *id == pane_key && self.is_pane_chat_view(focused, cx) =>
            {
                let key = id.clone();
                self.chat.focus_pending = None;
                let input = self.ensure_chat_input(key, window, cx);
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            _ => {}
        }
    }

    pub(super) fn ensure_question_other_input(
        &mut self,
        key: &ChatKey,
        answer_key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<gpui_component::input::InputState> {
        let map_key = (key.clone(), answer_key.to_owned());
        if let Some(existing) = self.chat.question_other_inputs.get(&map_key) {
            return existing.clone();
        }
        let state = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(ctext::OTHER_OPTION_PLACEHOLDER.get())
        });
        self.chat
            .question_other_inputs
            .insert(map_key, state.clone());
        state
    }

    pub(super) fn mark_chat_read(&mut self, key: &ChatKey) {
        if let Some(log) = self.chat.logs.get_mut(key)
            && log.unread_from.take().is_some()
        {
            log.cached_feed = None;
            log.revision = log.revision.wrapping_add(1);
        }
    }

    pub(super) fn steer_chat_prompt(
        &mut self,
        key: &ChatKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.submit_chat_prompt_with_mode(key, true, window, cx);
    }

    pub(super) fn submit_chat_prompt(
        &mut self,
        key: &ChatKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.submit_chat_prompt_with_mode(key, false, window, cx);
    }

    fn submit_chat_prompt_with_mode(
        &mut self,
        key: &ChatKey,
        steer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent_state = self.session_agent_state(&key.0, key.1, cx);
        let working = agent_state == krill_proto::control::AgentState::Working;
        if working && !steer {
            let Some((input, _)) = self.chat.inputs.get(key) else {
                return;
            };
            let input = input.clone();
            if Self::chat_input_is_composing(&input, window, cx) {
                return;
            }
            let display_text = input.read(cx).value().to_string();
            let attachments = self
                .chat
                .draft_attachments
                .get(key)
                .cloned()
                .unwrap_or_default();
            let Some(submitted_payload) = build_chat_payload_with_tokens(
                &display_text,
                input.read(cx).tokens(),
                &attachments,
            ) else {
                return;
            };
            self.cancel_auto_continue(key, cx);
            self.chat.next_pending_id = self.chat.next_pending_id.wrapping_add(1);
            let queued_id = self.chat.next_pending_id;
            self.record_chat_prompt_history(key, &display_text);
            input.update(cx, |state, cx| state.set_value(String::new(), window, cx));
            self.chat.draft_attachments.remove(key);
            if let Some(ac) = self.chat.autocomplete.get_mut(key) {
                ac.close();
                ac.dismissed = None;
            }
            self.chat
                .logs
                .entry(key.clone())
                .or_default()
                .queued_prompts
                .push_back(QueuedChatPrompt {
                    id: queued_id,
                    display_text,
                    submitted_payload,
                    attachments,
                });
            self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
            cx.notify();
            return;
        }
        if self.chat.prompt_tasks.contains_key(key)
            || self.chat.prompt_review.contains(key)
            || (!steer && working)
            || self
                .chat_pane(key)
                .is_some_and(|pane| self.paste_queue.has_pending_for(pane))
        {
            return;
        }
        let Some((input, _)) = self.chat.inputs.get(key) else {
            return;
        };
        let input = input.clone();
        if Self::chat_input_is_composing(&input, window, cx) {
            return;
        }
        let display_text = input.read(cx).value().to_string();
        let attachments = self
            .chat
            .draft_attachments
            .get(key)
            .cloned()
            .unwrap_or_default();
        let Some(submitted_payload) =
            build_chat_payload_with_tokens(&display_text, input.read(cx).tokens(), &attachments)
        else {
            return;
        };
        let Some(pane) = self.chat_pane(key) else {
            return;
        };
        let session = key.1;
        let modes = self
            .live
            .get(&pane)
            .map_or_else(Default::default, |live| live.pane.modes());
        // Use the terminal consumer's paste plan, then submit separately.
        // Unknown unbracketed consumers retain the multiline draft for review.
        let krill_app::security::PastePlan::Immediate(bytes) =
            self.plan_terminal_paste(pane, submitted_payload.as_bytes(), modes, cx)
        else {
            self.chat.multiline_blocked.insert(key.clone());
            cx.notify();
            return;
        };
        self.chat.multiline_blocked.remove(key);
        let (origin_epoch, origin_item, opens_terminal, local_slash) =
            self.pending_prompt_meta(key, &submitted_payload, attachments.is_empty());
        self.cancel_auto_continue(key, cx);
        self.chat.next_pending_id = self.chat.next_pending_id.wrapping_add(1);
        let pending_id = self.chat.next_pending_id;
        self.record_chat_prompt_history(key, &display_text);
        input.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        self.chat.draft_attachments.remove(key);
        if let Some(ac) = self.chat.autocomplete.get_mut(key) {
            ac.close();
            ac.dismissed = None;
        }
        self.chat
            .pending_prompts
            .entry(key.clone())
            .or_default()
            .push(PendingChatPrompt {
                id: pending_id,
                origin_item,
                submitted_payload: submitted_payload.clone(),
                display_text,
                attachments,
                status: PendingStatus::Sending,
            });
        self.chat
            .pending_epochs
            .insert((key.clone(), pending_id), origin_epoch);
        self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
        if let Some(list) = self.chat.lists.get_mut(&(pane, session)) {
            list.state.set_follow_mode(gpui::FollowMode::Tail);
        }
        self.dispatch_pending_chat_prompt(
            PendingDispatch {
                key: key.clone(),
                pane,
                session,
                pending_id,
                submitted_payload,
                bytes,
                modes,
                opens_terminal,
                local_slash,
                steer,
            },
            Some(window),
            cx,
        );
    }

    fn toggle_chat_multi_option(
        &mut self,
        request: &RequestIdentity,
        opt_idx: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.chat_request_is_current(request, cx) || opt_idx >= request.item.options.len() {
            return;
        }
        let answer_key = request.key.clone();
        let Some(log) = self.chat.logs.get_mut(&request.chat_key()) else {
            return;
        };
        if log
            .submissions
            .get(&answer_key)
            .is_some_and(Submission::blocks_input)
        {
            return;
        }
        let set = log.multi_selected.entry(answer_key).or_default();
        if !set.remove(&opt_idx) {
            set.insert(opt_idx);
        }
        cx.notify();
    }

    fn pane_live_status_line(&self, pane: super::PaneId) -> Option<String> {
        let live = self.live.get(&pane)?;
        let screen = live.pane.screen();
        for row in screen.rows.iter().rev() {
            let line: String = row.iter().map(|c| c.ch).collect();
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.chars().take(120).collect());
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComposerSubmissionState {
    Ready,
    InFlight,
    ReviewRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ComposerPrimaryInput {
    pub(super) agent_state: krill_proto::control::AgentState,
    pub(super) submission: ComposerSubmissionState,
    pub(super) draft_empty: bool,
    pub(super) interrupt_latched: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComposerPrimaryAction {
    Send { enabled: bool },
    Sending,
    Stop { latched: bool },
}

pub(super) fn composer_primary_action(input: ComposerPrimaryInput) -> ComposerPrimaryAction {
    if input.submission == ComposerSubmissionState::InFlight {
        ComposerPrimaryAction::Sending
    } else if input.agent_state == krill_proto::control::AgentState::Working {
        // Option A: Krill PTY blocks mid-turn submit, so Working shows Stop even with a draft.
        ComposerPrimaryAction::Stop {
            latched: input.interrupt_latched,
        }
    } else {
        ComposerPrimaryAction::Send {
            enabled: !input.draft_empty && input.submission == ComposerSubmissionState::Ready,
        }
    }
}

impl Shell {
    fn session_status(
        &self,
        key: &ChatKey,
        cx: &gpui::App,
    ) -> Option<(krill_proto::control::AgentState, u64)> {
        self.sidebar
            .read(cx)
            .session_on(&key.0, key.1)
            .map(|s| (s.status.state, s.status.since_unix_ns))
    }

    fn is_turn_still_latched(
        turn_since: &std::collections::HashMap<ChatKey, u64>,
        key: &ChatKey,
        state: krill_proto::control::AgentState,
        since: u64,
    ) -> bool {
        state == krill_proto::control::AgentState::Working
            && turn_since
                .get(key)
                .is_none_or(|&latched_since| latched_since == since)
    }

    pub(super) fn remove_queued_chat_prompt(
        &mut self,
        key: &ChatKey,
        queued_id: u64,
        cx: &mut Context<Self>,
    ) {
        if let Some(log) = self.chat.logs.get_mut(key) {
            let before = log.queued_prompts.len();
            log.queued_prompts.retain(|q| q.id != queued_id);
            if log.queued_prompts.len() != before {
                self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
                cx.notify();
            }
        }
    }

    fn pending_prompt_meta(
        &self,
        key: &ChatKey,
        submitted_payload: &str,
        no_attachments: bool,
    ) -> (u64, usize, bool, bool) {
        self.chat.logs.get(key).map_or_else(
            || {
                (
                    0,
                    0,
                    no_attachments
                        && catalog::slash_command_opens_in_terminal("", submitted_payload, false),
                    no_attachments
                        && catalog::slash_command_is_local_non_echoed("", submitted_payload),
                )
            },
            |l| {
                let opens = no_attachments
                    && catalog::slash_command_opens_in_terminal(
                        &l.agent,
                        submitted_payload,
                        l.meta.supported,
                    );
                let local = no_attachments
                    && !opens
                    && catalog::slash_command_is_local_non_echoed(&l.agent, submitted_payload);
                (
                    l.epoch,
                    l.base_index.saturating_add(l.items.len()),
                    opens,
                    local,
                )
            },
        )
    }

    pub(super) fn flush_queued_chat_prompt_if_ready(
        &mut self,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) {
        if self.chat.prompt_tasks.contains_key(key) || self.chat.prompt_review.contains(key) {
            return;
        }
        let agent_state = self.session_agent_state(&key.0, key.1, cx);
        if matches!(
            agent_state,
            krill_proto::control::AgentState::Working
                | krill_proto::control::AgentState::Waiting
                | krill_proto::control::AgentState::Blocked
                | krill_proto::control::AgentState::Exited
        ) {
            return;
        }
        let Some(pane) = self.chat_pane(key) else {
            return;
        };
        if self.paste_queue.has_pending_for(pane) {
            return;
        }
        let Some(queued) = self
            .chat
            .logs
            .get_mut(key)
            .and_then(|log| log.queued_prompts.pop_front())
        else {
            return;
        };
        let session = key.1;
        let modes = self
            .live
            .get(&pane)
            .map_or_else(Default::default, |live| live.pane.modes());
        let krill_app::security::PastePlan::Immediate(bytes) =
            self.plan_terminal_paste(pane, queued.submitted_payload.as_bytes(), modes, cx)
        else {
            if let Some(log) = self.chat.logs.get_mut(key) {
                log.queued_prompts.push_front(queued);
            }
            self.chat.multiline_blocked.insert(key.clone());
            cx.notify();
            return;
        };
        self.chat.multiline_blocked.remove(key);
        let (origin_epoch, origin_item, opens_terminal, local_slash) = self.pending_prompt_meta(
            key,
            &queued.submitted_payload,
            queued.attachments.is_empty(),
        );
        let pending_id = queued.id;
        let submitted_payload = queued.submitted_payload;
        self.chat
            .pending_prompts
            .entry(key.clone())
            .or_default()
            .push(PendingChatPrompt {
                id: pending_id,
                origin_item,
                submitted_payload: submitted_payload.clone(),
                display_text: queued.display_text,
                attachments: queued.attachments,
                status: PendingStatus::Sending,
            });
        self.chat
            .pending_epochs
            .insert((key.clone(), pending_id), origin_epoch);
        self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
        if let Some(list) = self.chat.lists.get_mut(&(pane, session)) {
            list.state.set_follow_mode(gpui::FollowMode::Tail);
        }
        self.dispatch_pending_chat_prompt(
            PendingDispatch {
                key: key.clone(),
                pane,
                session,
                pending_id,
                submitted_payload,
                bytes,
                modes,
                opens_terminal,
                local_slash,
                steer: false,
            },
            None,
            cx,
        );
    }

    pub(super) fn is_chat_stop_cooldown(&self, key: &ChatKey, cx: &gpui::App) -> bool {
        let Some(&cooldown_since) = self.chat.stop_cooldown_since.get(key) else {
            return false;
        };
        self.session_status(key, cx).is_some_and(|(state, since)| {
            state == krill_proto::control::AgentState::Working && since == cooldown_since
        })
    }

    pub(super) fn is_chat_interrupt_latched(&self, key: &ChatKey, cx: &gpui::App) -> bool {
        self.chat.interrupt_latched.contains(key)
            && self.session_status(key, cx).is_some_and(|(state, since)| {
                Self::is_turn_still_latched(&self.chat.interrupt_turn_since, key, state, since)
            })
    }

    pub(super) fn sync_chat_interrupt_latches(&mut self, cx: &gpui::App) {
        if self.chat.interrupt_latched.is_empty()
            && self.chat.interrupt_turn_since.is_empty()
            && self.chat.stop_cooldown_since.is_empty()
        {
            return;
        }
        let sidebar = self.sidebar.read(cx);
        let turn_since = &self.chat.interrupt_turn_since;
        self.chat.interrupt_latched.retain(|key| {
            sidebar.session_on(&key.0, key.1).is_some_and(|s| {
                Self::is_turn_still_latched(turn_since, key, s.status.state, s.status.since_unix_ns)
            })
        });
        let latched = &self.chat.interrupt_latched;
        self.chat
            .interrupt_turn_since
            .retain(|key, _| latched.contains(key));
        self.chat.stop_cooldown_since.retain(|key, &mut cd_since| {
            sidebar.session_on(&key.0, key.1).is_some_and(|s| {
                s.status.state == krill_proto::control::AgentState::Working
                    && s.status.since_unix_ns == cd_since
            })
        });
    }

    pub(super) fn interrupt_chat_turn(&mut self, key: &ChatKey, cx: &mut Context<Self>) -> bool {
        self.sync_chat_interrupt_latches(cx);
        let Some((krill_proto::control::AgentState::Working, since_unix_ns)) =
            self.session_status(key, cx)
        else {
            return false;
        };
        if self.chat.interrupt_latched.contains(key)
            || self.is_chat_stop_cooldown(key, cx)
            || self.chat.prompt_tasks.contains_key(key)
        {
            return false;
        }
        let Some(pane) = self.chat_pane(key) else {
            return false;
        };
        if self.paste_queue.has_pending_for(pane) {
            return false;
        }
        if !self.send_to_chat_pane(pane, key.1, b"\x03", cx) {
            return false;
        }
        self.chat.interrupt_latched.insert(key.clone());
        self.chat
            .interrupt_turn_since
            .insert(key.clone(), since_unix_ns);
        self.chat
            .stop_cooldown_since
            .insert(key.clone(), since_unix_ns);
        let cd_key = key.clone();
        let cd_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            this.update(cx, |this, cx| {
                this.chat.stop_cooldown_since.remove(&cd_key);
                this.chat.stop_cooldown_timers.remove(&cd_key);
                cx.notify();
            })
            .ok();
        });
        self.chat.stop_cooldown_timers.insert(key.clone(), cd_task);
        cx.notify();
        true
    }
}

enum ChatClipboardContent {
    Attachments(Vec<DraftAttachment>),
    Text(String),
    None,
}

fn classify_chat_clipboard_item(item: &gpui::ClipboardItem) -> ChatClipboardContent {
    const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
    let has_valid_image = item.entries().iter().any(|entry| {
        matches!(
            entry,
            gpui::ClipboardEntry::Image(img)
                if !img.bytes.is_empty() && img.bytes.len() <= MAX_IMAGE_BYTES
        )
    });
    let mut skipped_dead_external_paths = false;
    for entry in item.entries() {
        if let gpui::ClipboardEntry::ExternalPaths(paths) = entry
            && !paths.paths().is_empty()
        {
            if has_valid_image {
                let existing: Vec<DraftAttachment> = paths
                    .paths()
                    .iter()
                    .filter(|p| p.exists())
                    .cloned()
                    .filter_map(DraftAttachment::from_path)
                    .collect();
                if !existing.is_empty() {
                    return ChatClipboardContent::Attachments(existing);
                }
                skipped_dead_external_paths = true;
            } else {
                let staged: Vec<DraftAttachment> = paths
                    .paths()
                    .iter()
                    .cloned()
                    .filter_map(DraftAttachment::from_path)
                    .collect();
                if !staged.is_empty() {
                    return ChatClipboardContent::Attachments(staged);
                }
            }
        }
    }
    if !skipped_dead_external_paths {
        let has_usable_string = item.entries().iter().any(|entry| match entry {
            gpui::ClipboardEntry::String(s) => {
                if has_valid_image {
                    !s.text().trim().is_empty()
                } else {
                    !s.text().is_empty()
                }
            }
            _ => false,
        });
        if has_usable_string
            && let Some(text) = item.text()
            && !text.is_empty()
        {
            const LONG_PASTE_BYTES: usize = 32 * 1024;
            if text.len() >= LONG_PASTE_BYTES
                && let Ok(path) = super::security::save_clipboard_text(&text)
                && let Some(att) = DraftAttachment::from_path(path)
            {
                return ChatClipboardContent::Attachments(vec![att]);
            }
            return ChatClipboardContent::Text(text);
        }
    }
    for entry in item.entries() {
        if let gpui::ClipboardEntry::Image(img) = entry
            && !img.bytes.is_empty()
            && img.bytes.len() <= MAX_IMAGE_BYTES
            && let Ok(path) = super::security::save_clipboard_image(img)
            && let Some(att) = DraftAttachment::from_path(path)
        {
            return ChatClipboardContent::Attachments(vec![att]);
        }
    }
    ChatClipboardContent::None
}

/// The id prefix of an image chip in the composer text (its token id is this and the image path).
pub(super) const IMAGE_TOKEN_PREFIX: &str = "chat-image:";

/// Orca's composer image chip (`NativeChatPromptPill`, `promptPill` badge): the image icon and
/// "이미지 #n", or "이미지가 제거됨" with a crossed-out image once it is gone.
pub(super) fn image_token_pill(
    number: Option<usize>,
    selected: bool,
    cx: &gpui::App,
) -> gpui::AnyElement {
    use gpui::prelude::FluentBuilder as _;
    use gpui::{InteractiveElement as _, IntoElement as _, ParentElement as _, Styled as _};
    let palette = krill_app::ui::palette(cx);
    let selector = number.map_or_else(
        || "chat-attachment-image-pill-removed".to_owned(),
        |n| format!("chat-attachment-image-pill-{n}"),
    );
    gpui::div()
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_1()
        .rounded_full()
        .border_1()
        .border_color(palette.border)
        .when(number.is_none(), |d| d.border_dashed().opacity(0.7))
        .when(selected, |d| d.border_color(palette.ring))
        .bg(palette.secondary)
        .px_1p5()
        .text_size(krill_app::ui::TEXT_XS)
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(palette.muted_foreground)
        .child(
            gpui_component::Icon::default()
                .data(if number.is_some() {
                    krill_app::ui::icons::IMAGE_SVG
                } else {
                    krill_app::ui::icons::IMAGE_OFF_SVG
                })
                .size(gpui::px(12.)),
        )
        .child(number.map_or_else(
            || ctext::IMAGE_REMOVED.get().to_owned(),
            |n| ctext::IMAGE_NUMBER.format(&[("number", &n.to_string())]),
        ))
        .into_any_element()
}

/// Orca places a pasted image where its chip sits: each image chip becomes that image's quoted
/// path in place, a chip whose image was removed drops out, and images without a chip (or other
/// files) lead the prompt as before.
pub(super) fn build_chat_payload_with_tokens(
    display_text: &str,
    tokens: &[gpui_component::input::InlineTokenSpan],
    attachments: &[DraftAttachment],
) -> Option<String> {
    use krill_proto::os::ffi::OsStrExt as _;
    let mut text = String::with_capacity(display_text.len());
    let mut used: Vec<&std::path::Path> = Vec::new();
    let mut at = 0;
    for span in tokens {
        let Some(path) = span.token().id().strip_prefix(IMAGE_TOKEN_PREFIX) else {
            continue;
        };
        let range = span.range();
        let (Some(before), true) = (display_text.get(at..range.start), range.end >= at) else {
            continue;
        };
        text.push_str(before);
        if let Some(att) = attachments
            .iter()
            .find(|a| a.is_image && a.path.as_os_str() == std::ffi::OsStr::new(path))
        {
            let quoted = krill_term::paste::shell_quote(att.path.as_os_str().as_bytes());
            text.push_str(&String::from_utf8_lossy(&quoted));
            used.push(&att.path);
        }
        at = range.end;
    }
    text.push_str(display_text.get(at..).unwrap_or_default());
    let rest: Vec<DraftAttachment> = attachments
        .iter()
        .filter(|a| !used.contains(&a.path.as_path()))
        .cloned()
        .collect();
    build_submitted_chat_payload(&text, &rest)
}

pub(super) fn build_submitted_chat_payload(
    display_text: &str,
    attachments: &[DraftAttachment],
) -> Option<String> {
    use krill_proto::os::ffi::OsStrExt as _;
    if attachments.is_empty() {
        return (!display_text.trim().is_empty()).then(|| display_text.to_owned());
    }
    let escaped: Vec<String> = attachments
        .iter()
        .map(|att| {
            let quoted = krill_term::paste::shell_quote(att.path.as_os_str().as_bytes());
            String::from_utf8_lossy(&quoted).into_owned()
        })
        .collect();
    let joined = escaped.join(" ");
    if display_text.trim().is_empty() {
        Some(joined)
    } else {
        Some(format!("{joined} {display_text}"))
    }
}

#[derive(Clone, Copy)]
pub(super) struct PendingReview<'a> {
    pub key: &'a ChatKey,
    pub pending_id: u64,
    pub immediate_send_failure: bool,
}

fn format_chat_quote(text: &str) -> Option<String> {
    let stripped = krill_app::pull_request::checks::strip_ansi_codes(text);
    let cleaned = krill_term::select::clean_text(&stripped);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut quoted = String::new();
    for line in trimmed.lines() {
        if line.is_empty() {
            quoted.push_str(">\n");
        } else {
            quoted.push_str("> ");
            quoted.push_str(line);
            quoted.push('\n');
        }
    }
    quoted.push('\n');
    Some(quoted)
}

struct PendingDispatch {
    key: ChatKey,
    pane: super::PaneId,
    session: SessionId,
    pending_id: u64,
    submitted_payload: String,
    bytes: Vec<u8>,
    modes: krill_proto::term_modes::Modes,
    opens_terminal: bool,
    local_slash: bool,
    steer: bool,
}

struct UnbracketedWatch {
    key: ChatKey,
    pane: super::PaneId,
    session: SessionId,
    pending_id: u64,
    submitted_payload: String,
    steer: bool,
    is_agy: bool,
    baseline_queued: usize,
}

impl Shell {
    pub(super) fn record_chat_prompt_history(&mut self, key: &ChatKey, text: &str) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            let history = self.chat.sent_history.entry(key.clone()).or_default();
            if history.last().is_none_or(|prev| prev != trimmed) {
                history.push(text.to_owned());
                if history.len() > 50 {
                    let overflow = history.len() - 50;
                    history.drain(..overflow);
                }
            }
        }
        self.chat.history_cursor.remove(key);
        self.chat.history_saved_draft.remove(key);
    }

    pub(super) fn step_chat_prompt_history(
        &mut self,
        key: &ChatKey,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if delta == 0
            || self
                .chat
                .autocomplete
                .get(key)
                .is_some_and(catalog::AutocompleteState::is_open)
            || self.chat.quick_menu.contains(key)
            || self.chat.model_picker.contains(key)
        {
            return false;
        }
        let Some(history) = self
            .chat
            .sent_history
            .get(key)
            .filter(|h| !h.is_empty())
            .cloned()
        else {
            return false;
        };
        let Some((input, _)) = self.chat.inputs.get(key) else {
            return false;
        };
        let input = input.clone();
        if Self::chat_input_is_composing(&input, window, cx) {
            return false;
        }
        let (value, cursor) = {
            let state = input.read(cx);
            (state.value().to_string(), state.cursor())
        };
        let clamped = value
            .char_indices()
            .map(|(i, _)| i)
            .find(|&i| i >= cursor)
            .unwrap_or(value.len());
        let on_first_line = value
            .get(..clamped)
            .is_none_or(|before| !before.contains('\n'));
        let on_last_line = value
            .get(clamped..)
            .is_none_or(|after| !after.contains('\n'));

        let next_text = if delta < 0 {
            let current = self.chat.history_cursor.get(key).copied();
            if current.is_none() && !value.is_empty() && !on_first_line {
                return false;
            }
            if current.is_none() {
                self.chat.history_saved_draft.insert(key.clone(), value);
            }
            let next_idx = current.unwrap_or(history.len()).saturating_sub(1);
            self.chat.history_cursor.insert(key.clone(), next_idx);
            history.get(next_idx).cloned().unwrap_or_default()
        } else {
            let Some(current) = self.chat.history_cursor.get(key).copied() else {
                return false;
            };
            if !on_last_line {
                return false;
            }
            let next_idx = current.saturating_add(1);
            if next_idx >= history.len() {
                self.chat.history_cursor.remove(key);
                self.chat
                    .history_saved_draft
                    .remove(key)
                    .unwrap_or_default()
            } else {
                self.chat.history_cursor.insert(key.clone(), next_idx);
                history.get(next_idx).cloned().unwrap_or_default()
            }
        };
        input.update(cx, |state, cx| {
            state.set_value(next_text, window, cx);
        });
        true
    }

    pub(super) fn quote_text_into_chat_composer(
        &mut self,
        key: &ChatKey,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(quoted) = format_chat_quote(text) else {
            return;
        };
        let input = self.ensure_chat_input(key.clone(), window, cx);
        input.update(cx, |state, cx| {
            let current = state.value().to_string();
            let mut next = String::new();
            if !current.is_empty() {
                next.push_str(&current);
                if !current.ends_with("\n\n") {
                    if current.ends_with('\n') {
                        next.push('\n');
                    } else {
                        next.push_str("\n\n");
                    }
                }
            }
            next.push_str(&quoted);
            state.set_value(next, window, cx);
            state.focus(window, cx);
        });
        self.chat.history_cursor.remove(key);
        self.chat.history_saved_draft.remove(key);
        cx.notify();
    }

    pub(super) fn quote_chat_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page_open() || self.dashboard.is_some() || !self.is_active_chat_view_in(cx) {
            return;
        }
        let Some(pane) = self.layout.focused() else {
            return;
        };
        let Some(session) = self.layout.session(pane) else {
            return;
        };
        let key = (self.layout.host(pane).clone(), session);
        let selected = gpui_component::TextSelection::selected_text(window, cx);
        let fallback = if selected.trim().is_empty() {
            self.chat.logs.get(&key).and_then(|log| {
                log.items
                    .iter()
                    .rev()
                    .find(|item| {
                        matches!(item.kind, ChatItemKind::User | ChatItemKind::Assistant)
                            && !item.text.trim().is_empty()
                    })
                    .map(|item| item.text.clone())
            })
        } else {
            Some(selected)
        };
        if let Some(text) = fallback {
            self.quote_text_into_chat_composer(&key, &text, window, cx);
        }
    }

    pub(super) fn reuse_chat_prompt(
        &mut self,
        key: &ChatKey,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.ensure_chat_input(key.clone(), window, cx);
        let text = text.to_owned();
        input.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.focus(window, cx);
        });
        self.chat.history_cursor.remove(key);
        self.chat.history_saved_draft.remove(key);
        cx.notify();
    }

    pub(super) fn mark_chat_pending_copied(
        &mut self,
        key: ChatKey,
        pending_id: u64,
        cx: &mut Context<Self>,
    ) {
        let stamped = std::time::Instant::now();
        self.chat.copied_pending = Some((key.clone(), pending_id, stamped));
        self.chat.copied_pending_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            this.update(cx, |this, cx| {
                if this
                    .chat
                    .copied_pending
                    .as_ref()
                    .is_some_and(|(k, id, at)| *k == key && *id == pending_id && *at == stamped)
                {
                    this.chat.copied_pending = None;
                    this.chat.copied_pending_timer = None;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    pub(super) fn dismiss_open_composer_popover(&mut self, key: &ChatKey) -> bool {
        if self
            .chat
            .autocomplete
            .get(key)
            .is_some_and(catalog::AutocompleteState::is_open)
        {
            if let Some(ac) = self.chat.autocomplete.get_mut(key) {
                ac.dismiss_current();
            }
            return true;
        }
        if self.chat.quick_menu.contains(key) || self.chat.model_picker.contains(key) {
            self.chat.quick_menu.remove(key);
            self.chat.model_picker.remove(key);
            self.chat.model_picker_selected.remove(key);
            return true;
        }
        false
    }

    pub(super) fn chat_input_is_composing(
        input: &gpui::Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use gpui::Focusable as _;
        input.update(cx, |state, cx| {
            state.focus_handle(cx).is_focused(window)
                && gpui::EntityInputHandler::marked_text_range(state, window, cx).is_some()
        })
    }

    pub(super) fn chat_is_agy(&self, key: &ChatKey, cx: &gpui::App) -> bool {
        if let Some(agent) = self
            .chat
            .logs
            .get(key)
            .map(|l| l.agent.as_str())
            .filter(|a| !a.is_empty())
        {
            return krill_proto::agent::canonical_id(agent) == "agy";
        }
        if let Some(bot) = self.chat_bot(key) {
            return krill_proto::agent::canonical_id(&bot.agent_id) == "agy";
        }
        self.sidebar
            .read(cx)
            .session_on(&key.0, key.1)
            .is_some_and(|s| krill_proto::agent::canonical_id(&s.status.agent) == "agy")
    }

    fn dispatch_pending_chat_prompt(
        &mut self,
        mut dispatch: PendingDispatch,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let is_agy = self.chat_is_agy(&dispatch.key, cx);
        if dispatch
            .modes
            .contains(krill_proto::term_modes::Modes::BRACKETED_PASTE)
            || dispatch.submitted_payload.contains(['\n', '\r'])
        {
            let kind = if dispatch.opens_terminal {
                prompt::PromptKind::OpensTerminal
            } else if dispatch.local_slash {
                prompt::PromptKind::LocalSlash
            } else {
                prompt::PromptKind::Regular
            };
            self.start_chat_prompt(
                prompt::ChatPrompt {
                    pane: dispatch.pane,
                    session: dispatch.session,
                    pending_id: dispatch.pending_id,
                    submitted_payload: dispatch.submitted_payload,
                    bytes: dispatch.bytes,
                    kind,
                    steer: dispatch.steer,
                    is_agy,
                },
                window,
                cx,
            );
            return;
        }
        let baseline_queued = if is_agy {
            self.live.get(&dispatch.pane).map_or(0, |live| {
                krill_proto::screen::agy::count_queued_matches(
                    live.pane.screen(),
                    &dispatch.submitted_payload,
                )
                .count
            })
        } else {
            0
        };
        let initial_status = self.session_status(&dispatch.key, cx);
        dispatch.bytes.push(b'\r');
        if self.send_to_session(&dispatch.key, &dispatch.bytes, cx) {
            self.chat.prompt_failed.remove(&dispatch.key);
            if dispatch.opens_terminal || dispatch.local_slash {
                let origin = prompt::Origin {
                    pane: dispatch.pane,
                    session: dispatch.session,
                    host: dispatch.key.0.clone(),
                    target: self
                        .live
                        .get(&dispatch.pane)
                        .and_then(|live| live.target.clone()),
                    epoch: self.chat.logs.get(&dispatch.key).map_or(0, |log| log.epoch),
                    initial_status,
                };
                let Some(receipt) = self
                    .live
                    .get(&dispatch.pane)
                    .map(|live| live.pane.input_receipt())
                else {
                    self.transition_pending_to_review_required(
                        PendingReview {
                            key: &dispatch.key,
                            pending_id: dispatch.pending_id,
                            immediate_send_failure: true,
                        },
                        window,
                        cx,
                    );
                    cx.notify();
                    return;
                };
                match self.chat_prompt_delivery(&origin, receipt, cx) {
                    krill_proto::input::Delivery::Applied => {
                        self.confirm_pending_prompt(&dispatch.key, dispatch.pending_id);
                        if dispatch.opens_terminal {
                            self.switch_pane_to_terminal_opt(
                                dispatch.pane,
                                &dispatch.key,
                                window,
                                cx,
                            );
                        }
                    }
                    krill_proto::input::Delivery::Uncertain => {
                        self.transition_pending_to_review_required(
                            PendingReview {
                                key: &dispatch.key,
                                pending_id: dispatch.pending_id,
                                immediate_send_failure: false,
                            },
                            window,
                            cx,
                        );
                    }
                    krill_proto::input::Delivery::Pending => {
                        self.spawn_unbracketed_receipt_watcher(
                            origin,
                            dispatch.pending_id,
                            receipt,
                            dispatch.opens_terminal,
                            window.as_deref(),
                            cx,
                        );
                    }
                }
            } else {
                self.spawn_unbracketed_turn_watcher(
                    UnbracketedWatch {
                        key: dispatch.key,
                        pane: dispatch.pane,
                        session: dispatch.session,
                        pending_id: dispatch.pending_id,
                        submitted_payload: dispatch.submitted_payload,
                        steer: dispatch.steer,
                        is_agy,
                        baseline_queued,
                    },
                    initial_status,
                    window.as_deref(),
                    cx,
                );
            }
        } else {
            self.transition_pending_to_review_required(
                PendingReview {
                    key: &dispatch.key,
                    pending_id: dispatch.pending_id,
                    immediate_send_failure: true,
                },
                window,
                cx,
            );
        }
        cx.notify();
    }

    fn spawn_unbracketed_receipt_watcher(
        &mut self,
        origin: prompt::Origin,
        pending_id: u64,
        receipt: krill_proto::input::Receipt,
        switch_to_terminal: bool,
        window: Option<&Window>,
        cx: &mut Context<Self>,
    ) {
        let task_key = origin.key();
        let pane = origin.pane;
        let map_key = (task_key.clone(), pending_id);
        let win = window.map(Window::window_handle).or(self.window);
        let task = cx.spawn(async move |this, cx| {
            enum Step {
                Wait,
                Applied(bool),
                Failed,
            }
            for _ in 0..prompt::RECEIPT_POLLS {
                cx.background_executor().timer(prompt::POLL).await;
                let Ok(step) = this.update(cx, |this, cx| {
                    if !this.has_pending_prompt(&task_key, pending_id) {
                        return Step::Applied(false);
                    }
                    match this.chat_prompt_delivery(&origin, receipt, cx) {
                        krill_proto::input::Delivery::Applied => {
                            Step::Applied(this.chat_prompt_current(&origin, cx))
                        }
                        krill_proto::input::Delivery::Uncertain => Step::Failed,
                        krill_proto::input::Delivery::Pending => Step::Wait,
                    }
                }) else {
                    return;
                };
                match step {
                    Step::Wait => {}
                    Step::Applied(current) => {
                        prompt::update_with_optional_window(&this, win, cx, |this, window, cx| {
                            this.confirm_pending_prompt(&task_key, pending_id);
                            if current && switch_to_terminal {
                                this.switch_pane_to_terminal_opt(pane, &task_key, window, cx);
                            } else {
                                cx.notify();
                            }
                        });
                        return;
                    }
                    Step::Failed => break,
                }
            }
            prompt::update_with_optional_window(&this, win, cx, |this, window, cx| {
                this.transition_pending_to_review_required(
                    PendingReview {
                        key: &task_key,
                        pending_id,
                        immediate_send_failure: false,
                    },
                    window,
                    cx,
                );
            });
        });
        self.chat.turn_tasks.insert(map_key, task);
    }

    fn spawn_unbracketed_turn_watcher(
        &mut self,
        watch: UnbracketedWatch,
        initial_status: Option<(krill_proto::control::AgentState, u64)>,
        window: Option<&Window>,
        cx: &mut Context<Self>,
    ) {
        const POLL: Duration = Duration::from_millis(20);
        const TURN_POLLS: usize = 750;
        let task_key = watch.key;
        let pane = watch.pane;
        let session = watch.session;
        let pending_id = watch.pending_id;
        let submitted_payload = watch.submitted_payload;
        let steer = watch.steer;
        let is_agy = watch.is_agy;
        let baseline_queued = watch.baseline_queued;
        let origin_epoch = self
            .chat
            .pending_epochs
            .get(&(task_key.clone(), pending_id))
            .copied()
            .unwrap_or(0);
        let origin = prompt::Origin {
            pane,
            session,
            host: task_key.0.clone(),
            target: self.live.get(&pane).and_then(|live| live.target.clone()),
            epoch: origin_epoch,
            initial_status,
        };
        let win = window.map(Window::window_handle).or(self.window);
        let map_key = (task_key.clone(), pending_id);
        let task = cx.spawn(async move |this, cx| {
            enum Outcome {
                Confirmed,
                AgyQueued { send_immediately_hint: bool },
                Detached,
                Failed,
                Wait,
            }
            for step in 0..TURN_POLLS {
                cx.background_executor().timer(POLL).await;
                let Ok(outcome) = this.update(cx, |this, cx| {
                    this.reconcile_pending_prompts_on_poll(&task_key);
                    if !this.has_pending_prompt(&task_key, pending_id) {
                        return Outcome::Confirmed;
                    }
                    #[cfg(test)]
                    {
                        if let Some(confirmed) = this.chat.prompt_confirmed {
                            if confirmed {
                                return Outcome::Confirmed;
                            }
                        } else if this.chat_prompt_turn_started(&task_key, initial_status, cx)
                            || (this.chat.send_input.is_some() && this.chat.poll_call.is_none())
                        {
                            return Outcome::Confirmed;
                        }
                    }
                    #[cfg(not(test))]
                    if this.chat_prompt_turn_started(&task_key, initial_status, cx) {
                        return Outcome::Confirmed;
                    }
                    let same_epoch =
                        this.chat.logs.get(&task_key).map_or(0, |log| log.epoch) == origin_epoch;
                    if same_epoch
                        && is_agy
                        && let Some(qm) = this.live.get(&pane).and_then(|live| {
                            #[cfg(test)]
                            if this.chat.prompt_confirmed == Some(false) {
                                return None;
                            }
                            krill_proto::screen::agy::queued_submission_confirmed(
                                live.pane.screen(),
                                &submitted_payload,
                                baseline_queued,
                            )
                        })
                    {
                        return Outcome::AgyQueued {
                            send_immediately_hint: qm.send_immediately_hint,
                        };
                    }
                    if this.chat_prompt_view_detached(&origin, cx) {
                        return Outcome::Detached;
                    }
                    if !same_epoch {
                        return Outcome::Failed;
                    }
                    if step % 5 == 0 {
                        this.poll_session_chat(pane, session, cx);
                    }
                    Outcome::Wait
                }) else {
                    return;
                };
                match outcome {
                    Outcome::Confirmed | Outcome::Detached => {
                        this.update(cx, |this, cx| {
                            this.confirm_pending_prompt(&task_key, pending_id);
                            cx.notify();
                        })
                        .ok();
                        return;
                    }
                    Outcome::AgyQueued {
                        send_immediately_hint,
                    } => {
                        this.update(cx, |this, cx| {
                            if steer && send_immediately_hint && this.is_pane_chat_view(pane, cx) {
                                let _ = this.send_chat_prompt_input(
                                    pane,
                                    session,
                                    krill_proto::screen::agy::SEND_IMMEDIATELY_KEY,
                                    cx,
                                );
                            }
                            this.mark_pending_prompt_queued(&task_key, pending_id, cx);
                        })
                        .ok();
                        return;
                    }
                    Outcome::Failed => break,
                    Outcome::Wait => {}
                }
            }
            prompt::update_with_optional_window(&this, win, cx, |this, window, cx| {
                this.transition_pending_to_review_required(
                    PendingReview {
                        key: &task_key,
                        pending_id,
                        immediate_send_failure: false,
                    },
                    window,
                    cx,
                );
            });
        });
        self.chat.turn_tasks.insert(map_key, task);
    }

    pub(super) fn mark_pending_prompt_queued(
        &mut self,
        key: &ChatKey,
        pending_id: u64,
        cx: &mut Context<Self>,
    ) {
        self.chat.prompt_tasks.remove(key);
        self.chat.turn_tasks.remove(&(key.clone(), pending_id));
        self.chat.prompt_failed.remove(key);
        if let Some(entries) = self.chat.pending_prompts.get_mut(key)
            && let Some(entry) = entries.iter_mut().find(|p| p.id == pending_id)
            && entry.status != PendingStatus::Queued
        {
            entry.status = PendingStatus::Queued;
            self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
        }
        cx.notify();
    }

    pub(super) fn has_pending_prompt(&self, key: &ChatKey, pending_id: u64) -> bool {
        self.chat
            .pending_prompts
            .get(key)
            .is_some_and(|list| list.iter().any(|p| p.id == pending_id))
    }

    pub(super) fn confirm_pending_prompt(&mut self, key: &ChatKey, pending_id: u64) {
        self.chat.turn_tasks.remove(&(key.clone(), pending_id));
        self.chat.pending_epochs.remove(&(key.clone(), pending_id));
        if self
            .chat
            .copied_pending
            .as_ref()
            .is_some_and(|(k, id, _)| k == key && *id == pending_id)
        {
            self.chat.copied_pending = None;
            self.chat.copied_pending_timer = None;
        }
        let mut removed = false;
        if let Some(entries) = self.chat.pending_prompts.get_mut(key) {
            let before = entries.len();
            entries.retain(|p| p.id != pending_id);
            removed = entries.len() != before;
            if entries.is_empty() {
                self.chat.pending_prompts.remove(key);
            }
        }
        if removed {
            self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
        }
    }

    pub(super) fn reconcile_pending_prompts_on_poll(&mut self, key: &ChatKey) {
        let Some(log) = self.chat.logs.get_mut(key) else {
            return;
        };
        if !log.bound {
            return;
        }
        let Some(entries) = self.chat.pending_prompts.get_mut(key) else {
            return;
        };
        if entries.is_empty() {
            return;
        }
        let mut matched_ids = Vec::new();
        for entry in entries.iter() {
            if self.chat.pending_epochs.get(&(key.clone(), entry.id)) != Some(&log.epoch) {
                continue;
            }
            let target_norm = normalized(&entry.submitted_payload);
            let target_bounded = (entry.submitted_payload.len()
                > krill_proto::agent_history::MAX_CHAT_TEXT)
                .then(|| {
                    let mut end = krill_proto::agent_history::MAX_CHAT_TEXT;
                    while end > 0 && !entry.submitted_payload.is_char_boundary(end) {
                        end = end.saturating_sub(1);
                    }
                    normalized(entry.submitted_payload.get(..end).unwrap_or_default())
                });
            let found_abs = log.items.iter().enumerate().find_map(|(i, item)| {
                let abs_idx = log.base_index.saturating_add(i);
                let item_norm = (abs_idx >= entry.origin_item
                    && !log.consumed_user_indices.contains(&abs_idx)
                    && item.kind == ChatItemKind::User)
                    .then(|| normalized(&item.text));
                item_norm
                    .is_some_and(|norm| {
                        norm == target_norm
                            || target_bounded.as_ref().is_some_and(|b| norm == *b)
                            // Agents rewrite attached image paths (`[Image #1] …`, or krilld lifts
                            // them into `file_path`); match on the typed text then.
                            || (!entry.attachments.is_empty()
                                && normalized(&krill_proto::git::clean_user_prompt(&item.text))
                                    == normalized(&entry.display_text))
                    })
                    .then_some(abs_idx)
            });
            if let Some(abs_idx) = found_abs {
                log.consumed_user_indices.insert(abs_idx);
                matched_ids.push(entry.id);
            }
        }
        if matched_ids.is_empty() {
            return;
        }
        entries.retain(|entry| !matched_ids.contains(&entry.id));
        if entries.is_empty() {
            self.chat.pending_prompts.remove(key);
        }
        for id in matched_ids {
            self.chat.pending_epochs.remove(&(key.clone(), id));
            self.chat.turn_tasks.remove(&(key.clone(), id));
            if self
                .chat
                .copied_pending
                .as_ref()
                .is_some_and(|(k, pid, _)| k == key && *pid == id)
            {
                self.chat.copied_pending = None;
                self.chat.copied_pending_timer = None;
            }
        }
        self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
    }

    pub(super) fn transition_pending_to_review_required(
        &mut self,
        review: PendingReview<'_>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let PendingReview {
            key,
            pending_id,
            immediate_send_failure,
        } = review;
        self.chat.turn_tasks.remove(&(key.clone(), pending_id));
        if !self.has_pending_prompt(key, pending_id) {
            return;
        }
        let input_empty = self
            .chat
            .inputs
            .get(key)
            .is_none_or(|(inp, _)| inp.read(cx).value().is_empty());
        let attachments_empty = self
            .chat
            .draft_attachments
            .get(key)
            .is_none_or(Vec::is_empty);

        if input_empty
            && attachments_empty
            && let Some(window) = window
        {
            let removed = self.chat.pending_prompts.get_mut(key).and_then(|entries| {
                let pos = entries.iter().position(|p| p.id == pending_id)?;
                Some(entries.remove(pos))
            });
            if self
                .chat
                .pending_prompts
                .get(key)
                .is_some_and(Vec::is_empty)
            {
                self.chat.pending_prompts.remove(key);
            }
            self.chat.pending_epochs.remove(&(key.clone(), pending_id));
            if let Some(entry) = removed {
                if !entry.attachments.is_empty() {
                    self.chat
                        .draft_attachments
                        .insert(key.clone(), entry.attachments);
                }
                if !entry.display_text.is_empty() {
                    if let Some(trig) = catalog::detect_autocomplete_trigger(
                        &entry.display_text,
                        entry.display_text.len(),
                    ) {
                        self.chat
                            .autocomplete
                            .entry(key.clone())
                            .or_default()
                            .dismissed = Some((trig.kind, trig.replace_range.start, trig.query));
                    }
                    let input = self.ensure_chat_input(key.clone(), window, cx);
                    let restored_text = entry.display_text;
                    input.update(cx, |state, cx| state.set_value(restored_text, window, cx));
                }
                self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
            }
            if immediate_send_failure {
                self.chat.prompt_failed.insert(key.clone());
            } else {
                self.chat.prompt_review.insert(key.clone());
            }
            cx.notify();
            return;
        }

        if let Some(entries) = self.chat.pending_prompts.get_mut(key)
            && let Some(entry) = entries.iter_mut().find(|p| p.id == pending_id)
            && entry.status != PendingStatus::ReviewRequired
        {
            entry.status = PendingStatus::ReviewRequired;
            self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
        }
        if immediate_send_failure {
            self.chat.prompt_failed.insert(key.clone());
        }
        cx.notify();
    }

    pub(super) fn retry_pending_chat_prompt(
        &mut self,
        key: &ChatKey,
        pending_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chat.prompt_tasks.contains_key(key) {
            return;
        }
        let Some((submitted_payload, no_attachments)) =
            self.chat.pending_prompts.get(key).and_then(|entries| {
                entries
                    .iter()
                    .find(|p| p.id == pending_id && p.status == PendingStatus::ReviewRequired)
                    .map(|p| (p.submitted_payload.clone(), p.attachments.is_empty()))
            })
        else {
            return;
        };
        let Some(pane) = self.chat_pane(key) else {
            return;
        };
        let session = key.1;
        let modes = self
            .live
            .get(&pane)
            .map_or_else(Default::default, |live| live.pane.modes());
        let krill_app::security::PastePlan::Immediate(bytes) =
            self.plan_terminal_paste(pane, submitted_payload.as_bytes(), modes, cx)
        else {
            self.chat.multiline_blocked.insert(key.clone());
            cx.notify();
            return;
        };
        self.chat.multiline_blocked.remove(key);
        let (origin_epoch, origin_item, opens_terminal, local_slash) =
            self.pending_prompt_meta(key, &submitted_payload, no_attachments);
        if let Some(entries) = self.chat.pending_prompts.get_mut(key)
            && let Some(entry) = entries.iter_mut().find(|p| p.id == pending_id)
        {
            entry.origin_item = origin_item;
            entry.status = PendingStatus::Sending;
        }
        self.chat
            .pending_epochs
            .insert((key.clone(), pending_id), origin_epoch);
        self.chat.pending_revision = self.chat.pending_revision.wrapping_add(1);
        self.chat.prompt_failed.remove(key);
        self.chat.prompt_review.remove(key);
        self.dispatch_pending_chat_prompt(
            PendingDispatch {
                key: key.clone(),
                pane,
                session,
                pending_id,
                submitted_payload,
                bytes,
                modes,
                opens_terminal,
                local_slash,
                steer: false,
            },
            Some(window),
            cx,
        );
    }

    pub(super) fn stage_chat_attachments(
        &mut self,
        key: &ChatKey,
        attachments: Vec<DraftAttachment>,
        cx: &mut Context<Self>,
    ) {
        if attachments.is_empty() {
            return;
        }
        self.chat.prompt_review.remove(key);
        let list = self.chat.draft_attachments.entry(key.clone()).or_default();
        for att in attachments {
            if !list.iter().any(|existing| existing.path == att.path) {
                list.push(att);
            }
        }
        cx.notify();
    }

    /// Not now (`created: None`) or the bot made from a proposal, recorded on the proposal.
    pub(super) fn resolve_bot_offer(
        &mut self,
        key: &ChatKey,
        offer_id: &str,
        created: Option<krill_proto::bots::BotId>,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(&key.0) else {
            return;
        };
        let host = key.0.clone();
        let request = krill_proto::bots::BotRpcRequest::ResolveBotOffer {
            offer_id: offer_id.to_owned(),
            created,
        };
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { krill_app::bots::rpc::call_checked(&rpc, &request) })
                .await;
            this.update(cx, |this, cx| {
                if let Err(err) = res {
                    this.notice = Some(err);
                }
                // Read the cards again now rather than on the next refresh.
                this.chat.bot_notes.retain(|(h, _), _| h != &host);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Create bot on a proposal: a bot with its name, its purpose as its SOUL.md line, the
    /// proposer's agent, and the card's mascot (Orca's create offer).
    pub(super) fn create_offered_bot(
        &mut self,
        key: &ChatKey,
        offer: &krill_proto::bots::BotOffer,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(&key.0) else {
            return;
        };
        if !self.chat.creating_offers.insert(offer.offer_id.clone()) {
            return;
        }
        cx.notify();
        let host = key.0.clone();
        let agent_id = self
            .chat_bot(key)
            .map_or_else(|| "claude".to_owned(), |bot| bot.agent_id.clone());
        let params = krill_proto::bots::CreateBotParams {
            name: offer.name.clone(),
            agent_id,
            description: offer.purpose.clone(),
            emoji: None,
            avatar: view::offer_mascot(offer),
            model: None,
            effort: None,
            permission_mode: None,
            workspace: None,
            template_id: None,
            initial_soul: Some(format!("# {}\n\n{}\n", offer.name, offer.purpose)),
        };
        let offer_id = offer.offer_id.clone();
        let done_offer_id = offer_id.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let request = krill_proto::bots::BotRpcRequest::CreateBot(Box::new(params));
                    let bot = match krill_app::bots::rpc::call_checked(&rpc, &request)? {
                        krill_proto::bots::BotRpcResponse::Bot(bot) => bot.id,
                        other => return Err(format!("unexpected CreateBot response: {other:?}")),
                    };
                    krill_app::bots::rpc::call_checked(
                        &rpc,
                        &krill_proto::bots::BotRpcRequest::ResolveBotOffer {
                            offer_id,
                            created: Some(bot.clone()),
                        },
                    )?;
                    Ok(bot)
                })
                .await;
            this.update(cx, |this, cx| {
                this.chat.creating_offers.remove(&done_offer_id);
                if let Err(err) = res {
                    this.notice = Some(err);
                }
                this.chat.bot_notes.retain(|(h, _), _| h != &host);
                if host == this.launch_host {
                    this.refresh_bots_badge(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Puts a chip for each newly attached image at the caret (Orca inserts the image reference
    /// where you paste). A chip is put once; deleting it leaves the image attached.
    pub(super) fn sync_chat_image_tokens(
        &mut self,
        key: &ChatKey,
        input: &gpui::Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let images: Vec<std::path::PathBuf> = self
            .chat
            .draft_attachments
            .get(key)
            .map(|list| {
                list.iter()
                    .filter(|a| a.is_image)
                    .map(|a| a.path.clone())
                    .collect()
            })
            .unwrap_or_default();
        let placed = self.chat.image_tokens.entry(key.clone()).or_default();
        placed.retain(|path| images.contains(path));
        for (ix, path) in images.iter().enumerate() {
            if placed.contains(path) {
                continue;
            }
            placed.insert(path.clone());
            let number = (ix + 1).to_string();
            let token = gpui_component::input::InlineToken::new(
                format!("{IMAGE_TOKEN_PREFIX}{}", path.display()),
                format!("[Image #{number}]"),
            )
            .with_label(ctext::IMAGE_NUMBER.format(&[("number", &number)]));
            input.update(cx, |state, cx| {
                state.replace_with_token(token, window, cx).ok();
            });
        }
        if placed.is_empty() {
            self.chat.image_tokens.remove(key);
        }
    }

    pub(super) fn remove_chat_attachment(
        &mut self,
        key: &ChatKey,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        self.chat.prompt_review.remove(key);
        if let Some(list) = self.chat.draft_attachments.get_mut(key) {
            if index < list.len() {
                list.remove(index);
            }
            if list.is_empty() {
                self.chat.draft_attachments.remove(key);
            }
            cx.notify();
        }
    }

    pub(super) fn attach_dropped_paths_to_chat(
        &mut self,
        key: &ChatKey,
        paths: &gpui::ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let staged: Vec<DraftAttachment> = paths
            .paths()
            .iter()
            .cloned()
            .filter_map(DraftAttachment::from_path)
            .collect();
        self.stage_chat_attachments(key, staged, cx);
        let input = self.ensure_chat_input(key.clone(), window, cx);
        input.update(cx, |state, cx| state.focus(window, cx));
    }

    pub(super) fn prompt_for_chat_attachments(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(ctext::ATTACH_FILE.get().into()),
        });
        self.chat.next_attach_id = self.chat.next_attach_id.wrapping_add(1);
        let attach_id = self.chat.next_attach_id;
        let task_key = key.clone();
        let entry_key = (task_key.clone(), attach_id);
        let task = cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                this.update(cx, |this, _| {
                    this.chat.attach_tasks.remove(&(task_key, attach_id));
                })
                .ok();
                return;
            };
            let staged = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .filter_map(DraftAttachment::from_path)
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                this.chat
                    .attach_tasks
                    .remove(&(task_key.clone(), attach_id));
                this.stage_chat_attachments(&task_key, staged, cx);
            })
            .ok();
        });
        self.chat.attach_tasks.insert(entry_key, task);
    }

    pub(super) fn intercept_chat_clipboard_paste(
        &mut self,
        key: &ChatKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let read = cx.read_from_clipboard_async();
        self.chat.next_attach_id = self.chat.next_attach_id.wrapping_add(1);
        let attach_id = self.chat.next_attach_id;
        let task_key = key.clone();
        let entry_key = (task_key.clone(), attach_id);
        let task = cx.spawn_in(window, async move |this, cx| {
            let item = read.await.ok().flatten();
            #[cfg(not(test))]
            let item = super::security::augment_clipboard_item_from_pasteboard(item);
            let Some(item) = item else {
                this.update_in(cx, |this, _, _| {
                    this.chat.attach_tasks.remove(&(task_key, attach_id));
                })
                .ok();
                return;
            };
            let classified = cx
                .background_executor()
                .spawn(async move { classify_chat_clipboard_item(&item) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.chat
                    .attach_tasks
                    .remove(&(task_key.clone(), attach_id));
                match classified {
                    ChatClipboardContent::Attachments(atts) => {
                        this.stage_chat_attachments(&task_key, atts, cx);
                        let input = this.ensure_chat_input(task_key.clone(), window, cx);
                        input.update(cx, |state, cx| state.focus(window, cx));
                    }
                    ChatClipboardContent::Text(text) => {
                        let input = this.ensure_chat_input(task_key.clone(), window, cx);
                        input.update(cx, |state, cx| {
                            state.focus(window, cx);
                            gpui::EntityInputHandler::replace_text_in_range(
                                state, None, &text, window, cx,
                            );
                        });
                    }
                    ChatClipboardContent::None => {}
                }
            })
            .ok();
        });
        self.chat.attach_tasks.insert(entry_key, task);
    }

    pub(super) fn sync_chat_autocomplete(
        &mut self,
        key: &ChatKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((input, _)) = self.chat.inputs.get(key) else {
            return;
        };
        let input = input.clone();
        let (text, cursor, ime_composing) = input.update(cx, |state, cx| {
            (
                state.value().to_string(),
                state.cursor(),
                gpui::EntityInputHandler::marked_text_range(state, window, cx).is_some(),
            )
        });
        if ime_composing {
            if let Some(ac) = self.chat.autocomplete.get_mut(key) {
                ac.close();
            }
            return;
        }
        let Some(trigger) = catalog::detect_autocomplete_trigger(&text, cursor) else {
            if let Some(ac) = self.chat.autocomplete.get_mut(key) {
                ac.close();
                ac.dismissed = None;
            }
            return;
        };
        {
            let ac = self.chat.autocomplete.entry(key.clone()).or_default();
            if ac.dismissed.as_ref()
                == Some(&(
                    trigger.kind,
                    trigger.replace_range.start,
                    trigger.query.clone(),
                ))
            {
                ac.close();
                return;
            }
            ac.dismissed = None;
            if ac.trigger.as_ref() == Some(&trigger) && (!ac.items.is_empty() || ac.loading) {
                return;
            }
        }
        let is_local = key.0.is_local();
        let remote_call = (!is_local)
            .then(|| self.workflow_call_for_key(key))
            .flatten();
        let session_u64 = key.1.0;
        let cwd = if is_local {
            self.chat_pane(key).and_then(|pane| self.pane_cwd(pane, cx))
        } else {
            None
        };
        if trigger.kind == catalog::AutocompleteKind::File && cwd.is_none() && remote_call.is_none()
        {
            if let Some(ac) = self.chat.autocomplete.get_mut(key) {
                ac.close();
            }
            return;
        }
        let agent = self
            .chat
            .logs
            .get(key)
            .map(|l| l.agent.clone())
            .unwrap_or_default();
        let now = std::time::Instant::now();
        let cache_miss = trigger.kind == catalog::AutocompleteKind::File
            && cwd
                .as_deref()
                .is_some_and(|dir| !catalog::has_fresh_workspace_files_cache(dir));
        let debounce = if cache_miss
            && self
                .chat
                .autocomplete_missed_at
                .get(key)
                .is_some_and(|at| now.duration_since(*at) < Duration::from_millis(120))
        {
            Some(Duration::from_millis(120))
        } else {
            None
        };
        if cache_miss {
            self.chat.autocomplete_missed_at.insert(key.clone(), now);
        }
        let ac = self.chat.autocomplete.entry(key.clone()).or_default();
        ac.generation = ac.generation.wrapping_add(1);
        let generation = ac.generation;
        let trigger_kind = trigger.kind;
        let query = trigger.query.clone();
        ac.trigger = Some(trigger);
        ac.loading = true;
        let home = if is_local {
            catalog::user_home_dir()
        } else {
            None
        };
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            if let Some(delay) = debounce {
                cx.background_executor().timer(delay).await;
            }
            let items = cx
                .background_executor()
                .spawn(async move {
                    match trigger_kind {
                        catalog::AutocompleteKind::File => match cwd {
                            Some(dir) => catalog::scan_workspace_files(&dir, &query),
                            None => remote_call
                                .and_then(|call| {
                                    let req = serde_json::json!({
                                        "action": "mention_files",
                                        "session": session_u64,
                                        "query": query,
                                    })
                                    .to_string();
                                    catalog::parse_remote_mention_items(&call(&req).ok()?, &query)
                                })
                                .unwrap_or_default(),
                        },
                        catalog::AutocompleteKind::Slash => {
                            if let Some(call) = remote_call {
                                let req = serde_json::json!({
                                    "action": "command_catalog",
                                    "session": session_u64,
                                })
                                .to_string();
                                if let Some(remote_items) = call(&req)
                                    .ok()
                                    .and_then(|raw| catalog::parse_remote_slash_items(&raw, &query))
                                {
                                    return remote_items;
                                }
                            }
                            catalog::scan_slash_catalog(
                                home.as_deref(),
                                cwd.as_deref(),
                                &agent,
                                is_local,
                                &query,
                            )
                        }
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(ac) = this.chat.autocomplete.get_mut(&task_key) else {
                    return;
                };
                if ac.generation != generation {
                    return;
                }
                ac.loading = false;
                ac.task = None;
                ac.items = items;
                if ac.selected >= ac.items.len() {
                    ac.selected = 0;
                }
                if !ac.items.is_empty() {
                    this.chat.quick_menu.remove(&task_key);
                    this.chat.model_picker.remove(&task_key);
                    this.chat.model_picker_selected.remove(&task_key);
                }
                cx.notify();
            })
            .ok();
        });
        ac.task = Some(task);
    }

    pub(super) fn accept_chat_autocomplete(
        &mut self,
        key: &ChatKey,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((input, _)) = self.chat.inputs.get(key) else {
            return false;
        };
        let input = input.clone();
        if Self::chat_input_is_composing(&input, window, cx) {
            return false;
        }
        let Some((replace_range, insert_text)) =
            self.chat.autocomplete.get_mut(key).and_then(|ac| {
                let trigger = ac.trigger.as_ref()?;
                let idx = index.unwrap_or(ac.selected);
                let candidate = ac.items.get(idx)?;
                let range = trigger.replace_range.clone();
                let insert = candidate.insert_text.clone();
                ac.close();
                ac.dismissed = None;
                Some((range, insert))
            })
        else {
            return false;
        };
        input.update(cx, |state, cx| {
            let current = state.value().to_string();
            let start_u16 = catalog::utf8_to_utf16_offset(&current, replace_range.start);
            let end_u16 = catalog::utf8_to_utf16_offset(&current, replace_range.end);
            state.focus(window, cx);
            gpui::EntityInputHandler::replace_text_in_range(
                state,
                Some(start_u16..end_u16),
                &insert_text,
                window,
                cx,
            );
        });
        if let Some(ac) = self.chat.autocomplete.get_mut(key) {
            ac.close();
        }
        cx.notify();
        true
    }

    pub(super) fn chat_find_is_open(&self, _cx: &gpui::App) -> bool {
        let Some(pane) = self.layout.focused() else {
            return false;
        };
        let Some(key) = self.chat_key(pane) else {
            return false;
        };
        self.chat.finds.contains_key(&key)
    }

    pub(super) fn toggle_chat_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.layout.focused() else {
            return;
        };
        let Some(key) = self.chat_key(pane) else {
            return;
        };
        if let Some(find) = self.chat.finds.get(&key) {
            let input = find.input.clone();
            input.update(cx, |state, cx| state.focus(window, cx));
            cx.notify();
            return;
        }
        let input = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(ctext::FIND_PLACEHOLDER.get())
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        let find_key = key.clone();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, state, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    let next_query = state.read(cx).value().to_string();
                    if let Some(find) = this.chat.finds.get_mut(&find_key)
                        && find.query != next_query
                    {
                        find.query = next_query;
                        this.recompute_chat_find(&find_key);
                        this.reveal_active_chat_find(&find_key);
                        cx.notify();
                    }
                }
            },
        );
        self.chat.finds.insert(
            key.clone(),
            ChatFindState {
                query: String::new(),
                input,
                _sub: sub,
                matches: Vec::new(),
                active: 0,
            },
        );
        cx.notify();
    }

    pub(super) fn close_chat_find(
        &mut self,
        key: &ChatKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chat.finds.remove(key).is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    pub(super) fn step_chat_find(
        &mut self,
        direction: krill_proto::search::Direction,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.layout.focused() else {
            return;
        };
        let Some(key) = self.chat_key(pane) else {
            return;
        };
        let Some(find) = self.chat.finds.get_mut(&key) else {
            return;
        };
        if find.matches.is_empty() {
            return;
        }
        let len = find.matches.len();
        find.active = match direction {
            krill_proto::search::Direction::Next => (find.active + 1) % len,
            krill_proto::search::Direction::Previous => {
                if find.active == 0 {
                    len - 1
                } else {
                    find.active - 1
                }
            }
        };
        self.reveal_active_chat_find(&key);
        cx.notify();
    }

    pub(super) fn step_chat_find_for_key(
        &mut self,
        key: &ChatKey,
        direction: krill_proto::search::Direction,
        cx: &mut Context<Self>,
    ) {
        let Some(find) = self.chat.finds.get_mut(key) else {
            return;
        };
        if find.matches.is_empty() {
            return;
        }
        let len = find.matches.len();
        find.active = match direction {
            krill_proto::search::Direction::Next => (find.active + 1) % len,
            krill_proto::search::Direction::Previous => {
                if find.active == 0 {
                    len - 1
                } else {
                    find.active - 1
                }
            }
        };
        self.reveal_active_chat_find(key);
        cx.notify();
    }

    pub(super) fn active_chat_find_row(&self, key: &ChatKey) -> Option<usize> {
        let find = self.chat.finds.get(key)?;
        if find.query.trim().is_empty() {
            return None;
        }
        find.matches.get(find.active).copied()
    }

    fn recompute_chat_find(&mut self, key: &ChatKey) {
        let Some(query) = self.chat.finds.get(key).map(|f| f.query.trim().to_owned()) else {
            return;
        };
        let matches = if query.is_empty() {
            Vec::new()
        } else if let Some(log) = self.chat.logs.get_mut(key) {
            let feed = log.display_feed();
            let q_lower = query.to_lowercase();
            feed.rows()
                .iter()
                .enumerate()
                .filter_map(|(ix, row)| display_row_matches_query(row, &q_lower).then_some(ix))
                .collect()
        } else {
            Vec::new()
        };
        if let Some(find) = self.chat.finds.get_mut(key) {
            find.matches = matches;
            if find.active >= find.matches.len() {
                find.active = 0;
            }
        }
    }

    fn reveal_active_chat_find(&self, key: &ChatKey) {
        let Some(row_ix) = self.active_chat_find_row(key) else {
            return;
        };
        let Some(pane) = self.chat_pane(key) else {
            return;
        };
        if let Some(list) = self.chat.lists.get(&(pane, key.1)) {
            list.state.scroll_to(gpui::ListOffset {
                item_ix: row_ix,
                offset_in_item: gpui::px(0.),
            });
        }
    }

    pub(super) fn scroll_chat_overlay_home(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.layout.focused() else {
            return;
        };
        let Some(session) = self.layout.session(pane) else {
            return;
        };
        if let Some(list) = self.chat.lists.get(&(pane, session)) {
            list.state.scroll_to(gpui::ListOffset {
                item_ix: 0,
                offset_in_item: gpui::px(0.),
            });
            cx.notify();
        }
    }

    pub(super) fn scroll_chat_overlay_end(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.layout.focused() else {
            return;
        };
        let Some(session) = self.layout.session(pane) else {
            return;
        };
        if let Some(list) = self.chat.lists.get(&(pane, session)) {
            list.state.set_follow_mode(gpui::FollowMode::Tail);
            cx.notify();
        }
    }

    pub(super) fn scroll_chat_overlay_page(&mut self, delta_dir: isize, cx: &mut Context<Self>) {
        let Some(pane) = self.layout.focused() else {
            return;
        };
        let Some(session) = self.layout.session(pane) else {
            return;
        };
        if let Some(list) = self.chat.lists.get(&(pane, session)) {
            let total = list.state.item_count();
            if total == 0 {
                return;
            }
            if delta_dir < 0 {
                let top = list.state.logical_scroll_top().item_ix;
                let target = top.saturating_sub(6);
                list.state.scroll_to(gpui::ListOffset {
                    item_ix: target,
                    offset_in_item: gpui::px(0.),
                });
            } else {
                let top = list.state.logical_scroll_top().item_ix;
                let target = top.saturating_add(6);
                if target + 1 >= total {
                    list.state.set_follow_mode(gpui::FollowMode::Tail);
                } else {
                    list.state.scroll_to(gpui::ListOffset {
                        item_ix: target,
                        offset_in_item: gpui::px(0.),
                    });
                }
            }
            cx.notify();
        }
    }
}

fn display_row_matches_query(row: &work_run::DisplayRow, query_lower: &str) -> bool {
    match row {
        work_run::DisplayRow::Item { item, .. } => {
            item.text.to_lowercase().contains(query_lower)
                || item.title.to_lowercase().contains(query_lower)
                || item.file_path.to_lowercase().contains(query_lower)
        }
        work_run::DisplayRow::WorkRun(run) => run.steps.iter().any(|step| {
            step.headline().to_lowercase().contains(query_lower)
                || step.file_path().to_lowercase().contains(query_lower)
                || step
                    .body_sections()
                    .iter()
                    .any(|sec| sec.text.to_lowercase().contains(query_lower))
        }),
        work_run::DisplayRow::UnreadDivider { .. } | work_run::DisplayRow::TurnBar { .. } => false,
        work_run::DisplayRow::Notice { notice, .. } => {
            notice.detail.to_lowercase().contains(query_lower)
                || notice
                    .from
                    .iter()
                    .any(|f| f.to_lowercase().contains(query_lower))
        }
        work_run::DisplayRow::ItemNote { note, .. } => match &note.action {
            work_run::NoteAction::Memory(receipt) => receipt
                .added
                .iter()
                .chain(&receipt.removed)
                .any(|fact| fact.to_lowercase().contains(query_lower)),
            work_run::NoteAction::CreateOffer(offer) => {
                offer.name.to_lowercase().contains(query_lower)
                    || offer.purpose.to_lowercase().contains(query_lower)
            }
            _ => {
                note.title.to_lowercase().contains(query_lower)
                    || note.detail.to_lowercase().contains(query_lower)
            }
        },
    }
}

/// An item change and the chat that made it.
#[derive(Clone, Debug)]
pub(crate) struct BotNote {
    pub note: Arc<work_run::ItemNote>,
    pub session: Option<SessionId>,
}

/// A bot's proposals of new bots, as cards in the chat that made them (Orca's create offer).
fn offer_notes(offers: Vec<krill_proto::bots::BotOffer>) -> Vec<BotNote> {
    use work_run::{ItemNote, NoteAction, NoteActor};
    offers
        .into_iter()
        .map(|offer| BotNote {
            session: offer.session_id.map(SessionId),
            note: Arc::new(ItemNote {
                at_ms: offer.offered_at_ms,
                actor: NoteActor::Bot,
                title: offer.name.clone(),
                action: NoteAction::CreateOffer(Box::new(offer)),
                status: krill_proto::bots::BotItemStatus::Planned,
                detail: String::new(),
            }),
        })
        .collect()
}

/// A bot's own memory edits as chat lines (Orca's turn memory receipts). Owner and housekeeping
/// edits (tidy, heal) are not said in chat.
fn memory_notes(entries: &[krill_proto::bots::BotMemoryHistoryEntry]) -> Vec<BotNote> {
    use work_run::{ItemNote, MemoryReceipt, NoteAction, NoteActor};
    entries
        .iter()
        .filter(|e| e.actor.starts_with("bot:") || e.actor == "bot")
        .map(|e| {
            let other_changes = e.added.is_empty() && e.removed.is_empty();
            BotNote {
                note: Arc::new(ItemNote {
                    at_ms: e.timestamp_ms,
                    actor: NoteActor::Bot,
                    action: NoteAction::Memory(MemoryReceipt {
                        added: e.added.clone(),
                        removed: e.removed.clone(),
                        other_changes,
                    }),
                    title: String::new(),
                    status: krill_proto::bots::BotItemStatus::Done,
                    detail: String::new(),
                }),
                session: e.turn_session().map(SessionId),
            }
        })
        .collect()
}

/// A bot's item events as chat lines, oldest first (snoozes are not said in chat).
fn bot_notes(items: &[krill_proto::bots::BotItem]) -> Vec<BotNote> {
    use krill_proto::bots::{BotItemActor, BotItemEventKind, BotItemStatus};
    use work_run::{ItemNote, NoteAction, NoteActor};
    let mut out = Vec::new();
    for item in items {
        for event in &item.events {
            let action = match event.kind {
                BotItemEventKind::Made => NoteAction::Added,
                BotItemEventKind::Edited => NoteAction::Updated,
                BotItemEventKind::Commented => NoteAction::Commented,
                BotItemEventKind::Linked => NoteAction::Linked(
                    event
                        .text
                        .split_once(':')
                        .map_or(event.text.as_str(), |(_, target)| target)
                        .to_owned(),
                ),
                BotItemEventKind::Status => {
                    let to = event.text.rsplit("-> ").next().unwrap_or_default().trim();
                    let Some(status) = [
                        BotItemStatus::Planned,
                        BotItemStatus::Working,
                        BotItemStatus::NeedsYou,
                        BotItemStatus::Done,
                        BotItemStatus::Canceled,
                    ]
                    .into_iter()
                    .find(|s| s.as_str() == to) else {
                        continue;
                    };
                    NoteAction::Status(status)
                }
                BotItemEventKind::Snoozed | BotItemEventKind::Unsnoozed => continue,
            };
            let (actor, session) = match &event.actor {
                BotItemActor::Owner { .. } => (NoteActor::Owner, None),
                BotItemActor::Bot { session_id, .. } => (NoteActor::Bot, *session_id),
                BotItemActor::System { .. } => (NoteActor::Bot, None),
            };
            let detail = if event.kind == BotItemEventKind::Commented {
                event.text.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                String::new()
            };
            out.push(BotNote {
                note: Arc::new(ItemNote {
                    at_ms: event.created_at_ms,
                    actor,
                    action,
                    title: item.title.clone(),
                    status: item.status,
                    detail,
                }),
                session,
            });
        }
    }
    out.sort_by_key(|n| n.note.at_ms);
    out
}
