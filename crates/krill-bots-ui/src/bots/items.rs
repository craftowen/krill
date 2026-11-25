// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Tab 1 — Items Inbox: status filter pills, inline composer, status/snooze (1h/24h/unsnooze),
//! `close_note`, interactive `BotItemDecision` cards, item edit, full timeline, clickable links & `MarkBotItemsOpened`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui::{
    App, Context, Entity, FontWeight, SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use krill_proto::SessionId;
use krill_proto::bots::{
    BotConfig, BotId, BotItem, BotItemDecision, BotItemLink, BotItemStatus, BotRpcRequest,
    BotRpcResponse, CreateBotItemParams, FieldPatch, UpdateBotItemParams,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::rpc::{call_checked, expect_ok, expect_variant};
use super::{OpenSession, View, clear_input_on_window, now_unix_ms, opt_input_composing};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ItemFilter {
    #[default]
    All,
    NeedsYou,
    Working,
    Waiting,
    Planned,
    Done,
    Canceled,
}

impl ItemFilter {
    pub const ALL_FILTERS: [Self; 7] = [
        Self::All,
        Self::NeedsYou,
        Self::Working,
        Self::Waiting,
        Self::Planned,
        Self::Done,
        Self::Canceled,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::All => "bots-items-filter-all",
            Self::NeedsYou => "bots-items-filter-needs-you",
            Self::Working => "bots-items-filter-working",
            Self::Waiting => "bots-items-filter-waiting",
            Self::Planned => "bots-items-filter-planned",
            Self::Done => "bots-items-filter-done",
            Self::Canceled => "bots-items-filter-canceled",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => text::FILTER_ALL.get(),
            Self::NeedsYou => text::FILTER_NEEDS_YOU.get(),
            Self::Working => text::FILTER_WORKING.get(),
            Self::Waiting => text::FILTER_WAITING.get(),
            Self::Planned => text::FILTER_PLANNED.get(),
            Self::Done => text::FILTER_DONE.get(),
            Self::Canceled => text::FILTER_CANCELED.get(),
        }
    }

    #[must_use]
    pub const fn matches(self, status: BotItemStatus) -> bool {
        match self {
            Self::All => true,
            Self::NeedsYou => matches!(status, BotItemStatus::NeedsYou),
            Self::Working => matches!(status, BotItemStatus::Working),
            Self::Waiting => matches!(status, BotItemStatus::Waiting),
            Self::Planned => matches!(status, BotItemStatus::Planned),
            Self::Done => matches!(status, BotItemStatus::Done),
            Self::Canceled => matches!(status, BotItemStatus::Canceled),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum LinkKindChoice {
    #[default]
    Url,
    Pr,
    Dispatch,
    File,
}

/// A comment or answer whose RPC send failed, shown with Retry/Discard (Orca #47485).
#[derive(Debug, Clone)]
pub(super) struct FailedComment {
    pub id: u64,
    pub text: String,
    pub is_answer: bool,
    pub option_index: Option<u32>,
    pub ask_step_key: Option<(String, u64)>,
}

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent Home/Item UI panel toggle flags"
)]
pub(super) struct ItemsState {
    pub(super) items: Vec<BotItem>,
    /// Per-bot cached items so switching/reopening Home paints immediately (Orca #47162).
    pub(super) items_cache: BTreeMap<BotId, Vec<BotItem>>,
    /// Live header facts supplied by the shell so `BotPanelHeader` scrolls inside Home.
    pub(super) header_meta: Option<super::home::HomeHeaderMeta>,
    /// Monotonic revision bumped whenever `items` changes so Home's virtualized list cache
    /// skips O(N) re-grouping and cloning on hover, scroll, and selection updates.
    pub(super) items_rev: u64,
    /// Home: virtualized row projection and `gpui::ListState` for `bots-tab-items-body`.
    pub(super) home_list: super::home::HomeListCache,
    pub(super) filter: ItemFilter,
    pub(super) new_title_input: Option<Entity<InputState>>,
    pub(super) new_desc_input: Option<Entity<InputState>>,
    pub(super) composer_subs: Vec<Subscription>,
    pub(super) comment_inputs: BTreeMap<String, Entity<InputState>>,
    pub(super) link_inputs: BTreeMap<String, Entity<InputState>>,
    pub(super) decision_inputs: BTreeMap<String, Entity<InputState>>,
    pub(super) close_note_inputs: BTreeMap<String, Entity<InputState>>,
    pub(super) per_item_subs: BTreeMap<String, Vec<Subscription>>,
    pub(super) link_kinds: BTreeMap<String, LinkKindChoice>,
    pub(super) expanded_item_uid: Option<String>,
    pub(super) editing_item_uid: Option<String>,
    pub(super) edit_title_input: Option<Entity<InputState>>,
    pub(super) edit_desc_input: Option<Entity<InputState>>,
    pub(super) edit_done_input: Option<Entity<InputState>>,
    pub(super) busy: bool,
    /// Home (the right sidebar): Orca's "Needs you only" filter.
    pub(super) needs_you_only: bool,
    /// Home: the item open in the list's place.
    pub(super) open_uid: Option<String>,
    /// Home: sections folded or unfolded away from their default.
    pub(super) folds: std::collections::BTreeSet<&'static str>,
    /// Home: rows a group shows after "Show N more".
    pub(super) group_shown: BTreeMap<&'static str, usize>,
    /// Home: the row under the pointer, which shows its actions.
    pub(super) hover_uid: Option<String>,
    /// Home: open row/automation dropdown menu key so hover-out keeps the popover mounted & visible.
    pub(super) open_menu_key: Option<String>,
    /// Home: the open item's description is unclamped.
    pub(super) desc_open: bool,
    /// Home: comment event IDs the reader explicitly expanded past the 8-line clamp.
    pub(super) open_comments: std::collections::BTreeSet<u64>,
    /// Home: asking comment IDs that waited while this item was open, kept un-clamped until
    /// folded (`#47076`).
    pub(super) asked_here: std::collections::BTreeSet<u64>,
    /// Home: multi-question asks whose remaining lines were skipped via "send anyway" (`#47904`).
    pub(super) skipped_asks: std::collections::BTreeSet<(String, u64)>,
    /// Home: comments/answers whose send was refused by the host, with Retry/Discard (`#47485`).
    pub(super) failed_comments: BTreeMap<String, Vec<FailedComment>>,
    pub(super) next_failed_id: u64,
    /// Home: whether the Suggestions section shows Look around live steps (`BotLookSteps`).
    pub(super) look_steps_open: bool,
    /// Home: when the bot's automations ran and run next.
    pub(super) upcoming: Option<krill_proto::bots::BotUpcoming>,
    /// Home: why the last Look now failed.
    pub(super) look_error: Option<String>,
    /// Home: each bot's suggestions seen mark as its Home opened, so new rows keep their dot.
    pub(super) up_next_seen_at_open: BTreeMap<BotId, u64>,
    /// Home: latest `MarkUpNextSeen` watermark already sent per `(host, bot)` to avoid render-loop RPC storms.
    pub(super) up_next_seen_sent: BTreeMap<(crate::hosts::HostId, BotId), u64>,
    /// Home: the Snooze until dialog, while open.
    pub(super) snooze_pick: Option<super::home_snooze::SnoozePick>,
    /// Home: rows checked for a batch action (Orca's selection pill).
    pub(super) selected: std::collections::BTreeSet<String>,
    /// Home: the row last checked, where a shift-click range starts.
    pub(super) select_anchor: Option<String>,
    /// Home: the open rows in the list's order, for ranges and stepping.
    pub(super) shown_order: Vec<String>,
    /// Home: the Waiting note dialog, while open (`#47308`/`#47309`).
    pub(super) cancel_ask: Option<super::home_select::WaitAsk>,
    /// Home: Stale's "Cancel all" asks to confirm.
    pub(super) stale_confirm: Option<()>,
    /// Home: each bot's item seen mark as its Home opened, so rows changed since keep their dot.
    pub(super) seen_at_open: BTreeMap<BotId, u64>,
    /// Home: reads the automations' runs again each minute, so "next" moves on (Orca `useBotNow`).
    pub(super) upcoming_poll: Option<gpui::Task<()>>,
    /// Home: in-flight automation toggle / schedule-now RPC task.
    pub(super) home_rpc_task: Option<gpui::Task<()>>,
    /// Monotonic generation bumped on host/bot switch to discard stale item/home RPC completions.
    pub(super) rpc_gen: u64,
    /// Home: PR chips' state by URL.
    pub(super) pr_reads: BTreeMap<String, super::home_item_parts::PrRead>,
    /// Home: "changed {field}" rows opened (`event:field`).
    pub(super) open_changes: std::collections::BTreeSet<String>,
    /// Home: an open ask of several questions, per (item, event).
    pub(super) ask_steps: BTreeMap<(String, u64), super::home_item_parts::AskSteps>,
    /// Home: the bot folder's files, for @ mentions, keyed by `(host, bot, root)`.
    pub(super) mention_files: BTreeMap<(crate::hosts::HostId, BotId, PathBuf), Vec<String>>,
    /// Home: currently scanning `(host, bot, root)` for @ mentions.
    pub(super) mention_scanning: Option<(crate::hosts::HostId, BotId, PathBuf)>,
    /// Home: background task scanning files for @ mentions.
    pub(super) mention_scan_task: Option<gpui::Task<()>>,
    /// Home: items whose Activity shows its earlier events too.
    pub(super) events_all: std::collections::BTreeSet<String>,
    /// Home: files attached to a comment not sent yet, per item.
    pub(super) attachments: BTreeMap<String, Vec<PathBuf>>,
    /// Home: the waiting ask whose "Replying to" mark was hidden, per item.
    pub(super) reply_hidden: BTreeMap<String, u64>,
    /// Home: each bot session's agent state, from the shell (its waiting chats are asks).
    pub(super) session_states: BTreeMap<SessionId, krill_proto::control::AgentState>,
    /// Connectors: webhook rule ID whose URL was just copied (`#47479`).
    pub(super) copied_webhook_id: Option<String>,
}

impl ItemsState {
    pub(super) fn bump_items_rev(&mut self) {
        self.items_rev = self.items_rev.wrapping_add(1);
    }

    pub(super) fn set_items(&mut self, items: Vec<BotItem>) {
        if let Some(bot_id) = items.first().map(|i| i.owner_bot_id.clone()) {
            self.items_cache.insert(bot_id, items.clone());
        }
        self.items = items;
        self.bump_items_rev();
    }

    pub(super) fn set_items_for_bot(&mut self, bot_id: &BotId, items: Vec<BotItem>) {
        self.items_cache.insert(bot_id.clone(), items.clone());
        self.items = items;
        self.bump_items_rev();
    }

    pub(super) fn restore_cached(&mut self, bot_id: &BotId) -> bool {
        if let Some(cached) = self.items_cache.get(bot_id).cloned() {
            self.items = cached;
            self.bump_items_rev();
            true
        } else {
            false
        }
    }

    pub(super) fn clear_items(&mut self) {
        self.items.clear();
        self.bump_items_rev();
    }

    pub(super) fn push_front_item(&mut self, item: BotItem) {
        let bot_id = item.owner_bot_id.clone();
        self.items.insert(0, item);
        self.items_cache.insert(bot_id, self.items.clone());
        self.bump_items_rev();
    }

    /// Clears cached Home/Up-Next seen watermarks for `bot_id` so returning to Home refreshes dots.
    pub(super) fn clear_seen_for(&mut self, bot_id: &BotId) {
        self.seen_at_open.remove(bot_id);
        self.up_next_seen_at_open.remove(bot_id);
        self.home_list.seen_checked = None;
    }

    /// Clears all cached Home/Up-Next seen watermarks (e.g. on host switch).
    pub(super) fn clear_all_seen(&mut self) {
        self.seen_at_open.clear();
        self.up_next_seen_at_open.clear();
        self.home_list.seen_checked = None;
    }

    /// Cancels in-flight Home/Items background tasks and resets switch-sensitive state.
    pub(super) fn cancel_tasks_on_switch(&mut self) {
        self.upcoming_poll = None;
        self.home_rpc_task = None;
        self.mention_scan_task = None;
        self.mention_scanning = None;
        self.open_menu_key = None;
        self.rpc_gen = self.rpc_gen.wrapping_add(1);
        self.busy = false;
        self.up_next_seen_sent.clear();
    }

    /// Returns whether any text `InputState` owned by Items/Home currently holds keyboard focus.
    pub(super) fn any_input_focused(&self, window: &Window, app: &App) -> bool {
        use gpui::Focusable as _;
        let focused = |opt: Option<&Entity<InputState>>| {
            opt.is_some_and(|i| i.read(app).focus_handle(app).is_focused(window))
        };
        focused(self.new_title_input.as_ref())
            || focused(self.new_desc_input.as_ref())
            || focused(self.edit_title_input.as_ref())
            || focused(self.edit_desc_input.as_ref())
            || focused(self.edit_done_input.as_ref())
            || focused(self.cancel_ask.as_ref().and_then(|a| a.input.as_ref()))
            || self
                .comment_inputs
                .values()
                .chain(self.link_inputs.values())
                .chain(self.decision_inputs.values())
                .chain(self.close_note_inputs.values())
                .any(|i| i.read(app).focus_handle(app).is_focused(window))
    }

    pub(super) fn items_input_composing(&self, window: &mut Window, app: &mut App) -> bool {
        self.input_is_composing(window, app)
    }

    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        opt_input_composing(self.new_title_input.as_ref(), window, app)
            || opt_input_composing(self.new_desc_input.as_ref(), window, app)
            || opt_input_composing(self.edit_title_input.as_ref(), window, app)
            || opt_input_composing(self.edit_desc_input.as_ref(), window, app)
            || opt_input_composing(self.edit_done_input.as_ref(), window, app)
            || opt_input_composing(
                self.cancel_ask.as_ref().and_then(|a| a.input.as_ref()),
                window,
                app,
            )
            || self
                .comment_inputs
                .values()
                .chain(self.link_inputs.values())
                .chain(self.decision_inputs.values())
                .chain(self.close_note_inputs.values())
                .any(|i| crate::jump::entity_input_is_composing(i, window, app))
    }

    pub(super) fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<View>) {
        if let Some(i) = &self.new_title_input {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::NEW_ITEM_TITLE_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.new_desc_input {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::NEW_ITEM_DESC_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.edit_title_input {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::ITEM_EDIT_TITLE_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.edit_desc_input {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::ITEM_EDIT_DESC_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.edit_done_input {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::ITEM_EDIT_DONE_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = self.cancel_ask.as_ref().and_then(|a| a.input.as_ref()) {
            i.update(cx, |s, cx| {
                s.set_placeholder(
                    crate::i18n::catalog::bot_home::WAIT_ON_PLACEHOLDER.get(),
                    window,
                    cx,
                );
            });
        }
        for i in self.comment_inputs.values() {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::COMMENT_PLACEHOLDER.get(), window, cx);
            });
        }
        for i in self.link_inputs.values() {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::LINK_TARGET_PLACEHOLDER.get(), window, cx);
            });
        }
        for i in self.decision_inputs.values() {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::DECISION_ANSWER_PLACEHOLDER.get(), window, cx);
            });
        }
        for i in self.close_note_inputs.values() {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::ITEM_CLOSE_NOTE_PLACEHOLDER.get(), window, cx);
            });
        }
    }
}

