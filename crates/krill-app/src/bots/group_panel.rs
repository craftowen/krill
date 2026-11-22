// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Merged Home items panel for a multi-bot group (`BotGroupItemsPanel`, `use-bot-group-items`).
//! Shares item rows, status buckets, snooze/cancel dialogs, needs-you asks, and `BotItemPane`
//! directly with [`super::home`] and [`super::home_item`].

use std::collections::BTreeSet;

use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::bots::{BotConfig, BotId, BotItem, BotItemStatus, BotRpcRequest, BotRpcResponse};

use super::home::{Group, grouped, status_name};
use super::home_needs::Need;
use super::rpc::call_checked;
use super::{View, now_unix_ms};
use crate::i18n::catalog::bots as text;
use crate::ui;

const GROUP_ROWS: usize = 5;

/// Chat filter scope inside [`BotGroupItemsPanel`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum GroupChatScope {
    #[default]
    ThisGroup,
    AllTheirWork,
    Thread(String),
}

/// Interactive filters controlling [`BotGroupItemsPanel`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupItemsFilter {
    pub bot_ids: Vec<BotId>,
    pub chat_scope: GroupChatScope,
    pub needs_you_only: bool,
}

impl GroupItemsFilter {
    #[must_use]
    pub fn has_non_default_filters(&self) -> bool {
        !self.bot_ids.is_empty()
            || self.chat_scope != GroupChatScope::ThisGroup
            || self.needs_you_only
    }

    #[must_use]
    pub fn is_narrowed(&self) -> bool {
        !self.bot_ids.is_empty()
            || self.chat_scope != GroupChatScope::AllTheirWork
            || self.needs_you_only
    }
}

#[derive(Default)]
pub(super) struct GroupPanelState {
    pub active: bool,
    pub group_id: String,
    pub member_ids: Vec<BotId>,
    pub thread_ids: Vec<String>,
    pub filter: GroupItemsFilter,
    pub all_items: Vec<BotItem>,
    pub closed_loaded_key: Option<(String, Vec<BotId>)>,
    pub gen_seq: u64,
}

impl GroupPanelState {
    pub fn reset_for_group(
        &mut self,
        group_id: String,
        member_ids: Vec<BotId>,
        thread_ids: Vec<String>,
    ) {
        let same_group = self.active && self.group_id == group_id;
        self.active = true;
        self.group_id = group_id;
        self.member_ids = member_ids;
        self.thread_ids = thread_ids;
        if !same_group {
            self.filter = GroupItemsFilter::default();
            self.closed_loaded_key = None;
        }
    }
}

/// Checks whether `item` matches a group or thread identifier via its links or event text.
#[must_use]
pub fn item_matches_group_scope(
    item: &BotItem,
    group_id: &str,
    thread_ids: &[String],
    scope: &GroupChatScope,
) -> bool {
    match scope {
        GroupChatScope::AllTheirWork => true,
        GroupChatScope::ThisGroup => {
            item_mentions_id(item, group_id)
                || thread_ids.iter().any(|tid| item_mentions_id(item, tid))
        }
        GroupChatScope::Thread(tid) => item_mentions_id(item, tid),
    }
}

fn item_mentions_id(item: &BotItem, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    if item.links.iter().any(|l| match l {
        krill_proto::bots::BotItemLink::Dispatch(s)
        | krill_proto::bots::BotItemLink::Pr(s)
        | krill_proto::bots::BotItemLink::Url(s)
        | krill_proto::bots::BotItemLink::File(s) => s.contains(needle),
    }) {
        return true;
    }
    item.description
        .as_deref()
        .is_some_and(|d| d.contains(needle))
        || item.events.iter().any(|e| e.text.contains(needle))
}

impl View {
    pub fn clear_group_panel(&mut self, cx: &mut Context<Self>) {
        if self.group_panel.active {
            self.group_panel.active = false;
            cx.notify();
        }
    }

    #[must_use]
    pub fn group_panel_active(&self) -> bool {
        self.group_panel.active
    }

    #[must_use]
    pub fn group_items_filter(&self) -> GroupItemsFilter {
        self.group_panel.filter.clone()
    }

