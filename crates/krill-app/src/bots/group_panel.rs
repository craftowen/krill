// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Merged Home items panel for a multi-bot group (`BotGroupItemsPanel`, `use-bot-group-items`).
//! Shares item rows, status buckets, snooze/cancel dialogs, needs-you asks, and `BotItemPane`
//! directly with [`super::home`] and [`super::home_item`].

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};

use gpui::{
    AnyElement, Context, ListAlignment, ListState, SharedString, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::bots::{BotConfig, BotId, BotItem, BotItemStatus, BotRpcRequest, BotRpcResponse};

use super::home::{
    Group, ITEMS_LIST_OVERDRAW_PX, RowAnchor, activity_at, group_of, number_column_chars,
    reconcile_list_state, status_name,
};
use super::home_needs::Need;
use super::rpc::call_checked;
use super::{View, now_unix_ms};
use crate::i18n::catalog::bots as text;
use crate::ui;

const GROUP_ROWS: usize = 5;

#[derive(Debug, Clone)]
enum GroupListRow {
    FilterChips,
    AllClear,
    BucketHeader {
        bucket: Group,
        first_bucket: bool,
        count: usize,
        needs_count: usize,
        open: bool,
    },
    Need {
        owner_bot_id: BotId,
        need: Need,
    },
    Item {
        bucket: Group,
        item_index: usize,
    },
    ShowMore {
        bucket: Group,
        shown: usize,
        more: usize,
    },
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupCacheKey {
    group_id: String,
    items_rev: u64,
    items_len: usize,
    bots_sig: Vec<(BotId, String)>,
    member_ids: Vec<BotId>,
    all_members_paused: bool,
    thread_ids: Vec<String>,
    filter: GroupItemsFilter,
    folds: Vec<&'static str>,
    group_shown: Vec<(&'static str, usize)>,
    needs_keys: Vec<(BotId, String)>,
    live_working_sessions: Vec<krill_proto::SessionId>,
    minute_bucket: u64,
}

pub(super) struct GroupListCache {
    list_state: ListState,
    key: Option<GroupCacheKey>,
    last_group_id: String,
    rows: Vec<GroupListRow>,
    row_keys: Vec<RowAnchor>,
    number_chars: usize,
    owner_index: HashMap<BotId, usize>,
    #[cfg(test)]
    pub(super) rebuild_count: usize,
    #[cfg(test)]
    pub(super) rendered_item_rows: std::cell::Cell<usize>,
}

impl Default for GroupListCache {
    fn default() -> Self {
        Self {
            list_state: ListState::new(0, ListAlignment::Top, px(ITEMS_LIST_OVERDRAW_PX)),
            key: None,
            last_group_id: String::new(),
            rows: Vec::new(),
            row_keys: Vec::new(),
            number_chars: 3,
            owner_index: HashMap::new(),
            #[cfg(test)]
            rebuild_count: 0,
            #[cfg(test)]
            rendered_item_rows: std::cell::Cell::new(0),
        }
    }
}

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
    pub(super) list_cache: GroupListCache,
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
            self.all_items.clear();
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

    pub(super) fn retry_group_panel_items(&mut self, cx: &mut Context<Self>) {
        if !self.group_panel.active {
            return;
        }
        let key = (
            self.group_panel.group_id.clone(),
            self.group_panel.member_ids.clone(),
        );
        self.group_panel.closed_loaded_key = None;
        self.load_group_panel_items(key, cx);
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
                match loaded {
                    Ok(BotRpcResponse::Items(list)) => {
                        this.group_panel.all_items = list
                            .into_iter()
                            .filter(|i| member_set.contains(&i.owner_bot_id))
                            .collect();
                        this.group_panel.closed_loaded_key = Some(key);
                        this.sync_group_items_backing();
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected ListItems response: {other:?}"
                        )));
                        this.notice = None;
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                        this.notice = None;
                    }
                }
                cx.notify();
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
            let next = self.resolved_group_items();
            let same = next.len() == self.items.items.len()
                && next
                    .iter()
                    .zip(&self.items.items)
                    .all(|(a, b)| a.uid == b.uid && a.version == b.version && a.status == b.status);
            if !same {
                self.items.set_items(next);
            }
            if let Some(uid) = &self.items.open_uid
                && let Some(item) = self.items.items.iter().find(|i| &i.uid == uid)
            {
                self.selected_bot_id = Some(item.owner_bot_id.clone());
            }
        }
    }

    fn sync_group_list_cache(&mut self, now: u64) {
        let filter = self.group_panel.filter.clone();
        let active_bots: Vec<BotConfig> = self
            .group_panel
            .member_ids
            .iter()
            .filter(|bid| filter.bot_ids.is_empty() || filter.bot_ids.contains(*bid))
            .filter_map(|bid| self.bots.iter().find(|b| &b.id == bid).cloned())
            .collect();
        let mut group_needs: Vec<(BotId, Need)> = Vec::new();
        for bot in &active_bots {
            for need in self.home_needs(bot) {
                group_needs.push((bot.id.clone(), need));
            }
        }
        let bots_sig: Vec<(BotId, String)> = self
            .bots
            .iter()
            .filter(|b| self.group_panel.member_ids.contains(&b.id))
            .map(|b| (b.id.clone(), b.name.clone()))
            .collect();
        let live_working_sessions: Vec<krill_proto::SessionId> = self
            .bot_sessions
            .iter()
            .filter(|s| self.group_panel.member_ids.contains(&s.bot_id) && s.live && s.working)
            .map(|s| s.session_id)
            .collect();
        let all_members_paused = !active_bots.is_empty() && active_bots.iter().all(|b| b.paused);
        let next_key = GroupCacheKey {
            group_id: self.group_panel.group_id.clone(),
            items_rev: self.items.items_rev,
            items_len: self.items.items.len(),
            bots_sig,
            member_ids: self.group_panel.member_ids.clone(),
            all_members_paused,
            thread_ids: self.group_panel.thread_ids.clone(),
            filter: filter.clone(),
            folds: self.items.folds.iter().copied().collect(),
            group_shown: self
                .items
                .group_shown
                .iter()
                .map(|(&k, &v)| (k, v))
                .collect(),
            needs_keys: group_needs
                .iter()
                .map(|(bid, n)| (bid.clone(), n.key.clone()))
                .collect(),
            live_working_sessions,
            minute_bucket: now / 60_000,
        };
        if self.group_panel.list_cache.key.as_ref() == Some(&next_key) {
            return;
        }
        let reset_scroll = self.group_panel.list_cache.last_group_id != self.group_panel.group_id;
        self.group_panel
            .list_cache
            .last_group_id
            .clone_from(&self.group_panel.group_id);
        self.group_panel.list_cache.key = Some(next_key);
        #[cfg(test)]
        {
            self.group_panel.list_cache.rebuild_count += 1;
        }

        let mut grouped_indices: [Vec<usize>; 6] = Default::default();
        let mut max_number = 0u64;
        let mut shown_count = 0usize;
        for (idx, item) in self.items.items.iter().enumerate() {
            max_number = max_number.max(item.number);
            if !filter.bot_ids.is_empty() && !filter.bot_ids.contains(&item.owner_bot_id) {
                continue;
            }
            if !item_matches_group_scope(
                item,
                &self.group_panel.group_id,
                &self.group_panel.thread_ids,
                &filter.chat_scope,
            ) {
                continue;
            }
            if filter.needs_you_only && item.status != BotItemStatus::NeedsYou {
                continue;
            }
            shown_count += 1;
            let live = self.item_live(item);
            let bucket = group_of(item, live, Some(5), now);
            grouped_indices[bucket as usize].push(idx);
        }
        for (i, indices) in grouped_indices.iter_mut().enumerate() {
            if i == Group::Snoozed as usize {
                indices.sort_by_key(|&idx| {
                    let it = &self.items.items[idx];
                    (it.snoozed_until_ms.unwrap_or(0), Reverse(it.number))
                });
            } else {
                indices.sort_by_key(|&idx| {
                    let it = &self.items.items[idx];
                    (Reverse(activity_at(it, now)), Reverse(it.number))
                });
            }
        }

        self.items.shown_order = Group::ALL
            .iter()
            .filter(|g| g.folded_by_default() == self.items.folds.contains(g.id()))
            .flat_map(|g| {
                grouped_indices[*g as usize]
                    .iter()
                    .map(|&idx| self.items.items[idx].uid.clone())
            })
            .collect();
        if !self.items.selected.is_empty() {
            let live_uids: std::collections::HashSet<&str> =
                self.items.items.iter().map(|i| i.uid.as_str()).collect();
            self.items
                .selected
                .retain(|uid| live_uids.contains(uid.as_str()));
        }

        let has_chips = !filter.bot_ids.is_empty()
            || !matches!(filter.chat_scope, GroupChatScope::AllTheirWork)
            || filter.needs_you_only;
        let mut rows = Vec::new();
        let mut row_keys = Vec::new();
        if has_chips {
            rows.push(GroupListRow::FilterChips);
            row_keys.push(RowAnchor::FilterChip);
        }
        // Orca `eefd93a3` / `#46740`: show the greyed group faces above folded buckets when
        // nothing needs attention, nothing is running, and at least one member bot isn't paused.
        let all_clear = shown_count > 0
            && !filter.has_non_default_filters()
            && !all_members_paused
            && group_needs.is_empty()
            && grouped_indices[Group::NeedsYou as usize].is_empty()
            && grouped_indices[Group::Working as usize].is_empty();
        if all_clear {
            rows.push(GroupListRow::AllClear);
            row_keys.push(RowAnchor::AllClear);
        }
        let mut first_bucket = true;
        for bucket in Group::ALL {
            let indices = &grouped_indices[bucket as usize];
            if indices.is_empty() && (bucket != Group::NeedsYou || group_needs.is_empty()) {
                continue;
            }
            let open = bucket.folded_by_default() == self.items.folds.contains(bucket.id());
            rows.push(GroupListRow::BucketHeader {
                bucket,
                first_bucket,
                count: indices.len(),
                needs_count: if bucket == Group::NeedsYou {
                    group_needs.len()
                } else {
                    0
                },
                open,
            });
            row_keys.push(RowAnchor::GroupHeader(bucket as u8));
            first_bucket = false;
            if !open {
                continue;
            }
            if bucket == Group::NeedsYou {
                for (owner_bot_id, need) in &group_needs {
                    row_keys.push(RowAnchor::Need(format!(
                        "{}:{}",
                        owner_bot_id.as_str(),
                        need.key
                    )));
                    rows.push(GroupListRow::Need {
                        owner_bot_id: owner_bot_id.clone(),
                        need: need.clone(),
                    });
                }
            }
            let shown = self
                .items
                .group_shown
                .get(bucket.id())
                .copied()
                .unwrap_or(GROUP_ROWS);
            for &idx in indices.iter().take(shown) {
                row_keys.push(RowAnchor::Item(self.items.items[idx].uid.clone()));
                rows.push(GroupListRow::Item {
                    bucket,
                    item_index: idx,
                });
            }
            let more = indices.len().saturating_sub(shown);
            if more > 0 {
                rows.push(GroupListRow::ShowMore {
                    bucket,
                    shown,
                    more,
                });
                row_keys.push(RowAnchor::ShowMore(bucket as u8));
            }
        }
        if shown_count == 0 && group_needs.is_empty() {
            rows.push(GroupListRow::Empty);
            row_keys.push(RowAnchor::HomeNote);
        }

        let owner_index: HashMap<BotId, usize> = self
            .bots
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id.clone(), i))
            .collect();
        let old_keys = std::mem::replace(&mut self.group_panel.list_cache.row_keys, row_keys);
        reconcile_list_state(
            &mut self.group_panel.list_cache.list_state,
            &old_keys,
            &self.group_panel.list_cache.row_keys,
            reset_scroll,
        );
        self.group_panel.list_cache.rows = rows;
        self.group_panel.list_cache.number_chars = number_column_chars(max_number);
        self.group_panel.list_cache.owner_index = owner_index;
    }

    fn render_group_virtual_row(
        &self,
        row: &GroupListRow,
        number_chars: usize,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            GroupListRow::FilterChips => {
                let chips = self.render_group_filter_chips(&self.group_panel.filter, p, cx);
                div()
                    .pb(px(12.))
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .px_2()
                    .children(chips)
                    .into_any_element()
            }
            GroupListRow::AllClear => {
                // Orca `#47436` (`BotItemsAllClear.tsx`): left-aligned 1-line muted status at
                // the list's top edge (panel header already shows the group's faces).
                div()
                    .id("bot-group-all-clear")
                    .debug_selector(|| "bot-group-all-clear".into())
                    .role(gpui::Role::Status)
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .text_size(ui::TEXT_MD)
                    .line_height(px(20.))
                    .text_color(p.muted_foreground)
                    .child(text::HM_ALL_CLEAR.get())
                    .into_any_element()
            }
            GroupListRow::BucketHeader {
                bucket,
                first_bucket,
                count,
                needs_count,
                open,
            } => {
                let bucket = *bucket;
                let open = *open;
                let summary = (!open).then(|| {
                    if bucket == Group::NeedsYou {
                        (count + needs_count).to_string()
                    } else {
                        count.to_string()
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
                div()
                    .when(!first_bucket, gpui::Styled::pt_2)
                    .pb_0p5()
                    .child(header)
                    .into_any_element()
            }
            GroupListRow::Need { owner_bot_id, need } => {
                let Some(owner) = self
                    .group_panel
                    .list_cache
                    .owner_index
                    .get(owner_bot_id)
                    .and_then(|&i| self.bots.get(i))
                else {
                    return div().into_any_element();
                };
                div()
                    .pb_0p5()
                    .child(self.render_need_row(owner, need, p, cx))
                    .into_any_element()
            }
            GroupListRow::Item { bucket, item_index } => {
                let Some(item) = self.items.items.get(*item_index) else {
                    return div().into_any_element();
                };
                let Some(owner) = self
                    .group_panel
                    .list_cache
                    .owner_index
                    .get(&item.owner_bot_id)
                    .and_then(|&i| self.bots.get(i))
                else {
                    return div().into_any_element();
                };
                #[cfg(test)]
                {
                    let n = self.group_panel.list_cache.rendered_item_rows.get();
                    self.group_panel.list_cache.rendered_item_rows.set(n + 1);
                }
                let row = self.render_home_row(owner, item, *bucket, number_chars, now, p, cx);
                let upload = self.settings.avatar_cache.get(&owner.id).cloned();
                let badge = super::avatar::render(
                    &super::avatar::Avatar::of(owner),
                    upload,
                    &owner.name,
                    px(14.),
                    px(4.),
                    p,
                );
                div()
                    .pb_0p5()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(div().flex_none().child(badge))
                    .child(div().flex_1().min_w_0().child(row))
                    .into_any_element()
            }
            GroupListRow::ShowMore {
                bucket,
                shown,
                more,
            } => {
                let (bucket, shown, more) = (*bucket, *shown, *more);
                div()
                    .pb_0p5()
                    .child(
                        div()
                            .id(SharedString::from(format!("grp-more-{}", bucket.id())))
                            .debug_selector(move || format!("grp-more-{}", bucket.id()))
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
                    )
                    .into_any_element()
            }
            GroupListRow::Empty => div()
                .pt_3()
                .child(Self::render_group_items_empty(
                    &self.group_panel.filter,
                    p,
                    cx,
                ))
                .into_any_element(),
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
        self.sync_group_list_cache(now);
        #[cfg(test)]
        {
            self.group_panel.list_cache.rendered_item_rows.set(0);
        }

        let list_state = self.group_panel.list_cache.list_state.clone();
        let number_chars = self.group_panel.list_cache.number_chars;
        let list = gpui::list(
            list_state,
            cx.processor(move |this, ix: usize, _, cx| {
                let p = ui::palette(cx);
                let Some(row) = this.group_panel.list_cache.rows.get(ix) else {
                    return div().into_any_element();
                };
                this.render_group_virtual_row(row, number_chars, now, p, cx)
            }),
        )
        .size_full()
        .pl(px(14.))
        .pr_2()
        .pt(px(10.))
        .pb_3();

        let body = div()
            .id("bot-group-items-body")
            .debug_selector(|| "bot-group-items-body".into())
            .flex_1()
            .min_h_0()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("bot-group-items-list")
                    .debug_selector(|| "bot-group-items-list".into())
                    .role(gpui::Role::List)
                    .flex_1()
                    .min_h_0()
                    .size_full()
                    .child(list),
            );

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