fn status_badge_spec(status: BotItemStatus) -> (ui::BadgeTone, &'static str) {
    match status {
        BotItemStatus::Planned => (ui::BadgeTone::Muted, text::STATUS_PLANNED.get()),
        BotItemStatus::Working => (ui::BadgeTone::Accent, text::STATUS_WORKING.get()),
        BotItemStatus::Waiting => (ui::BadgeTone::Muted, text::STATUS_WAITING.get()),
        BotItemStatus::NeedsYou => (ui::BadgeTone::Warning, text::STATUS_NEEDS_YOU.get()),
        BotItemStatus::Done => (ui::BadgeTone::Success, text::STATUS_DONE.get()),
        BotItemStatus::Canceled => (ui::BadgeTone::Destructive, text::STATUS_CANCELED.get()),
    }
}

enum LinkAction {
    OpenUrl(String),
    DispatchSession(SessionId),
    RevealFile(PathBuf),
    None,
}

fn link_chip_parts(link: &BotItemLink) -> (&'static str, &str, LinkAction) {
    match link {
        BotItemLink::Pr(v) => {
            let act = super::sanitize_http_url(v).map_or(LinkAction::None, LinkAction::OpenUrl);
            (text::LINK_PR.get(), v.as_str(), act)
        }
        BotItemLink::Dispatch(v) => {
            let act = v.trim().parse::<u64>().map_or(LinkAction::None, |num| {
                LinkAction::DispatchSession(SessionId(num))
            });
            (text::LINK_DISPATCH.get(), v.as_str(), act)
        }
        BotItemLink::Url(v) => {
            let act = super::sanitize_http_url(v).map_or(LinkAction::None, LinkAction::OpenUrl);
            (text::LINK_URL.get(), v.as_str(), act)
        }
        BotItemLink::File(v) => {
            let trimmed = v.trim();
            let act = if trimmed.is_empty() {
                LinkAction::None
            } else {
                LinkAction::RevealFile(PathBuf::from(trimmed))
            };
            (text::LINK_FILE.get(), v.as_str(), act)
        }
    }
}

impl View {
    pub fn set_item_filter(&mut self, filter: ItemFilter, cx: &mut Context<Self>) {
        self.items.filter = filter;
        cx.notify();
    }

