// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Full workspace Bot Group Chat Page (`BotGroupChat`, `BotGroupTurn`, `BotGroupReplyPicker`,
//! `BotGroupApprovalDock`, `@mention` completion, peer request status lines, and thread switching).

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui::{
    AnyElement, Context, Entity, Focusable as _, FontWeight, KeyDownEvent, ScrollHandle,
    SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use krill_app_core::hosts::HostId;
use krill_app_core::i18n::catalog::{bots as text, chat as ctext};
use krill_app_core::layout::{GroupPaneTarget, PaneId};
use krill_app_core::ui;
use krill_bots_ui::bots::{SidebarBot, avatar, group_faces, rpc::call_checked};
use krill_proto::SessionId;
use krill_proto::agent_history::{Action, ChatItem, ChatItemKind, Response};
use krill_proto::bots::{
    BotGroupMessage, BotGroupReplyAudience, BotGroupRoomView, BotGroupTurnInfo, BotId,
    BotRpcRequest, BotRpcResponse, extract_group_mention_ids,
};
use krill_proto::control::{AgentState, Reply, Request};

use super::Shell;

const ROOM_PAGE_LIMIT: u32 = 80;
const EVERYONE_HANDLE: &str = "@everyone";
const TURN_COLLAPSE_LINE_LIMIT: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct GroupRoomKey {
    pub host: HostId,
    pub group_id: String,
    pub thread_id: Option<String>,
}

#[derive(Clone, Debug)]
struct MemberPromptState {
    bot: SidebarBot,
    session_id: SessionId,
    state: AgentState,
    item: Option<ChatItem>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum ComposerFocus {
    #[default]
    Keep,
    Focus,
}

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent in-flight request flags for room poll, repoll, earlier pagination, send, and stop"
)]
pub(super) struct RoomState {
    pub room: Option<BotGroupRoomView>,
    pub reply_audience: BotGroupReplyAudience,
    pub loading: bool,
    pub poll_gen: u64,
    pub repoll_needed: bool,
    pub loading_earlier: bool,
    pub sending: bool,
    pub stopping_all: bool,
    pub stopping_bots: HashSet<BotId>,
    pub error: Option<String>,
    pub action_error: Option<String>,
    pub composer: Option<(Entity<TextareaState>, Subscription)>,
    pub draft: String,
    pub mention_selected: usize,
    pub mention_dismissed_at: Option<usize>,
    pub expanded_turns: HashSet<String>,
    pub selected_approval_bot: Option<BotId>,
    pub member_items: BTreeMap<SessionId, Vec<ChatItem>>,
    pub last_poll_ms: u64,
    pub focus_composer: ComposerFocus,
    pub scroll: ScrollHandle,
    pub follow_paused: bool,
    pub last_tail_message_id: Option<String>,
    pub last_tail_sig: Option<(String, usize, usize)>,
    pub initial_scrolled: std::cell::Cell<bool>,
}

fn is_group_timeline_near_bottom(scroll: &ScrollHandle) -> bool {
    let max_y = f32::from(scroll.max_offset().y);
    if max_y <= 1.0 {
        return true;
    }
    let cur_y = -f32::from(scroll.offset().y);
    (max_y - cur_y) <= 24.0
}

#[derive(Default)]
pub(super) struct BotGroupUiState {
    pub rooms: HashMap<GroupRoomKey, RoomState>,
    pub last_seen_ms: HashMap<(HostId, String), u64>,
    poll_timer: Option<Task<()>>,
}

/// Returns true when a bot session's label belongs to `group_id` (and `thread_id`, if specified),
/// never matching private Home Chat or other groups (`BOT-06`).
#[must_use]
pub(in crate::shell) fn session_belongs_to_group(
    label: &str,
    group_id: &str,
    thread_id: Option<&str>,
) -> bool {
    if let Some(tid) = thread_id {
        let exact = format!("(group:{group_id}:{tid})");
        if label.ends_with(&exact) {
            return true;
        }
        let legacy = format!("(group:{group_id})");
        return label.ends_with(&legacy);
    }
    let legacy = format!("(group:{group_id})");
    let prefix = format!("(group:{group_id}:");
    label.ends_with(&legacy) || (label.contains(&prefix) && label.ends_with(')'))
}

fn latest_unread_bot_message_ms(summary: &krill_proto::bots::BotGroupSummary) -> Option<u64> {
    let msg = summary.latest_message.as_ref()?;
    let body = msg.body.trim();
    if msg.sender_id == "owner"
        || msg.sender_id == "user"
        || msg.in_progress
        || body.is_empty()
        || body == "[stopped]"
    {
        return None;
    }
    Some(msg.created_at_ms)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MentionCandidate {
    handle: String,
    bot_id: Option<BotId>,
}

fn mention_candidates(query: &str, members: &[SidebarBot]) -> Vec<MentionCandidate> {
    let needle = query.to_ascii_lowercase();
    let mut out = Vec::new();
    if needle.is_empty() || "everyone".starts_with(&needle) {
        out.push(MentionCandidate {
            handle: "everyone".to_owned(),
            bot_id: None,
        });
    }
    for b in members {
        if needle.is_empty()
            || b.name.to_ascii_lowercase().contains(&needle)
            || b.slug.to_ascii_lowercase().contains(&needle)
        {
            out.push(MentionCandidate {
                handle: b.slug.clone(),
                bot_id: Some(b.id.clone()),
            });
        }
    }
    out
}

/// Cursor-aware `@mention` query detection supporting mid-draft edits and spaced display names
/// like `@Bob Ops` (`BOT-08`, `CHAT-23`, `BOT-28`).
///
/// Returns `(at_byte_index, cursor_byte_index, query)` when the cursor is inside an active
/// `@mention` token.
#[must_use]
pub fn mention_query_at_cursor(
    text: &str,
    cursor_byte: usize,
    members: &[SidebarBot],
) -> Option<(usize, usize, String)> {
    let cursor = cursor_byte.min(text.len());
    let before = text.get(..cursor)?;
    let line_start = before.rfind('\n').map_or(0, |idx| idx + 1);
    let line = before.get(line_start..)?;
    for (rel_at, ch) in line.char_indices().rev() {
        if ch != '@' {
            continue;
        }
        let at_pos = line_start + rel_at;
        if at_pos > 0 {
            let prev = text.get(..at_pos)?.chars().next_back()?;
            if prev.is_alphanumeric() || prev == '_' {
                continue;
            }
        }
        let token = text.get(at_pos.saturating_add(1)..cursor)?;
        if token.chars().any(|c| c == '\n' || c == '\r' || c == '\t') {
            return None;
        }
        if !token.contains(' ') {
            return Some((at_pos, cursor, token.to_owned()));
        }
        // Allow spaces only while the query still matches a spaced member display name (`BOT-28`).
        let needle = token.to_ascii_lowercase();
        if members
            .iter()
            .any(|b| b.name.to_ascii_lowercase().starts_with(&needle))
        {
            return Some((at_pos, cursor, token.to_owned()));
        }
        return None;
    }
    None
}

/// Splices `@<handle> ` over `text[at_pos..cursor_byte]`, preserving any trailing suffix after
/// `cursor_byte` (`BOT-08`). Returns `(new_text, new_cursor_byte)`.
#[must_use]
pub fn splice_mention_token(
    text: &str,
    at_pos: usize,
    cursor_byte: usize,
    handle: &str,
) -> (String, usize) {
    let clamped_at = at_pos.min(text.len());
    let clamped_cursor = cursor_byte.clamp(clamped_at, text.len());
    let prefix = text.get(..clamped_at).unwrap_or_default();
    let suffix = text.get(clamped_cursor..).unwrap_or_default();
    let sep = if suffix.starts_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    let inserted = format!("@{handle}{sep}");
    let new_cursor = prefix.len() + inserted.len() + usize::from(sep.is_empty());
    let next = format!("{prefix}{inserted}{suffix}");
    (
        next,
        new_cursor.min(prefix.len() + inserted.len() + suffix.len()),
    )
}

fn byte_offset_to_position(text: &str, byte_offset: usize) -> gpui_component::input::Position {
    let clamped = byte_offset.min(text.len());
    let prefix = text.get(..clamped).unwrap_or(text);
    let line = u32::try_from(prefix.bytes().filter(|&b| b == b'\n').count()).unwrap_or(0);
    let line_start = prefix.rfind('\n').map_or(0, |idx| idx + 1);
    let col =
        u32::try_from(prefix.get(line_start..).unwrap_or_default().chars().count()).unwrap_or(0);
    gpui_component::input::Position::new(line, col)
}

/// Computes the concise label shown on `BotGroupReplyPicker` (`Replies: ...`), accounting for
/// any live `@Name` or `@everyone` mentions typed in `draft`.
#[must_use]
pub fn reply_picker_summary(
    audience: &BotGroupReplyAudience,
    draft: &str,
    members: &[SidebarBot],
) -> String {
    let mentions = extract_draft_mentions(draft, members);
    if !mentions.is_empty() {
        if matches!(audience, BotGroupReplyAudience::All) {
            return text::GRP_ALL_AND_N.format(&[("n", &mentions.len().to_string())]);
        }
        if mentions.len() == members.len() && members.len() > 1 {
            return text::GRP_ALL_BOTS.get().to_owned();
        }
        if mentions.len() == 1
            && let Some(bot) = members.iter().find(|b| b.id == mentions[0])
        {
            return bot.name.clone();
        }
        return text::GRP_N_MENTIONED.format(&[("n", &mentions.len().to_string())]);
    }
    match audience {
        BotGroupReplyAudience::All => text::GRP_ALL_BOTS.get().to_owned(),
        BotGroupReplyAudience::Mentions => text::GRP_ONLY_MENTIONED.get().to_owned(),
        BotGroupReplyAudience::Bot { bot_id } => {
            members.iter().find(|b| &b.id == bot_id).map_or_else(
                || text::GRP_ONLY_MENTIONED.get().to_owned(),
                |b| b.name.clone(),
            )
        }
    }
}

/// Extracts mentioned group member IDs from draft text (supporting `@Bot`, `@slug`, `@Name`, `@everyone`).
#[must_use]
pub fn extract_draft_mentions(text: &str, members: &[SidebarBot]) -> Vec<BotId> {
    let refs: Vec<(&BotId, &str, &str)> = members
        .iter()
        .map(|b| (&b.id, b.slug.as_str(), b.name.as_str()))
        .collect();
    extract_group_mention_ids(text, &refs)
}

/// Returns `(at_byte_index, filter_query)` when the trailing `@mention` token is actively being typed.
#[cfg(test)]
#[must_use]
pub fn trailing_mention_query(text: &str) -> Option<(usize, String)> {
    let at_pos = text.rfind('@')?;
    if at_pos > 0 {
        let prev = text.get(..at_pos)?.chars().next_back()?;
        if prev.is_alphanumeric() || prev == '_' {
            return None;
        }
    }
    let token = text.get(at_pos.saturating_add(1)..)?;
    if token.chars().any(char::is_whitespace) {
        return None;
    }
    Some((at_pos, token.to_owned()))
}

/// Replaces `@<token>` starting at `at_pos` with `@<handle> `.
#[cfg(test)]
#[must_use]
pub fn insert_mention_token(text: &str, at_pos: usize, handle: &str) -> String {
    let prefix = text.get(..at_pos).unwrap_or_default();
    format!("{prefix}@{handle} ")
}

fn menu_heading(label: impl Into<SharedString>) -> PopupMenuItem {
    let label = label.into();
    PopupMenuItem::element(move |_, cx| {
        let p = ui::palette(cx);
        div()
            .py_0p5()
            .text_size(ui::TEXT_XS)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.muted_foreground)
            .child(label.clone())
    })
    .disabled(true)
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn format_elapsed(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        text::GRP_ELAPSED_SEC.format(&[("s", &secs.to_string())])
    } else {
        let mins = secs / 60;
        let rem = secs % 60;
        text::GRP_ELAPSED_MIN_SEC.format(&[("m", &mins.to_string()), ("s", &rem.to_string())])
    }
}

/// Returns whether an existing group pane targets the same thread as `requested_tid`,
/// treating `None` as equivalent to its loaded `active_thread_id` or `default_tid`.
fn same_group_thread(
    existing_tid: Option<&str>,
    existing_active_tid: Option<&str>,
    requested_tid: Option<&str>,
    default_tid: Option<&str>,
) -> bool {
    let eff_existing = existing_tid.or(existing_active_tid).or(default_tid);
    let eff_requested = requested_tid.or(default_tid);
    match (eff_existing, eff_requested) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    }
}

fn resolved_thread_id(key: &GroupRoomKey, state: Option<&RoomState>) -> Option<String> {
    key.thread_id.clone().or_else(|| {
        state
            .and_then(|r| r.room.as_ref())
            .and_then(|v| v.active_thread_id.clone())
    })
}

/// Returns `(display_body, is_clamped)` for a finished group bot turn.
/// When `expanded` is false and `body` has more than `TURN_COLLAPSE_LINE_LIMIT` lines,
/// returns the first `TURN_COLLAPSE_LINE_LIMIT` lines followed by an ellipsis (closing any
/// unclosed triple-backtick fence so markdown stays well-formed).
fn group_turn_display_body(body: &str, expanded: bool) -> (String, bool) {
    let mut lines = body.lines();
    let head: Vec<&str> = lines.by_ref().take(TURN_COLLAPSE_LINE_LIMIT).collect();
    if expanded || lines.next().is_none() {
        return (body.to_owned(), false);
    }
    let mut preview = head.join("\n");
    let open_fences = head
        .iter()
        .filter(|line| line.trim_start().starts_with("```"))
        .count();
    if !open_fences.is_multiple_of(2) {
        preview.push_str("\n```");
    }
    preview.push_str("\n…");
    (preview, true)
}