    #[must_use]
    pub fn group_filter_counts(&self) -> (Vec<(BotId, usize)>, usize) {
        let mut by_bot: Vec<(BotId, usize)> = Vec::new();
        let all = self.resolved_group_items();
        let member_set: BTreeSet<&BotId> = self.group_panel.member_ids.iter().collect();
        for bid in &self.group_panel.member_ids {
            let count = all
                .iter()
                .filter(|i| &i.owner_bot_id == bid && i.status.is_open())
                .count();
            by_bot.push((bid.clone(), count));
        }
        let needs_you = all
            .iter()
            .filter(|i| {
                member_set.contains(&i.owner_bot_id)
                    && i.status == BotItemStatus::NeedsYou
                    && (self.group_panel.filter.bot_ids.is_empty()
                        || self.group_panel.filter.bot_ids.contains(&i.owner_bot_id))
                    && item_matches_group_scope(
                        i,
                        &self.group_panel.group_id,
                        &self.group_panel.thread_ids,
                        &self.group_panel.filter.chat_scope,
                    )
            })
            .count();
        (by_bot, needs_you)
    }

    pub fn toggle_group_filter_bot(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        let list = &mut self.group_panel.filter.bot_ids;
        if let Some(pos) = list.iter().position(|id| id == &bot_id) {
            list.remove(pos);
        } else {
            list.push(bot_id);
        }
        cx.notify();
    }

    pub fn clear_group_bot_filter(&mut self, cx: &mut Context<Self>) {
        if !self.group_panel.filter.bot_ids.is_empty() {
            self.group_panel.filter.bot_ids.clear();
            cx.notify();
        }
    }

    pub fn set_group_chat_scope(&mut self, scope: GroupChatScope, cx: &mut Context<Self>) {
        if self.group_panel.filter.chat_scope != scope {
            self.group_panel.filter.chat_scope = scope;
            cx.notify();
        }
    }

    pub fn set_group_needs_you_only(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.group_panel.filter.needs_you_only != on {
            self.group_panel.filter.needs_you_only = on;
            cx.notify();
        }
    }

    pub fn clear_group_filters(&mut self, cx: &mut Context<Self>) {
        self.group_panel.filter = GroupItemsFilter::default();
        cx.notify();
    }

    pub fn show_group_in_panel(
        &mut self,
        group_id: &str,
        member_ids: &[BotId],
        thread_ids: &[String],
        cx: &mut Context<Self>,
    ) {
        let prev_group = self.group_panel.group_id.clone();
        self.group_panel.reset_for_group(
            group_id.to_owned(),
            member_ids.to_vec(),
            thread_ids.to_vec(),
        );
        if prev_group != group_id {
            self.items.open_uid = None;
            self.items.selected.clear();
        }
        let key = (group_id.to_owned(), member_ids.to_vec());
        if self.group_panel.closed_loaded_key.as_ref() != Some(&key) {
            self.load_group_panel_items(key, cx);
        }
        self.sync_group_items_backing();
    }

