// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Tab 1 — Items Inbox: status filter pills, inline item composer, status actions, snooze, comments, and link chips.

use std::collections::BTreeMap;

use gpui::{App, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use krill_proto::bots::{
    BotConfig, BotId, BotItem, BotItemLink, BotItemStatus, BotRpcRequest, BotRpcResponse,
    CreateBotItemParams, FieldPatch, UpdateBotItemParams,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::{View, now_unix_ms, opt_input_composing};

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
    pub(super) comment_inputs: BTreeMap<String, Entity<InputState>>,
    pub(super) link_inputs: BTreeMap<String, Entity<InputState>>,
    pub(super) link_kinds: BTreeMap<String, LinkKindChoice>,
    pub(super) expanded_item_uid: Option<String>,
}

impl ItemsState {
    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        opt_input_composing(self.new_title_input.as_ref(), window, app)
            || opt_input_composing(self.new_desc_input.as_ref(), window, app)
            || self
                .comment_inputs
                .values()
                .any(|i| crate::jump::entity_input_is_composing(i, window, app))
            || self
                .link_inputs
                .values()
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

fn link_chip_parts(link: &BotItemLink) -> (&'static str, &str, Option<&str>) {
    match link {
        BotItemLink::Pr(v) => {
            let openable =
                (v.starts_with("https://") || v.starts_with("http://")).then_some(v.as_str());
            (text::LINK_PR.get(), v.as_str(), openable)
        }
        BotItemLink::Dispatch(v) => (text::LINK_DISPATCH.get(), v.as_str(), None),
        BotItemLink::Url(v) => {
            let openable =
                (v.starts_with("https://") || v.starts_with("http://")).then_some(v.as_str());
            (text::LINK_URL.get(), v.as_str(), openable)
        }
        BotItemLink::File(v) => (text::LINK_FILE.get(), v.as_str(), None),
    }
}

impl View {
    pub fn set_item_filter(&mut self, filter: ItemFilter, cx: &mut Context<Self>) {
        self.items.filter = filter;
        cx.notify();
    }

    fn ensure_items_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        if self.items.new_title_input.is_none() {
            self.items.new_title_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::NEW_ITEM_TITLE_PLACEHOLDER.get())
            }));
        }
        if self.items.new_desc_input.is_none() {
            self.items.new_desc_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::NEW_ITEM_DESC_PLACEHOLDER.get())
            }));
        }
    }

    fn ensure_item_comment_input(
        &mut self,
        uid: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<InputState>> {
        if !cx.has_global::<gpui_component::Theme>() {
            return None;
        }
        if let Some(existing) = self.items.comment_inputs.get(uid) {
            return Some(existing.clone());
        }
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder(text::COMMENT_PLACEHOLDER.get()));
        self.items
            .comment_inputs
            .insert(uid.to_owned(), input.clone());
        Some(input)
    }

    fn ensure_item_link_input(
        &mut self,
        uid: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<InputState>> {
        if !cx.has_global::<gpui_component::Theme>() {
            return None;
        }
        if let Some(existing) = self.items.link_inputs.get(uid) {
            return Some(existing.clone());
        }
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder(text::LINK_TARGET_PLACEHOLDER.get()));
        self.items.link_inputs.insert(uid.to_owned(), input.clone());
        Some(input)
    }

    pub fn create_bot_item(
        &mut self,
        bot_id: BotId,
        title: &str,
        description: Option<String>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return;
        }
        if let Some(window) = window {
            if let Some(i) = &self.items.new_title_input {
                i.update(cx, |s, cx| s.set_value(String::new(), window, cx));
            }
            if let Some(i) = &self.items.new_desc_input {
                i.update(cx, |s, cx| s.set_value(String::new(), window, cx));
            }
        }
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    rpc(&BotRpcRequest::CreateItem(Box::new(CreateBotItemParams {
                        bot_id: bot_id_bg,
                        title,
                        description: description.filter(|s| !s.trim().is_empty()),
                        done_means: None,
                        status: Some(BotItemStatus::Planned),
                        links: Vec::new(),
                    })))
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(item)) => {
                        this.items.items.insert(0, *item);
                        this.sync_open_items_for_bot(&bot_id);
                        this.emit_badge(cx);
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
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
                close_note: None,
                snoozed_until_ms: FieldPatch::Unchanged,
            },
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
            cx,
        );
    }

    fn dispatch_item_update(
        &mut self,
        bot_id: BotId,
        params: UpdateBotItemParams,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { rpc(&BotRpcRequest::UpdateItem(Box::new(params))) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        if let Some(slot) =
                            this.items.items.iter_mut().find(|i| i.uid == updated.uid)
                        {
                            *slot = *updated;
                        }
                        this.sync_open_items_for_bot(&bot_id);
                        this.emit_badge(cx);
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
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
        comment_text: &str,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let trimmed = comment_text.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        if let Some(window) = window
            && let Some(input) = self.items.comment_inputs.get(&item_uid)
        {
            input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        }
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    rpc(&BotRpcRequest::AddItemComment {
                        item_id: item_uid,
                        author: "owner".to_owned(),
                        text: trimmed,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        if let Some(slot) =
                            this.items.items.iter_mut().find(|i| i.uid == updated.uid)
                        {
                            *slot = *updated;
                        }
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
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
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let trimmed = target.trim().to_owned();
        if trimmed.is_empty() {
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
        if let Some(window) = window
            && let Some(input) = self.items.link_inputs.get(&item_uid)
        {
            input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        }
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    rpc(&BotRpcRequest::AddItemLink {
                        item_id: item_uid,
                        link,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::Item(updated)) => {
                        if let Some(slot) =
                            this.items.items.iter_mut().find(|i| i.uid == updated.uid)
                        {
                            *slot = *updated;
                        }
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Items Inbox filter bar, composer, and item cards"
    )]
    pub(super) fn render_items_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_items_inputs(window, cx);
        let bot_id = bot.id.clone();

        // Filter pills with per-status counts.
        let mut filter_bar = div()
            .id("bots-items-filters")
            .debug_selector(|| "bots-items-filters".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1();
        for filter in ItemFilter::ALL_FILTERS {
            let count = self
                .items
                .items
                .iter()
                .filter(|item| filter.matches(item.status))
                .count();
            let pill_label = format!("{} ({count})", filter.label());
            let fid = filter.id();
            filter_bar = filter_bar.child(
                ui::segment(p, fid, pill_label, self.items.filter == filter)
                    .debug_selector(move || fid.into())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_item_filter(filter, cx))),
            );
        }

        // Inline new-item composer.
        let bot_id_add = bot_id.clone();
        let composer = ui::card(p)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .children(
                        self.items
                            .new_title_input
                            .as_ref()
                            .map(|i| div().flex_1().min_w(px(200.)).child(Input::new(i).small())),
                    )
                    .child(
                        ui::primary("bots-item-add-btn", text::ADD_ITEM_BTN.get())
                            .debug_selector(|| "bots-item-add-btn".into())
                            .disabled(self.busy)
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
                                    bot_id_add.clone(),
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
                    .map(|i| Input::new(i).small()),
            );

        let visible_items: Vec<BotItem> = self
            .items
            .items
            .iter()
            .filter(|i| self.items.filter.matches(i.status))
            .cloned()
            .collect();

        let mut list = div()
            .id("bots-items-list")
            .debug_selector(|| "bots-items-list".into())
            .w_full()
            .flex()
            .flex_col()
            .gap_2p5();

        if visible_items.is_empty() {
            list = list.child(
                div()
                    .id("bots-items-empty")
                    .debug_selector(|| "bots-items-empty".into())
                    .py_6()
                    .text_center()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::ITEMS_EMPTY.get()),
            );
        } else {
            for item in visible_items {
                let uid = item.uid.clone();
                let comment_input = self.ensure_item_comment_input(&uid, window, cx);
                let link_input = self.ensure_item_link_input(&uid, window, cx);
                let current_link_kind =
                    self.items.link_kinds.get(&uid).copied().unwrap_or_default();
                let (tone, status_label) = status_badge_spec(item.status);
                let number_label = format!("#{}", item.number);
                let is_snoozed = item.snoozed_until_ms.is_some();
                let card_id = format!("bots-item-card-{}", item.uid);
                let card_sel = card_id.clone();

                let mut links_row = div().flex().flex_wrap().items_center().gap_1p5();
                for (lidx, link) in item.links.iter().enumerate() {
                    let (prefix, val, open_url) = link_chip_parts(link);
                    let chip_text = format!("{prefix}: {val}");
                    let chip_id = format!("bots-item-link-{}-{lidx}", item.uid);
                    let open_target = open_url.map(ToOwned::to_owned);
                    links_row = links_row.child(
                        div()
                            .id(SharedString::from(chip_id))
                            .px_2()
                            .py_0p5()
                            .rounded(ui::RADIUS_XS)
                            .bg(p.muted)
                            .border_1()
                            .border_color(p.border)
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.foreground)
                            .when_some(open_target, |d, url| {
                                d.cursor_pointer()
                                    .hover(|s| s.bg(p.accent))
                                    .on_click(move |_, _, cx| cx.open_url(&url))
                            })
                            .child(chip_text),
                    );
                }

                let mut events_box = div().flex().flex_col().gap_1();
                for ev in item.events.iter().rev().take(4) {
                    if ev.text.trim().is_empty() {
                        continue;
                    }
                    events_box = events_box.child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded(ui::RADIUS_XS)
                            .bg(p.muted.opacity(0.4))
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(ev.text.clone()),
                    );
                }

                let uid_done = uid.clone();
                let uid_working = uid.clone();
                let uid_needs = uid.clone();
                let uid_cancel = uid.clone();
                let uid_snooze = uid.clone();
                let uid_comment = uid.clone();
                let uid_link = uid.clone();
                let bot_done = bot_id.clone();
                let bot_working = bot_id.clone();
                let bot_needs = bot_id.clone();
                let bot_cancel = bot_id.clone();
                let bot_snooze = bot_id.clone();

                let comment_count_label =
                    text::COMMENTS_COUNT.format(&[("n", &item.events.len().to_string())]);
                let uid_expand = uid.clone();
                let is_expanded = self.items.expanded_item_uid.as_deref() == Some(uid.as_str());

                list = list.child(
                    ui::card(p)
                        .id(SharedString::from(card_id))
                        .debug_selector(move || card_sel.clone())
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .child(
                                            div()
                                                .flex_none()
                                                .font_family(ui::FONT_MONO)
                                                .text_size(ui::TEXT_XS)
                                                .text_color(p.muted_foreground)
                                                .child(number_label),
                                        )
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
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(ui::TEXT_SM)
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(p.foreground)
                                                .child(item.title.clone()),
                                        ),
                                )
                                .child(
                                    ui::controls()
                                        .when(item.status != BotItemStatus::Done, |d| {
                                            let bid = format!("bots-item-done-{uid_done}");
                                            let sel = bid.clone();
                                            d.child(
                                                ui::secondary(
                                                    SharedString::from(bid),
                                                    text::ACT_MARK_DONE.get(),
                                                )
                                                .debug_selector(move || sel.clone())
                                                .disabled(self.busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.update_item_status(
                                                        bot_done.clone(),
                                                        uid_done.clone(),
                                                        BotItemStatus::Done,
                                                        cx,
                                                    );
                                                })),
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
                                                .disabled(self.busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.update_item_status(
                                                        bot_working.clone(),
                                                        uid_working.clone(),
                                                        BotItemStatus::Working,
                                                        cx,
                                                    );
                                                })),
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
                                                .disabled(self.busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.update_item_status(
                                                        bot_needs.clone(),
                                                        uid_needs.clone(),
                                                        BotItemStatus::NeedsYou,
                                                        cx,
                                                    );
                                                })),
                                            )
                                        })
                                        .when(item.status != BotItemStatus::Canceled, |d| {
                                            let bid = format!("bots-item-cancel-{uid_cancel}");
                                            let sel = bid.clone();
                                            d.child(
                                                ui::ghost(
                                                    SharedString::from(bid),
                                                    text::ACT_CANCEL_ITEM.get(),
                                                )
                                                .debug_selector(move || sel.clone())
                                                .disabled(self.busy)
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
                                            .disabled(self.busy)
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    let next_until = (!is_snoozed).then(|| {
                                                        now_unix_ms().saturating_add(3_600_000)
                                                    });
                                                    this.snooze_item(
                                                        bot_snooze.clone(),
                                                        uid_snooze.clone(),
                                                        next_until,
                                                        cx,
                                                    );
                                                }),
                                            )
                                        }),
                                ),
                        )
                        .when_some(
                            item.description.as_deref().filter(|s| !s.trim().is_empty()),
                            |d, desc| {
                                d.child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(desc.to_owned()),
                                )
                            },
                        )
                        .when_some(
                            item.done_means.as_deref().filter(|s| !s.trim().is_empty()),
                            |d, dm| {
                                let dm_text = text::DONE_MEANS_LABEL.format(&[("text", dm)]);
                                d.child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(dm_text),
                                )
                            },
                        )
                        .when(!item.links.is_empty(), |d| d.child(links_row))
                        .when(!item.events.is_empty(), |d| d.child(events_box))
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_1p5()
                                .children(comment_input.as_ref().map(|i| {
                                    div().flex_1().min_w(px(180.)).child(Input::new(i).small())
                                }))
                                .child({
                                    let bid = format!("bots-item-comment-btn-{uid_comment}");
                                    let sel = bid.clone();
                                    let input_clone = comment_input.clone();
                                    ui::secondary(
                                        SharedString::from(bid),
                                        text::ADD_COMMENT_BTN.get(),
                                    )
                                    .debug_selector(move || sel.clone())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            let body = input_clone
                                                .as_ref()
                                                .map(|i| i.read(cx).value().to_string())
                                                .unwrap_or_default();
                                            this.submit_item_comment(
                                                uid_comment.clone(),
                                                &body,
                                                Some(window),
                                                cx,
                                            );
                                        },
                                    ))
                                })
                                .child(
                                    ui::ghost(
                                        SharedString::from(format!(
                                            "bots-item-expand-{uid_expand}"
                                        )),
                                        comment_count_label,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            if this.items.expanded_item_uid.as_deref()
                                                == Some(uid_expand.as_str())
                                            {
                                                this.items.expanded_item_uid = None;
                                            } else {
                                                this.items.expanded_item_uid =
                                                    Some(uid_expand.clone());
                                            }
                                            cx.notify();
                                        },
                                    )),
                                ),
                        )
                        .when(is_expanded, |d| {
                            let link_kinds = [
                                (LinkKindChoice::Pr, text::LINK_PR.get()),
                                (LinkKindChoice::Dispatch, text::LINK_DISPATCH.get()),
                                (LinkKindChoice::Url, text::LINK_URL.get()),
                                (LinkKindChoice::File, text::LINK_FILE.get()),
                            ];
                            let kind_bar = ui::segmented_control(
                                p,
                                link_kinds.into_iter().map(|(choice, lbl)| {
                                    let uid_k = uid_link.clone();
                                    let kid = format!("bots-link-kind-{uid_k}-{choice:?}");
                                    ui::segment(
                                        p,
                                        SharedString::from(kid),
                                        lbl,
                                        current_link_kind == choice,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.items.link_kinds.insert(uid_k.clone(), choice);
                                            cx.notify();
                                        },
                                    ))
                                }),
                            );
                            let input_link_clone = link_input.clone();
                            let uid_link_btn = uid_link.clone();
                            d.child(
                                div()
                                    .w_full()
                                    .pt_1()
                                    .border_t_1()
                                    .border_color(p.border)
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_1p5()
                                    .child(kind_bar)
                                    .children(link_input.as_ref().map(|i| {
                                        div().flex_1().min_w(px(160.)).child(Input::new(i).small())
                                    }))
                                    .child(
                                        ui::secondary(
                                            SharedString::from(format!(
                                                "bots-item-add-link-{uid_link_btn}"
                                            )),
                                            text::ADD_LINK_BTN.get(),
                                        )
                                        .disabled(self.busy)
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                let target = input_link_clone
                                                    .as_ref()
                                                    .map(|i| i.read(cx).value().to_string())
                                                    .unwrap_or_default();
                                                this.submit_item_link(
                                                    uid_link_btn.clone(),
                                                    &target,
                                                    Some(window),
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
            .child(filter_bar)
            .child(composer)
            .child(list)
    }
}