    fn ensure_items_inputs(
        &mut self,
        _bot_id: &BotId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        if self.items.new_title_input.is_none() {
            let title_inp = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::NEW_ITEM_TITLE_PLACEHOLDER.get())
            });
            let sub = cx.subscribe_in(
                &title_inp,
                window,
                |this, inp, ev: &InputEvent, window, cx| {
                    if matches!(
                        ev,
                        InputEvent::PressEnter {
                            secondary: false,
                            shift: false,
                        }
                    ) && !crate::jump::entity_input_is_composing(inp, window, cx)
                        && let Some(bid) = this.selected_bot_id.clone()
                    {
                        let title = inp.read(cx).value().to_string();
                        let desc = this
                            .items
                            .new_desc_input
                            .as_ref()
                            .map(|d| d.read(cx).value().to_string());
                        this.create_bot_item(bid, &title, desc, Some(window), cx);
                    }
                },
            );
            self.items.composer_subs.push(sub);
            self.items.new_title_input = Some(title_inp);
        }
        if self.items.new_desc_input.is_none() {
            let desc_inp = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::NEW_ITEM_DESC_PLACEHOLDER.get())
            });
            let sub = cx.subscribe_in(
                &desc_inp,
                window,
                |this, inp, ev: &InputEvent, window, cx| {
                    if matches!(
                        ev,
                        InputEvent::PressEnter {
                            secondary: false,
                            shift: false,
                        }
                    ) && !crate::jump::entity_input_is_composing(inp, window, cx)
                        && let Some(bid) = this.selected_bot_id.clone()
                    {
                        let title = this
                            .items
                            .new_title_input
                            .as_ref()
                            .map(|t| t.read(cx).value().to_string())
                            .unwrap_or_default();
                        let desc = inp.read(cx).value().to_string();
                        this.create_bot_item(bid, &title, Some(desc), Some(window), cx);
                    }
                },
            );
            self.items.composer_subs.push(sub);
            self.items.new_desc_input = Some(desc_inp);
        }
    }

    pub(super) fn ensure_per_item_inputs(
        &mut self,
        uid: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        if !self.items.comment_inputs.contains_key(uid) {
            let input = cx
                .new(|cx| InputState::new(window, cx).placeholder(text::COMMENT_PLACEHOLDER.get()));
            let uid_owned = uid.to_owned();
            let input_clone = input.clone();
            let sub = cx.subscribe_in(
                &input,
                window,
                move |this, inp, ev: &InputEvent, window, cx| match ev {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter {
                        secondary: false,
                        shift: false,
                    } if !crate::jump::entity_input_is_composing(inp, window, cx) => {
                        let body = inp.read(cx).value().to_string();
                        this.send_item_comment(
                            uid_owned.clone(),
                            &body,
                            Some(input_clone.clone()),
                            cx,
                        );
                    }
                    _ => {}
                },
            );
            self.items
                .per_item_subs
                .entry(uid.to_owned())
                .or_default()
                .push(sub);
            self.items.comment_inputs.insert(uid.to_owned(), input);
        }
        if !self.items.link_inputs.contains_key(uid) {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::LINK_TARGET_PLACEHOLDER.get())
            });
            let uid_owned = uid.to_owned();
            let input_clone = input.clone();
            let sub = cx.subscribe_in(
                &input,
                window,
                move |this, inp, ev: &InputEvent, window, cx| {
                    if matches!(
                        ev,
                        InputEvent::PressEnter {
                            secondary: false,
                            shift: false,
                        }
                    ) && !crate::jump::entity_input_is_composing(inp, window, cx)
                    {
                        let target = inp.read(cx).value().to_string();
                        this.submit_item_link(
                            uid_owned.clone(),
                            &target,
                            Some(input_clone.clone()),
                            cx,
                        );
                    }
                },
            );
            self.items
                .per_item_subs
                .entry(uid.to_owned())
                .or_default()
                .push(sub);
            self.items.link_inputs.insert(uid.to_owned(), input);
        }
        if !self.items.close_note_inputs.contains_key(uid) {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::ITEM_CLOSE_NOTE_PLACEHOLDER.get())
            });
            self.items.close_note_inputs.insert(uid.to_owned(), input);
        }
    }

    fn ensure_decision_input(
        &mut self,
        uid: &str,
        decision_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<InputState>> {
        if !cx.has_global::<gpui_component::Theme>() {
            return None;
        }
        let key = format!("{uid}:{decision_id}");
        if let Some(existing) = self.items.decision_inputs.get(&key) {
            return Some(existing.clone());
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(text::DECISION_ANSWER_PLACEHOLDER.get())
        });
        let uid_owned = uid.to_owned();
        let dec_id_owned = decision_id.to_owned();
        let input_clone = input.clone();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, inp, ev: &InputEvent, window, cx| {
                if matches!(
                    ev,
                    InputEvent::PressEnter {
                        secondary: false,
                        shift: false,
                    }
                ) && !crate::jump::entity_input_is_composing(inp, window, cx)
                {
                    let ans = inp.read(cx).value().to_string();
                    this.answer_item_decision_for(
                        uid_owned.clone(),
                        Some(ans),
                        None,
                        Some(dec_id_owned.clone()),
                        Some(input_clone.clone()),
                        cx,
                    );
                }
            },
        );
        self.items
            .per_item_subs
            .entry(key.clone())
            .or_default()
            .push(sub);
        self.items.decision_inputs.insert(key, input.clone());
        Some(input)
    }

    pub fn create_bot_item(
        &mut self,
        bot_id: BotId,
        title: &str,
        description: Option<String>,
        _window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if self.items.busy {
            return;
        }
        let title = title.trim().to_owned();
        if title.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            cx.notify();
            return;
        }
        // Keep inputs intact until RPC succeeds so network/daemon errors do not erase drafts.
        let rpc = self.rpc.clone();
        let req_host = self.host.clone();
        let req_gen = self.items.rpc_gen;
        self.items.busy = true;
        self.error = None;
        self.notice = None;
        self.item_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    expect_variant(
                        &rpc,
                        &BotRpcRequest::CreateItem(Box::new(CreateBotItemParams {
                            bot_id: bot_id_bg,
                            title,
                            description: description.filter(|s| !s.trim().is_empty()),
                            done_means: None,
                            status: Some(BotItemStatus::Planned),
                            links: Vec::new(),
                            waiting_reason: None,
                        })),
                        |r| match r {
                            BotRpcResponse::Item(item) => Some(*item),
                            _ => None,
                        },
                        "Item",
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host || this.items.rpc_gen != req_gen {
                    return;
                }
                this.items.busy = false;
                match res {
                    Ok(item) => {
                        let owner_id = item.owner_bot_id.clone();
                        this.upsert_item(item, &owner_id);
                        let handle = this.window_handle;
                        clear_input_on_window(&mut this.items.new_title_input, handle, cx);
                        clear_input_on_window(&mut this.items.new_desc_input, handle, cx);
                        this.emit_badge(cx);
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn update_item_status(
        &mut self,
        bot_id: BotId,
        item_uid: String,
        status: BotItemStatus,
        cx: &mut Context<Self>,
    ) {
        let was_waiting = self
            .items
            .items
            .iter()
            .find(|i| i.uid == item_uid)
            .is_some_and(|i| super::home::waiting_note(i).is_some());
        let close_note = if matches!(status, BotItemStatus::Done | BotItemStatus::Canceled) {
            self.items
                .close_note_inputs
                .get(&item_uid)
                .map(|i| i.read(cx).value().trim().to_owned())
                .filter(|s| !s.is_empty())
                .or_else(|| was_waiting.then(String::new))
        } else if was_waiting {
            Some(String::new())
        } else {
            None
        };
        let note_input = self.items.close_note_inputs.get(&item_uid).cloned();
        self.dispatch_item_update(
            bot_id,
            UpdateBotItemParams {
                item_id: item_uid,
                title: None,
                description: None,
                done_means: None,
                status: Some(status),
                comment: None,
                decisions: Vec::new(),
                close_note,
                snoozed_until_ms: FieldPatch::Unchanged,
                waiting_reason: None,
            },
            note_input,
            false,
            cx,
        );
    }

    pub fn save_item_edits(&mut self, bot_id: BotId, item_uid: String, cx: &mut Context<Self>) {
        let title = self
            .items
            .edit_title_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty());
        if title.is_none() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            cx.notify();
            return;
        }
        let description = self
            .items
            .edit_desc_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned());
        let done_means = self
            .items
            .edit_done_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned());
        self.dispatch_item_update(
            bot_id,
            UpdateBotItemParams {
                item_id: item_uid,
                title,
                description,
                done_means,
                status: None,
                comment: None,
                decisions: Vec::new(),
                close_note: None,
                snoozed_until_ms: FieldPatch::Unchanged,
                waiting_reason: None,
            },
            None,
            true,
            cx,
        );
    }

    pub fn snooze_item(
        &mut self,
        bot_id: BotId,
        item_uid: String,
        until_ms: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let patch = match until_ms {
            Some(ts) => FieldPatch::Set(ts),
            None => FieldPatch::Clear,
        };
        self.dispatch_item_update(
            bot_id,
            UpdateBotItemParams {
                item_id: item_uid,
                title: None,
                description: None,
                done_means: None,
                status: None,
                comment: None,
                decisions: Vec::new(),
                close_note: None,
                snoozed_until_ms: patch,
                waiting_reason: None,
            },
            None,
            false,
            cx,
        );
    }

    fn dispatch_item_update(
        &mut self,
        bot_id: BotId,
        params: UpdateBotItemParams,
        clear_on_ok: Option<Entity<InputState>>,
        close_edit: bool,
        cx: &mut Context<Self>,
    ) {
        if self.items.busy {
            return;
        }
        let rpc = self.rpc.clone();
        let req_host = self.host.clone();
        let req_gen = self.items.rpc_gen;
        self.items.busy = true;
        self.error = None;
        self.notice = None;
        self.item_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(
                    async move { call_checked(&rpc, &BotRpcRequest::UpdateItem(Box::new(params))) },
                )
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host || this.items.rpc_gen != req_gen {
                    return;
                }
                this.items.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        let owner_id = updated.owner_bot_id.clone();
                        this.upsert_item(*updated, &owner_id);
                        let _ = &bot_id;
                        if close_edit {
                            this.items.editing_item_uid = None;
                        }
                        if let Some(inp) = clear_on_ok {
                            let mut slot = Some(inp);
                            clear_input_on_window(&mut slot, this.window_handle, cx);
                        }
                        this.emit_badge(cx);
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected UpdateItem response: {other:?}"
                        )));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn submit_item_comment(
        &mut self,
        item_uid: String,
        body: &str,
        clear_input: Option<Entity<InputState>>,
        cx: &mut Context<Self>,
    ) {
        self.submit_item_comment_with_attachments(item_uid, body, clear_input, false, cx);
    }

    pub(super) fn submit_item_comment_with_attachments(
        &mut self,
        item_uid: String,
        body: &str,
        clear_input: Option<Entity<InputState>>,
        clear_attachments: bool,
        cx: &mut Context<Self>,
    ) {
        if self.items.busy {
            return;
        }
        let trimmed = body.trim().to_owned();
        if trimmed.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            self.notice = None;
            cx.notify();
            return;
        }
        let rpc = self.rpc.clone();
        let req_host = self.host.clone();
        let req_gen = self.items.rpc_gen;
        let uid_for_clear = item_uid.clone();
        let body_for_retry = trimmed.clone();
        self.items.busy = true;
        self.error = None;
        self.notice = None;
        self.item_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::AddItemComment {
                            item_id: item_uid,
                            author: "owner".to_owned(),
                            text: trimmed,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host || this.items.rpc_gen != req_gen {
                    return;
                }
                this.items.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        let owner_id = updated.owner_bot_id.clone();
                        this.upsert_item(*updated, &owner_id);
                        if clear_attachments {
                            this.items.attachments.remove(&uid_for_clear);
                        }
                        if let Some(inp) = clear_input {
                            let mut slot = Some(inp);
                            clear_input_on_window(&mut slot, this.window_handle, cx);
                        }
                        this.emit_badge(cx);
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected AddItemComment response: {other:?}"
                        )));
                        this.record_failed_comment(
                            &uid_for_clear,
                            body_for_retry,
                            false,
                            None,
                            None,
                            cx,
                        );
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                        this.record_failed_comment(
                            &uid_for_clear,
                            body_for_retry,
                            false,
                            None,
                            None,
                            cx,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn submit_item_link(
        &mut self,
        item_uid: String,
        target: &str,
        clear_input: Option<Entity<InputState>>,
        cx: &mut Context<Self>,
    ) {
        if self.items.busy {
            return;
        }
        let trimmed = target.trim().to_owned();
        if trimmed.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            self.notice = None;
            cx.notify();
            return;
        }
        let kind = self
            .items
            .link_kinds
            .get(&item_uid)
            .copied()
            .unwrap_or_default();
        let link = match kind {
            LinkKindChoice::Url => BotItemLink::Url(trimmed),
            LinkKindChoice::Pr => BotItemLink::Pr(trimmed),
            LinkKindChoice::Dispatch => BotItemLink::Dispatch(trimmed),
            LinkKindChoice::File => BotItemLink::File(trimmed),
        };
        let rpc = self.rpc.clone();
        let req_host = self.host.clone();
        let req_gen = self.items.rpc_gen;
        self.items.busy = true;
        self.error = None;
        self.notice = None;
        self.item_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::AddItemLink {
                            item_id: item_uid,
                            link,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host || this.items.rpc_gen != req_gen {
                    return;
                }
                this.items.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        let owner_id = updated.owner_bot_id.clone();
                        this.upsert_item(*updated, &owner_id);
                        if let Some(inp) = clear_input {
                            let mut slot = Some(inp);
                            clear_input_on_window(&mut slot, this.window_handle, cx);
                        }
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected AddItemLink response: {other:?}"
                        )));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn answer_item_decision(
        &mut self,
        item_uid: String,
        answer_text: Option<String>,
        option_index: Option<u32>,
        clear_input: Option<Entity<InputState>>,
        cx: &mut Context<Self>,
    ) {
        self.answer_item_decision_with_cleanup(
            item_uid,
            answer_text,
            option_index,
            None,
            clear_input,
            false,
            None,
            cx,
        );
    }

    pub(super) fn answer_item_decision_for(
        &mut self,
        item_uid: String,
        answer_text: Option<String>,
        option_index: Option<u32>,
        decision_id: Option<String>,
        clear_input: Option<Entity<InputState>>,
        cx: &mut Context<Self>,
    ) {
        self.answer_item_decision_with_cleanup(
            item_uid,
            answer_text,
            option_index,
            decision_id,
            clear_input,
            false,
            None,
            cx,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "coordinates optional attachment and multi-step cleanup on RPC success"
    )]
    pub(super) fn answer_item_decision_with_cleanup(
        &mut self,
        item_uid: String,
        answer_text: Option<String>,
        option_index: Option<u32>,
        decision_id: Option<String>,
        clear_input: Option<Entity<InputState>>,
        clear_attachments: bool,
        clear_ask_step: Option<(String, u64)>,
        cx: &mut Context<Self>,
    ) {
        if self.items.busy {
            return;
        }
        let cleaned_text = answer_text.map(|s| s.trim().to_owned());
        if option_index.is_none() && cleaned_text.as_deref().is_none_or(str::is_empty) {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            self.notice = None;
            cx.notify();
            return;
        }
        let rpc = self.rpc.clone();
        let req_host = self.host.clone();
        let req_gen = self.items.rpc_gen;
        let selected_bot_id = self.selected_bot_id.clone();
        let uid_for_clear = item_uid.clone();
        let text_for_recover = cleaned_text.clone();
        let ask_step_for_recover = clear_ask_step.clone();
        self.items.busy = true;
        self.error = None;
        self.notice = None;
        self.item_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::AnswerBotItem {
                            item_id: item_uid,
                            answer_text: cleaned_text,
                            option_index,
                            decision_id,
                            request_id: None,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host || this.items.rpc_gen != req_gen {
                    return;
                }
                this.items.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        let owner_id = updated.owner_bot_id.clone();
                        this.upsert_item(*updated, &owner_id);
                        if clear_attachments {
                            this.items.attachments.remove(&uid_for_clear);
                        }
                        if let Some(key) = clear_ask_step {
                            this.items.ask_steps.remove(&key);
                        }
                        if let Some(inp) = clear_input {
                            let mut slot = Some(inp);
                            clear_input_on_window(&mut slot, this.window_handle, cx);
                        }
                        this.emit_badge(cx);
                    }
                    Ok(_) => {
                        if clear_attachments {
                            this.items.attachments.remove(&uid_for_clear);
                        }
                        if let Some(key) = clear_ask_step {
                            this.items.ask_steps.remove(&key);
                        }
                        if let Some(inp) = clear_input {
                            let mut slot = Some(inp);
                            clear_input_on_window(&mut slot, this.window_handle, cx);
                        }
                        if let Some(bot_id) = selected_bot_id
                            && this.selected_bot_id.as_ref() == Some(&bot_id)
                        {
                            this.load_selected_bot_detail(bot_id, cx);
                        }
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                        // Orca #47485: restore a refused answer into the comment box and keep
                        // Retry/Discard available so the owner never loses typed or picked answers.
                        if let Some(words) = text_for_recover.filter(|w| !w.is_empty()) {
                            this.restore_comment_draft(&uid_for_clear, &words, cx);
                            this.record_failed_comment(
                                &uid_for_clear,
                                words,
                                true,
                                option_index,
                                ask_step_for_recover,
                                cx,
                            );
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Quietly records that the owner opened Home and saw the bot's items, without blocking
    /// item actions (`items.busy`) or flashing a "Saved" notice banner.
    pub(super) fn mark_bot_items_opened_silently(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        self.mark_items_seen_task = Some(cx.spawn(async move |_this, cx| {
            drop(
                cx.background_executor()
                    .spawn(async move {
                        expect_ok(
                            &rpc,
                            &BotRpcRequest::MarkBotItemsOpened {
                                bot_id,
                                up_to_event_id: None,
                            },
                        )
                    })
                    .await,
            );
        }));
    }

    pub fn mark_bot_items_opened(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        let req_host = self.host.clone();
        let req_gen = self.items.rpc_gen;
        self.error = None;
        self.notice = None;
        self.mark_items_seen_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    expect_ok(
                        &rpc,
                        &BotRpcRequest::MarkBotItemsOpened {
                            bot_id,
                            up_to_event_id: None,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host || this.items.rpc_gen != req_gen {
                    return;
                }
                match res {
                    Ok(()) => this.notice = Some(SharedString::from(text::SAVED.get())),
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Restores a refused answer or recovered draft into the item's comment box (`#47485`).
    pub(super) fn restore_comment_draft(
        &mut self,
        item_uid: &str,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.items.comment_inputs.get(item_uid).cloned() else {
            return;
        };
        let Some(handle) = self.window_handle else {
            return;
        };
        let text = text.to_owned();
        drop(handle.update(cx, |_, window, cx| {
            input.update(cx, |state, cx| {
                if state.value().trim().is_empty() {
                    state.set_value(text, window, cx);
                }
                state.focus(window, cx);
            });
        }));
    }

    pub(super) fn record_failed_comment(
        &mut self,
        item_uid: &str,
        text: String,
        is_answer: bool,
        option_index: Option<u32>,
        ask_step_key: Option<(String, u64)>,
        _cx: &mut Context<Self>,
    ) {
        self.items.next_failed_id = self.items.next_failed_id.wrapping_add(1);
        let id = self.items.next_failed_id;
        self.items
            .failed_comments
            .entry(item_uid.to_owned())
            .or_default()
            .push(FailedComment {
                id,
                text,
                is_answer,
                option_index,
                ask_step_key,
            });
    }

    pub(super) fn upsert_item(&mut self, updated: BotItem, _bot_id: &BotId) {
        let owner_id = updated.owner_bot_id.clone();
        if let Some(slot) = self
            .group_panel
            .all_items
            .iter_mut()
            .find(|i| i.uid == updated.uid)
        {
            *slot = updated.clone();
        } else if self.group_panel.active && self.group_panel.member_ids.contains(&owner_id) {
            self.group_panel.all_items.insert(0, updated.clone());
        }
        let visible_in_current_list = if self.group_panel.active {
            self.group_panel.member_ids.contains(&owner_id)
        } else {
            self.selected_bot_id
                .as_ref()
                .is_none_or(|selected| selected == &owner_id)
        };
        if visible_in_current_list {
            if let Some(slot) = self.items.items.iter_mut().find(|i| i.uid == updated.uid) {
                *slot = updated.clone();
            } else {
                self.items.items.insert(0, updated.clone());
            }
            if !self.group_panel.active {
                self.items
                    .items_cache
                    .insert(owner_id.clone(), self.items.items.clone());
            } else if let Some(cached) = self.items.items_cache.get_mut(&owner_id) {
                if let Some(slot) = cached.iter_mut().find(|i| i.uid == updated.uid) {
                    *slot = updated;
                } else {
                    cached.insert(0, updated);
                }
            }
            self.items.bump_items_rev();
            self.sync_open_items_for_bot(&owner_id);
        } else {
            // User switched away from `owner_id` while the item mutation RPC was in flight.
            // Update cached items and `all_open_items` in place without polluting the active bot's
            // `self.items.items` or wiping `owner_id`'s other open items.
            if let Some(cached) = self.items.items_cache.get_mut(&owner_id) {
                if let Some(slot) = cached.iter_mut().find(|i| i.uid == updated.uid) {
                    *slot = updated.clone();
                } else {
                    cached.insert(0, updated.clone());
                }
            }
            if updated.status.is_open() {
                if let Some(slot) = self
                    .all_open_items
                    .iter_mut()
                    .find(|i| i.uid == updated.uid)
                {
                    *slot = updated;
                } else {
                    self.all_open_items.push(updated);
                }
            } else {
                self.all_open_items.retain(|i| i.uid != updated.uid);
            }
        }
    }

    pub(super) fn render_items_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_items_inputs(&bot.id, window, cx);
        let item_uids: Vec<String> = self
            .items
            .items
            .iter()
            .filter(|i| i.owner_bot_id == bot.id)
            .map(|i| i.uid.clone())
            .collect();
        let has_bot_items = !item_uids.is_empty();
        for uid in item_uids {
            self.ensure_per_item_inputs(&uid, window, cx);
        }

        let bot_id_create = bot.id.clone();
        let bot_id_mark_read = bot.id.clone();

        let mut filters = div()
            .id("bots-items-filters")
            .debug_selector(|| "bots-items-filters".into())
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2();

        let mut pill_row = div().flex().flex_wrap().items_center().gap_1();
        for filter in ItemFilter::ALL_FILTERS {
            let count = self
                .items
                .items
                .iter()
                .filter(|item| item.owner_bot_id == bot.id && filter.matches(item.status))
                .count();
            let fid = filter.id();
            pill_row = pill_row.child(
                ui::segment(
                    p,
                    fid,
                    format!("{} ({count})", filter.label()),
                    self.items.filter == filter,
                )
                .debug_selector(move || fid.into())
                .on_click(cx.listener(move |this, _, _, cx| this.set_item_filter(filter, cx))),
            );
        }
        filters = filters.child(pill_row).when(has_bot_items, |d| {
            d.child(
                ui::ghost("bots-items-mark-opened", text::ITEM_MARK_READ_BTN.get())
                    .disabled(self.items.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mark_bot_items_opened(bot_id_mark_read.clone(), cx);
                    })),
            )
        });

        let composer = ui::card(p)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(
                        self.items
                            .new_title_input
                            .as_ref()
                            .map(|i| div().flex_1().min_w_0().child(Input::new(i).small())),
                    )
                    .child(
                        ui::primary("bots-item-add-btn", text::ADD_ITEM_BTN.get())
                            .debug_selector(|| "bots-item-add-btn".into())
                            .disabled(self.items.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let title = this
                                    .items
                                    .new_title_input
                                    .as_ref()
                                    .map(|i| i.read(cx).value().to_string())
                                    .unwrap_or_default();
                                let desc = this
                                    .items
                                    .new_desc_input
                                    .as_ref()
                                    .map(|i| i.read(cx).value().to_string());
                                this.create_bot_item(
                                    bot_id_create.clone(),
                                    &title,
                                    desc,
                                    Some(window),
                                    cx,
                                );
                            })),
                    ),
            )
            .children(
                self.items
                    .new_desc_input
                    .as_ref()
                    .map(|i| div().w_full().child(Input::new(i).small())),
            );

        let filtered: Vec<BotItem> = self
            .items
            .items
            .iter()
            .filter(|item| item.owner_bot_id == bot.id && self.items.filter.matches(item.status))
            .cloned()
            .collect();

        let mut list = div()
            .id("bots-items-list")
            .debug_selector(|| "bots-items-list".into())
            .flex()
            .flex_col()
            .gap_2p5();

        if filtered.is_empty() {
            list = list.child(
                ui::card(p)
                    .debug_selector(|| "bots-items-empty".into())
                    .py_6()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::ITEMS_EMPTY.get()),
                    ),
            );
        } else {
            for item in &filtered {
                list = list.child(self.render_item_card(bot, item, p, window, cx));
            }
        }

        div()
            .id("bots-tab-items-body")
            .debug_selector(|| "bots-tab-items-body".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(filters)
            .child(composer)
            .child(list)
    }

    fn render_item_card(
        &mut self,
        bot: &BotConfig,
        item: &BotItem,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (tone, status_label) = status_badge_spec(item.status);
        let now = now_unix_ms();
        let is_snoozed = super::home::is_snoozed(item, now);
        let uid = item.uid.clone();
        let expanded = self.items.expanded_item_uid.as_deref() == Some(uid.as_str());
        let is_editing = self.items.editing_item_uid.as_deref() == Some(uid.as_str());
        let card_id = format!("bots-item-card-{uid}");
        let card_sel = card_id.clone();

        let active_ask: Option<(u64, Vec<BotItemDecision>, Vec<Option<String>>)> =
            super::home_item::waiting_question(item).map(|ev| {
                let answered = super::home_item_parts::answered_lines(item, ev);
                (ev.id, ev.decisions.clone(), answered)
            });

        let bot_done = bot.id.clone();
        let bot_working = bot.id.clone();
        let bot_needs = bot.id.clone();
        let bot_cancel = bot.id.clone();
        let bot_snooze = bot.id.clone();
        let bot_snooze24 = bot.id.clone();
        let bot_edit_save = bot.id.clone();
        let bot_dispatch = bot.id.clone();
        let uid_done = uid.clone();
        let uid_working = uid.clone();
        let uid_needs = uid.clone();
        let uid_cancel = uid.clone();
        let uid_snooze = uid.clone();
        let uid_snooze24 = uid.clone();
        let uid_edit = uid.clone();
        let uid_edit_save = uid.clone();
        let uid_comment = uid.clone();
        let uid_link = uid.clone();
        let uid_expand = uid.clone();

        let cur_title = item.title.clone();
        let cur_desc = item.description.clone().unwrap_or_default();
        let cur_done = item.done_means.clone().unwrap_or_default();

        let mut card = ui::card(p)
            .id(SharedString::from(card_id))
            .debug_selector(move || card_sel.clone())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap_1p5()
                            .child(ui::badge(
                                p,
                                ui::BadgeTone::Muted,
                                format!("#{}", item.number),
                            ))
                            .child(ui::badge(p, tone, status_label))
                            .when(is_snoozed, |d| {
                                d.child(ui::badge(
                                    p,
                                    ui::BadgeTone::Muted,
                                    text::SNOOZED_BADGE.get(),
                                ))
                            })
                            .child(
                                div()
                                    .min_w_0()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.foreground)
                                    .child(item.title.clone()),
                            ),
                    )
                    .child(
                        ui::controls()
                            .child(
                                ui::ghost(
                                    SharedString::from(format!("bots-item-edit-{uid}")),
                                    if is_editing {
                                        text::CANCEL.get()
                                    } else {
                                        text::ITEM_EDIT_BTN.get()
                                    },
                                )
                                .disabled(self.items.busy)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        if this.items.editing_item_uid.as_deref()
                                            == Some(uid_edit.as_str())
                                        {
                                            this.items.editing_item_uid = None;
                                        } else {
                                            this.items.editing_item_uid = Some(uid_edit.clone());
                                            let title_val = cur_title.clone();
                                            let desc_val = cur_desc.clone();
                                            let done_val = cur_done.clone();
                                            this.items.edit_title_input = Some(cx.new(|cx| {
                                                let mut st = InputState::new(window, cx)
                                                    .placeholder(
                                                        text::ITEM_EDIT_TITLE_PLACEHOLDER.get(),
                                                    );
                                                st.set_value(title_val, window, cx);
                                                st
                                            }));
                                            this.items.edit_desc_input = Some(cx.new(|cx| {
                                                let mut st = InputState::new(window, cx)
                                                    .placeholder(
                                                        text::ITEM_EDIT_DESC_PLACEHOLDER.get(),
                                                    );
                                                st.set_value(desc_val, window, cx);
                                                st
                                            }));
                                            this.items.edit_done_input = Some(cx.new(|cx| {
                                                let mut st = InputState::new(window, cx)
                                                    .placeholder(
                                                        text::ITEM_EDIT_DONE_PLACEHOLDER.get(),
                                                    );
                                                st.set_value(done_val, window, cx);
                                                st
                                            }));
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                            .when(item.status != BotItemStatus::Done, |d| {
                                let bid = format!("bots-item-done-{uid_done}");
                                let sel = bid.clone();
                                d.child(
                                    ui::secondary(
                                        SharedString::from(bid),
                                        text::ACT_MARK_DONE.get(),
                                    )
                                    .debug_selector(move || sel.clone())
                                    .disabled(self.items.busy)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.update_item_status(
                                                bot_done.clone(),
                                                uid_done.clone(),
                                                BotItemStatus::Done,
                                                cx,
                                            );
                                        },
                                    )),
                                )
                            })
                            .when(item.status != BotItemStatus::Working, |d| {
                                let bid = format!("bots-item-working-{uid_working}");
                                let sel = bid.clone();
                                d.child(
                                    ui::ghost(
                                        SharedString::from(bid),
                                        text::ACT_MARK_WORKING.get(),
                                    )
                                    .debug_selector(move || sel.clone())
                                    .disabled(self.items.busy)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.update_item_status(
                                                bot_working.clone(),
                                                uid_working.clone(),
                                                BotItemStatus::Working,
                                                cx,
                                            );
                                        },
                                    )),
                                )
                            })
                            .when(item.status != BotItemStatus::NeedsYou, |d| {
                                let bid = format!("bots-item-needs-{uid_needs}");
                                let sel = bid.clone();
                                d.child(
                                    ui::ghost(
                                        SharedString::from(bid),
                                        text::ACT_MARK_NEEDS_YOU.get(),
                                    )
                                    .debug_selector(move || sel.clone())
                                    .disabled(self.items.busy)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.update_item_status(
                                                bot_needs.clone(),
                                                uid_needs.clone(),
                                                BotItemStatus::NeedsYou,
                                                cx,
                                            );
                                        },
                                    )),
                                )
                            })
                            .when(item.status != BotItemStatus::Canceled, |d| {
                                let bid = format!("bots-item-cancel-{uid_cancel}");
                                let sel = bid.clone();
                                d.child(
                                    ui::ghost(SharedString::from(bid), text::ACT_CANCEL_ITEM.get())
                                        .debug_selector(move || sel.clone())
                                        .disabled(self.items.busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.update_item_status(
                                                bot_cancel.clone(),
                                                uid_cancel.clone(),
                                                BotItemStatus::Canceled,
                                                cx,
                                            );
                                        })),
                                )
                            })
                            .child({
                                let bid = format!("bots-item-snooze-{uid_snooze}");
                                let sel = bid.clone();
                                ui::ghost(
                                    SharedString::from(bid),
                                    if is_snoozed {
                                        text::ACT_UNSNOOZE.get()
                                    } else {
                                        text::ACT_SNOOZE_1H.get()
                                    },
                                )
                                .debug_selector(move || sel.clone())
                                .disabled(self.items.busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let target = if is_snoozed {
                                            None
                                        } else {
                                            Some(now_unix_ms().saturating_add(3_600_000))
                                        };
                                        this.snooze_item(
                                            bot_snooze.clone(),
                                            uid_snooze.clone(),
                                            target,
                                            cx,
                                        );
                                    },
                                ))
                            })
                            .when(!is_snoozed, |d| {
                                d.child(
                                    ui::ghost(
                                        SharedString::from(format!(
                                            "bots-item-snooze24-{uid_snooze24}"
                                        )),
                                        text::ACT_SNOOZE_24H.get(),
                                    )
                                    .disabled(self.items.busy)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            let target =
                                                Some(now_unix_ms().saturating_add(86_400_000));
                                            this.snooze_item(
                                                bot_snooze24.clone(),
                                                uid_snooze24.clone(),
                                                target,
                                                cx,
                                            );
                                        },
                                    )),
                                )
                            }),
                    ),
            );

        if is_editing {
            card = card.child(
                div()
                    .p_2()
                    .rounded(ui::RADIUS_SM)
                    .bg(p.muted.opacity(0.4))
                    .border_1()
                    .border_color(p.border)
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .children(
                        self.items
                            .edit_title_input
                            .as_ref()
                            .map(|i| Input::new(i).small()),
                    )
                    .children(
                        self.items
                            .edit_desc_input
                            .as_ref()
                            .map(|i| Input::new(i).small()),
                    )
                    .children(
                        self.items
                            .edit_done_input
                            .as_ref()
                            .map(|i| Input::new(i).small()),
                    )
                    .child(
                        div().flex().justify_end().child(
                            ui::primary(
                                SharedString::from(format!("bots-item-edit-save-{uid}")),
                                text::SAVE.get(),
                            )
                            .disabled(self.items.busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.save_item_edits(
                                        bot_edit_save.clone(),
                                        uid_edit_save.clone(),
                                        cx,
                                    );
                                },
                            )),
                        ),
                    ),
            );
        }

        if item.status.is_open()
            && let Some(close_note_inp) = self.items.close_note_inputs.get(&uid)
        {
            card = card.child(
                div()
                    .w_full()
                    .max_w(px(360.))
                    .child(Input::new(close_note_inp).small()),
            );
        }

        if let Some(desc) = &item.description
            && !desc.trim().is_empty()
        {
            card = card.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(desc.clone()),
            );
        }

        if let Some(done_means) = &item.done_means
            && !done_means.trim().is_empty()
        {
            card = card.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.foreground)
                    .child(text::DONE_MEANS_LABEL.format(&[("text", done_means)])),
            );
        }

        if let Some(close_note) = &item.close_note
            && !close_note.trim().is_empty()
        {
            card = card.child(
                div()
                    .text_size(ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(close_note.clone()),
            );
        }

        // Interactive decision prompt cards when an active (non-stale) waiting ask has open decisions.
        if let Some((event_id, latest_decisions, answered)) = active_ask {
            for (didx, decision) in latest_decisions.into_iter().enumerate() {
                let decision_id = format!("{event_id}:{didx}");
                let card_key = format!("bots-item-decision-card-{uid}-{event_id}-{didx}");
                if let Some(answer_text) = answered.get(didx).and_then(Option::as_deref) {
                    let ans_sel = format!("bots-item-decision-answered-{uid}-{didx}");
                    card = card.child(
                        div()
                            .id(SharedString::from(card_key))
                            .debug_selector(move || ans_sel.clone())
                            .w_full()
                            .p_2p5()
                            .rounded(ui::RADIUS_SM)
                            .bg(p.muted.opacity(0.45))
                            .border_1()
                            .border_color(p.border)
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.muted_foreground)
                                    .child(decision.question),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.success)
                                    .child(format!("✓ {answer_text}")),
                            ),
                    );
                    continue;
                }
                let mut opts_row = div().flex().flex_wrap().items_center().gap_1p5();
                for (oidx, opt) in decision.options.into_iter().enumerate() {
                    let is_rec = decision.recommended == Some(oidx);
                    let uid_ans = uid.clone();
                    let dec_id_opt = decision_id.clone();
                    let label = if is_rec {
                        format!("{opt} ({})", text::DECISION_RECOMMENDED.get())
                    } else {
                        opt
                    };
                    let btn_id = format!("bots-item-decision-{uid}-{didx}-{oidx}");
                    let btn_sel = btn_id.clone();
                    let btn = if is_rec {
                        ui::primary(SharedString::from(btn_id), label)
                    } else {
                        ui::secondary(SharedString::from(btn_id), label)
                    };
                    opts_row = opts_row.child(
                        btn.debug_selector(move || btn_sel.clone())
                            .disabled(self.items.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.answer_item_decision_for(
                                    uid_ans.clone(),
                                    None,
                                    u32::try_from(oidx).ok(),
                                    Some(dec_id_opt.clone()),
                                    None,
                                    cx,
                                );
                            })),
                    );
                }
                let dec_inp = self.ensure_decision_input(&uid, &decision_id, window, cx);
                let uid_custom = uid.clone();
                let dec_id_custom = decision_id.clone();
                card = card.child(
                    div()
                        .id(SharedString::from(card_key))
                        .w_full()
                        .p_2p5()
                        .rounded(ui::RADIUS_SM)
                        .bg(p.warning.opacity(0.08))
                        .border_1()
                        .border_color(p.warning.opacity(0.3))
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(ui::TEXT_XS)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.foreground)
                                .child(decision.question),
                        )
                        .child(opts_row)
                        .when_some(dec_inp, |d, inp| {
                            let inp_read = inp.clone();
                            let submit_id = format!("bots-item-decision-submit-{uid}-{didx}");
                            let submit_sel = submit_id.clone();
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(div().flex_1().min_w_0().child(Input::new(&inp).small()))
                                    .child(
                                        ui::secondary(
                                            SharedString::from(submit_id),
                                            text::DECISION_SUBMIT_BTN.get(),
                                        )
                                        .debug_selector(move || submit_sel.clone())
                                        .disabled(self.items.busy)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                let val = inp_read.read(cx).value().to_string();
                                                this.answer_item_decision_for(
                                                    uid_custom.clone(),
                                                    Some(val),
                                                    None,
                                                    Some(dec_id_custom.clone()),
                                                    Some(inp_read.clone()),
                                                    cx,
                                                );
                                            }),
                                        ),
                                    ),
                            )
                        }),
                );
            }
        }

        // Clickable Link chips
        if !item.links.is_empty() {
            let mut row = div().flex().flex_wrap().items_center().gap_1p5().child(
                div()
                    .text_size(ui::TEXT_2XS)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted_foreground)
                    .child(format!("{}:", text::LINKS_LABEL.get())),
            );
            for (lidx, link) in item.links.iter().enumerate() {
                let (kind_label, target, action) = link_chip_parts(link);
                let text_label = format!("{kind_label}: {target}");
                let bid_dispatch = bot_dispatch.clone();
                let link_id = format!("bots-item-link-{uid}-{lidx}");
                match action {
                    LinkAction::OpenUrl(url) => {
                        row =
                            row.child(ui::ghost(SharedString::from(link_id), text_label).on_click(
                                move |_, _, cx| {
                                    if let Some(clean) = super::sanitize_http_url(&url) {
                                        cx.open_url(&clean);
                                    }
                                },
                            ));
                    }
                    LinkAction::DispatchSession(session_id) => {
                        let uid_disp = uid.clone();
                        row = row.child(
                            ui::secondary(SharedString::from(link_id), text_label)
                                .debug_selector(move || {
                                    format!("bots-item-dispatch-{uid_disp}-{lidx}")
                                })
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(OpenSession {
                                        bot_id: bid_dispatch.clone(),
                                        session_id,
                                    });
                                })),
                        );
                    }
                    LinkAction::RevealFile(path) => {
                        row = row.child(
                            ui::ghost(SharedString::from(link_id), text_label)
                                .on_click(move |_, _, cx| cx.reveal_path(&path)),
                        );
                    }
                    LinkAction::None => {
                        row = row.child(ui::badge(p, ui::BadgeTone::Neutral, text_label));
                    }
                }
            }
            card = card.child(row);
        }

        // Events / comments timeline (collapsible: recent 4 or all)
        if !item.events.is_empty() {
            let total_events = item.events.len();
            let comments_count = text::COMMENTS_COUNT.format(&[("n", &total_events.to_string())]);
            let shown_events: Vec<_> = if expanded {
                item.events.iter().collect()
            } else {
                item.events
                    .iter()
                    .rev()
                    .take(4)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            };
            let mut ev_box = div()
                .flex()
                .flex_col()
                .gap_1()
                .pt_1()
                .border_t_1()
                .border_color(p.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_size(ui::TEXT_2XS)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.muted_foreground)
                                .child(comments_count),
                        )
                        .when(total_events > 4, |d| {
                            d.child(
                                ui::ghost(
                                    SharedString::from(format!("bots-item-expand-{uid_expand}")),
                                    if expanded {
                                        SharedString::from(text::ITEM_TIMELINE_COLLAPSE.get())
                                    } else {
                                        SharedString::from(
                                            text::ITEM_TIMELINE_SHOW_ALL
                                                .format(&[("n", &total_events.to_string())]),
                                        )
                                    },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if this.items.expanded_item_uid.as_deref()
                                            == Some(uid_expand.as_str())
                                        {
                                            this.items.expanded_item_uid = None;
                                        } else {
                                            this.items.expanded_item_uid = Some(uid_expand.clone());
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                        }),
                );
            for ev in shown_events {
                if ev.text.trim().is_empty() {
                    continue;
                }
                ev_box = ev_box.child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.foreground)
                        .child(format!("• {}", ev.text)),
                );
            }
            card = card.child(ev_box);
        }

        // Inline comment + add-link row
        let comment_inp = self.items.comment_inputs.get(&uid).cloned();
        let link_inp = self.items.link_inputs.get(&uid).cloned();
        let link_kind = self.items.link_kinds.get(&uid).copied().unwrap_or_default();
        let _ = window;

        card = card.child(
            div()
                .w_full()
                .pt_1()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .when_some(comment_inp, |d, inp| {
                    let inp_read = inp.clone();
                    let uid_c = uid_comment.clone();
                    d.child(
                        div()
                            .flex_1()
                            .min_w(px(180.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().flex_1().min_w_0().child(Input::new(&inp).small()))
                            .child(
                                ui::secondary(
                                    SharedString::from(format!("bots-item-comment-btn-{uid_c}")),
                                    text::ADD_COMMENT_BTN.get(),
                                )
                                .disabled(self.items.busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let body = inp_read.read(cx).value().to_string();
                                        this.submit_item_comment(
                                            uid_c.clone(),
                                            &body,
                                            Some(inp_read.clone()),
                                            cx,
                                        );
                                    },
                                )),
                            ),
                    )
                })
                .when_some(link_inp, |d, inp| {
                    let inp_read = inp.clone();
                    let uid_l = uid_link.clone();
                    let uid_kind = uid_link.clone();
                    let kind_label = match link_kind {
                        LinkKindChoice::Url => text::LINK_URL.get(),
                        LinkKindChoice::Pr => text::LINK_PR.get(),
                        LinkKindChoice::Dispatch => text::LINK_DISPATCH.get(),
                        LinkKindChoice::File => text::LINK_FILE.get(),
                    };
                    d.child(
                        div()
                            .flex_1()
                            .min_w(px(220.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                ui::ghost(
                                    SharedString::from(format!("bots-item-link-kind-{uid_kind}")),
                                    kind_label,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let next = match this
                                            .items
                                            .link_kinds
                                            .get(&uid_kind)
                                            .copied()
                                            .unwrap_or_default()
                                        {
                                            LinkKindChoice::Url => LinkKindChoice::Pr,
                                            LinkKindChoice::Pr => LinkKindChoice::Dispatch,
                                            LinkKindChoice::Dispatch => LinkKindChoice::File,
                                            LinkKindChoice::File => LinkKindChoice::Url,
                                        };
                                        this.items.link_kinds.insert(uid_kind.clone(), next);
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(div().flex_1().min_w_0().child(Input::new(&inp).small()))
                            .child(
                                ui::secondary(
                                    SharedString::from(format!("bots-item-link-btn-{uid_l}")),
                                    text::ADD_LINK_BTN.get(),
                                )
                                .disabled(self.items.busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let target = inp_read.read(cx).value().to_string();
                                        this.submit_item_link(
                                            uid_l.clone(),
                                            &target,
                                            Some(inp_read.clone()),
                                            cx,
                                        );
                                    },
                                )),
                            ),
                    )
                }),
        );

        card
    }
}