    fn load_group_panel_items(&mut self, key: (String, Vec<BotId>), cx: &mut Context<Self>) {
        self.group_panel.gen_seq = self.group_panel.gen_seq.wrapping_add(1);
        let seq = self.group_panel.gen_seq;
        let rpc = self.rpc.clone();
        let member_set: BTreeSet<BotId> = key.1.iter().cloned().collect();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::ListItems {
                            bot_id: None,
                            include_closed: true,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.group_panel.gen_seq != seq {
                    return;
                }
                if let Ok(BotRpcResponse::Items(list)) = loaded {
                    this.group_panel.all_items = list
                        .into_iter()
                        .filter(|i| member_set.contains(&i.owner_bot_id))
                        .collect();
                    this.group_panel.closed_loaded_key = Some(key);
                    this.sync_group_items_backing();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn resolved_group_items(&self) -> Vec<BotItem> {
        let members: BTreeSet<&BotId> = self.group_panel.member_ids.iter().collect();
        if !self.group_panel.all_items.is_empty() {
            return self
                .group_panel
                .all_items
                .iter()
                .filter(|i| members.contains(&i.owner_bot_id))
                .cloned()
                .collect();
        }
        self.all_open_items
            .iter()
            .filter(|i| members.contains(&i.owner_bot_id))
            .cloned()
            .collect()
    }

    fn sync_group_items_backing(&mut self) {
        if self.group_panel.active {
            self.items.items = self.resolved_group_items();
            if let Some(uid) = &self.items.open_uid
                && let Some(item) = self.items.items.iter().find(|i| &i.uid == uid)
            {
                self.selected_bot_id = Some(item.owner_bot_id.clone());
            }
        }
    }

    pub(super) fn render_group_panel(
        &mut self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.sync_group_items_backing();
        if let Some(item) = self
            .items
            .open_uid
            .as_ref()
            .and_then(|uid| self.items.items.iter().find(|i| &i.uid == uid))
            .cloned()
            && let Some(owner) = self
                .bots
                .iter()
                .find(|b| b.id == item.owner_bot_id)
                .cloned()
        {
            return self
                .render_home_item(&owner, &item, p, window, cx)
                .into_any_element();
        }

        let now = now_unix_ms();
        let all = self.resolved_group_items();
        let filter = self.group_panel.filter.clone();
        let shown: Vec<&BotItem> = all
            .iter()
            .filter(|i| filter.bot_ids.is_empty() || filter.bot_ids.contains(&i.owner_bot_id))
            .filter(|i| {
                item_matches_group_scope(
                    i,
                    &self.group_panel.group_id,
                    &self.group_panel.thread_ids,
                    &filter.chat_scope,
                )
            })
            .filter(|i| !filter.needs_you_only || i.status == BotItemStatus::NeedsYou)
            .collect();

        let groups = grouped(shown.iter().copied(), |i| self.item_live(i), Some(5), now);
        self.items.shown_order = Group::ALL
            .iter()
            .filter(|g| g.folded_by_default() == self.items.folds.contains(g.id()))
            .flat_map(|g| groups[*g as usize].iter().map(|i| i.uid.clone()))
            .collect();
        self.items
            .selected
            .retain(|uid| all.iter().any(|i| &i.uid == uid));
        let number_chars = all
            .iter()
            .map(|i| format!("#{}", i.number).len())
            .max()
            .unwrap_or(3);

        // Gather needs across active member bots.
        let active_bots: Vec<BotConfig> = self
            .group_panel
            .member_ids
            .iter()
            .filter(|bid| filter.bot_ids.is_empty() || filter.bot_ids.contains(*bid))
            .filter_map(|bid| self.bots.iter().find(|b| &b.id == bid).cloned())
            .collect();
        let mut group_needs: Vec<(BotConfig, Need)> = Vec::new();
        for bot in &active_bots {
            for need in self.home_needs(bot) {
                group_needs.push((bot.clone(), need));
            }
        }

        let mut body = div()
            .id("bot-group-items-body")
            .debug_selector(|| "bot-group-items-body".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pl(px(14.))
            .pr_2()
            .pt(px(10.))
            .pb_3()
            .flex()
            .flex_col()
            .gap(px(12.));

        let chips = self.render_group_filter_chips(&filter, p, cx);
        if !chips.is_empty() {
            body = body.child(div().flex().flex_wrap().gap_1().px_2().children(chips));
        }

        let mut list = div()
            .id("bot-group-items-list")
            .debug_selector(|| "bot-group-items-list".into())
            .role(gpui::Role::List)
            .flex()
            .flex_col()
            .gap_2();
        for group_bucket in Group::ALL {
            let rows = &groups[group_bucket as usize];
            if rows.is_empty() && (group_bucket != Group::NeedsYou || group_needs.is_empty()) {
                continue;
            }
            list = list.child(self.render_group_bucket(
                group_bucket,
                rows,
                if group_bucket == Group::NeedsYou {
                    &group_needs
                } else {
                    &[]
                },
                number_chars,
                now,
                p,
                cx,
            ));
        }

        let any_items = !shown.is_empty() || !group_needs.is_empty();
        body = body.child(list).when(!any_items, |d| {
            d.child(Self::render_group_items_empty(&filter, p, cx))
        });

        let first_bot_id = self
            .group_panel
            .member_ids
            .first()
            .cloned()
            .or_else(|| self.bots.first().map(|b| b.id.clone()));
        let pill = first_bot_id
            .as_ref()
            .and_then(|bid| self.render_selection_pill(bid, p, cx));
        let snooze = self.render_snooze_pick(p, window, cx);
        let cancel = self.render_cancel_ask(p, window, cx);

        div()
            .id("bot-group-items")
            .relative()
            .flex_1()
            .min_h_0()
            .size_full()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" && !this.items.selected.is_empty() {
                    this.items.selected.clear();
                    cx.notify();
                }
            }))
            .child(body)
            .children(pill)
            .children(snooze)
            .children(cancel)
            .into_any_element()
    }