/// Merges a fresh latest-page poll into an existing room view so older messages previously
/// fetched via "Load earlier" (and the corresponding `has_earlier` flag) are preserved.
fn merge_polled_room(
    prev: Option<&BotGroupRoomView>,
    mut polled: BotGroupRoomView,
) -> BotGroupRoomView {
    let Some(prev) = prev else {
        return polled;
    };
    if let (Some(a), Some(b)) = (&prev.active_thread_id, &polled.active_thread_id)
        && a != b
    {
        return polled;
    }
    let Some(first_polled) = polled.messages.first() else {
        return polled;
    };
    let Some(overlap_idx) = prev
        .messages
        .iter()
        .position(|m| m.message_id == first_polled.message_id)
    else {
        return polled;
    };
    if overlap_idx == 0 {
        return polled;
    }
    let polled_ids: HashSet<&str> = polled
        .messages
        .iter()
        .map(|m| m.message_id.as_str())
        .collect();
    let mut merged: Vec<BotGroupMessage> = prev.messages[..overlap_idx]
        .iter()
        .filter(|m| !polled_ids.contains(m.message_id.as_str()))
        .cloned()
        .collect();
    merged.append(&mut polled.messages);
    polled.messages = merged;
    polled.has_earlier = prev.has_earlier;
    polled
}

fn invalidate_rooms_map(
    rooms: &mut HashMap<GroupRoomKey, RoomState>,
    host: Option<&HostId>,
    group_ids: Option<&[String]>,
) {
    let targeted = group_ids.filter(|ids| !ids.is_empty());
    for (k, room) in rooms.iter_mut() {
        if host.is_none_or(|h| &k.host == h)
            && targeted.is_none_or(|ids| ids.iter().any(|id| id == &k.group_id))
        {
            room.last_poll_ms = 0;
            room.poll_gen = room.poll_gen.wrapping_add(1);
            if room.loading {
                room.repoll_needed = true;
            }
        }
    }
}

impl Shell {
    pub(super) fn is_group_pane(&self, pane: PaneId) -> bool {
        self.layout.group_pane(pane).is_some()
    }

    pub(super) fn group_pane_has_focus(
        &self,
        pane: PaneId,
        window: &Window,
        cx: &gpui::App,
    ) -> bool {
        let Some(target) = self.layout.group_pane(pane) else {
            return false;
        };
        let key = GroupRoomKey {
            host: self.layout.host(pane).clone(),
            group_id: target.group_id.clone(),
            thread_id: target.thread_id.clone(),
        };
        self.bot_ui
            .group_chat
            .rooms
            .get(&key)
            .and_then(|r| r.composer.as_ref())
            .is_some_and(|(c, _)| c.read(cx).focus_handle(cx).is_focused(window))
    }

    pub(super) fn group_tab_title(&self, pane: PaneId) -> Option<String> {
        let target = self.layout.group_pane(pane)?;
        let host = self.layout.host(pane);
        let summary = self
            .sidebar_groups
            .iter()
            .find(|s| &s.host == host && s.group.id == target.group_id);
        let group_name = summary.map_or(target.group_id.as_str(), |s| s.group.name.as_str());
        if let Some(ref tid) = target.thread_id
            && let Some(thread) =
                summary.and_then(|s| s.threads.iter().find(|t| &t.thread_id == tid))
            && thread.title != "Main thread"
            && !thread.title.trim().is_empty()
        {
            return Some(format!("{group_name} · {}", thread.title));
        }
        Some(group_name.to_owned())
    }

    pub(super) fn invalidate_group_room(&mut self, host: &HostId, group_id: &str) {
        self.invalidate_group_rooms(Some(host), Some(&[group_id.to_owned()]));
    }

    /// Orca `#47523`: refresh only the specific group rooms whose state changed instead of
    /// polling every open room; `None` or an empty list invalidates all rooms on the host.
    pub(super) fn invalidate_group_rooms(
        &mut self,
        host: Option<&HostId>,
        group_ids: Option<&[String]>,
    ) {
        invalidate_rooms_map(&mut self.bot_ui.group_chat.rooms, host, group_ids);
    }

    pub(super) fn close_panes_for_group(
        &mut self,
        host: &HostId,
        group_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.bot_ui
            .group_chat
            .rooms
            .retain(|k, _| !(k.host == *host && k.group_id == group_id));
        let matching: Vec<PaneId> = self
            .layout
            .tabs()
            .iter()
            .flat_map(|t| {
                let mut leaves = Vec::new();
                t.root.leaves(&mut leaves);
                leaves
            })
            .filter(|p| {
                self.layout.host(*p) == host
                    && self
                        .layout
                        .group_pane(*p)
                        .is_some_and(|g| g.group_id == group_id)
            })
            .collect();
        if matching.is_empty() {
            return;
        }
        self.change(true, cx, |l| {
            let prior_focused = l.focused();
            let prior_tab_survivor = l.visible().into_iter().find(|p| !matching.contains(p));
            let mut target_active_idx = l.active();
            for pane in matching {
                if let Some(tab_idx) = l.tab_of_pane(pane) {
                    if l.tab_panes(tab_idx).len() <= 1 {
                        l.close_tab(tab_idx);
                        if tab_idx < target_active_idx {
                            target_active_idx -= 1;
                        } else if tab_idx == target_active_idx && !l.tabs().is_empty() {
                            target_active_idx = target_active_idx.min(l.tabs().len() - 1);
                        }
                    } else {
                        l.activate(tab_idx);
                        let tab_prior_focus = l.focused();
                        l.focus(pane);
                        l.close_pane();
                        if let Some(prev) = tab_prior_focus
                            && prev != pane
                            && l.tab_of_pane(prev) == Some(tab_idx)
                        {
                            l.focus(prev);
                        }
                    }
                }
            }
            if let Some(pane) = prior_focused
                && let Some(tab_idx) = l.tab_of_pane(pane)
            {
                l.activate(tab_idx);
                l.focus(pane);
            } else if let Some(pane) = prior_tab_survivor
                && let Some(tab_idx) = l.tab_of_pane(pane)
            {
                l.activate(tab_idx);
            } else if !l.tabs().is_empty() {
                l.activate(target_active_idx.min(l.tabs().len() - 1));
            }
        });
    }

    #[cfg(all(test, unix))]
    pub(in crate::shell) fn open_bot_group(
        &mut self,
        group_id: &str,
        thread_id: Option<&str>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let host = self.launch_host.clone();
        self.open_bot_group_on(&host, group_id, thread_id, window, cx);
    }

    pub(in crate::shell) fn open_bot_group_on(
        &mut self,
        host: &HostId,
        group_id: &str,
        thread_id: Option<&str>,
        _window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let host = host.clone();
        self.dashboard = None;
        self.close_settings(None, cx);

        let target = GroupPaneTarget {
            group_id: group_id.to_owned(),
            thread_id: thread_id.map(str::to_owned),
        };
        let default_tid: Option<String> = self
            .bot_ui
            .group_chat
            .rooms
            .get(&GroupRoomKey {
                host: host.clone(),
                group_id: group_id.to_owned(),
                thread_id: None,
            })
            .and_then(|r| r.room.as_ref())
            .and_then(|v| {
                v.active_thread_id
                    .clone()
                    .or_else(|| v.threads.first().map(|t| t.thread_id.clone()))
            })
            .or_else(|| {
                self.sidebar_groups
                    .iter()
                    .find(|s| s.host == host && s.group.id == group_id)
                    .and_then(|s| s.threads.first().map(|t| t.thread_id.clone()))
            });
        let existing: Option<(PaneId, Option<String>)> =
            (0..self.layout.tabs().len()).find_map(|tab_idx| {
                self.layout.tab_panes(tab_idx).into_iter().find_map(|p| {
                    if self.layout.host(p) != &host {
                        return None;
                    }
                    let g = self.layout.group_pane(p)?;
                    if g.group_id != group_id {
                        return None;
                    }
                    let existing_active_tid = self
                        .bot_ui
                        .group_chat
                        .rooms
                        .get(&GroupRoomKey {
                            host: host.clone(),
                            group_id: group_id.to_owned(),
                            thread_id: g.thread_id.clone(),
                        })
                        .and_then(|r| r.room.as_ref())
                        .and_then(|v| v.active_thread_id.as_deref());
                    same_group_thread(
                        g.thread_id.as_deref(),
                        existing_active_tid,
                        target.thread_id.as_deref(),
                        default_tid.as_deref(),
                    )
                    .then(|| (p, g.thread_id.clone()))
                })
            });

        let (pane, key_thread_id) = if let Some((pane, existing_tid)) = existing {
            self.change(true, cx, |l| {
                if let Some(tab) = l.tab_of_pane(pane) {
                    l.activate(tab);
                }
                l.focus(pane);
            });
            (pane, existing_tid)
        } else {
            let mut opened = None;
            self.change(true, cx, |l| {
                l.new_tab_after(l.tabs().len().saturating_sub(1));
                if let Some(pane) = l.focused() {
                    l.set_host(pane, host.clone());
                    l.set_group_pane(pane, Some(target.clone()));
                    opened = Some(pane);
                }
            });
            let Some(pane) = opened else {
                return;
            };
            (pane, target.thread_id)
        };

        let key = GroupRoomKey {
            host: host.clone(),
            group_id: group_id.to_owned(),
            thread_id: key_thread_id,
        };
        let entry = self.bot_ui.group_chat.rooms.entry(key.clone()).or_default();
        entry.focus_composer = ComposerFocus::Focus;
        let mut seen_ms = now_unix_ms();
        if let Some(summary) = self
            .sidebar_groups
            .iter()
            .find(|s| s.host == host && s.group.id == group_id)
        {
            entry.reply_audience = summary.reply_audience.clone();
            if let Some(msg) = &summary.latest_message {
                seen_ms = seen_ms.max(msg.created_at_ms);
            }
        }
        self.mark_group_seen(&host, group_id, seen_ms);
        let _ = pane;
        self.poll_group_room(key, cx);
        self.ensure_group_poll_timer(cx);
        cx.notify();
    }

    pub(in crate::shell) fn mark_group_seen(
        &mut self,
        host: &HostId,
        group_id: &str,
        seen_ms: u64,
    ) {
        let entry = self
            .bot_ui
            .group_chat
            .last_seen_ms
            .entry((host.clone(), group_id.to_owned()))
            .or_insert(0);
        if seen_ms > *entry {
            *entry = seen_ms;
        }
    }

    pub(in crate::shell) fn group_is_unread(
        &self,
        host: &HostId,
        group_id: &str,
        summary: &krill_proto::bots::BotGroupSummary,
    ) -> bool {
        let Some(latest_ms) = latest_unread_bot_message_ms(summary) else {
            return false;
        };
        let seen_ms = self
            .bot_ui
            .group_chat
            .last_seen_ms
            .get(&(host.clone(), group_id.to_owned()))
            .copied()
            .unwrap_or(0);
        latest_ms > seen_ms
    }

    /// Immediately repolls any visible group chat rooms belonging to reconnected/invalidated
    /// remote hosts (`BOT-42`).
    pub(in crate::shell) fn repoll_visible_group_rooms_for_hosts(
        &mut self,
        hosts: &[HostId],
        cx: &mut Context<Self>,
    ) {
        let visible: Vec<GroupRoomKey> = self
            .layout
            .visible()
            .into_iter()
            .filter_map(|p| {
                let host = self.layout.host(p);
                if !hosts.contains(host) {
                    return None;
                }
                let g = self.layout.group_pane(p)?;
                Some(GroupRoomKey {
                    host: host.clone(),
                    group_id: g.group_id.clone(),
                    thread_id: g.thread_id.clone(),
                })
            })
            .collect();
        for key in visible {
            if let Some(state) = self.bot_ui.group_chat.rooms.get_mut(&key) {
                state.loading = false;
                state.error = None;
                state.poll_gen = state.poll_gen.wrapping_add(1);
            }
            self.poll_group_room(key, cx);
        }
    }

