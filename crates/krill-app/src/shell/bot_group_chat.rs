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
use krill_app::bots::{SidebarBot, avatar, group_faces, rpc::call_checked};
use krill_app::hosts::HostId;
use krill_app::i18n::catalog::{bots as text, chat as ctext};
use krill_app::layout::{GroupPaneTarget, PaneId};
use krill_app::ui;
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
pub(super) struct RoomState {
    pub room: Option<BotGroupRoomView>,
    pub reply_audience: BotGroupReplyAudience,
    pub loading: bool,
    pub loading_earlier: bool,
    pub sending: bool,
    pub error: Option<String>,
    pub composer: Option<(Entity<TextareaState>, Subscription)>,
    pub draft: String,
    pub expanded_turns: HashSet<String>,
    pub selected_approval_bot: Option<BotId>,
    pub member_items: BTreeMap<SessionId, Vec<ChatItem>>,
    pub last_poll_ms: u64,
    pub focus_composer: ComposerFocus,
    pub scroll: ScrollHandle,
}

#[derive(Default)]
pub(super) struct BotGroupUiState {
    pub rooms: HashMap<GroupRoomKey, RoomState>,
    poll_timer: Option<Task<()>>,
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
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
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
        let summary = self
            .sidebar_groups
            .iter()
            .find(|s| s.group.id == target.group_id);
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

    pub(super) fn invalidate_group_room(&mut self, group_id: &str) {
        for (k, room) in &mut self.bot_ui.group_chat.rooms {
            if k.group_id == group_id {
                room.last_poll_ms = 0;
            }
        }
    }

