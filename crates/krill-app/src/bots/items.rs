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
    Planned,
    Done,
    Canceled,
}

impl ItemFilter {
    pub const ALL_FILTERS: [Self; 6] = [
        Self::All,
        Self::NeedsYou,
        Self::Working,
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

#[derive(Default)]
pub(super) struct ItemsState {
    pub(super) items: Vec<BotItem>,
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
    /// Home: the open item's description is unclamped.
    pub(super) desc_open: bool,
}

impl ItemsState {
    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        opt_input_composing(self.new_title_input.as_ref(), window, app)
            || opt_input_composing(self.new_desc_input.as_ref(), window, app)
            || opt_input_composing(self.edit_title_input.as_ref(), window, app)
            || opt_input_composing(self.edit_desc_input.as_ref(), window, app)
            || opt_input_composing(self.edit_done_input.as_ref(), window, app)
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
            let act = if v.starts_with("https://") || v.starts_with("http://") {
                LinkAction::OpenUrl(v.clone())
            } else {
                LinkAction::None
            };
            (text::LINK_PR.get(), v.as_str(), act)
        }
        BotItemLink::Dispatch(v) => {
            let act = v.trim().parse::<u64>().map_or(LinkAction::None, |num| {
                LinkAction::DispatchSession(SessionId(num))
            });
            (text::LINK_DISPATCH.get(), v.as_str(), act)
        }
        BotItemLink::Url(v) => {
            let act = if v.starts_with("https://") || v.starts_with("http://") {
                LinkAction::OpenUrl(v.clone())
            } else {
                LinkAction::None
            };
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

    fn ensure_items_inputs(&mut self, bot_id: &BotId, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        if self.items.new_title_input.is_none() {
            let title_inp = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::NEW_ITEM_TITLE_PLACEHOLDER.get())
            });
            let bid = bot_id.clone();
            let sub = cx.subscribe_in(
                &title_inp,
                window,
                move |this, inp, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. })
                        && !crate::jump::entity_input_is_composing(inp, window, cx)
                    {
                        let title = inp.read(cx).value().to_string();
                        let desc = this
                            .items
                            .new_desc_input
                            .as_ref()
                            .map(|d| d.read(cx).value().to_string());
                        this.create_bot_item(bid.clone(), &title, desc, Some(window), cx);
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
            let bid = bot_id.clone();
            let sub = cx.subscribe_in(
                &desc_inp,
                window,
                move |this, inp, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. })
                        && !crate::jump::entity_input_is_composing(inp, window, cx)
                    {
                        let title = this
                            .items
                            .new_title_input
                            .as_ref()
                            .map(|t| t.read(cx).value().to_string())
                            .unwrap_or_default();
                        let desc = inp.read(cx).value().to_string();
                        this.create_bot_item(bid.clone(), &title, Some(desc), Some(window), cx);
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
                move |this, inp, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. })
                        && !crate::jump::entity_input_is_composing(inp, window, cx)
                    {
                        let body = inp.read(cx).value().to_string();
                        this.send_item_comment(
                            uid_owned.clone(),
                            &body,
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
                    if matches!(ev, InputEvent::PressEnter { .. })
                        && !crate::jump::entity_input_is_composing(inp, window, cx)
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
        if !self.items.decision_inputs.contains_key(uid) {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::DECISION_ANSWER_PLACEHOLDER.get())
            });
            let uid_owned = uid.to_owned();
            let input_clone = input.clone();
            let sub = cx.subscribe_in(
                &input,
                window,
                move |this, inp, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. })
                        && !crate::jump::entity_input_is_composing(inp, window, cx)
                    {
                        let ans = inp.read(cx).value().to_string();
                        this.answer_item_decision(
                            uid_owned.clone(),
                            Some(ans),
                            None,
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
            self.items.decision_inputs.insert(uid.to_owned(), input);
        }
        if !self.items.close_note_inputs.contains_key(uid) {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::ITEM_CLOSE_NOTE_PLACEHOLDER.get())
            });
            self.items.close_note_inputs.insert(uid.to_owned(), input);
        }
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
        self.items.busy = true;
        self.error = None;
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
                this.items.busy = false;
                match res {
                    Ok(item) => {
                        this.items.items.insert(0, item);
                        let handle = this.window_handle;
                        clear_input_on_window(&mut this.items.new_title_input, handle, cx);
                        clear_input_on_window(&mut this.items.new_desc_input, handle, cx);
                        this.sync_open_items_for_bot(&bot_id);
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
        let close_note = if matches!(status, BotItemStatus::Done | BotItemStatus::Canceled) {
            self.items
                .close_note_inputs
                .get(&item_uid)
                .map(|i| i.read(cx).value().trim().to_owned())
                .filter(|s| !s.is_empty())
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
        self.items.busy = true;
        self.error = None;
        self.item_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(
                    async move { call_checked(&rpc, &BotRpcRequest::UpdateItem(Box::new(params))) },
                )
                .await;
            this.update(cx, |this, cx| {
                this.items.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        this.upsert_item(*updated, &bot_id);
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
        if self.items.busy {
            return;
        }
        let trimmed = body.trim().to_owned();
        if trimmed.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            cx.notify();
            return;
        }
        let rpc = self.rpc.clone();
        let selected_bot_id = self.selected_bot_id.clone();
        self.items.busy = true;
        self.error = None;
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
                this.items.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        if let Some(bot_id) = selected_bot_id {
                            this.upsert_item(*updated, &bot_id);
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
        let selected_bot_id = self.selected_bot_id.clone();
        self.items.busy = true;
        self.error = None;
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
                this.items.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        if let Some(bot_id) = selected_bot_id {
                            this.upsert_item(*updated, &bot_id);
                        }
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
        if self.items.busy {
            return;
        }
        let cleaned_text = answer_text.map(|s| s.trim().to_owned());
        if option_index.is_none() && cleaned_text.as_deref().is_none_or(str::is_empty) {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            cx.notify();
            return;
        }
        let rpc = self.rpc.clone();
        let selected_bot_id = self.selected_bot_id.clone();
        self.items.busy = true;
        self.error = None;
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
                            decision_id: None,
                            request_id: None,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.items.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        if let Some(bot_id) = selected_bot_id {
                            this.upsert_item(*updated, &bot_id);
                        }
                        if let Some(inp) = clear_input {
                            let mut slot = Some(inp);
                            clear_input_on_window(&mut slot, this.window_handle, cx);
                        }
                        this.emit_badge(cx);
                    }
                    Ok(_) => {
                        if let Some(inp) = clear_input {
                            let mut slot = Some(inp);
                            clear_input_on_window(&mut slot, this.window_handle, cx);
                        }
                        if let Some(bot_id) = selected_bot_id {
                            this.load_selected_bot_detail(bot_id, cx);
                        }
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

    pub fn mark_bot_items_opened(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.items.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.items.busy = true;
        self.error = None;
        self.item_task = Some(cx.spawn(async move |this, cx| {
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
                this.items.busy = false;
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

    fn upsert_item(&mut self, updated: BotItem, bot_id: &BotId) {
        if let Some(slot) = self.items.items.iter_mut().find(|i| i.uid == updated.uid) {
            *slot = updated;
        } else {
            self.items.items.insert(0, updated);
        }
        self.sync_open_items_for_bot(bot_id);
    }

    pub(super) fn render_items_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_items_inputs(&bot.id, window, cx);
        let item_uids: Vec<String> = self.items.items.iter().map(|i| i.uid.clone()).collect();
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
                .filter(|item| filter.matches(item.status))
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
        filters = filters
            .child(pill_row)
            .when(!self.items.items.is_empty(), |d| {
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
            .filter(|item| self.items.filter.matches(item.status))
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
        let is_snoozed = item.snoozed_until_ms.is_some();
        let uid = item.uid.clone();
        let expanded = self.items.expanded_item_uid.as_deref() == Some(uid.as_str());
        let is_editing = self.items.editing_item_uid.as_deref() == Some(uid.as_str());
        let card_id = format!("bots-item-card-{uid}");
        let card_sel = card_id.clone();

        let latest_decisions: Vec<BotItemDecision> = item
            .events
            .iter()
            .rev()
            .find(|ev| !ev.decisions.is_empty())
            .map(|ev| ev.decisions.clone())
            .unwrap_or_default();

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

        // Interactive decision prompt cards when NeedsYou decisions are present.
        if item.status == BotItemStatus::NeedsYou && !latest_decisions.is_empty() {
            for (didx, decision) in latest_decisions.into_iter().enumerate() {
                let mut opts_row = div().flex().flex_wrap().items_center().gap_1p5();
                for (oidx, opt) in decision.options.into_iter().enumerate() {
                    let is_rec = decision.recommended == Some(oidx);
                    let uid_ans = uid.clone();
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
                                this.answer_item_decision(
                                    uid_ans.clone(),
                                    None,
                                    u32::try_from(oidx).ok(),
                                    None,
                                    cx,
                                );
                            })),
                    );
                }
                let dec_inp = self.items.decision_inputs.get(&uid).cloned();
                let uid_custom = uid.clone();
                card = card.child(
                    div()
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
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(div().flex_1().min_w_0().child(Input::new(&inp).small()))
                                    .child(
                                        ui::secondary(
                                            SharedString::from(format!(
                                                "bots-item-decision-submit-{uid}-{didx}"
                                            )),
                                            text::DECISION_SUBMIT_BTN.get(),
                                        )
                                        .disabled(self.items.busy)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                let val = inp_read.read(cx).value().to_string();
                                                this.answer_item_decision(
                                                    uid_custom.clone(),
                                                    Some(val),
                                                    None,
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
                        row = row.child(
                            ui::ghost(SharedString::from(link_id), text_label)
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        );
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