#[cfg(test)]
mod swarm_regression_tests {
    use super::*;

    #[test]
    fn set_items_for_bot_clears_cached_items_when_empty() {
        let mut state = ItemsState::default();
        let bot_id = BotId::new("bot-alpha").unwrap();
        let sample = BotItem {
            uid: "item-1".to_owned(),
            number: 1,
            owner_bot_id: bot_id.clone(),
            title: "First item".to_owned(),
            description: None,
            done_means: None,
            status: BotItemStatus::Planned,
            snoozed_until_ms: None,
            close_note: None,
            closed_at_ms: None,
            version: 1,
            links: Vec::new(),
            events: Vec::new(),
            last_activity_at_ms: 1_000,
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
            waiting_reason: None,
            waiting_since_ms: None,
        };
        state.set_items_for_bot(&bot_id, vec![sample]);
        assert_eq!(state.items_cache.get(&bot_id).map(Vec::len), Some(1));
        state.set_items_for_bot(&bot_id, Vec::new());
        assert_eq!(
            state.items_cache.get(&bot_id).map(Vec::len),
            Some(0),
            "empty ListItems result must overwrite cached items instead of flashing deleted items"
        );
        assert!(state.restore_cached(&bot_id));
        assert!(state.items.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::{Modifiers, px};
    use krill_proto::bots::{
        BotConfig, BotId, BotItem, BotItemActor, BotItemDecision, BotItemEvent, BotItemEventKind,
        BotItemStatus, BotRpcRequest, BotRpcResponse,
    };

    use super::*;

    fn sample_item(
        bot_id: &BotId,
        uid: &str,
        number: u64,
        status: BotItemStatus,
        now: u64,
    ) -> BotItem {
        BotItem {
            uid: uid.to_owned(),
            number,
            owner_bot_id: bot_id.clone(),
            title: format!("Task #{number}"),
            description: None,
            done_means: None,
            status,
            close_note: None,
            links: vec![],
            events: vec![],
            snoozed_until_ms: None,
            version: 1,
            last_activity_at_ms: now,
            created_at_ms: now,
            updated_at_ms: now,
            closed_at_ms: None,
            waiting_reason: None,
            waiting_since_ms: None,
        }
    }

    #[test]
    fn items_state_clear_seen_and_cancel_tasks_on_switch() {
        let b1 = BotId::new("b-one").unwrap();
        let b2 = BotId::new("b-two").unwrap();
        let mut st = ItemsState::default();
        st.seen_at_open.insert(b1.clone(), 10);
        st.seen_at_open.insert(b2.clone(), 20);
        st.up_next_seen_at_open.insert(b1.clone(), 100);
        st.up_next_seen_at_open.insert(b2.clone(), 200);
        st.home_list.seen_checked = Some((b1.clone(), 1, 10));

        st.clear_seen_for(&b1);
        assert!(!st.seen_at_open.contains_key(&b1));
        assert!(st.seen_at_open.contains_key(&b2));
        assert!(!st.up_next_seen_at_open.contains_key(&b1));
        assert!(st.up_next_seen_at_open.contains_key(&b2));
        assert!(st.home_list.seen_checked.is_none());

        st.home_list.seen_checked = Some((b2.clone(), 2, 20));
        st.clear_all_seen();
        assert!(st.seen_at_open.is_empty());
        assert!(st.up_next_seen_at_open.is_empty());
        assert!(st.home_list.seen_checked.is_none());

        st.busy = true;
        st.up_next_seen_sent
            .insert((crate::hosts::HostId::Local, b1.clone()), 500);
        st.open_menu_key = Some("row-more:u1".to_owned());
        let gen_before = st.rpc_gen;
        st.cancel_tasks_on_switch();
        assert!(!st.busy);
        assert!(st.up_next_seen_sent.is_empty());
        assert!(st.open_menu_key.is_none());
        assert_ne!(st.rpc_gen, gen_before);
    }

    #[gpui::test]
    fn modal_multi_question_cards_use_distinct_decision_ids_and_inputs_and_respect_snooze_expiry(
        cx: &mut gpui::TestAppContext,
    ) {
        type CapturedAnswer = (Option<String>, Option<u32>, Option<String>);
        let now = now_unix_ms();
        let b1 = BotId::new("modal-b1").unwrap();
        let b2 = BotId::new("modal-b2").unwrap();
        let cfg1 = BotConfig::new(b1.clone(), "Alpha", "agy", now).unwrap();
        let cfg2 = BotConfig::new(b2.clone(), "Beta", "agy", now).unwrap();

        let mut multi = sample_item(&b1, "u-multi", 1, BotItemStatus::NeedsYou, now);
        // Expired snooze in the past: must NOT be treated as snoozed (UB-34).
        multi.snoozed_until_ms = Some(now.saturating_sub(60_000));
        multi.events.push(BotItemEvent {
            id: 42,
            item_uid: "u-multi".to_owned(),
            kind: BotItemEventKind::Commented,
            actor: BotItemActor::Bot {
                bot_id: b1.clone(),
                session_id: None,
            },
            text: "Two questions".to_owned(),
            decisions: vec![
                BotItemDecision {
                    question: "First?".to_owned(),
                    options: vec!["A1".to_owned(), "A2".to_owned()],
                    recommended: Some(0),
                    multi: false,
                },
                BotItemDecision {
                    question: "Second?".to_owned(),
                    options: vec!["B1".to_owned(), "B2".to_owned()],
                    recommended: None,
                    multi: false,
                },
            ],
            created_at_ms: now,
        });
        // Foreign bot item: must NOT be shown in b1's modal Items tab (UB-28).
        let foreign = sample_item(&b2, "u-foreign", 2, BotItemStatus::Planned, now);

        let captured: Arc<Mutex<Vec<CapturedAnswer>>> = Arc::new(Mutex::new(Vec::new()));
        let bots_rpc = vec![cfg1, cfg2];
        let items_rpc = vec![multi.clone(), foreign];
        let cap_rpc = captured.clone();
        let multi_rpc = multi.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(bots_rpc.clone())),
            BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(items_rpc.clone())),
            BotRpcRequest::AnswerBotItem {
                decision_id,
                option_index,
                answer_text,
                ..
            } => {
                cap_rpc.lock().unwrap().push((
                    decision_id.clone(),
                    *option_index,
                    answer_text.clone(),
                ));
                Ok(BotRpcResponse::Item(Box::new(multi_rpc.clone())))
            }
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx));
        vcx.simulate_resize(gpui::size(px(1000.), px(800.)));
        vcx.run_until_parked();

        // UB-28: foreign bot's card must not render under b1.
        assert!(vcx.debug_bounds("bots-item-card-u-multi").is_some());
        assert!(vcx.debug_bounds("bots-item-card-u-foreign").is_none());

        // UB-26: each question has its own distinct InputState entity.
        vcx.update(|_, cx| {
            let v = view.read(cx);
            let i0 = v
                .items
                .decision_inputs
                .get("u-multi:42:0")
                .expect("decision 0 input");
            let i1 = v
                .items
                .decision_inputs
                .get("u-multi:42:1")
                .expect("decision 1 input");
            assert_ne!(i0.entity_id(), i1.entity_id());
        });

        // Clicking question #2's option #1 sends decision_id = Some("42:1") and option_index = Some(1).
        let q2_opt1 = vcx
            .debug_bounds("bots-item-decision-u-multi-1-1")
            .expect("second question option 1");
        vcx.simulate_click(q2_opt1.center(), Modifiers::default());
        vcx.run_until_parked();
        assert_eq!(
            *captured.lock().unwrap(),
            vec![(Some("42:1".to_owned()), Some(1), None)]
        );

        // UB-34: expired snooze must show Snooze 1h (not Unsnooze) — verify via helper & upsert_item isolation (UB-29).
        assert!(!super::super::home::is_snoozed(&multi, now));
        vcx.update(|_, cx| {
            view.update(cx, |v, _| {
                v.all_open_items
                    .push(sample_item(&b2, "b2-other", 9, BotItemStatus::Working, now));
                let b2_upd = sample_item(&b2, "b2-new", 10, BotItemStatus::Planned, now);
                v.upsert_item(b2_upd, &b2);
                assert!(
                    !v.items.items.iter().any(|i| i.uid == "b2-new"),
                    "upsert_item for non-selected bot must not inject into current bot's items"
                );
                assert!(
                    v.all_open_items.iter().any(|i| i.uid == "b2-other"),
                    "upsert_item for non-selected bot must not wipe its other open items"
                );
                assert!(v.all_open_items.iter().any(|i| i.uid == "b2-new"));
            });
        });
    }

    #[gpui::test]
    fn modal_items_tab_hides_stale_decisions_and_marks_partial_answers_settled(
        cx: &mut gpui::TestAppContext,
    ) {
        let now = now_unix_ms();
        let b1 = BotId::new("modal-stale").unwrap();
        let cfg1 = BotConfig::new(b1.clone(), "Alpha", "agy", now).unwrap();

        // Item 1: historical decision #10 followed by Done -> plain NeedsYou (no decisions).
        let mut stale = sample_item(&b1, "u-stale", 1, BotItemStatus::NeedsYou, now);
        stale.events = vec![
            BotItemEvent {
                id: 10,
                item_uid: "u-stale".to_owned(),
                kind: BotItemEventKind::Commented,
                actor: BotItemActor::Bot {
                    bot_id: b1.clone(),
                    session_id: None,
                },
                text: "Old ask".to_owned(),
                decisions: vec![BotItemDecision {
                    question: "Old question?".to_owned(),
                    options: vec!["OldA".to_owned()],
                    recommended: None,
                    multi: false,
                }],
                created_at_ms: now - 30_000,
            },
            BotItemEvent {
                id: 11,
                item_uid: "u-stale".to_owned(),
                kind: BotItemEventKind::Status,
                actor: BotItemActor::Bot {
                    bot_id: b1.clone(),
                    session_id: None,
                },
                text: "needs_you -> done".to_owned(),
                decisions: vec![],
                created_at_ms: now - 20_000,
            },
            BotItemEvent {
                id: 12,
                item_uid: "u-stale".to_owned(),
                kind: BotItemEventKind::Status,
                actor: BotItemActor::Bot {
                    bot_id: b1.clone(),
                    session_id: None,
                },
                text: "done -> needs_you".to_owned(),
                decisions: vec![],
                created_at_ms: now - 10_000,
            },
        ];

        // Item 2: active multi-decision ask where Q1 is already answered and Q2 is still open.
        let mut partial = sample_item(&b1, "u-partial", 2, BotItemStatus::NeedsYou, now);
        partial.events = vec![
            BotItemEvent {
                id: 20,
                item_uid: "u-partial".to_owned(),
                kind: BotItemEventKind::Commented,
                actor: BotItemActor::Bot {
                    bot_id: b1.clone(),
                    session_id: None,
                },
                text: "Two questions".to_owned(),
                decisions: vec![
                    BotItemDecision {
                        question: "**Env?**".to_owned(),
                        options: vec!["Staging".to_owned(), "Prod".to_owned()],
                        recommended: Some(0),
                        multi: false,
                    },
                    BotItemDecision {
                        question: "Notify?".to_owned(),
                        options: vec!["Yes".to_owned(), "No".to_owned()],
                        recommended: None,
                        multi: false,
                    },
                ],
                created_at_ms: now - 5_000,
            },
            BotItemEvent {
                id: 21,
                item_uid: "u-partial".to_owned(),
                kind: BotItemEventKind::Commented,
                actor: BotItemActor::Owner { device: None },
                text: "**Env?** → Staging".to_owned(),
                decisions: vec![],
                created_at_ms: now - 2_000,
            },
        ];

        let bots_rpc = vec![cfg1];
        let items_rpc = vec![stale, partial];
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(bots_rpc.clone())),
            BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(items_rpc.clone())),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (_view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx));
        vcx.simulate_resize(gpui::size(px(1000.), px(900.)));
        vcx.run_until_parked();

        assert!(
            vcx.debug_bounds("bots-item-decision-u-stale-0-0").is_none(),
            "stale historical decision after Done must not render as active"
        );
        assert!(
            vcx.debug_bounds("bots-item-decision-answered-u-partial-0")
                .is_some(),
            "already-answered decision slot must render as settled"
        );
        assert!(
            vcx.debug_bounds("bots-item-decision-u-partial-0-0")
                .is_none(),
            "already-answered decision slot must not render active option buttons"
        );
        assert!(
            vcx.debug_bounds("bots-item-decision-u-partial-1-0")
                .is_some(),
            "remaining unanswered decision slot must stay interactive"
        );
    }
}