    pub(super) fn close_panes_for_group(&mut self, group_id: &str, cx: &mut Context<Self>) {
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
                self.layout
                    .group_pane(*p)
                    .is_some_and(|g| g.group_id == group_id)
            })
            .collect();
        if matching.is_empty() {
            return;
        }
        self.change(true, cx, |l| {
            for pane in matching {
                if let Some(tab_idx) = l.tab_of_pane(pane) {
                    if l.tab_panes(tab_idx).len() <= 1 {
                        l.close_tab(tab_idx);
                    } else {
                        l.focus(pane);
                        l.close_pane();
                    }
                }
            }
        });
    }

    pub(in crate::shell) fn open_bot_group(
        &mut self,
        group_id: &str,
        thread_id: Option<&str>,
        _window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let host = self.launch_host.clone();
        self.dashboard = None;
        self.close_settings(None, cx);

        let target = GroupPaneTarget {
            group_id: group_id.to_owned(),
            thread_id: thread_id.map(str::to_owned),
        };
        let existing = (0..self.layout.tabs().len()).find_map(|tab_idx| {
            self.layout.tab_panes(tab_idx).into_iter().find(|&p| {
                self.layout.host(p) == &host
                    && self.layout.group_pane(p).is_some_and(|g| {
                        g.group_id == group_id
                            && match &target.thread_id {
                                Some(tid) => g.thread_id.as_deref() == Some(tid.as_str()),
                                None => g.thread_id.is_none(),
                            }
                    })
            })
        });

        let pane = if let Some(pane) = existing {
            self.change(true, cx, |l| {
                if let Some(tab) = l.tab_of_pane(pane) {
                    l.activate(tab);
                }
                l.focus(pane);
            });
            pane
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
            pane
        };

        let key = GroupRoomKey {
            host,
            group_id: group_id.to_owned(),
            thread_id: target.thread_id,
        };
        let entry = self.bot_ui.group_chat.rooms.entry(key.clone()).or_default();
        entry.focus_composer = ComposerFocus::Focus;
        if let Some(summary) = self.sidebar_groups.iter().find(|s| s.group.id == group_id) {
            entry.reply_audience = summary.reply_audience.clone();
        }
        let _ = pane;
        self.poll_group_room(key, cx);
        self.ensure_group_poll_timer(cx);
        cx.notify();
    }

    pub(in crate::shell) fn create_and_open_group_thread(
        &mut self,
        group_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc() else {
            return;
        };
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
                        this.open_bot_group(&group_id, newest.as_deref(), None, cx);
                        this.refresh_bots_badge(cx);
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

    fn poll_group_room(&mut self, key: GroupRoomKey, cx: &mut Context<Self>) {
        let Some(rpc) = self.bot_rpc_on(&key.host) else {
            return;
        };
        let gid = key.group_id.clone();
        let tid = key.thread_id.clone();
        let target = self.target_for_host(&key.host);

        // Identify live/waiting/active member sessions to inspect via ChatPoll for turns & ApprovalDock.
        let mut sessions_to_poll: Vec<SessionId> = Vec::new();
        if let Some(summary) = self.sidebar_groups.iter().find(|s| s.group.id == gid) {
            for t in &summary.active_turns {
                if !sessions_to_poll.contains(&t.session_id) {
                    sessions_to_poll.push(t.session_id);
                }
            }
            let sidebar = self.sidebar.read(cx);
            for bid in &summary.group.bot_ids {
                if let Some(bot) = self.sidebar_bots.iter().find(|b| &b.id == bid) {
                    for s in &bot.sessions {
                        let st = sidebar
                            .session_on(&key.host, s.session_id)
                            .map(|x| x.status.state);
                        if matches!(
                            st,
                            Some(AgentState::Working | AgentState::Waiting | AgentState::Blocked)
                        ) && !sessions_to_poll.contains(&s.session_id)
                        {
                            sessions_to_poll.push(s.session_id);
                        }
                    }
                }
            }
        }
        let state = self.bot_ui.group_chat.rooms.entry(key.clone()).or_default();
        if state.loading {
            return;
        }
        state.loading = true;
        state.last_poll_ms = now_unix_ms();
        if let Some(room) = &state.room {
            for t in &room.active_turns {
                if !sessions_to_poll.contains(&t.session_id) {
                    sessions_to_poll.push(t.session_id);
                }
            }
        }

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
                    if !sessions_to_poll.is_empty() {
                        let call = krill_app::resources::daemon_call_for(target);
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
                let state = this.bot_ui.group_chat.rooms.entry(key.clone()).or_default();
                state.loading = false;
                for (sid, items) in chat_items_map {
                    state.member_items.insert(sid, items);
                }
                match room_res {
                    Ok(BotRpcResponse::GroupRoom(view)) => {
                        state.reply_audience = view.reply_audience.clone();
                        state.error = None;
                        state.room = Some(view);
                    }
                    Ok(BotRpcResponse::GroupView(old)) => {
                        // Graceful fallback if daemon predates ReadGroupRoom.
                        let active_thread_id = old.threads.first().map(|t| t.thread_id.clone());
                        state.room = Some(BotGroupRoomView {
                            group: old.group,
                            reply_audience: state.reply_audience.clone(),
                            threads: old.threads,
                            active_thread_id,
                            messages: old.messages,
                            has_earlier: false,
                            active_turns: Vec::new(),
                        });
                    }
                    Ok(_) => {}
                    Err(err) => state.error = Some(err),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_earlier_group_messages(&mut self, key: GroupRoomKey, cx: &mut Context<Self>) {
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
        let gid = key.group_id.clone();
        let tid = key.thread_id.clone();
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
                let state = this.bot_ui.group_chat.rooms.entry(key).or_default();
                state.loading_earlier = false;
                if let Ok(BotRpcResponse::GroupRoom(earlier)) = res
                    && let Some(room) = &mut state.room
                {
                    let mut merged = earlier.messages;
                    merged.append(&mut room.messages);
                    room.messages = merged;
                    room.has_earlier = earlier.has_earlier;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn ensure_group_composer(
        &mut self,
        key: &GroupRoomKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        let entry = self.bot_ui.group_chat.rooms.entry(key.clone()).or_default();
        if let Some((input, _)) = &entry.composer {
            let input = input.clone();
            if std::mem::take(&mut entry.focus_composer) == ComposerFocus::Focus {
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
                    if let Some(room) = this.bot_ui.group_chat.rooms.get_mut(&sub_key) {
                        room.draft = val;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { secondary, .. } if !secondary => {
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

    fn set_group_reply_audience(
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
        if let Some(s) = self
            .sidebar_groups
            .iter_mut()
            .find(|s| s.group.id == key.group_id)
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
                    room.error = Some(err);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_group_composer_message(
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
        state.sending = true;
        state.error = None;
        state.draft.clear();
        if let Some(inp) = input {
            inp.update(cx, |s, cx| s.set_value("", window, cx));
        }
        let gid = key.group_id.clone();
        let tid = key.thread_id.clone();
        cx.notify();

        cx.spawn(async move |this, cx| {
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
            this.update(cx, |this, cx| {
                if let Some(room_state) = this.bot_ui.group_chat.rooms.get_mut(&key) {
                    room_state.sending = false;
                    match res {
                        Ok(BotRpcResponse::GroupMessagePosted { message, .. }) => {
                            if let Some(room) = &mut room_state.room {
                                room.active_thread_id
                                    .get_or_insert_with(|| message.thread_id.clone());
                                if !room
                                    .messages
                                    .iter()
                                    .any(|m| m.message_id == message.message_id)
                                {
                                    room.messages.push(message);
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(err) => {
                            room_state.error = Some(text::GRP_SEND_FAILED.format(&[("err", &err)]));
                        }
                    }
                }
                this.poll_group_room(key, cx);
                this.refresh_bots_badge(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn stop_group_turn(
        &mut self,
        key: GroupRoomKey,
        bot_id: Option<BotId>,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(&key.host) else {
            return;
        };
        let gid = key.group_id.clone();
        let tid = key.thread_id.clone();
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
                if let Err(err) = res
                    && let Some(room) = this.bot_ui.group_chat.rooms.get_mut(&key)
                {
                    room.error = Some(err);
                }
                this.poll_group_room(key, cx);
                this.refresh_bots_badge(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_group_approval_input(
        &mut self,
        key: GroupRoomKey,
        session: SessionId,
        raw: &str,
        cx: &mut Context<Self>,
    ) {
        let target = self.target_for_host(&key.host);
        let bytes = raw.as_bytes().to_vec();
        cx.spawn(async move |this, cx| {
            let err = cx
                .background_executor()
                .spawn(async move {
                    let call = krill_app::resources::daemon_call_for(target);
                    call(&Request::SendInput {
                        session,
                        bytes,
                        submit: true,
                        wait_submit: false,
                    })
                    .err()
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some(err) = err
                    && let Some(state) = this.bot_ui.group_chat.rooms.get_mut(&key)
                {
                    state.error = Some(err.to_string());
                }
                this.poll_group_room(key, cx);
                this.refresh_bots_badge(cx);
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
                for s in &bot.sessions {
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
            .find(|s| s.group.id == key.group_id)
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
                    .filter_map(|id| self.sidebar_bots.iter().find(|b| &b.id == id).cloned())
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
        let error_msg = self
            .bot_ui
            .group_chat
            .rooms
            .get(&key)
            .and_then(|r| r.error.clone());
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

        div()
            .id(SharedString::from(format!("bot-group-page-{}", pane.get())))
            .debug_selector(|| "bot-group-page".into())
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(p.background)
            .text_color(p.foreground)
            .flex()
            .flex_col()
            .font_family(ui::FONT_UI)
            .child(header)
            .children(error_msg.map(|err| {
                div()
                    .mx_4()
                    .mt_2()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.destructive.opacity(0.4))
                    .bg(p.destructive.opacity(0.1))
                    .text_size(ui::TEXT_XS)
                    .text_color(p.destructive)
                    .child(err)
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
        let face_members = group_faces::resolve_members(&member_ids, &self.sidebar_bots, &|id| {
            self.bot_ui.avatars.get(id).and_then(|(_, img)| img.clone())
        });
        let faces = group_faces::render_group_faces(&face_members, 28., p.background, p);
        let threads = room.map_or(&[][..], |r| r.threads.as_slice());
        let active_tid = room
            .and_then(|r| r.active_thread_id.as_deref())
            .or(key.thread_id.as_deref());
        let group_clone = group.cloned();
        let aud_clone = audience.clone();
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
                let gid = key.group_id.clone();
                let tid = thr.thread_id.clone();
                thread_strip = thread_strip.child(
                    Button::new(SharedString::from(format!("grp-thr-{}", thr.thread_id)))
                        .ghost()
                        .xsmall()
                        .toggled(selected)
                        .label(label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_bot_group(&gid, Some(&tid), Some(window), cx);
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
                            ),
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
                                this.create_and_open_group_thread(gid_thread.clone(), window, cx);
                            })),
                    )
                    .when_some(group_clone, |d, g| {
                        d.child(
                            Button::new("group-page-manage-members")
                                .debug_selector(|| "group-page-manage-members".into())
                                .ghost()
                                .xsmall()
                                .icon(IconName::Settings)
                                .tooltip(text::GRP_MANAGE_MEMBERS.get())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_manage_group_dialog(
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
            let face_members =
                group_faces::resolve_members(&member_ids, &self.sidebar_bots, &|id| {
                    self.bot_ui.avatars.get(id).and_then(|(_, img)| img.clone())
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
                            .text_color(p.muted_foreground.opacity(0.7))
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

        let scroll = self
            .bot_ui
            .group_chat
            .rooms
            .get(key)
            .map(|r| r.scroll.clone());

        let mut scroller = div()
            .id("bot-group-timeline")
            .debug_selector(|| "bot-group-timeline".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_4()
            .py_4()
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
        let age = krill_app::jump::format_age_secs(msg.created_at_ms / 1000, now / 1000);
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
            krill_app::jump::format_age_secs(msg.created_at_ms / 1000, now / 1000)
        };

        let msg_id_toggle = msg.message_id.clone();
        let key_toggle = key.clone();
        let stop_key = key.clone();
        let reply_key = key.clone();
        let stop_bot_id = bot.map(|b| b.id.clone());
        let reply_handle = bot.map_or_else(|| msg.sender_id.clone(), |b| b.slug.clone());

        let has_details = live_preview.is_some() || msg.body.lines().count() > 8;
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
                    .when(is_working, |d| {
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
                            .icon(Icon::default().data(ui::icons::STOP_SQUARE_SVG))
                            .tooltip(label)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.stop_group_turn(stop_key.clone(), bid.clone(), cx);
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
            let content = if is_working {
                live_preview.unwrap_or(msg.body.trim()).to_owned()
            } else {
                msg.body.clone()
            };
            let body_el = if expanded || !is_working {
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
                        .selectable(true),
                    )
                    .into_any_element()
            } else {
                div()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .line_clamp(3)
                    .child(content)
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
            .child(
                Button::new("grp-approval-open-chat")
                    .ghost()
                    .xsmall()
                    .label(text::OPEN_URL_BTN.get())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_bot_session(&bid, sid, cx);
                    })),
            );

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
        let mention_menu = trailing_mention_query(draft).and_then(|(at_pos, query)| {
            let needle = query.to_ascii_lowercase();
            let show_everyone = needle.is_empty() || "everyone".starts_with(&needle);
            let matched_bots: Vec<&SidebarBot> = members
                .iter()
                .filter(|b| {
                    needle.is_empty()
                        || b.name.to_ascii_lowercase().contains(&needle)
                        || b.slug.to_ascii_lowercase().contains(&needle)
                })
                .collect();
            if !show_everyone && matched_bots.is_empty() {
                return None;
            }
            let mut list = div()
                .id("group-mention-menu")
                .debug_selector(|| "group-mention-menu".into())
                .w_full()
                .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
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
            if show_everyone {
                let k = key.clone();
                let d = draft.to_owned();
                let inp = input_state.clone();
                list = list.child(
                    Button::new("grp-mention-everyone")
                        .ghost()
                        .xsmall()
                        .w_full()
                        .justify_start()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let next = insert_mention_token(&d, at_pos, "everyone");
                            inp.update(cx, |s, cx| {
                                s.set_value(next.clone(), window, cx);
                                s.focus(window, cx);
                            });
                            if let Some(r) = this.bot_ui.group_chat.rooms.get_mut(&k) {
                                r.draft = next;
                            }
                            cx.notify();
                        }))
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .text_size(ui::TEXT_XS)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(text::GRP_EVERYONE_LABEL.get()),
                                        )
                                        .child(
                                            div()
                                                .text_color(p.muted_foreground)
                                                .child(text::GRP_EVERYONE_DESC.get()),
                                        ),
                                )
                                .child(div().text_color(p.muted_foreground).child(EVERYONE_HANDLE)),
                        ),
                );
            }
            for bot in matched_bots {
                let k = key.clone();
                let d = draft.to_owned();
                let slug = bot.slug.clone();
                let inp = input_state.clone();
                let name = bot.name.clone();
                let av = self.bot_avatar(bot, 18., px(5.), p);
                let mention_handle = format!("@{slug}");
                list = list.child(
                    Button::new(SharedString::from(format!(
                        "grp-mention-{}",
                        bot.id.as_str()
                    )))
                    .ghost()
                    .xsmall()
                    .w_full()
                    .justify_start()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let next = insert_mention_token(&d, at_pos, &slug);
                        inp.update(cx, |s, cx| {
                            s.set_value(next.clone(), window, cx);
                            s.focus(window, cx);
                        });
                        if let Some(r) = this.bot_ui.group_chat.rooms.get_mut(&k) {
                            r.draft = next;
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(div().flex().items_center().gap_2().child(av).child(name))
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(mention_handle),
                            ),
                    ),
                );
            }
            Some(list)
        });

        let summary = reply_picker_summary(audience, draft, members);
        let picker_label = format!("{}: {summary}", text::GRP_REPLIES_PREFIX.get());
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
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(picker_label)
                .child(Icon::new(IconName::ChevronDown).size(px(12.))),
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
            .border_color(p.border)
            .bg(p.card)
            .flex()
            .flex_col()
            .gap_1p5()
            .on_key_down(cx.listener(|_, ev: &KeyDownEvent, _, _| {
                let _ = ev;
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
                    .child(reply_picker)
                    .child(
                        div()
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
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.stop_group_turn(stop_key.clone(), None, cx);
                                        })),
                                )
                            })
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
            .bg(p.background)
            .flex()
            .flex_col()
            .children(mention_menu)
            .child(inner_card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_app::bots::avatar::Avatar;

    fn sample_bot(id: &str, slug: &str, name: &str) -> SidebarBot {
        SidebarBot {
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
}