    pub(in crate::shell) fn create_and_open_group_thread_on(
        &mut self,
        host: &HostId,
        group_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(host) else {
            return;
        };
        let host_owned = host.clone();
        let window_handle = window.window_handle();
        let gid = group_id.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::NewGroupThread {
                            group_id: gid,
                            title: None,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::GroupView(view)) => {
                        let newest = view.threads.first().map(|t| t.thread_id.clone());
                        let _ = window_handle;
                        this.open_bot_group_on(&host_owned, &group_id, newest.as_deref(), None, cx);
                        this.refresh_bots_for_host(&host_owned, true, cx);
                    }
                    Ok(_) => {}
                    Err(err) => this.notice = Some(err),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn ensure_group_poll_timer(&mut self, cx: &mut Context<Self>) {
        if self.bot_ui.group_chat.poll_timer.is_some() {
            return;
        }
        self.bot_ui.group_chat.poll_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(800))
                    .await;
                let keep = this
                    .update(cx, |this, cx| {
                        let visible: Vec<GroupRoomKey> = this
                            .layout
                            .visible()
                            .into_iter()
                            .filter_map(|p| {
                                let g = this.layout.group_pane(p)?;
                                Some(GroupRoomKey {
                                    host: this.layout.host(p).clone(),
                                    group_id: g.group_id.clone(),
                                    thread_id: g.thread_id.clone(),
                                })
                            })
                            .collect();
                        if visible.is_empty() {
                            return false;
                        }
                        let now = now_unix_ms();
                        for key in visible {
                            let active = this.bot_ui.group_chat.rooms.get(&key).is_some_and(|r| {
                                r.room.as_ref().is_some_and(|v| !v.active_turns.is_empty())
                                    || !r.member_items.is_empty()
                            });
                            let stale_ms = if active { 800 } else { 3000 };
                            let should_poll =
                                this.bot_ui.group_chat.rooms.get(&key).is_none_or(|r| {
                                    !r.loading && now.saturating_sub(r.last_poll_ms) >= stale_ms
                                });
                            if should_poll {
                                this.poll_group_room(key, cx);
                            }
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    this.update(cx, |this, _| {
                        this.bot_ui.group_chat.poll_timer = None;
                    })
                    .ok();
                    return;
                }
            }
        }));
    }

    fn active_group_member_sessions(
        &self,
        key: &GroupRoomKey,
        room: Option<&BotGroupRoomView>,
        cx: &gpui::App,
    ) -> Vec<SessionId> {
        let mut sessions: Vec<SessionId> = Vec::new();
        let active_tid = key
            .thread_id
            .as_deref()
            .or_else(|| room.and_then(|r| r.active_thread_id.as_deref()));
        if let Some(summary) = self
            .sidebar_groups
            .iter()
            .find(|s| s.host == key.host && s.group.id == key.group_id)
        {
            for t in &summary.active_turns {
                if active_tid.is_none_or(|tid| t.thread_id == tid)
                    && !sessions.contains(&t.session_id)
                {
                    sessions.push(t.session_id);
                }
            }
            let sidebar = self.sidebar.read(cx);
            for bid in &summary.group.bot_ids {
                if let Some(bot) = self
                    .sidebar_bots
                    .iter()
                    .find(|b| b.host == key.host && &b.id == bid)
                {
                    for s in &bot.sessions {
                        if !session_belongs_to_group(&s.label, &key.group_id, active_tid) {
                            continue;
                        }
                        let st = sidebar
                            .session_on(&key.host, s.session_id)
                            .map(|x| x.status.state);
                        if matches!(
                            st,
                            Some(AgentState::Working | AgentState::Waiting | AgentState::Blocked)
                        ) && !sessions.contains(&s.session_id)
                        {
                            sessions.push(s.session_id);
                        }
                    }
                }
            }
        }
        if let Some(room) = room {
            for t in &room.active_turns {
                if !sessions.contains(&t.session_id) {
                    sessions.push(t.session_id);
                }
            }
        }
        sessions
    }

    pub(in crate::shell) fn poll_group_room(&mut self, key: GroupRoomKey, cx: &mut Context<Self>) {
        let Some(rpc) = self.bot_rpc_on(&key.host) else {
            return;
        };
        let target_for_sessions = self.resolved_target_for_host(&key.host);
        let gid = key.group_id.clone();

        if let Some(state) = self.bot_ui.group_chat.rooms.get_mut(&key)
            && state.loading
        {
            state.repoll_needed = true;
            return;
        }
        // Identify live/waiting/active member sessions to inspect via ChatPoll for turns & ApprovalDock.
        let sessions_to_poll = self.active_group_member_sessions(
            &key,
            self.bot_ui
                .group_chat
                .rooms
                .get(&key)
                .and_then(|s| s.room.as_ref()),
            cx,
        );
        let state = self.bot_ui.group_chat.rooms.entry(key.clone()).or_default();
        let tid = resolved_thread_id(&key, Some(state));
        state.loading = true;
        state.repoll_needed = false;
        let poll_gen = state.poll_gen;
        state.last_poll_ms = now_unix_ms();

        cx.spawn(async move |this, cx| {
            let (room_res, chat_items_map) = cx
                .background_executor()
                .spawn(async move {
                    let room_res = call_checked(
                        &rpc,
                        &BotRpcRequest::ReadGroupRoom {
                            group_id: gid,
                            thread_id: tid,
                            before_message_id: None,
                            limit: Some(ROOM_PAGE_LIMIT),
                        },
                    );
                    let mut map: BTreeMap<SessionId, Vec<ChatItem>> = BTreeMap::new();
                    // Skip serial member ChatPoll calls when ReadGroupRoom already failed (`BOT-42`).
                    if room_res.is_ok()
                        && !sessions_to_poll.is_empty()
                        && !matches!(
                            target_for_sessions,
                            super::bot_section::ResolvedHostTarget::Offline
                        )
                    {
                        let call = krill_app_core::resources::daemon_call_for(
                            target_for_sessions.into_target(),
                        );
                        for sid in sessions_to_poll {
                            if let Ok(Reply::AgentHistory(Response::ChatPolled(snap))) =
                                call(&Request::AgentHistory(Action::ChatPoll {
                                    session: sid,
                                    offset: 0,
                                }))
                            {
                                map.insert(sid, snap.items);
                            }
                        }
                    }
                    (room_res, map)
                })
                .await;

            this.update(cx, |this, cx| {
                let Some(state) = this.bot_ui.group_chat.rooms.get_mut(&key) else {
                    return;
                };
                state.loading = false;
                let repoll = std::mem::take(&mut state.repoll_needed);
                if state.poll_gen != poll_gen {
                    if repoll {
                        this.poll_group_room(key, cx);
                    }
                    cx.notify();
                    return;
                }
                let had_room = state.room.is_some();
                let prev_tail = state.last_tail_message_id.clone().or_else(|| {
                    state
                        .room
                        .as_ref()
                        .and_then(|r| r.messages.last())
                        .map(|m| m.message_id.clone())
                });
                let prev_tail_sig = state.last_tail_sig.clone();
                let mut applied_snapshot = false;
                match room_res {
                    Ok(BotRpcResponse::GroupRoom(view)) => {
                        state.reply_audience = view.reply_audience.clone();
                        state.error = None;
                        let merged = merge_polled_room(state.room.as_ref(), view);
                        state.room = Some(merged);
                        applied_snapshot = true;
                    }
                    Ok(BotRpcResponse::GroupView(old)) => {
                        // Graceful fallback if daemon predates ReadGroupRoom.
                        state.error = None;
                        let active_thread_id = old.threads.first().map(|t| t.thread_id.clone());
                        let fallback = BotGroupRoomView {
                            group: old.group,
                            reply_audience: state.reply_audience.clone(),
                            threads: old.threads,
                            active_thread_id,
                            messages: old.messages,
                            has_earlier: false,
                            active_turns: Vec::new(),
                        };
                        let merged = merge_polled_room(state.room.as_ref(), fallback);
                        state.room = Some(merged);
                        applied_snapshot = true;
                    }
                    Ok(_) => {}
                    Err(err) => state.error = Some(err),
                }
                if applied_snapshot {
                    if let Some(rv) = &state.room {
                        if rv.active_turns.is_empty() {
                            state.stopping_all = false;
                            state.stopping_bots.clear();
                        } else {
                            state
                                .stopping_bots
                                .retain(|bid| rv.active_turns.iter().any(|t| &t.bot_id == bid));
                        }
                    }
                    // Drop cached member items for sessions that are no longer active so the room
                    // returns to idle polling cadence (3s) and never surfaces stale prompts.
                    let active_sids: HashSet<SessionId> = this
                        .active_group_member_sessions(
                            &key,
                            this.bot_ui
                                .group_chat
                                .rooms
                                .get(&key)
                                .and_then(|r| r.room.as_ref()),
                            cx,
                        )
                        .into_iter()
                        .collect();
                    let is_focused = this.layout.focused().is_some_and(|p| {
                        this.layout.host(p) == &key.host
                            && this
                                .layout
                                .group_pane(p)
                                .is_some_and(|g| g.group_id == key.group_id)
                    });
                    let mut focused_seen_ms = None;
                    if let Some(state) = this.bot_ui.group_chat.rooms.get_mut(&key) {
                        for (sid, items) in chat_items_map {
                            if active_sids.contains(&sid) {
                                state.member_items.insert(sid, items);
                            }
                        }
                        state
                            .member_items
                            .retain(|sid, _| active_sids.contains(sid));
                        let next_tail_msg = state.room.as_ref().and_then(|r| r.messages.last());
                        let next_tail = next_tail_msg.map(|m| m.message_id.clone());
                        let live_preview_len = state
                            .room
                            .as_ref()
                            .and_then(|r| r.active_turns.last())
                            .and_then(|t| state.member_items.get(&t.session_id))
                            .and_then(|items| {
                                items.iter().rev().find(|i| !i.text.trim().is_empty())
                            })
                            .map_or(0, |i| i.title.len() + i.text.len());
                        let next_tail_sig = next_tail_msg
                            .map(|m| (m.message_id.clone(), m.body.len(), live_preview_len));
                        if is_focused && let Some(m) = next_tail_msg {
                            focused_seen_ms = Some(m.created_at_ms);
                        }
                        if !had_room {
                            state.follow_paused = false;
                            state.initial_scrolled.set(true);
                            state.scroll.scroll_to_bottom();
                        } else if (next_tail != prev_tail || next_tail_sig != prev_tail_sig)
                            && next_tail.is_some()
                        {
                            if !state.follow_paused || is_group_timeline_near_bottom(&state.scroll)
                            {
                                state.follow_paused = false;
                                state.scroll.scroll_to_bottom();
                            } else {
                                state.follow_paused = true;
                            }
                        }
                        state.last_tail_message_id = next_tail;
                        state.last_tail_sig = next_tail_sig;
                    }
                    if let Some(seen_ms) = focused_seen_ms {
                        this.mark_group_seen(&key.host, &key.group_id, seen_ms);
                    }
                }
                if repoll {
                    this.poll_group_room(key, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn load_earlier_group_messages(
        &mut self,
        key: GroupRoomKey,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(&key.host) else {
            return;
        };
        let Some(state) = self.bot_ui.group_chat.rooms.get_mut(&key) else {
            return;
        };
        if state.loading_earlier {
            return;
        }
        let Some(anchor) = state
            .room
            .as_ref()
            .and_then(|r| r.messages.first())
            .map(|m| m.message_id.clone())
        else {
            return;
        };
        state.loading_earlier = true;
        state.action_error = None;
        let gid = key.group_id.clone();
        let tid = resolved_thread_id(&key, Some(state));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::ReadGroupRoom {
                            group_id: gid,
                            thread_id: tid,
                            before_message_id: Some(anchor),
                            limit: Some(ROOM_PAGE_LIMIT),
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(state) = this.bot_ui.group_chat.rooms.get_mut(&key) else {
                    return;
                };
                state.loading_earlier = false;
                match res {
                    Ok(BotRpcResponse::GroupRoom(earlier)) => {
                        if let Some(room) = &mut state.room {
                            let same_thread = room
                                .active_thread_id
                                .as_deref()
                                .zip(earlier.active_thread_id.as_deref())
                                .is_none_or(|(a, b)| a == b);
                            if same_thread {
                                let existing_ids: HashSet<&str> = room
                                    .messages
                                    .iter()
                                    .map(|m| m.message_id.as_str())
                                    .collect();
                                let mut merged: Vec<BotGroupMessage> = earlier
                                    .messages
                                    .into_iter()
                                    .filter(|m| !existing_ids.contains(m.message_id.as_str()))
                                    .collect();
                                merged.append(&mut room.messages);
                                room.messages = merged;
                                room.has_earlier = earlier.has_earlier;
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        state.action_error = Some(err);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn ensure_group_composer(
        &mut self,
        key: &GroupRoomKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        let entry = self.bot_ui.group_chat.rooms.entry(key.clone()).or_default();
        if let Some((input, _)) = &entry.composer {
            let input = input.clone();
            let focus_now = std::mem::take(&mut entry.focus_composer) == ComposerFocus::Focus;
            krill_app_core::i18n::sync_textarea_placeholder(
                &input,
                text::GRP_COMPOSER_HINT.get(),
                window,
                cx,
            );
            if focus_now {
                input.update(cx, |s, cx| s.focus(window, cx));
            }
            return input;
        }
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .submit_on_enter(true)
                .placeholder(text::GRP_COMPOSER_HINT.get())
        });
        let sub_key = key.clone();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, state, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => {
                    let val = state.read(cx).value().to_string();
                    let cursor = state.read(cx).cursor();
                    let members = this.group_members_for_key(&sub_key);
                    let active_at =
                        mention_query_at_cursor(&val, cursor, &members).map(|(at, _, _)| at);
                    if let Some(room) = this.bot_ui.group_chat.rooms.get_mut(&sub_key) {
                        room.draft = val;
                        if room.mention_dismissed_at != active_at {
                            room.mention_dismissed_at = None;
                        }
                        room.mention_selected = 0;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                } => {
                    if Self::chat_input_is_composing(state, window, cx) {
                        return;
                    }
                    if this.accept_active_group_mention(&sub_key, state, window, cx) {
                        return;
                    }
                    this.send_group_composer_message(sub_key.clone(), window, cx);
                }
                _ => {}
            },
        );
        let focus_now = std::mem::take(&mut entry.focus_composer) == ComposerFocus::Focus;
        if focus_now {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
        entry.composer = Some((input.clone(), sub));
        input
    }

    fn group_members_for_key(&self, key: &GroupRoomKey) -> Vec<SidebarBot> {
        let group = self
            .bot_ui
            .group_chat
            .rooms
            .get(key)
            .and_then(|r| r.room.as_ref())
            .map(|rv| &rv.group)
            .or_else(|| {
                self.sidebar_groups
                    .iter()
                    .find(|s| s.host == key.host && s.group.id == key.group_id)
                    .map(|s| &s.group)
            });
        group
            .map(|g| {
                g.bot_ids
                    .iter()
                    .filter_map(|id| {
                        self.sidebar_bots
                            .iter()
                            .find(|b| b.host == key.host && &b.id == id)
                            .cloned()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn active_group_mention(
        &self,
        key: &GroupRoomKey,
        draft: &str,
        cursor: usize,
        members: &[SidebarBot],
    ) -> Option<(usize, usize, Vec<MentionCandidate>)> {
        let (at_pos, cursor_byte, query) = mention_query_at_cursor(draft, cursor, members)?;
        if self
            .bot_ui
            .group_chat
            .rooms
            .get(key)
            .and_then(|r| r.mention_dismissed_at)
            == Some(at_pos)
        {
            return None;
        }
        let candidates = mention_candidates(&query, members);
        if candidates.is_empty() {
            return None;
        }
        Some((at_pos, cursor_byte, candidates))
    }

    fn apply_group_mention(
        &mut self,
        key: &GroupRoomKey,
        input: &Entity<TextareaState>,
        span: std::ops::Range<usize>,
        handle: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = input.read(cx).value().to_string();
        let (next, new_cursor_byte) = splice_mention_token(&draft, span.start, span.end, handle);
        let pos = byte_offset_to_position(&next, new_cursor_byte);
        input.update(cx, |s, cx| {
            s.set_value(next.clone(), window, cx);
            s.set_cursor_position(pos, window, cx);
            s.focus(window, cx);
        });
        if let Some(r) = self.bot_ui.group_chat.rooms.get_mut(key) {
            r.draft = next;
            r.mention_selected = 0;
            r.mention_dismissed_at = None;
        }
        cx.notify();
    }

    fn accept_active_group_mention(
        &mut self,
        key: &GroupRoomKey,
        input: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let draft = input.read(cx).value().to_string();
        let cursor = input.read(cx).cursor();
        let members = self.group_members_for_key(key);
        let Some((at_pos, cursor_byte, candidates)) =
            self.active_group_mention(key, &draft, cursor, &members)
        else {
            return false;
        };
        let selected = self
            .bot_ui
            .group_chat
            .rooms
            .get(key)
            .map_or(0, |r| r.mention_selected)
            .min(candidates.len().saturating_sub(1));
        let handle = candidates[selected].handle.clone();
        self.apply_group_mention(key, input, at_pos..cursor_byte, &handle, window, cx);
        true
    }

    fn prepend_reply_mention(
        &mut self,
        key: &GroupRoomKey,
        handle: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.ensure_group_composer(key, window, cx);
        let mention = format!("@{handle} ");
        let current = input.read(cx).value().to_string();
        let next = if current
            .to_ascii_lowercase()
            .starts_with(&mention.to_ascii_lowercase())
        {
            current
        } else {
            format!("{mention}{current}")
        };
        input.update(cx, |s, cx| {
            s.set_value(next.clone(), window, cx);
            s.focus(window, cx);
        });
        if let Some(room) = self.bot_ui.group_chat.rooms.get_mut(key) {
            room.draft = next;
        }
        cx.notify();
    }

    pub(super) fn set_group_reply_audience(
        &mut self,
        key: GroupRoomKey,
        audience: BotGroupReplyAudience,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(&key.host) else {
            return;
        };
        let state = self.bot_ui.group_chat.rooms.entry(key.clone()).or_default();
        state.reply_audience = audience.clone();
        state.action_error = None;
        state.poll_gen = state.poll_gen.wrapping_add(1);
        if let Some(s) = self
            .sidebar_groups
            .iter_mut()
            .find(|s| s.host == key.host && s.group.id == key.group_id)
        {
            s.reply_audience = audience.clone();
        }
        cx.notify();
        let gid = key.group_id.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::SetGroupReplyAudience {
                            group_id: gid,
                            audience,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if let Err(err) = res
                    && let Some(room) = this.bot_ui.group_chat.rooms.get_mut(&key)
                {
                    room.action_error = Some(err);
                    room.poll_gen = room.poll_gen.wrapping_add(1);
                    this.poll_group_room(key, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn send_group_composer_message(
        &mut self,
        key: GroupRoomKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(&key.host) else {
            return;
        };
        let Some(state) = self.bot_ui.group_chat.rooms.get_mut(&key) else {
            return;
        };
        let text_body = state.draft.trim().to_owned();
        if text_body.is_empty() || state.sending {
            return;
        }
        let audience = state.reply_audience.clone();
        let input = state.composer.as_ref().map(|(c, _)| c.clone());
        let sent_draft = state.draft.clone();
        state.sending = true;
        state.action_error = None;
        let gid = key.group_id.clone();
        let tid = resolved_thread_id(&key, Some(state));

        // Intercept `/clear` as a local group-thread reset (`BOT-05`) rather than posting literal
        // `"/clear"` as an owner message to member bots.
        if text_body.eq_ignore_ascii_case("/clear") {
            cx.notify();
            cx.spawn_in(window, async move |this, cx| {
                let res = cx
                    .background_executor()
                    .spawn(async move {
                        call_checked(
                            &rpc,
                            &BotRpcRequest::ClearGroupThread {
                                group_id: gid,
                                thread_id: tid,
                            },
                        )
                    })
                    .await;
                this.update_in(cx, |this, window, cx| {
                    let mut clear_input = false;
                    let mut new_thread_id: Option<String> = None;
                    if let Some(room_state) = this.bot_ui.group_chat.rooms.get_mut(&key) {
                        room_state.sending = false;
                        room_state.poll_gen = room_state.poll_gen.wrapping_add(1);
                        match res {
                            Ok(BotRpcResponse::GroupRoom(view)) => {
                                if room_state.draft == sent_draft {
                                    room_state.draft.clear();
                                    clear_input = true;
                                }
                                new_thread_id.clone_from(&view.active_thread_id);
                                room_state.reply_audience = view.reply_audience.clone();
                                room_state.room = Some(view);
                                room_state.expanded_turns.clear();
                                room_state.member_items.clear();
                                room_state.stopping_all = false;
                                room_state.stopping_bots.clear();
                                room_state.follow_paused = false;
                                room_state.last_tail_message_id = None;
                                room_state.last_tail_sig = None;
                                room_state.initial_scrolled.set(false);
                                room_state.error = None;
                            }
                            Ok(_) => {
                                if room_state.draft == sent_draft {
                                    room_state.draft.clear();
                                    clear_input = true;
                                }
                            }
                            Err(err) => {
                                room_state.action_error =
                                    Some(text::GRP_SEND_FAILED.format(&[("err", &err)]));
                            }
                        }
                    }
                    if clear_input && let Some(inp) = input {
                        inp.update(cx, |s, cx| s.set_value("", window, cx));
                    }
                    let host = key.host.clone();
                    let next_key = if let Some(ref fresh_tid) = new_thread_id
                        && key.thread_id.is_some()
                        && key.thread_id.as_deref() != Some(fresh_tid.as_str())
                    {
                        let target_gid = key.group_id.clone();
                        let fresh_tid_owned = fresh_tid.clone();
                        this.change(true, cx, |l| {
                            let panes: Vec<PaneId> =
                                (0..l.tabs().len()).flat_map(|i| l.tab_panes(i)).collect();
                            for p in panes {
                                if l.host(p) == &key.host
                                    && l.group_pane(p).is_some_and(|g| {
                                        g.group_id == target_gid && g.thread_id == key.thread_id
                                    })
                                {
                                    l.set_group_pane(
                                        p,
                                        Some(GroupPaneTarget {
                                            group_id: target_gid.clone(),
                                            thread_id: Some(fresh_tid_owned.clone()),
                                        }),
                                    );
                                }
                            }
                        });
                        GroupRoomKey {
                            host: key.host.clone(),
                            group_id: key.group_id.clone(),
                            thread_id: Some(fresh_tid.clone()),
                        }
                    } else {
                        key.clone()
                    };
                    this.poll_group_room(next_key, cx);
                    this.refresh_bots_for_host(&host, true, cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }

        let voice_text = text_body.clone();
        self.notify_voice_sent(
            &super::voice::VoiceComposerTarget::Group(key.clone()),
            &voice_text,
            cx,
        );
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::SendGroupMessageWithAudience {
                            group_id: gid,
                            thread_id: tid,
                            sender: "owner".to_owned(),
                            text: text_body,
                            audience,
                        },
                    )
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                let mut clear_input = false;
                if let Some(room_state) = this.bot_ui.group_chat.rooms.get_mut(&key) {
                    room_state.sending = false;
                    room_state.poll_gen = room_state.poll_gen.wrapping_add(1);
                    match res {
                        Ok(BotRpcResponse::GroupMessagePosted { message, .. }) => {
                            if room_state.draft == sent_draft {
                                room_state.draft.clear();
                                clear_input = true;
                            }
                            if let Some(room) = &mut room_state.room {
                                room.active_thread_id
                                    .get_or_insert_with(|| message.thread_id.clone());
                                let posted_id = message.message_id.clone();
                                if !room
                                    .messages
                                    .iter()
                                    .any(|m| m.message_id == message.message_id)
                                {
                                    room.messages.push(message);
                                }
                                room_state.follow_paused = false;
                                room_state.last_tail_message_id = Some(posted_id);
                                room_state.initial_scrolled.set(true);
                                room_state.scroll.scroll_to_bottom();
                            }
                        }
                        Ok(_) => {
                            if room_state.draft == sent_draft {
                                room_state.draft.clear();
                                clear_input = true;
                            }
                        }
                        Err(err) => {
                            room_state.action_error =
                                Some(text::GRP_SEND_FAILED.format(&[("err", &err)]));
                        }
                    }
                }
                if clear_input && let Some(inp) = input {
                    inp.update(cx, |s, cx| s.set_value("", window, cx));
                }
                let host = key.host.clone();
                this.poll_group_room(key, cx);
                this.refresh_bots_for_host(&host, true, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn stop_group_turn(
        &mut self,
        key: GroupRoomKey,
        bot_id: Option<BotId>,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(&key.host) else {
            return;
        };
        let tid = resolved_thread_id(&key, self.bot_ui.group_chat.rooms.get(&key));
        if let Some(room) = self.bot_ui.group_chat.rooms.get_mut(&key) {
            // Cooldown / deduplicate rapid clicks & Esc repeats while a stop RPC is in flight (`BOT-04`/`CHAT-09`).
            if room.stopping_all {
                return;
            }
            if let Some(ref bid) = bot_id {
                if !room.stopping_bots.insert(bid.clone()) {
                    return;
                }
            } else {
                room.stopping_all = true;
            }
            room.action_error = None;
            room.poll_gen = room.poll_gen.wrapping_add(1);
        }
        let gid = key.group_id.clone();
        let stop_target = bot_id.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::StopGroupMember {
                            group_id: gid,
                            thread_id: tid,
                            bot_id,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(room) = this.bot_ui.group_chat.rooms.get_mut(&key) {
                    if let Some(ref bid) = stop_target {
                        room.stopping_bots.remove(bid);
                    } else {
                        room.stopping_all = false;
                    }
                    room.poll_gen = room.poll_gen.wrapping_add(1);
                    if let Err(err) = res {
                        room.action_error = Some(err);
                    }
                }
                let host = key.host.clone();
                this.poll_group_room(key, cx);
                this.refresh_bots_for_host(&host, true, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn send_group_approval_input(
        &mut self,
        key: GroupRoomKey,
        session: SessionId,
        raw: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.bot_ui.group_chat.rooms.get_mut(&key) {
            state.action_error = None;
        }
        let resolved = self.resolved_target_for_host(&key.host);
        if matches!(resolved, super::bot_section::ResolvedHostTarget::Offline) {
            if let Some(state) = self.bot_ui.group_chat.rooms.get_mut(&key) {
                state.action_error = Some(
                    krill_app_core::i18n::catalog::sidebar::HOST_DISCONNECTED
                        .get()
                        .to_owned(),
                );
            }
            cx.notify();
            return;
        }
        let target = resolved.into_target();
        let bytes = raw.as_bytes().to_vec();
        cx.spawn(async move |this, cx| {
            let err = cx
                .background_executor()
                .spawn(async move {
                    let call = krill_app_core::resources::daemon_call_for(target);
                    match call(&Request::SendInput {
                        session,
                        bytes,
                        submit: false,
                        wait_submit: false,
                    }) {
                        Ok(Reply::Failed(failure)) => Some(failure.to_string()),
                        Ok(_) => None,
                        Err(err) => Some(err.to_string()),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(state) = this.bot_ui.group_chat.rooms.get_mut(&key) {
                    state.poll_gen = state.poll_gen.wrapping_add(1);
                    if let Some(err) = err {
                        state.action_error = Some(err);
                    }
                }
                let host = key.host.clone();
                this.poll_group_room(key, cx);
                this.refresh_bots_for_host(&host, true, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn collect_member_prompts(
        &self,
        key: &GroupRoomKey,
        members: &[SidebarBot],
        cx: &gpui::App,
    ) -> Vec<MemberPromptState> {
        let mut prompts = Vec::new();
        let mut seen_bots = HashSet::new();
        let sidebar = self.sidebar.read(cx);
        let room_state = self.bot_ui.group_chat.rooms.get(key);

        for bot in members {
            let mut waiting_sid: Option<(SessionId, AgentState)> = None;
            if let Some(room) = room_state.and_then(|r| r.room.as_ref()) {
                for t in &room.active_turns {
                    if t.bot_id == bot.id
                        && let Some(sess) = sidebar.session_on(&key.host, t.session_id)
                        && matches!(sess.status.state, AgentState::Waiting | AgentState::Blocked)
                    {
                        waiting_sid = Some((t.session_id, sess.status.state));
                        break;
                    }
                }
            }
            if waiting_sid.is_none() {
                let active_tid = key.thread_id.as_deref().or_else(|| {
                    room_state
                        .and_then(|r| r.room.as_ref())
                        .and_then(|rv| rv.active_thread_id.as_deref())
                });
                for s in &bot.sessions {
                    if !session_belongs_to_group(&s.label, &key.group_id, active_tid) {
                        continue;
                    }
                    if let Some(sess) = sidebar.session_on(&key.host, s.session_id)
                        && matches!(sess.status.state, AgentState::Waiting | AgentState::Blocked)
                    {
                        waiting_sid = Some((s.session_id, sess.status.state));
                        break;
                    }
                }
            }
            if let Some((session_id, state)) = waiting_sid
                && seen_bots.insert(bot.id.clone())
            {
                let item = room_state
                    .and_then(|r| r.member_items.get(&session_id))
                    .and_then(|items| {
                        items
                            .iter()
                            .rev()
                            .find(|i| {
                                matches!(i.kind, ChatItemKind::Question | ChatItemKind::Permission)
                            })
                            .cloned()
                    })
                    .or_else(|| {
                        self.chat
                            .logs
                            .get(&(key.host.clone(), session_id))
                            .and_then(|log| {
                                log.items
                                    .iter()
                                    .rev()
                                    .find(|i| {
                                        matches!(
                                            i.kind,
                                            ChatItemKind::Question | ChatItemKind::Permission
                                        )
                                    })
                                    .map(|arc| (**arc).clone())
                            })
                    });
                prompts.push(MemberPromptState {
                    bot: bot.clone(),
                    session_id,
                    state,
                    item,
                });
            }
        }
        prompts
    }

    pub(super) fn render_group_chat_pane(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(target) = self.layout.group_pane(pane).cloned() else {
            return div().into_any_element();
        };
        let host = self.layout.host(pane).clone();
        let key = GroupRoomKey {
            host: host.clone(),
            group_id: target.group_id.clone(),
            thread_id: target.thread_id.clone(),
        };
        self.ensure_group_poll_timer(cx);
        let should_seed = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .is_none_or(|r| !r.loading && r.room.is_none() && r.last_poll_ms == 0);
        if should_seed {
            self.poll_group_room(key.clone(), cx);
        }

        let input_state = self.ensure_group_composer(&key, window, cx);
        let summary = self
            .sidebar_groups
            .iter()
            .find(|s| s.host == key.host && s.group.id == key.group_id)
            .cloned();
        let group = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .and_then(|r| r.room.as_ref().map(|v| v.group.clone()))
            .or_else(|| summary.as_ref().map(|s| s.group.clone()));

        let members: Vec<SidebarBot> = group
            .as_ref()
            .map(|g| {
                g.bot_ids
                    .iter()
                    .filter_map(|id| {
                        self.sidebar_bots
                            .iter()
                            .find(|b| b.host == key.host && &b.id == id)
                            .cloned()
                    })
                    .collect()
            })
            .unwrap_or_default();

        let p = ui::palette(cx);
        let room_snapshot = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .and_then(|r| r.room.clone());
        let audience = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .map(|r| r.reply_audience.clone())
            .or_else(|| summary.as_ref().map(|s| s.reply_audience.clone()))
            .unwrap_or_default();
        let draft = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .map_or_else(String::new, |r| r.draft.clone());
        let sending = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .is_some_and(|r| r.sending);
        let loading_earlier = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .is_some_and(|r| r.loading_earlier);
        let poll_error = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .and_then(|r| r.error.clone());
        let action_error = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .and_then(|r| r.action_error.clone());
        let dismissible_action_error = action_error.is_some();
        let error_msg = action_error.or(poll_error);
        let expanded_turns = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .map(|r| r.expanded_turns.clone())
            .unwrap_or_default();

        let active_turns: Vec<BotGroupTurnInfo> = room_snapshot
            .as_ref()
            .map(|v| v.active_turns.clone())
            .or_else(|| summary.as_ref().map(|s| s.active_turns.clone()))
            .unwrap_or_default();
        let active_run = !active_turns.is_empty()
            || room_snapshot
                .as_ref()
                .is_some_and(|v| v.messages.iter().any(|m| m.in_progress));

        let prompts = self.collect_member_prompts(&key, &members, cx);

        let header = self.render_group_room_subheader(
            &key,
            group.as_ref(),
            room_snapshot.as_ref(),
            &members,
            &audience,
            p,
            cx,
        );
        let body = self.render_group_timeline(
            &key,
            group.as_ref(),
            room_snapshot.as_ref(),
            &members,
            &active_turns,
            &expanded_turns,
            loading_earlier,
            p,
            cx,
        );
        let approval_dock = self.render_group_approval_dock(&key, &prompts, p, cx);
        let composer = self.render_group_composer(
            pane,
            &key,
            &input_state,
            &members,
            &audience,
            &draft,
            sending,
            active_run,
            p,
            cx,
        );

        let offline_remote_host: Option<(String, String)> = key.host.remote_id().and_then(|rid| {
            let offline = self.bot_rpc_on(&key.host).is_none()
                || self.host_statuses.iter().any(|(spec, status)| {
                    spec.id == rid
                        && matches!(
                            status,
                            krill_app_core::hosts::HostStatus::Disconnected
                                | krill_app_core::hosts::HostStatus::AuthFailed
                        )
                });
            offline.then(|| {
                (
                    rid.to_owned(),
                    self.host_display_label(&key.host, cx).to_string(),
                )
            })
        });

        div()
            .id(SharedString::from(format!("bot-group-page-{}", pane.get())))
            .debug_selector(|| "bot-group-page".into())
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(p.chat_canvas())
            .text_color(p.foreground)
            .flex()
            .flex_col()
            .font_family(ui::FONT_UI)
            .child(header)
            .children(offline_remote_host.map(|(remote_id, host_label)| {
                div()
                    .id("group-remote-offline-banner")
                    .debug_selector(|| "group-remote-offline-banner".into())
                    .mx_4()
                    .mt_2()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.warning.opacity(0.45))
                    .bg(p.warning.opacity(0.1))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.foreground)
                    .child(div().flex_1().min_w_0().truncate().child(format!(
                        "{host_label} · {}",
                        krill_app_core::i18n::catalog::sidebar::HOST_DISCONNECTED.get()
                    )))
                    .child(
                        Button::new("group-remote-reconnect")
                            .debug_selector(|| "group-remote-reconnect".into())
                            .ghost()
                            .xsmall()
                            .label(krill_app_core::i18n::catalog::sidebar::HOST_RECONNECT.get())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.sync_hosts_after_reconnect(Some(remote_id.clone()), cx);
                            })),
                    )
            }))
            .children(error_msg.map(|err| {
                let dismiss_key = key.clone();
                div()
                    .mx_4()
                    .mt_2()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.destructive.opacity(0.4))
                    .bg(p.destructive.opacity(0.1))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.destructive)
                    .child(div().flex_1().min_w_0().child(err))
                    .when(dismissible_action_error, |d| {
                        d.child(
                            Button::new("group-action-error-dismiss")
                                .debug_selector(|| "group-action-error-dismiss".into())
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(room) =
                                        this.bot_ui.group_chat.rooms.get_mut(&dismiss_key)
                                    {
                                        room.action_error = None;
                                    }
                                    cx.notify();
                                })),
                        )
                    })
            }))
            .child(body)
            .children(approval_dock)
            .child(composer)
            .into_any_element()
    }

    #[expect(clippy::too_many_arguments, reason = "subheader chrome inputs")]
    fn render_group_room_subheader(
        &self,
        key: &GroupRoomKey,
        group: Option<&krill_proto::bots::BotGroupChat>,
        room: Option<&BotGroupRoomView>,
        members: &[SidebarBot],
        audience: &BotGroupReplyAudience,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let title = group.map_or_else(|| key.group_id.clone(), |g| g.name.clone());
        let member_ids: Vec<BotId> = group.map_or_else(Vec::new, |g| g.bot_ids.clone());
        let host_bots: Vec<SidebarBot> = self
            .sidebar_bots
            .iter()
            .filter(|b| b.host == key.host)
            .cloned()
            .collect();
        let face_members = group_faces::resolve_members(&member_ids, &host_bots, &|id| {
            self.bot_ui
                .avatars
                .get(&(key.host.clone(), id.clone()))
                .and_then(|(_, img)| img.clone())
        });
        let faces = group_faces::render_group_faces(&face_members, 28., p.chat_canvas(), p);
        let threads = room.map_or(&[][..], |r| r.threads.as_slice());
        let active_tid = room
            .and_then(|r| r.active_thread_id.as_deref())
            .or(key.thread_id.as_deref());
        let group_clone = group.cloned();
        let aud_clone = audience.clone();
        let host_thread = key.host.clone();
        let host_manage = key.host.clone();
        let gid_thread = key.group_id.clone();

        let mut thread_strip = div().flex().items_center().gap_1().overflow_hidden();
        if threads.len() > 1 {
            for thr in threads.iter().take(4) {
                let selected = active_tid == Some(thr.thread_id.as_str());
                let label = if thr.title.trim().is_empty() || thr.title == "Main thread" {
                    text::GRP_THREAD_MAIN.get().to_owned()
                } else {
                    thr.title.clone()
                };
                let open_host = key.host.clone();
                let gid = key.group_id.clone();
                let tid = thr.thread_id.clone();
                thread_strip = thread_strip.child(
                    Button::new(SharedString::from(format!("grp-thr-{}", thr.thread_id)))
                        .ghost()
                        .xsmall()
                        .toggled(selected)
                        .label(label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_bot_group_on(&open_host, &gid, Some(&tid), Some(window), cx);
                        })),
                );
            }
        }

        div()
            .flex_none()
            .h(px(44.))
            .px_4()
            .border_b_1()
            .border_color(p.border)
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .child(faces)
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui::TEXT_MD)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(
                                        text::GRP_PANEL_MEMBERS
                                            .format(&[("n", &members.len().to_string())]),
                                    ),
                            )
                            .when(!key.host.is_local(), |d| {
                                let label = self.host_display_label(&key.host, cx);
                                d.child(super::bot_section::render_remote_host_badge(
                                    format!("group-page-host-{}", key.host.as_key()),
                                    "group-page-host".to_owned(),
                                    label,
                                    p,
                                ))
                            }),
                    )
                    .child(thread_strip),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("group-page-new-thread")
                            .debug_selector(|| "group-page-new-thread".into())
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip(
                                text::GRP_NEW_THREAD
                                    .format(&[("name", group.map_or("", |g| g.name.as_str()))]),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.create_and_open_group_thread_on(
                                    &host_thread,
                                    gid_thread.clone(),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .when_some(group_clone, |d, g| {
                        let host_manage = host_manage.clone();
                        d.child(
                            Button::new("group-page-manage-members")
                                .debug_selector(|| "group-page-manage-members".into())
                                .ghost()
                                .xsmall()
                                .icon(IconName::Settings)
                                .tooltip(text::GRP_MANAGE_MEMBERS.get())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_manage_group_dialog_on(
                                        &host_manage,
                                        g.clone(),
                                        aud_clone.clone(),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                    }),
            )
    }

    #[expect(clippy::too_many_arguments, reason = "group timeline render")]
    fn render_group_timeline(
        &self,
        key: &GroupRoomKey,
        group: Option<&krill_proto::bots::BotGroupChat>,
        room: Option<&BotGroupRoomView>,
        members: &[SidebarBot],
        active_turns: &[BotGroupTurnInfo],
        expanded_turns: &HashSet<String>,
        loading_earlier: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let messages = room.map_or(&[][..], |r| r.messages.as_slice());
        let has_earlier = room.is_some_and(|r| r.has_earlier);

        if messages.is_empty() {
            let member_ids: Vec<BotId> = group.map_or_else(Vec::new, |g| g.bot_ids.clone());
            let host_bots: Vec<SidebarBot> = self
                .sidebar_bots
                .iter()
                .filter(|b| b.host == key.host)
                .cloned()
                .collect();
            let face_members = group_faces::resolve_members(&member_ids, &host_bots, &|id| {
                self.bot_ui
                    .avatars
                    .get(&(key.host.clone(), id.clone()))
                    .and_then(|(_, img)| img.clone())
            });
            let cluster = group_faces::render_group_faces(&face_members, 72., p.background, p);
            let title = group.map_or_else(|| key.group_id.clone(), |g| g.name.clone());
            return div()
                .id("bot-group-empty-state")
                .debug_selector(|| "bot-group-empty-state".into())
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .px_6()
                .gap_3()
                .child(cluster)
                .child(
                    div()
                        .text_size(ui::TEXT_LG)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .max_w(px(360.))
                        .text_center()
                        .text_size(ui::TEXT_SM)
                        .text_color(p.muted_foreground)
                        .child(text::GRP_EMPTY_HINT.get()),
                )
                .into_any_element();
        }

        let mut list = div()
            .w_full()
            .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
            .mx_auto()
            .flex()
            .flex_col()
            .gap_4();

        if has_earlier {
            let load_key = key.clone();
            list = list.child(
                div().w_full().flex().justify_center().child(
                    Button::new("group-load-earlier")
                        .debug_selector(|| "group-load-earlier".into())
                        .ghost()
                        .xsmall()
                        .disabled(loading_earlier)
                        .label(if loading_earlier {
                            text::GRP_LOADING.get()
                        } else {
                            text::GRP_LOAD_EARLIER.get()
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.load_earlier_group_messages(load_key.clone(), cx);
                        })),
                ),
            );
        }

        let now = now_unix_ms();
        let member_items_map = self
            .bot_ui
            .group_chat
            .rooms
            .get(key)
            .map(|r| &r.member_items);

        let mut last_bot_sender: Option<(String, &[BotId])> = None;
        for msg in messages {
            if msg.sender_id == "owner" || msg.sender_id == "user" {
                last_bot_sender = None;
                list = list.child(Self::render_owner_message(msg, now, p));
            } else {
                let to_name = members
                    .iter()
                    .find(|b| b.id.as_str() == msg.sender_id)
                    .map_or(msg.sender_name.as_str(), |b| b.name.as_str());
                if let Some((ref from_name, prev_mentions)) = last_bot_sender
                    && prev_mentions.iter().any(|id| id.as_str() == msg.sender_id)
                {
                    list = list.child(
                        div()
                            .id(SharedString::from(format!("grp-peer-{}", msg.message_id)))
                            .debug_selector(|| "group-peer-request".into())
                            .w_full()
                            .text_center()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(
                                text::GRP_PEER_SAID_ONE
                                    .format(&[("from", from_name), ("to", to_name)]),
                            ),
                    );
                }
                let turn = active_turns.iter().find(|t| {
                    t.message_id == msg.message_id
                        || t.bot_id.as_str() == msg.sender_id && msg.in_progress
                });
                let live_preview = turn.and_then(|t| {
                    member_items_map
                        .and_then(|m| m.get(&t.session_id))
                        .and_then(|items| {
                            items
                                .iter()
                                .rev()
                                .find(|i| !i.text.trim().is_empty())
                                .map(|i| {
                                    if i.title.is_empty() {
                                        i.text.clone()
                                    } else {
                                        format!("{}: {}", i.title, i.text)
                                    }
                                })
                        })
                });
                let expanded = expanded_turns.contains(&msg.message_id);
                list = list.child(self.render_bot_turn(
                    key,
                    msg,
                    turn,
                    members,
                    live_preview.as_deref(),
                    expanded,
                    now,
                    p,
                    cx,
                ));
                last_bot_sender = Some((to_name.to_owned(), msg.mentions.as_slice()));
            }
        }

        let scroll = self.bot_ui.group_chat.rooms.get(key).map(|r| {
            if !r.initial_scrolled.get()
                && r.room.as_ref().is_some_and(|rv| !rv.messages.is_empty())
            {
                r.initial_scrolled.set(true);
                r.scroll.scroll_to_bottom();
            }
            r.scroll.clone()
        });

        let wheel_key = key.clone();
        let dismiss_key = key.clone();
        let mut scroller = div()
            .id("bot-group-timeline")
            .debug_selector(|| "bot-group-timeline".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_4()
            .py_4()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let members = this.group_members_for_key(&dismiss_key);
                    let active_at = this
                        .bot_ui
                        .group_chat
                        .rooms
                        .get(&dismiss_key)
                        .and_then(|r| {
                            let cursor = r
                                .composer
                                .as_ref()
                                .map_or(r.draft.len(), |(inp, _)| inp.read(cx).cursor());
                            mention_query_at_cursor(&r.draft, cursor, &members).map(|(at, _, _)| at)
                        });
                    if let Some(at_pos) = active_at
                        && let Some(state) = this.bot_ui.group_chat.rooms.get_mut(&dismiss_key)
                    {
                        state.mention_dismissed_at = Some(at_pos);
                        cx.notify();
                    }
                }),
            )
            .on_scroll_wheel(cx.listener(move |this, _, _, _| {
                if let Some(state) = this.bot_ui.group_chat.rooms.get_mut(&wheel_key) {
                    state.follow_paused = !is_group_timeline_near_bottom(&state.scroll);
                }
            }))
            .child(list);
        if let Some(handle) = scroll {
            scroller = scroller.track_scroll(&handle);
        }
        scroller.into_any_element()
    }

    fn render_owner_message(
        msg: &BotGroupMessage,
        now: u64,
        p: &ui::Palette,
    ) -> gpui::Stateful<gpui::Div> {
        let age = ui::format_short_time_ago_secs(msg.created_at_ms / 1000, now / 1000);
        div()
            .id(SharedString::from(format!("grp-owner-{}", msg.message_id)))
            .w_full()
            .flex()
            .justify_end()
            .child(
                div()
                    .max_w(px(540.))
                    .px_3p5()
                    .py_2p5()
                    .rounded(ui::RADIUS_XL)
                    .bg(p.muted)
                    .border_1()
                    .border_color(p.border.opacity(0.6))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(text::GRP_YOU_SENDER.get())
                            .child(age),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_MD)
                            .text_color(p.foreground)
                            .whitespace_normal()
                            .child(msg.body.clone()),
                    ),
            )
    }

    #[expect(clippy::too_many_arguments, reason = "BotGroupTurn row builder")]
    fn render_bot_turn(
        &self,
        key: &GroupRoomKey,
        msg: &BotGroupMessage,
        active_turn: Option<&BotGroupTurnInfo>,
        members: &[SidebarBot],
        live_preview: Option<&str>,
        expanded: bool,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let bot = members.iter().find(|b| b.id.as_str() == msg.sender_id);
        let bot_name = bot.map_or_else(|| msg.sender_name.clone(), |b| b.name.clone());
        let is_member = bot.is_some();
        let is_working = msg.in_progress || active_turn.is_some();
        let is_stopped = !is_working && msg.body.trim() == "[stopped]";

        let avatar_el = if let Some(b) = bot {
            self.bot_avatar(b, 22., px(6.), p)
        } else {
            avatar::render(
                &avatar::Avatar::from_fields(None, None, None, &bot_name),
                None,
                &bot_name,
                px(22.),
                px(6.),
                p,
            )
        };

        let status_label = if is_working {
            let elapsed = active_turn
                .map(|t| format_elapsed(now.saturating_sub(t.started_at_ms)))
                .unwrap_or_default();
            if elapsed.is_empty() {
                text::GROUP_RUNNING_BADGE.get().to_owned()
            } else {
                format!("{} · {elapsed}", text::GROUP_RUNNING_BADGE.get())
            }
        } else if is_stopped {
            text::CHAT_SESSION_STOPPED.get().to_owned()
        } else if !is_member {
            text::GRP_MEMBER_REMOVED.get().to_owned()
        } else {
            ui::format_short_time_ago_secs(msg.created_at_ms / 1000, now / 1000)
        };

        let msg_id_toggle = msg.message_id.clone();
        let key_toggle = key.clone();
        let stop_key = key.clone();
        let reply_key = key.clone();
        // Never pass `None` to per-turn stop when a member was removed (`BOT-26`): resolve
        // the bot ID from the active turn, live member list, or sender_id.
        let stop_bot_id = active_turn
            .map(|t| t.bot_id.clone())
            .or_else(|| bot.map(|b| b.id.clone()))
            .or_else(|| BotId::new(&msg.sender_id).ok());
        let turn_stopping = self.bot_ui.group_chat.rooms.get(key).is_some_and(|r| {
            r.stopping_all
                || stop_bot_id
                    .as_ref()
                    .is_some_and(|bid| r.stopping_bots.contains(bid))
        });
        let reply_handle = bot.map_or_else(|| msg.sender_id.clone(), |b| b.slug.clone());
        let bot_base_dir = bot.and_then(|b| self.bot_folder(b));

        let has_details =
            live_preview.is_some() || msg.body.lines().count() > TURN_COLLAPSE_LINE_LIMIT;
        let show_body = if is_working {
            live_preview.is_some() || !msg.body.trim().is_empty()
        } else {
            !is_stopped && !msg.body.trim().is_empty()
        };

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(avatar_el)
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(bot_name.clone()),
                    )
                    .when(is_working, |d| {
                        d.child(ui::status::status_glyph(
                            AgentState::Working,
                            false,
                            ui::status::Size::Sm,
                            cx,
                        ))
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui::TEXT_XS)
                            .text_color(if is_working {
                                p.foreground
                            } else {
                                p.muted_foreground
                            })
                            .child(status_label),
                    )
                    .when(has_details, |d| {
                        d.child(
                            Button::new(SharedString::from(format!(
                                "grp-turn-exp-{}",
                                msg.message_id
                            )))
                            .ghost()
                            .xsmall()
                            .icon(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .tooltip(if expanded {
                                text::GRP_TURN_COLLAPSE.get()
                            } else {
                                text::GRP_TURN_EXPAND.get()
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let entry = this
                                        .bot_ui
                                        .group_chat
                                        .rooms
                                        .entry(key_toggle.clone())
                                        .or_default();
                                    if !entry.expanded_turns.remove(&msg_id_toggle) {
                                        entry.expanded_turns.insert(msg_id_toggle.clone());
                                    }
                                    cx.notify();
                                },
                            )),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(is_working && stop_bot_id.is_some(), |d| {
                        let bid = stop_bot_id.clone();
                        let label = text::GRP_TURN_STOP.format(&[("name", &bot_name)]);
                        d.child(
                            Button::new(SharedString::from(format!(
                                "grp-turn-stop-{}",
                                msg.message_id
                            )))
                            .debug_selector(|| "grp-turn-stop".into())
                            .ghost()
                            .xsmall()
                            .disabled(turn_stopping)
                            .icon(Icon::default().data(ui::icons::STOP_SQUARE_SVG))
                            .tooltip(label)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if bid.is_some() {
                                        this.stop_group_turn(stop_key.clone(), bid.clone(), cx);
                                    }
                                },
                            )),
                        )
                    })
                    .child(
                        Button::new(SharedString::from(format!(
                            "grp-turn-reply-{}",
                            msg.message_id
                        )))
                        .debug_selector(|| "grp-turn-reply".into())
                        .ghost()
                        .xsmall()
                        .icon(Icon::default().data(ui::icons::REPLY_SVG))
                        .tooltip(text::GRP_TURN_REPLY.format(&[("name", &bot_name)]))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.prepend_reply_mention(&reply_key, &reply_handle, window, cx);
                            },
                        )),
                    ),
            );

        let mut card = div()
            .id(SharedString::from(format!("grp-turn-{}", msg.message_id)))
            .debug_selector({
                let id = msg.sender_id.clone();
                move || format!("grp-turn-{id}")
            })
            .w_full()
            .flex()
            .flex_col()
            .gap_1p5()
            .pl_1()
            .child(header);

        if show_body {
            let body_el = if is_working {
                let content = live_preview.unwrap_or(msg.body.trim()).to_owned();
                if expanded {
                    let shell = cx.weak_entity();
                    let link_host = key.host.clone();
                    let link_base = bot_base_dir.clone();
                    div()
                        .min_w_0()
                        .text_size(ui::TEXT_MD)
                        .text_color(p.foreground)
                        .child(
                            gpui_component::text::TextView::markdown(
                                SharedString::from(format!("grp-md-{}", msg.message_id)),
                                content,
                            )
                            .scrollable(false)
                            .selectable(true)
                            .on_link_click(move |url, event, _, cx| {
                                if shell
                                    .update(cx, |this, cx| {
                                        this.open_chat_markdown_link(
                                            None,
                                            link_host.clone(),
                                            link_base.as_deref(),
                                            url,
                                            event,
                                            cx,
                                        );
                                    })
                                    .is_err()
                                    && krill_app_core::terminal::path_link::should_open_link_click(
                                        event,
                                    )
                                {
                                    krill_app_core::terminal::path_link::open_markdown_link_with_app(
                                        url,
                                        link_base.as_deref(),
                                        cx,
                                    );
                                }
                            }),
                        )
                        .into_any_element()
                } else {
                    div()
                        .text_size(ui::TEXT_SM)
                        .text_color(p.muted_foreground)
                        .line_clamp(3)
                        .child(content)
                        .into_any_element()
                }
            } else {
                let (content, _clamped) = group_turn_display_body(&msg.body, expanded);
                let shell = cx.weak_entity();
                let link_host = key.host.clone();
                let link_base = bot_base_dir.clone();
                div()
                    .min_w_0()
                    .text_size(ui::TEXT_MD)
                    .text_color(p.foreground)
                    .child(
                        gpui_component::text::TextView::markdown(
                            SharedString::from(format!("grp-md-{}", msg.message_id)),
                            content,
                        )
                        .scrollable(false)
                        .selectable(true)
                        .on_link_click(move |url, event, _, cx| {
                            if shell
                                .update(cx, |this, cx| {
                                    this.open_chat_markdown_link(
                                        None,
                                        link_host.clone(),
                                        link_base.as_deref(),
                                        url,
                                        event,
                                        cx,
                                    );
                                })
                                .is_err()
                                && krill_app_core::terminal::path_link::should_open_link_click(
                                    event,
                                )
                            {
                                krill_app_core::terminal::path_link::open_markdown_link_with_app(
                                    url,
                                    link_base.as_deref(),
                                    cx,
                                );
                            }
                        }),
                    )
                    .into_any_element()
            };
            card = card.child(div().pl(px(30.)).child(body_el));
        }

        card
    }

    fn render_group_approval_dock(
        &self,
        key: &GroupRoomKey,
        prompts: &[MemberPromptState],
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        if prompts.is_empty() {
            return None;
        }
        let selected_id = self
            .bot_ui
            .group_chat
            .rooms
            .get(key)
            .and_then(|r| r.selected_approval_bot.as_ref());
        let active = selected_id
            .and_then(|id| prompts.iter().find(|pr| &pr.bot.id == id))
            .unwrap_or(&prompts[0]);

        let title = text::GRP_NEEDS_YOU_TITLE.format(&[("name", &active.bot.name)]);
        let active_id = active.bot.id.clone();
        let av = self.bot_avatar(&active.bot, 20., px(6.), p);
        let sid = active.session_id;
        let bid = active.bot.id.clone();
        let bid_body = active.bot.id.clone();

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(av)
                    .when(prompts.len() > 1, |d| {
                        let key_pick = key.clone();
                        let list = prompts.to_vec();
                        let active_id = active_id.clone();
                        let this = cx.weak_entity();
                        d.child(
                            Button::new("grp-approval-member-pick")
                                .debug_selector(|| "grp-approval-member-pick".into())
                                .ghost()
                                .xsmall()
                                .label(title.clone())
                                .icon(IconName::ChevronDown)
                                .dropdown_menu(move |mut menu, _, _| {
                                    for item in &list {
                                        let pick_id = item.bot.id.clone();
                                        let label = text::GRP_NEEDS_YOU_TITLE
                                            .format(&[("name", &item.bot.name)]);
                                        let key_p = key_pick.clone();
                                        let this = this.clone();
                                        menu = menu.item(
                                            PopupMenuItem::new(label)
                                                .checked(item.bot.id == active_id)
                                                .on_click(move |_, _, cx| {
                                                    this.update(cx, |s, cx| {
                                                        if let Some(r) = s
                                                            .bot_ui
                                                            .group_chat
                                                            .rooms
                                                            .get_mut(&key_p)
                                                        {
                                                            r.selected_approval_bot =
                                                                Some(pick_id.clone());
                                                        }
                                                        cx.notify();
                                                    })
                                                    .ok();
                                                }),
                                        );
                                    }
                                    menu
                                }),
                        )
                    })
                    .when(prompts.len() <= 1, |d| {
                        d.child(
                            div()
                                .truncate()
                                .text_size(ui::TEXT_SM)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                    }),
            )
            .child({
                let open_host = key.host.clone();
                Button::new("grp-approval-open-chat")
                    .ghost()
                    .xsmall()
                    .label(text::OPEN_URL_BTN.get())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_bot_session_on(&open_host, &bid, sid, cx);
                    }))
            });

        let prompt_body = match &active.item {
            Some(item) if item.kind == ChatItemKind::Question && !item.options.is_empty() => {
                let mut opts = div().flex().flex_wrap().gap_1p5();
                for (idx, opt) in item.options.iter().enumerate() {
                    let k = key.clone();
                    let label = format!("{}. {}", idx + 1, opt.label);
                    let reply_num = format!("{}\r", idx + 1);
                    opts = opts.child(
                        Button::new(SharedString::from(format!("grp-dock-opt-{idx}")))
                            .ghost()
                            .xsmall()
                            .label(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_group_approval_input(k.clone(), sid, &reply_num, cx);
                            })),
                    );
                }
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(!item.text.trim().is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.foreground)
                                .line_clamp(3)
                                .child(item.text.clone()),
                        )
                    })
                    .child(opts)
            }
            Some(item) if item.kind == ChatItemKind::Question => div()
                .flex()
                .flex_col()
                .gap_2()
                .when(!item.text.trim().is_empty(), |d| {
                    d.child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.foreground)
                            .line_clamp(3)
                            .child(item.text.clone()),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::GRP_QUESTION_OPEN_HINT.get()),
                        )
                        .child({
                            let open_host = key.host.clone();
                            Button::new("grp-dock-open-chat")
                                .debug_selector(|| "grp-dock-open-chat".into())
                                .primary()
                                .xsmall()
                                .label(text::OPEN_CHAT.get())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_bot_session_on(&open_host, &bid_body, sid, cx);
                                }))
                        }),
                ),
            Some(item) => {
                let k1 = key.clone();
                let k2 = key.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(!item.text.trim().is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.foreground)
                                .line_clamp(3)
                                .child(item.text.clone()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("grp-dock-approve")
                                    .debug_selector(|| "grp-dock-approve".into())
                                    .primary()
                                    .xsmall()
                                    .label(ctext::APPROVE.get())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send_group_approval_input(k1.clone(), sid, "y\r", cx);
                                    })),
                            )
                            .child(
                                Button::new("grp-dock-deny")
                                    .debug_selector(|| "grp-dock-deny".into())
                                    .ghost()
                                    .xsmall()
                                    .label(ctext::DENY.get())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send_group_approval_input(k2.clone(), sid, "n\r", cx);
                                    })),
                            ),
                    )
            }
            None if active.state == AgentState::Blocked => {
                let k1 = key.clone();
                let k2 = key.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(ctext::PERMISSION.get()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("grp-dock-approve")
                                    .debug_selector(|| "grp-dock-approve".into())
                                    .primary()
                                    .xsmall()
                                    .label(ctext::APPROVE.get())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send_group_approval_input(k1.clone(), sid, "y\r", cx);
                                    })),
                            )
                            .child(
                                Button::new("grp-dock-deny")
                                    .debug_selector(|| "grp-dock-deny".into())
                                    .ghost()
                                    .xsmall()
                                    .label(ctext::DENY.get())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.send_group_approval_input(k2.clone(), sid, "n\r", cx);
                                    })),
                            ),
                    )
            }
            None => div()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(ctext::QUESTION.get()),
        };

        Some(
            div().w_full().px_4().pb_1().flex_none().child(
                div()
                    .id("bot-group-approval-dock")
                    .debug_selector(|| "bot-group-approval-dock".into())
                    .w_full()
                    .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                    .mx_auto()
                    .p_3()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.agent_question)
                    .bg(p.card)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(header)
                    .child(prompt_body),
            ),
        )
    }

    #[expect(clippy::too_many_arguments, reason = "group composer + pickers")]
    fn render_group_composer(
        &self,
        pane: PaneId,
        key: &GroupRoomKey,
        input_state: &Entity<TextareaState>,
        members: &[SidebarBot],
        audience: &BotGroupReplyAudience,
        draft: &str,
        sending: bool,
        active_run: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let cursor_byte = input_state.read(cx).cursor();
        let active_mention = self.active_group_mention(key, draft, cursor_byte, members);
        let mention_selected = self
            .bot_ui
            .group_chat
            .rooms
            .get(key)
            .map_or(0, |r| r.mention_selected);
        let stopping_all = self
            .bot_ui
            .group_chat
            .rooms
            .get(key)
            .is_some_and(|r| r.stopping_all);

        let mention_menu = active_mention
            .as_ref()
            .map(|(at_pos, cursor_end, candidates)| {
                let at_pos = *at_pos;
                let cursor_end = *cursor_end;
                let selected_idx = mention_selected.min(candidates.len().saturating_sub(1));
                let mut list = div()
                    .id("group-mention-menu")
                    .debug_selector(|| "group-mention-menu".into())
                    .occlude()
                    .w_full()
                    .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .mx_auto()
                    .mb_1p5()
                    .p_1()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.popover)
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_0p5();
                for (idx, cand) in candidates.iter().enumerate() {
                    let is_selected = idx == selected_idx;
                    let k = key.clone();
                    let inp = input_state.clone();
                    let handle = cand.handle.clone();
                    if cand.bot_id.is_none() {
                        list = list.child(
                            Button::new("grp-mention-everyone")
                                .debug_selector(|| "grp-mention-everyone".into())
                                .ghost()
                                .xsmall()
                                .w_full()
                                .justify_start()
                                .toggled(is_selected)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.apply_group_mention(
                                        &k,
                                        &inp,
                                        at_pos..cursor_end,
                                        &handle,
                                        window,
                                        cx,
                                    );
                                }))
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .text_size(ui::TEXT_XS)
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .flex_none()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(text::GRP_EVERYONE_LABEL.get()),
                                                )
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .truncate()
                                                        .text_color(p.muted_foreground)
                                                        .child(text::GRP_EVERYONE_DESC.get()),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_color(p.muted_foreground)
                                                .child(EVERYONE_HANDLE),
                                        ),
                                ),
                        );
                    } else if let Some(bot) = cand
                        .bot_id
                        .as_ref()
                        .and_then(|bid| members.iter().find(|b| &b.id == bid))
                    {
                        let name = bot.name.clone();
                        let av = self.bot_avatar(bot, 18., px(5.), p);
                        let mention_handle = format!("@{handle}");
                        list = list.child(
                            Button::new(SharedString::from(format!(
                                "grp-mention-{}",
                                bot.id.as_str()
                            )))
                            .ghost()
                            .xsmall()
                            .w_full()
                            .justify_start()
                            .toggled(is_selected)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.apply_group_mention(
                                    &k,
                                    &inp,
                                    at_pos..cursor_end,
                                    &handle,
                                    window,
                                    cx,
                                );
                            }))
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(av)
                                            .child(div().min_w_0().truncate().child(name)),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .max_w(px(140.))
                                            .truncate()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(mention_handle),
                                    ),
                            ),
                        );
                    }
                }
                list
            });

        let summary = reply_picker_summary(audience, draft, members);
        let prefix_label = format!("{}:", text::GRP_REPLIES_PREFIX.get());
        let this_picker = cx.weak_entity();
        let key_picker = key.clone();
        let members_picker = members.to_vec();
        let audience_picker = audience.clone();

        let reply_picker = Button::new(SharedString::from(format!(
            "group-reply-picker-{}",
            pane.get()
        )))
        .debug_selector(|| "group-reply-picker".into())
        .ghost()
        .xsmall()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_size(ui::TEXT_SM)
                .child(div().text_color(p.muted_foreground).child(prefix_label))
                .child(div().text_color(p.foreground).child(summary))
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(px(12.))
                        .text_color(p.muted_foreground),
                ),
        )
        .dropdown_menu(move |mut menu, _, _| {
            menu = menu.item(menu_heading(text::GRP_REPLY_PICKER_TITLE.get()));
            for bot in &members_picker {
                let checked = matches!(
                    &audience_picker,
                    BotGroupReplyAudience::Bot { bot_id } if bot_id == &bot.id
                );
                let bid = bot.id.clone();
                let k = key_picker.clone();
                let t = this_picker.clone();
                menu = menu.item(
                    PopupMenuItem::new(bot.name.clone())
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            t.update(cx, |s, cx| {
                                s.set_group_reply_audience(
                                    k.clone(),
                                    BotGroupReplyAudience::Bot {
                                        bot_id: bid.clone(),
                                    },
                                    cx,
                                );
                            })
                            .ok();
                        }),
                );
            }
            let k_all = key_picker.clone();
            let t_all = this_picker.clone();
            let k_mentions = key_picker.clone();
            let t_mentions = this_picker.clone();
            menu.separator()
                .item(
                    PopupMenuItem::new(text::GRP_ALL_BOTS.get())
                        .checked(matches!(audience_picker, BotGroupReplyAudience::All))
                        .on_click(move |_, _, cx| {
                            t_all
                                .update(cx, |s, cx| {
                                    s.set_group_reply_audience(
                                        k_all.clone(),
                                        BotGroupReplyAudience::All,
                                        cx,
                                    );
                                })
                                .ok();
                        }),
                )
                .item(
                    PopupMenuItem::new(text::GRP_ONLY_MENTIONED.get())
                        .checked(matches!(audience_picker, BotGroupReplyAudience::Mentions))
                        .on_click(move |_, _, cx| {
                            t_mentions
                                .update(cx, |s, cx| {
                                    s.set_group_reply_audience(
                                        k_mentions.clone(),
                                        BotGroupReplyAudience::Mentions,
                                        cx,
                                    );
                                })
                                .ok();
                        }),
                )
        });

        let send_key = key.clone();
        let stop_key = key.clone();
        let key_kbd = key.clone();
        let inp_kbd = input_state.clone();
        let can_send = !draft.trim().is_empty() && !sending;

        let inner_card = div()
            .id("group-composer-card")
            .debug_selector(|| "group-composer-card".into())
            .w_full()
            .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
            .mx_auto()
            .p_2p5()
            .rounded(ui::RADIUS_XL)
            .border_1()
            .border_color(p.chat_composer_border())
            .bg(p.chat_composer_surface())
            .flex()
            .flex_col()
            .gap_1p5()
            .on_action(
                cx.listener(|_, action: &gpui_component::input::Enter, _, cx| {
                    // Consume plain Enter so GPUI does not fall through to text-input `\n`,
                    // while letting Shift+Enter (newline) and Secondary+Shift+Enter (ToggleZoomPane) propagate.
                    if action.secondary || action.shift {
                        cx.propagate();
                    }
                }),
            )
            .capture_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.modifiers.modified()
                    || Self::chat_input_is_composing(&inp_kbd, window, cx)
                {
                    return;
                }
                let draft_now = inp_kbd.read(cx).value().to_string();
                let cursor_now = inp_kbd.read(cx).cursor();
                let members_now = this.group_members_for_key(&key_kbd);
                if let Some((at_pos, _, candidates)) =
                    this.active_group_mention(&key_kbd, &draft_now, cursor_now, &members_now)
                {
                    match ev.keystroke.key.as_str() {
                        "up" => {
                            if let Some(room) = this.bot_ui.group_chat.rooms.get_mut(&key_kbd) {
                                let len = candidates.len();
                                room.mention_selected = if room.mention_selected == 0 {
                                    len.saturating_sub(1)
                                } else {
                                    room.mention_selected.saturating_sub(1)
                                };
                            }
                            cx.stop_propagation();
                            window.prevent_default();
                            cx.notify();
                        }
                        "down" => {
                            if let Some(room) = this.bot_ui.group_chat.rooms.get_mut(&key_kbd) {
                                let len = candidates.len().max(1);
                                room.mention_selected = (room.mention_selected + 1) % len;
                            }
                            cx.stop_propagation();
                            window.prevent_default();
                            cx.notify();
                        }
                        "tab" => {
                            if this.accept_active_group_mention(&key_kbd, &inp_kbd, window, cx) {
                                cx.stop_propagation();
                                window.prevent_default();
                            }
                        }
                        "escape" => {
                            if let Some(room) = this.bot_ui.group_chat.rooms.get_mut(&key_kbd) {
                                room.mention_dismissed_at = Some(at_pos);
                            }
                            cx.stop_propagation();
                            window.prevent_default();
                            cx.notify();
                        }
                        _ => {}
                    }
                } else if ev.keystroke.key == "escape" && active_run {
                    // Esc stops active group turn when `@mention` popup is closed (`BOT-04`/`CHAT-09`).
                    this.stop_group_turn(key_kbd.clone(), None, cx);
                    cx.stop_propagation();
                    window.prevent_default();
                }
            }))
            .child(
                Textarea::new(input_state)
                    .small()
                    .appearance(false)
                    .w_full()
                    .min_w_0()
                    .accessibility_id("group-composer-input")
                    .aria_label(text::GRP_COMPOSER_HINT.get()),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(reply_picker),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .when(active_run, |d| {
                                d.child(
                                    Button::new("group-composer-stop")
                                        .debug_selector(|| "group-composer-stop".into())
                                        .icon(Icon::default().data(ui::icons::STOP_SQUARE_SVG))
                                        .tooltip(text::GROUP_STOP_BTN.get())
                                        .secondary()
                                        .rounded(px(999.))
                                        .disabled(stopping_all)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.stop_group_turn(stop_key.clone(), None, cx);
                                        })),
                                )
                            })
                            .child(self.render_group_voice_control(&send_key, sending, cx))
                            .child(
                                Button::new("group-composer-send")
                                    .debug_selector(|| "group-composer-send".into())
                                    .icon(IconName::ArrowUp)
                                    .tooltip(ctext::SEND.get())
                                    .primary()
                                    .rounded(px(999.))
                                    .disabled(!can_send)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.send_group_composer_message(
                                            send_key.clone(),
                                            window,
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            );

        div()
            .w_full()
            .flex_none()
            .px_4()
            .pt_1()
            .pb_3()
            .bg(p.chat_canvas())
            .flex()
            .flex_col()
            .children(mention_menu)
            .child(inner_card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_bots_ui::bots::avatar::Avatar;

    fn sample_bot(id: &str, slug: &str, name: &str) -> SidebarBot {
        SidebarBot {
            host: HostId::Local,
            id: BotId::new(id).unwrap(),
            name: name.to_owned(),
            emoji: None,
            avatar: Avatar::from_fields(None, None, None, name),
            avatar_color: None,
            description: String::new(),
            slug: slug.to_owned(),
            paused: false,
            agent_id: "claude".to_owned(),
            permission: krill_proto::agent::Permission::Manual,
            workspace: None,
            attention: 0,
            items_need_you: 0,
            access_requesters: Vec::new(),
            sessions: Vec::new(),
            schedules: Vec::new(),
            updated_at_ms: 0,
        }
    }

    #[test]
    fn reply_picker_and_mentions_behave_like_orca() {
        let members = vec![
            sample_bot("b1", "alice", "Alice"),
            sample_bot("b2", "bob-ops", "Bob Ops"),
        ];
        assert_eq!(
            reply_picker_summary(
                &BotGroupReplyAudience::Bot {
                    bot_id: members[0].id.clone()
                },
                "",
                &members
            ),
            "Alice"
        );
        assert_eq!(
            reply_picker_summary(&BotGroupReplyAudience::Mentions, "@alice check", &members),
            "Alice"
        );
        assert_eq!(
            reply_picker_summary(
                &BotGroupReplyAudience::Mentions,
                "@everyone hello",
                &members
            ),
            text::GRP_ALL_BOTS.get()
        );
        assert_eq!(
            trailing_mention_query("hey @al"),
            Some((4, "al".to_owned()))
        );
        assert_eq!(trailing_mention_query("email@domain.com"), None);
        assert_eq!(insert_mention_token("hey @al", 4, "alice"), "hey @alice ");
    }

    fn sample_room_msg(id: &str, body: &str, created_at_ms: u64) -> BotGroupMessage {
        BotGroupMessage {
            message_id: id.to_owned(),
            group_id: "grp-1".to_owned(),
            thread_id: "thr-main".to_owned(),
            sender_id: "owner".to_owned(),
            sender_name: "You".to_owned(),
            body: body.to_owned(),
            mentions: Vec::new(),
            created_at_ms,
            in_progress: false,
        }
    }

    fn sample_room(
        thread_id: Option<&str>,
        messages: Vec<BotGroupMessage>,
        has_earlier: bool,
    ) -> BotGroupRoomView {
        BotGroupRoomView {
            group: krill_proto::bots::BotGroupChat {
                id: "grp-1".to_owned(),
                name: "Crew".to_owned(),
                bot_ids: Vec::new(),
                description: String::new(),
                created_at_ms: 0,
                updated_at_ms: 0,
            },
            reply_audience: BotGroupReplyAudience::Mentions,
            threads: Vec::new(),
            active_thread_id: thread_id.map(str::to_owned),
            messages,
            has_earlier,
            active_turns: Vec::new(),
        }
    }

    #[test]
    fn merge_polled_room_preserves_earlier_messages_and_flags() {
        let prev = sample_room(
            Some("thr-main"),
            vec![
                sample_room_msg("m1", "old 1", 10),
                sample_room_msg("m2", "old 2", 20),
                sample_room_msg("m3", "mid 3", 30),
                sample_room_msg("m4", "draft", 40),
            ],
            false,
        );
        let polled = sample_room(
            Some("thr-main"),
            vec![
                sample_room_msg("m3", "mid 3", 30),
                sample_room_msg("m4", "final", 40),
                sample_room_msg("m5", "new 5", 50),
            ],
            true,
        );
        let merged = merge_polled_room(Some(&prev), polled);
        let ids: Vec<&str> = merged
            .messages
            .iter()
            .map(|m| m.message_id.as_str())
            .collect();
        assert_eq!(ids, vec!["m1", "m2", "m3", "m4", "m5"]);
        assert_eq!(merged.messages[3].body, "final");
        assert!(
            !merged.has_earlier,
            "should keep prev.has_earlier when older prefix is preserved"
        );

        // Switching threads discards previous thread messages.
        let other_thread = sample_room(
            Some("thr-other"),
            vec![sample_room_msg("m3", "other", 30)],
            true,
        );
        let replaced = merge_polled_room(Some(&prev), other_thread);
        assert_eq!(replaced.messages.len(), 1);
        assert!(replaced.has_earlier);

        // When no older prefix existed (overlap at index 0), polled has_earlier wins.
        let same_start = sample_room(
            Some("thr-main"),
            vec![sample_room_msg("m1", "old 1", 10)],
            true,
        );
        let merged_zero = merge_polled_room(Some(&prev), same_start);
        assert_eq!(merged_zero.messages.len(), 1);
        assert!(merged_zero.has_earlier);
    }

    #[test]
    fn same_group_thread_deduplicates_none_and_default_thread() {
        // Sidebar opened with None, room resolved thr-main; header clicks thr-main.
        assert!(same_group_thread(
            None,
            Some("thr-main"),
            Some("thr-main"),
            Some("thr-main")
        ));
        // Opened with explicit thr-main; sidebar clicks group (None).
        assert!(same_group_thread(
            Some("thr-main"),
            Some("thr-main"),
            None,
            Some("thr-main")
        ));
        // Sidebar opened with None (resolved thr-main); header clicks secondary thr-2.
        assert!(!same_group_thread(
            None,
            Some("thr-main"),
            Some("thr-2"),
            Some("thr-main")
        ));
        // Secondary thread pane does not match default None request when default is thr-main.
        assert!(!same_group_thread(
            Some("thr-2"),
            Some("thr-2"),
            None,
            Some("thr-main")
        ));
        // Group with no threads yet: None matches None.
        assert!(same_group_thread(None, None, None, None));
    }

    #[test]
    fn format_elapsed_uses_localized_templates() {
        assert_eq!(
            format_elapsed(42_500),
            text::GRP_ELAPSED_SEC.format(&[("s", "42")])
        );
        assert_eq!(
            format_elapsed(125_000),
            text::GRP_ELAPSED_MIN_SEC.format(&[("m", "2"), ("s", "5")])
        );
    }

    #[test]
    fn targeted_group_invalidation_only_resets_changed_rooms() {
        let mut rooms = HashMap::new();
        let k1 = GroupRoomKey {
            host: HostId::Local,
            group_id: "grp-a".to_owned(),
            thread_id: None,
        };
        let k2 = GroupRoomKey {
            host: HostId::Local,
            group_id: "grp-b".to_owned(),
            thread_id: None,
        };
        rooms.insert(
            k1.clone(),
            RoomState {
                last_poll_ms: 9_999,
                ..RoomState::default()
            },
        );
        rooms.insert(
            k2.clone(),
            RoomState {
                last_poll_ms: 9_999,
                ..RoomState::default()
            },
        );

        invalidate_rooms_map(
            &mut rooms,
            Some(&HostId::Local),
            Some(&["grp-a".to_owned()]),
        );
        assert_eq!(rooms[&k1].last_poll_ms, 0);
        assert_eq!(
            rooms[&k2].last_poll_ms, 9_999,
            "unrelated group room must not be invalidated (#47523)"
        );

        invalidate_rooms_map(&mut rooms, None, None);
        assert_eq!(rooms[&k2].last_poll_ms, 0);
    }

    #[test]
    fn resolved_thread_id_uses_loaded_active_thread_when_key_is_none() {
        let key_none = GroupRoomKey {
            host: HostId::Local,
            group_id: "grp-1".to_owned(),
            thread_id: None,
        };
        let state = RoomState {
            room: Some(sample_room(Some("thr-main"), Vec::new(), false)),
            ..Default::default()
        };
        assert_eq!(
            resolved_thread_id(&key_none, Some(&state)).as_deref(),
            Some("thr-main")
        );

        let key_explicit = GroupRoomKey {
            host: HostId::Local,
            group_id: "grp-1".to_owned(),
            thread_id: Some("thr-2".to_owned()),
        };
        assert_eq!(
            resolved_thread_id(&key_explicit, Some(&state)).as_deref(),
            Some("thr-2")
        );
    }

    #[test]
    fn group_turn_display_body_clamps_long_finished_messages_and_closes_fences() {
        let short = (1..=8)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (out, clamped) = group_turn_display_body(&short, false);
        assert!(!clamped);
        assert_eq!(out, short);

        let long = (1..=12)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (collapsed, clamped) = group_turn_display_body(&long, false);
        assert!(clamped);
        assert!(collapsed.contains("line 8"));
        assert!(!collapsed.contains("line 9"));
        assert!(collapsed.ends_with("\n…"));

        let (expanded, clamped_exp) = group_turn_display_body(&long, true);
        assert!(!clamped_exp);
        assert_eq!(expanded, long);

        let fenced = "intro\n```rust\nfn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\nfn e() {}\nfn f() {}\nfn g() {}\n```";
        let (fenced_collapsed, fenced_clamped) = group_turn_display_body(fenced, false);
        assert!(fenced_clamped);
        assert!(
            fenced_collapsed.ends_with("\n```\n…"),
            "clamped code fence should be closed before ellipsis, got {fenced_collapsed:?}"
        );
    }

    #[test]
    fn cursor_aware_mentions_preserve_suffix_match_spaced_names_and_group_session_scope() {
        let members = vec![
            sample_bot("b1", "alice", "Alice"),
            sample_bot("b2", "bob-ops", "Bob Ops"),
        ];

        // BOT-08: mid-draft cursor-aware mention detection & splice preserving trailing text.
        let draft = "hey @al please check";
        let cursor = "hey @al".len();
        assert_eq!(
            mention_query_at_cursor(draft, cursor, &members),
            Some((4, cursor, "al".to_owned()))
        );
        let (spliced, new_cursor) = splice_mention_token(draft, 4, cursor, "alice");
        assert_eq!(spliced, "hey @alice please check");
        assert_eq!(new_cursor, "hey @alice ".len());

        // BOT-28: spaced display name prefix matches `@Bob Ops`.
        let spaced = "ping @Bob O";
        assert_eq!(
            mention_query_at_cursor(spaced, spaced.len(), &members),
            Some((5, spaced.len(), "Bob O".to_owned()))
        );
        assert_eq!(
            mention_query_at_cursor("ping @Bob unknown", 17, &members),
            None
        );

        // BOT-06: strict `(group:<gid>:<tid>)` and legacy `(group:<gid>)` session scoping.
        assert!(session_belongs_to_group(
            "Alice (group:g1:t1)",
            "g1",
            Some("t1")
        ));
        assert!(!session_belongs_to_group(
            "Alice (group:g1:t2)",
            "g1",
            Some("t1")
        ));
        assert!(session_belongs_to_group("Alice (group:g1:t2)", "g1", None));
        assert!(session_belongs_to_group(
            "Alice (group:g1)",
            "g1",
            Some("t1")
        ));
        assert!(!session_belongs_to_group("Alice (group:g2:t1)", "g1", None));
        assert!(!session_belongs_to_group("Alice", "g1", None));
    }
}