    fn render_group_filter_chips(
        &self,
        filter: &GroupItemsFilter,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut chips = Vec::new();
        for bid in &filter.bot_ids {
            let name = self
                .bots
                .iter()
                .find(|b| &b.id == bid)
                .map_or_else(|| bid.as_str().to_owned(), |b| b.name.clone());
            let rm_id = bid.clone();
            chips.push(Self::filter_chip_el(
                format!("grp-chip-bot-{}", bid.as_str()),
                name,
                p,
                cx.listener(move |this, _, _, cx| {
                    this.toggle_group_filter_bot(rm_id.clone(), cx);
                }),
            ));
        }
        match &filter.chat_scope {
            GroupChatScope::ThisGroup => {
                chips.push(Self::filter_chip_el(
                    "grp-chip-scope-group",
                    text::GRP_PANEL_SCOPE_THIS_GROUP.get().to_owned(),
                    p,
                    cx.listener(|this, _, _, cx| {
                        this.set_group_chat_scope(GroupChatScope::AllTheirWork, cx);
                    }),
                ));
            }
            GroupChatScope::Thread(tid) => {
                chips.push(Self::filter_chip_el(
                    format!("grp-chip-thread-{tid}"),
                    tid.clone(),
                    p,
                    cx.listener(|this, _, _, cx| {
                        this.set_group_chat_scope(GroupChatScope::ThisGroup, cx);
                    }),
                ));
            }
            GroupChatScope::AllTheirWork => {}
        }
        if filter.needs_you_only {
            chips.push(Self::filter_chip_el(
                "grp-chip-needs-you",
                status_name(BotItemStatus::NeedsYou).to_owned(),
                p,
                cx.listener(|this, _, _, cx| this.set_group_needs_you_only(false, cx)),
            ));
        }
        chips
    }

    fn filter_chip_el(
        id: impl Into<SharedString>,
        label: String,
        p: &ui::Palette,
        on_clear: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> AnyElement {
        let chip_id = id.into();
        let remove = text::HM_REMOVE_FILTER.format(&[("name", &label)]);
        div()
            .h(px(24.))
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .rounded(ui::RADIUS_SM)
            .border_1()
            .border_color(p.input)
            .pl_2()
            .pr(px(2.))
            .text_size(ui::TEXT_SM)
            .child(div().min_w_0().truncate().child(label))
            .child(
                Button::new(chip_id)
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Close).size(px(12.)))
                    .accessibility_label(remove)
                    .on_click(on_clear),
            )
            .into_any_element()
    }

    #[expect(clippy::too_many_arguments, reason = "group bucket row renderer")]
    fn render_group_bucket(
        &self,
        bucket: Group,
        rows: &[&BotItem],
        needs: &[(BotConfig, Need)],
        number_chars: usize,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let open = bucket.folded_by_default() == self.items.folds.contains(bucket.id());
        let summary = (!open).then(|| {
            if bucket == Group::NeedsYou {
                (rows.len() + needs.len()).to_string()
            } else {
                rows.len().to_string()
            }
        });
        let header = super::home::section_header(
            &format!("bot-group-bucket-{}", bucket.id()),
            match bucket {
                Group::NeedsYou => text::HM_STATUS_NEEDS_YOU.get(),
                Group::Working => text::HM_STATUS_WORKING.get(),
                Group::Planned => text::HM_STATUS_PLANNED.get(),
                Group::Snoozed => text::HM_GROUP_SNOOZED.get(),
                Group::Stale => text::HM_GROUP_STALE.get(),
                Group::Closed => text::HM_GROUP_CLOSED.get(),
            },
            open,
            summary,
            p,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if !this.items.folds.remove(bucket.id()) {
                this.items.folds.insert(bucket.id());
            }
            cx.notify();
        }));
        let mut section = div().flex().flex_col().gap_0p5().child(header);
        if !open {
            return section;
        }
        for (owner, need) in needs {
            section = section.child(self.render_need_row(owner, need, p, cx));
        }
        let shown = self
            .items
            .group_shown
            .get(bucket.id())
            .copied()
            .unwrap_or(GROUP_ROWS);
        for item in rows.iter().take(shown) {
            if let Some(owner) = self.bots.iter().find(|b| b.id == item.owner_bot_id) {
                let row = self.render_home_row(owner, item, bucket, number_chars, now, p, cx);
                let badge = super::avatar::render(
                    &super::avatar::Avatar::of(owner),
                    None,
                    &owner.name,
                    px(14.),
                    px(4.),
                    p,
                );
                section = section.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(div().flex_none().child(badge))
                        .child(div().flex_1().min_w_0().child(row)),
                );
            }
        }
        let more = rows.len().saturating_sub(shown);
        if more > 0 {
            section = section.child(
                div()
                    .id(SharedString::from(format!("grp-more-{}", bucket.id())))
                    .role(gpui::Role::Button)
                    .self_start()
                    .py_0p5()
                    .pl_8()
                    .pr_2()
                    .cursor_pointer()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .hover(|d| d.text_color(p.foreground))
                    .child(text::HM_SHOW_MORE.format(&[("n", &more.to_string())]))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.items
                            .group_shown
                            .insert(bucket.id(), shown + GROUP_ROWS.max(more));
                        cx.notify();
                    })),
            );
        }
        section
    }

    fn render_group_items_empty(
        filter: &GroupItemsFilter,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_this_group = filter.chat_scope == GroupChatScope::ThisGroup
            && filter.bot_ids.is_empty()
            && !filter.needs_you_only;
        let msg = if is_this_group {
            text::GRP_PANEL_NO_ITEMS.get()
        } else {
            text::GRP_PANEL_NO_MATCHES.get()
        };
        let mut box_div = div()
            .id("bot-group-items-empty")
            .debug_selector(|| "bot-group-items-empty".into())
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .px_6()
            .py_10()
            .text_center()
            .text_size(ui::TEXT_SM)
            .text_color(p.muted_foreground)
            .child(
                Icon::default()
                    .data(ui::icons::LIST_CHECKS_SVG)
                    .size(px(20.))
                    .text_color(p.muted_foreground),
            )
            .child(msg);
        if is_this_group {
            box_div = box_div.child(
                ui::secondary(
                    "bot-group-show-all-work",
                    text::GRP_PANEL_SHOW_ALL_WORK.get(),
                )
                .xsmall()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.set_group_chat_scope(GroupChatScope::AllTheirWork, cx);
                })),
            );
        } else if filter.is_narrowed() {
            box_div = box_div.child(
                ui::secondary(
                    "bot-group-clear-filters",
                    text::GRP_PANEL_CLEAR_FILTERS.get(),
                )
                .xsmall()
                .on_click(cx.listener(|this, _, _, cx| this.clear_group_filters(cx))),
            );
        }
        box_div.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::bots::BotItemLink;

    fn sample_item(uid: &str, group_id: Option<&str>) -> BotItem {
        BotItem {
            uid: uid.to_owned(),
            number: 1,
            owner_bot_id: BotId::new("alice").unwrap(),
            title: "Check logs".to_owned(),
            description: group_id.map(|g| format!("From {g}")),
            done_means: None,
            status: BotItemStatus::NeedsYou,
            close_note: None,
            links: group_id
                .map(|g| vec![BotItemLink::Url(format!("krill://group/{g}"))])
                .unwrap_or_default(),
            events: Vec::new(),
            snoozed_until_ms: None,
            version: 1,
            last_activity_at_ms: 100,
            created_at_ms: 100,
            updated_at_ms: 100,
            closed_at_ms: None,
        }
    }

    #[test]
    fn group_scope_filter_matches_group_and_threads() {
        let linked = sample_item("u1", Some("grp-1"));
        let unrelated = sample_item("u2", None);
        assert!(item_matches_group_scope(
            &linked,
            "grp-1",
            &["thr-9".to_owned()],
            &GroupChatScope::ThisGroup,
        ));
        assert!(!item_matches_group_scope(
            &unrelated,
            "grp-1",
            &["thr-9".to_owned()],
            &GroupChatScope::ThisGroup,
        ));
        assert!(item_matches_group_scope(
            &unrelated,
            "grp-1",
            &["thr-9".to_owned()],
            &GroupChatScope::AllTheirWork,
        ));
    }
}
