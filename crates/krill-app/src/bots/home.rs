// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's bot Home in the right sidebar (`BotActivityTab`): the bot's items in folding groups
//! (Needs you and In progress open; Planned, Snoozed, Stale and Closed folded), five rows each
//! then "Show N more", then Automations (Tidy memory, Look around and the bot's schedules).
//! A row opens its item in the list's place (`home_item.rs`). The shell draws the tab bar and
//! the header (with the filter icon) above it.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use gpui::{
    AnyElement, Context, FontWeight, ListAlignment, ListState, SharedString, Window, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::SessionId;
use krill_proto::bots::{BotConfig, BotId, BotItem, BotItemActor, BotItemLink, BotItemStatus};

use super::home_needs::Need;
use super::{View, now_unix_ms};
use crate::i18n::catalog::{bot_home as hm, bots as text};
use crate::ui;

/// Rows a group shows before "Show N more" (Orca `BOT_ITEM_GROUP_ROWS`).
const GROUP_ROWS: usize = 5;
/// Viewport overdraw in pixels for virtualized Home & Group item lists (Orca `OVERSCAN_PX = 300`).
pub(super) const ITEMS_LIST_OVERDRAW_PX: f32 = 300.;
const DAY_MS: u64 = 86_400_000;

/// Width in characters of `#{n}` without heap allocation.
pub(super) fn number_column_chars(max_number: u64) -> usize {
    let digits = if max_number == 0 {
        1
    } else {
        usize::try_from(max_number.ilog10()).unwrap_or(0) + 1
    };
    (digits + 1).max(3)
}

/// Prefix stored in `BotItem::close_note` on open items representing Orca's `Waiting` status
/// (`#47308`/`#47309`) without altering `BotItemStatus` across other domains.
pub(super) const WAITING_NOTE_PREFIX: &str = "waiting:";

/// What an open `Waiting` item is waiting on, if any.
pub(super) fn waiting_note(item: &BotItem) -> Option<&str> {
    if !item.status.is_open() {
        return None;
    }
    item.close_note
        .as_deref()
        .and_then(|n| n.strip_prefix(WAITING_NOTE_PREFIX))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub(super) fn encode_waiting_note(note: &str) -> String {
    format!("{WAITING_NOTE_PREFIX}{}", note.trim())
}

/// Shell-supplied header context so `BotPanelHeader` renders inside Home's scroller
/// (`BotActivityPanel.tsx` L188).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HomeHeaderMeta {
    pub bot_id: Option<BotId>,
    pub narrow: bool,
    pub activity: String,
    pub attention: bool,
    pub working_count: usize,
    pub needs_you_count: usize,
    pub usage_limit_note: Option<String>,
}

/// Stable identity key for a flattened virtual row, used to preserve the scroll anchor across
/// item updates and group folds (Orca #45971).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum RowAnchor {
    Header,
    FilterChip,
    AllClear,
    GroupHeader(u8),
    StaleConfirm,
    Need(String),
    Item(String),
    ShowMore(u8),
    HomeNote,
    Automations,
    Suggestions,
}

/// Reconciles a `gpui::ListState` against updated row anchors via minimal `splice` while keeping
/// the top visible row anchored across updates (matching `sidebar/viewport.rs` and Orca #45971).
pub(super) fn reconcile_list_state(
    state: &mut ListState,
    old_keys: &[RowAnchor],
    new_keys: &[RowAnchor],
    reset_scroll: bool,
) {
    if reset_scroll {
        state.reset(new_keys.len());
        return;
    }
    if old_keys == new_keys {
        return;
    }
    let old_offset = state.logical_scroll_top();
    let new_index: BTreeMap<&RowAnchor, usize> = new_keys
        .iter()
        .enumerate()
        .map(|(idx, key)| (key, idx))
        .collect();
    let mut next_offset = old_offset;
    let anchor = old_keys
        .iter()
        .enumerate()
        .skip(old_offset.item_ix)
        .chain(old_keys.iter().enumerate().take(old_offset.item_ix).rev())
        .find_map(|(old_ix, key)| new_index.get(key).copied().map(|ix| (old_ix, ix)));
    if let Some((old_ix, ix)) = anchor {
        next_offset.item_ix = ix;
        if old_ix != old_offset.item_ix {
            next_offset.offset_in_item = px(0.);
        }
    } else {
        next_offset.item_ix = old_offset.item_ix.min(new_keys.len().saturating_sub(1));
        next_offset.offset_in_item = px(0.);
    }
    let prefix = old_keys
        .iter()
        .zip(new_keys)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = old_keys
        .iter()
        .skip(prefix)
        .rev()
        .zip(new_keys.iter().skip(prefix).rev())
        .take_while(|(a, b)| a == b)
        .count();
    state.splice(
        prefix..old_keys.len() - suffix,
        new_keys.len() - prefix - suffix,
    );
    if !new_keys.is_empty() && (old_offset.item_ix > 0 || old_offset.offset_in_item > px(0.)) {
        state.scroll_to(next_offset);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HomeNoteKind {
    Empty,
    FilteredEmpty,
}

#[derive(Debug, Clone)]
enum HomeRow {
    Header,
    FilterChip,
    AllClear,
    GroupHeader {
        bucket: HomeBucket,
        first_group: bool,
        count: usize,
        fresh: usize,
        needs_count: usize,
        open: bool,
    },
    StaleConfirm {
        stale_days: u64,
        item_indices: Vec<usize>,
    },
    Need(Need),
    Item {
        bucket: HomeBucket,
        item_index: usize,
    },
    ShowMore {
        bucket: HomeBucket,
        shown: usize,
        more: usize,
    },
    HomeNote(HomeNoteKind),
    Automations,
    Suggestions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent Home cache invalidation flags"
)]
struct HomeCacheKey {
    bot_id: BotId,
    paused: bool,
    header_meta: Option<HomeHeaderMeta>,
    items_rev: u64,
    items_len: usize,
    first_sig: Option<(String, u64)>,
    last_sig: Option<(String, u64)>,
    filter_on: bool,
    folds: Vec<&'static str>,
    group_shown: Vec<(&'static str, usize)>,
    stale_days: Option<u64>,
    stale_confirm: bool,
    needs_keys: Vec<String>,
    live_working_sessions: Vec<SessionId>,
    seen_mark: u64,
    has_suggestions: bool,
    look_around_running: bool,
    look_steps_open: bool,
    minute_bucket: u64,
}

/// Cached virtualized projection of the bot Home tab so hover, selection, and scroll never
/// re-clone, re-group, or mount hundreds of off-screen rows (Orca #45971).
pub(super) struct HomeListCache {
    pub(super) list_state: ListState,
    key: Option<HomeCacheKey>,
    last_bot_id: Option<BotId>,
    seen_checked: Option<(BotId, u64, u64)>,
    rows: Vec<HomeRow>,
    row_keys: Vec<RowAnchor>,
    number_chars: usize,
    #[cfg(test)]
    pub(super) rebuild_count: usize,
    #[cfg(test)]
    pub(super) rendered_item_rows: std::cell::Cell<usize>,
}

impl Default for HomeListCache {
    fn default() -> Self {
        Self {
            list_state: ListState::new(0, ListAlignment::Top, px(ITEMS_LIST_OVERDRAW_PX)),
            key: None,
            last_bot_id: None,
            seen_checked: None,
            rows: Vec::new(),
            row_keys: Vec::new(),
            number_chars: 3,
            #[cfg(test)]
            rebuild_count: 0,
            #[cfg(test)]
            rendered_item_rows: std::cell::Cell::new(0),
        }
    }
}

/// Home's 7-bucket grouping matching Orca `BOT_ITEM_GROUP_IDS` (includes `Waiting`, `#47308`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HomeBucket {
    NeedsYou,
    Working,
    Waiting,
    Planned,
    Snoozed,
    Stale,
    Closed,
}

impl HomeBucket {
    const ALL: [Self; 7] = [
        Self::NeedsYou,
        Self::Working,
        Self::Waiting,
        Self::Planned,
        Self::Snoozed,
        Self::Stale,
        Self::Closed,
    ];

    const fn id(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs-you",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Planned => "planned",
            Self::Snoozed => "snoozed",
            Self::Stale => "stale",
            Self::Closed => "closed",
        }
    }

    const fn folded_by_default(self) -> bool {
        matches!(
            self,
            Self::Planned | Self::Snoozed | Self::Stale | Self::Closed
        )
    }

    fn title(self) -> &'static str {
        match self {
            Self::NeedsYou => text::HM_STATUS_NEEDS_YOU.get(),
            Self::Working => text::HM_STATUS_WORKING.get(),
            Self::Waiting => hm::STATUS_WAITING.get(),
            Self::Planned => text::HM_STATUS_PLANNED.get(),
            Self::Snoozed => text::HM_GROUP_SNOOZED.get(),
            Self::Stale => text::HM_GROUP_STALE.get(),
            Self::Closed => text::HM_GROUP_CLOSED.get(),
        }
    }

    const fn as_group(self) -> Group {
        match self {
            Self::NeedsYou => Group::NeedsYou,
            Self::Working => Group::Working,
            Self::Waiting | Self::Planned => Group::Planned,
            Self::Snoozed => Group::Snoozed,
            Self::Stale => Group::Stale,
            Self::Closed => Group::Closed,
        }
    }
}

fn home_bucket_of(item: &BotItem, live: bool, stale_days: Option<u64>, now: u64) -> HomeBucket {
    match item.status {
        BotItemStatus::Done | BotItemStatus::Canceled => HomeBucket::Closed,
        _ if is_snoozed(item, now) => HomeBucket::Snoozed,
        _ if is_stale(item, live, stale_days, now) => HomeBucket::Stale,
        BotItemStatus::NeedsYou => HomeBucket::NeedsYou,
        _ if waiting_note(item).is_some() => HomeBucket::Waiting,
        BotItemStatus::Working => HomeBucket::Working,
        BotItemStatus::Planned => HomeBucket::Planned,
    }
}

/// Orca's item groups, in list order (kept 6-way for `group_panel.rs` compatibility).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Group {
    NeedsYou,
    Working,
    Planned,
    Snoozed,
    Stale,
    Closed,
}

impl Group {
    pub(super) const ALL: [Self; 6] = [
        Self::NeedsYou,
        Self::Working,
        Self::Planned,
        Self::Snoozed,
        Self::Stale,
        Self::Closed,
    ];

    pub(super) const fn id(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs-you",
            Self::Working => "working",
            Self::Planned => "planned",
            Self::Snoozed => "snoozed",
            Self::Stale => "stale",
            Self::Closed => "closed",
        }
    }

    pub(super) const fn folded_by_default(self) -> bool {
        matches!(
            self,
            Self::Planned | Self::Snoozed | Self::Stale | Self::Closed
        )
    }
}

/// An item status as Orca names it.
pub(super) fn status_name(status: BotItemStatus) -> &'static str {
    match status {
        BotItemStatus::Planned => text::HM_STATUS_PLANNED.get(),
        BotItemStatus::Working => text::HM_STATUS_WORKING.get(),
        BotItemStatus::NeedsYou => text::HM_STATUS_NEEDS_YOU.get(),
        BotItemStatus::Done => text::HM_STATUS_DONE.get(),
        BotItemStatus::Canceled => text::HM_STATUS_CANCELED.get(),
    }
}

pub(super) fn is_snoozed(item: &BotItem, now: u64) -> bool {
    item.status.is_open() && item.snoozed_until_ms.is_some_and(|until| until > now)
}

/// What the row's age, its order and the stale clock count from: its last activity, or the time
/// it came back from a snooze when that is later.
pub(super) fn activity_at(item: &BotItem, now: u64) -> u64 {
    match item.snoozed_until_ms {
        Some(until) if until <= now => item.last_activity_at_ms.max(until),
        _ => item.last_activity_at_ms,
    }
}

/// The newest event someone other than the owner wrote (Orca `othersLastEventId`): what makes a
/// row unseen.
pub(super) fn others_last_event(item: &BotItem) -> u64 {
    item.events
        .iter()
        .filter(|e| !matches!(e.actor, BotItemActor::Owner { .. }))
        .map(|e| e.id)
        .max()
        .unwrap_or(0)
}

/// Orca's derived Stale: Needs you, or In progress with no live worker, untouched for the bot's
/// stale days (Settings › Items, `None` for Never); Planned for three times that. A snoozed item
/// is never stale.
fn is_stale(item: &BotItem, live: bool, stale_days: Option<u64>, now: u64) -> bool {
    let Some(days) = stale_days else {
        return false;
    };
    if is_snoozed(item, now) {
        return false;
    }
    let idle = now.saturating_sub(activity_at(item, now));
    match item.status {
        BotItemStatus::NeedsYou => idle >= days * DAY_MS,
        BotItemStatus::Working => !live && idle >= days * DAY_MS,
        BotItemStatus::Planned => idle >= 3 * days * DAY_MS,
        BotItemStatus::Done | BotItemStatus::Canceled => false,
    }
}

pub(super) fn group_of(item: &BotItem, live: bool, stale_days: Option<u64>, now: u64) -> Group {
    match item.status {
        BotItemStatus::Done | BotItemStatus::Canceled => Group::Closed,
        _ if is_snoozed(item, now) => Group::Snoozed,
        _ if is_stale(item, live, stale_days, now) => Group::Stale,
        BotItemStatus::NeedsYou => Group::NeedsYou,
        BotItemStatus::Working => Group::Working,
        BotItemStatus::Planned => Group::Planned,
    }
}

/// Items in Orca's groups: the latest change first (then the higher number); Snoozed soonest
/// back first. `live` says whether an item's worker is running.
pub(super) fn grouped<'a>(
    items: impl IntoIterator<Item = &'a BotItem>,
    live: impl Fn(&BotItem) -> bool,
    stale_days: Option<u64>,
    now: u64,
) -> [Vec<&'a BotItem>; 6] {
    let mut groups: [Vec<&BotItem>; 6] = Default::default();
    for item in items {
        groups[group_of(item, live(item), stale_days, now) as usize].push(item);
    }
    for (i, rows) in groups.iter_mut().enumerate() {
        if i == Group::Snoozed as usize {
            rows.sort_by_key(|it| (it.snoozed_until_ms.unwrap_or(0), Reverse(it.number)));
        } else {
            rows.sort_by_key(|it| (Reverse(activity_at(it, now)), Reverse(it.number)));
        }
    }
    groups
}

/// Orca's `formatShortTimeAgo`: now, 6m, 22h, 3d (shared Wave 1 formatter).
pub(super) fn short_age(ts_ms: u64, now_ms: u64) -> String {
    ui::format_short_time_ago_secs(ts_ms / 1000, now_ms / 1000)
}

/// Orca's snooze presets: Later today (6 PM, or three hours out after 5 PM), Tomorrow 9 AM and
/// Monday 9 AM (next Monday on a Monday). `offset_secs` is the local UTC offset.
pub(super) fn snooze_presets(now_ms: u64, offset_secs: i64) -> [(String, u64); 3] {
    let local = i64::try_from(now_ms / 1000).unwrap_or(0) + offset_secs;
    let day = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (hour, minute) = (secs / 3_600, (secs % 3_600) / 60);
    let at = |day: i64, hour: i64| {
        u64::try_from((day * 86_400 + hour * 3_600 - offset_secs) * 1000).unwrap_or(0)
    };
    let later = if hour < 17 {
        (text::HM_SNOOZE_LATER.get().to_owned(), at(day, 18))
    } else {
        (
            text::HM_SNOOZE_IN_HOURS.format(&[("n", "3")]),
            at(day, hour + 3 + i64::from(minute > 0)),
        )
    };
    // 1970-01-01 was a Thursday: (day + 4) % 7 is the weekday, Sunday 0.
    let weekday = (day + 4).rem_euclid(7);
    let to_monday = match (8 - weekday) % 7 {
        0 => 7,
        n => n,
    };
    [
        later,
        (text::HM_SNOOZE_TOMORROW.get().to_owned(), at(day + 1, 9)),
        (
            text::HM_SNOOZE_MONDAY.get().to_owned(),
            at(day + to_monday, 9),
        ),
    ]
}

/// The local UTC offset now, for the snooze presets.
pub(super) fn local_offset_secs(now_ms: u64) -> i64 {
    let epoch = i64::try_from(now_ms / 1000).unwrap_or(0);
    crate::local_time::civil_from_epoch(epoch)
        .and_then(|utc| crate::platform::local_utc_offset_secs(epoch, utc))
        .map_or(0, i64::from)
}

// Lucide `circle-check` / `circle-slash` at Orca's `absoluteStrokeWidth` 2 px in a 10 px box (4.8 of 24).
const CIRCLE_CHECK_BOLD_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="4.8" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="9.6"/><path d="m8 12 3 3 5-6"/></svg>"#;
const CIRCLE_SLASH_BOLD_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="4.8" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="9.6"/><path d="M8.5 15.5 15.5 8.5"/></svg>"#;
// Orca `BotItemStatusIcon` Waiting: 10 px grey ring with its lower half filled.
const WAITING_HALF_CIRCLE_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="4.8" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="10"/><path d="M4.4 12a7.6 7.6 0 0 0 15.2 0z" fill="currentColor" stroke="none"/></svg>"#;

pub(super) fn waiting_icon(p: &ui::Palette) -> AnyElement {
    Icon::default()
        .data(WAITING_HALF_CIRCLE_SVG)
        .size(px(10.))
        .text_color(p.muted_foreground)
        .into_any_element()
}

pub(super) fn item_status_icon(
    item: &BotItem,
    live: bool,
    quiet: bool,
    p: &ui::Palette,
    cx: &gpui::App,
) -> AnyElement {
    if item.status != BotItemStatus::NeedsYou && waiting_note(item).is_some() {
        waiting_icon(p)
    } else {
        status_icon(item.status, live, quiet, p, cx)
    }
}

/// Orca's `BotItemStatusIcon`: every glyph in one 10 px box. Planned a dashed ring, In progress a
/// spinner while its worker runs (a ring otherwise), Needs you an orange dot (grey once stale),
/// Done a check, Canceled a slashed circle.
pub(super) fn status_icon(
    status: BotItemStatus,
    live: bool,
    quiet: bool,
    p: &ui::Palette,
    cx: &gpui::App,
) -> AnyElement {
    let ring = || div().size(px(10.)).flex_none().rounded_full().border_2();
    match status {
        BotItemStatus::NeedsYou => div()
            .size(px(10.))
            .flex_none()
            .rounded_full()
            .bg(if quiet {
                p.muted_foreground.opacity(0.6)
            } else {
                p.agent_question
            })
            .into_any_element(),
        BotItemStatus::Working if live => ui::status::status_glyph(
            krill_proto::control::AgentState::Working,
            false,
            ui::status::Size::Sm,
            cx,
        )
        .into_any_element(),
        BotItemStatus::Working => ring().border_color(p.warning).into_any_element(),
        BotItemStatus::Planned => ring()
            .border_dashed()
            .border_color(p.muted_foreground)
            .into_any_element(),
        BotItemStatus::Done => Icon::default()
            .data(CIRCLE_CHECK_BOLD_SVG)
            .size(px(10.))
            .text_color(p.success)
            .into_any_element(),
        BotItemStatus::Canceled => Icon::default()
            .data(CIRCLE_SLASH_BOLD_SVG)
            .size(px(10.))
            .text_color(p.muted_foreground)
            .into_any_element(),
    }
}

/// Orca's `BotPanelSectionHeader`: a chevron on the left, the whole header folds the section.
pub(super) fn section_header(
    sel: &str,
    title: &'static str,
    open: bool,
    summary: Option<String>,
    p: &ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let group: SharedString = format!("{sel}-group").into();
    let debug = sel.to_owned();
    div()
        .id(SharedString::from(sel.to_owned()))
        .debug_selector(move || debug.clone())
        .group(group.clone())
        .role(gpui::Role::Button)
        .aria_label(title)
        .aria_expanded(open)
        .tab_index(0)
        .focus_visible(|d| d.border_1().border_color(p.ring))
        .h(px(28.))
        .min_w_0()
        .flex()
        .items_center()
        .gap_1()
        .pl_1()
        .pr_1()
        .rounded(ui::RADIUS_XS)
        .cursor_pointer()
        .child(
            div()
                .ml(px(2.))
                .size(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(ui::RADIUS_SM)
                .group_hover(group, |d| d.bg(p.sidebar_accent.opacity(0.7)))
                .child(
                    Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(14.))
                    .text_color(p.muted_foreground),
                ),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(ui::TEXT_SM)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.muted_foreground.opacity(0.8))
                .child(title),
        )
        .children(summary.map(|s| {
            div()
                .flex_none()
                .text_size(ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(s)
        }))
}

impl View {
    /// Supplies live header state from the shell so `BotPanelHeader` scrolls inside Home's list
    /// (`BotActivityPanel.tsx` L188).
    #[expect(
        clippy::too_many_arguments,
        reason = "avoids re-exporting an extra struct through bots/mod.rs"
    )]
    pub fn set_home_header_meta(
        &mut self,
        bot_id: BotId,
        narrow: bool,
        activity: String,
        attention: bool,
        working_count: usize,
        needs_you_count: usize,
        usage_limit_note: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let next = HomeHeaderMeta {
            bot_id: Some(bot_id),
            narrow,
            activity,
            attention,
            working_count,
            needs_you_count,
            usage_limit_note,
        };
        if self.items.header_meta.as_ref() != Some(&next) {
            self.items.header_meta = Some(next);
            cx.notify();
        }
    }

    /// Immediately restores cached items for `bot_id` on bot switch/reopen (`#47162`) so Home
    /// paints with zero flash while the background refresh runs.
    pub fn hydrate_cached_items(&mut self, bot_id: &BotId, cx: &mut Context<Self>) {
        if self.items.restore_cached(bot_id) {
            cx.notify();
        }
    }

    /// True once `View` has loaded `bot_id` and renders `BotPanelHeader` inside Home's scroller.
    #[must_use]
    pub fn home_renders_header(&self, bot_id: &BotId) -> bool {
        self.items.header_meta.is_some()
            && self.selected_bot().is_some_and(|b| &b.id == bot_id)
            && !self.home_item_open()
    }

    /// The bot's Home has items: the header shows the filter icon (Orca hides it otherwise).
    #[must_use]
    pub fn home_filter(&self) -> Option<HomeFilter> {
        (!self.items.items.is_empty()).then(|| HomeFilter {
            needs_you_only: self.items.needs_you_only,
            needs_you: self
                .items
                .items
                .iter()
                .filter(|i| i.status == BotItemStatus::NeedsYou)
                .count(),
        })
    }

    /// Orca's "Needs you only" filter.
    pub fn set_needs_you_only(&mut self, on: bool, cx: &mut Context<Self>) {
        self.items.needs_you_only = on;
        cx.notify();
    }

    /// An item is open in Home's place: the shell leaves its header out, as Orca's pane does.
    #[must_use]
    pub fn home_item_open(&self) -> bool {
        self.items
            .open_uid
            .as_ref()
            .is_some_and(|uid| self.items.items.iter().any(|i| &i.uid == uid))
    }

    /// Opens an item in Home's place.
    pub fn open_home_item(&mut self, uid: String, cx: &mut Context<Self>) {
        self.items.open_uid = Some(uid);
        self.items.desc_open = false;
        self.items.open_comments.clear();
        self.items.asked_here.clear();
        self.items.hover_uid = None;
        cx.notify();
    }

    pub(super) fn close_home_item(&mut self, cx: &mut Context<Self>) {
        self.items.open_uid = None;
        cx.notify();
    }

    /// The item's worker runs: a live, working bot session it links as its Dispatch.
    pub(super) fn item_live(&self, item: &BotItem) -> bool {
        item.status == BotItemStatus::Working
            && item.links.iter().any(|link| match link {
                BotItemLink::Dispatch(id) => id.trim().parse::<u64>().is_ok_and(|n| {
                    self.bot_sessions
                        .iter()
                        .any(|s| s.session_id == SessionId(n) && s.live && s.working)
                }),
                _ => false,
            })
    }

    /// Rebuilds the cached virtualized Home row projection only when inputs change (O(1) on hover,
    /// selection, and scroll).
    fn sync_home_list_cache(&mut self, bot: &BotConfig, now: u64, cx: &mut Context<Self>) {
        let filter_on = self.items.needs_you_only;
        let stale_days = self.settings_extras(&bot.id).items_stale.days();
        let needs = self.home_needs(bot);
        let live_working_sessions: Vec<SessionId> = self
            .bot_sessions
            .iter()
            .filter(|s| s.bot_id == bot.id && s.live && s.working)
            .map(|s| s.session_id)
            .collect();
        let seen_mark = self.seen_mark(bot);
        let first_sig = self.items.items.first().map(|i| (i.uid.clone(), i.version));
        let last_sig = self.items.items.last().map(|i| (i.uid.clone(), i.version));
        let next_key = HomeCacheKey {
            bot_id: bot.id.clone(),
            paused: bot.paused,
            header_meta: self.items.header_meta.clone(),
            items_rev: self.items.items_rev,
            items_len: self.items.items.len(),
            first_sig,
            last_sig,
            filter_on,
            folds: self.items.folds.iter().copied().collect(),
            group_shown: self
                .items
                .group_shown
                .iter()
                .map(|(&k, &v)| (k, v))
                .collect(),
            stale_days,
            stale_confirm: self.items.stale_confirm.is_some(),
            needs_keys: needs.iter().map(|n| n.key.clone()).collect(),
            live_working_sessions,
            seen_mark,
            has_suggestions: !self.soul_memory.suggestions.is_empty(),
            look_around_running: self.soul_memory.look_around_running,
            look_steps_open: self.items.look_steps_open,
            minute_bucket: now / 60_000,
        };
        let upcoming_seen = self.items.upcoming.as_ref().map(|u| u.items_seen_event_id);
        if let Some(host_seen) = upcoming_seen {
            let check_key = (bot.id.clone(), self.items.items_rev, host_seen);
            if self.items.home_list.seen_checked.as_ref() != Some(&check_key) {
                self.items.home_list.seen_checked = Some(check_key);
                self.note_items_seen(bot, cx);
            }
        }
        if self.items.home_list.key.as_ref() == Some(&next_key) {
            return;
        }
        let reset_scroll = self.items.home_list.last_bot_id.as_ref() != Some(&bot.id);
        self.items.home_list.last_bot_id = Some(bot.id.clone());
        self.items.home_list.key = Some(next_key);
        #[cfg(test)]
        {
            self.items.home_list.rebuild_count += 1;
        }

        // Group indices without cloning `BotItem`s (Orca 7-bucket order with Waiting).
        let mut grouped_indices: [Vec<usize>; 7] = Default::default();
        let mut max_number = 0u64;
        for (idx, item) in self.items.items.iter().enumerate() {
            max_number = max_number.max(item.number);
            if filter_on && item.status != BotItemStatus::NeedsYou {
                continue;
            }
            let live = self.item_live(item);
            let bucket = home_bucket_of(item, live, stale_days, now);
            grouped_indices[bucket as usize].push(idx);
        }
        for (i, indices) in grouped_indices.iter_mut().enumerate() {
            if i == HomeBucket::Snoozed as usize {
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

        self.items.shown_order = HomeBucket::ALL
            .iter()
            .filter(|b| b.folded_by_default() == self.items.folds.contains(b.id()))
            .flat_map(|b| {
                grouped_indices[*b as usize]
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

        let seen = self.seen_mark(bot);
        let mut rows = Vec::new();
        let mut row_keys = Vec::new();
        if self.items.header_meta.is_some() {
            rows.push(HomeRow::Header);
            row_keys.push(RowAnchor::Header);
        }
        if filter_on {
            rows.push(HomeRow::FilterChip);
            row_keys.push(RowAnchor::FilterChip);
        }
        // Orca `#47436`: plain left-aligned line above folded groups when nothing needs attention
        // and nothing is running (no avatar).
        let all_clear = !self.items.items.is_empty()
            && !filter_on
            && !bot.paused
            && needs.is_empty()
            && grouped_indices[HomeBucket::NeedsYou as usize].is_empty()
            && grouped_indices[HomeBucket::Working as usize].is_empty();
        if all_clear {
            rows.push(HomeRow::AllClear);
            row_keys.push(RowAnchor::AllClear);
        }
        let mut first_group = true;
        for bucket in HomeBucket::ALL {
            let indices = &grouped_indices[bucket as usize];
            if indices.is_empty() && (bucket != HomeBucket::NeedsYou || needs.is_empty()) {
                continue;
            }
            let open = bucket.folded_by_default() == self.items.folds.contains(bucket.id());
            let fresh = if !open && bucket == HomeBucket::Closed {
                indices
                    .iter()
                    .filter(|&&idx| others_last_event(&self.items.items[idx]) > seen)
                    .count()
            } else {
                0
            };
            rows.push(HomeRow::GroupHeader {
                bucket,
                first_group,
                count: indices.len(),
                fresh,
                needs_count: if bucket == HomeBucket::NeedsYou {
                    needs.len()
                } else {
                    0
                },
                open,
            });
            row_keys.push(RowAnchor::GroupHeader(bucket as u8));
            first_group = false;

            if bucket == HomeBucket::Stale && self.items.stale_confirm.is_some() {
                rows.push(HomeRow::StaleConfirm {
                    stale_days: stale_days.unwrap_or(5),
                    item_indices: indices.clone(),
                });
                row_keys.push(RowAnchor::StaleConfirm);
            }
            if !open {
                continue;
            }
            if bucket == HomeBucket::NeedsYou {
                for need in &needs {
                    row_keys.push(RowAnchor::Need(need.key.clone()));
                    rows.push(HomeRow::Need(need.clone()));
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
                rows.push(HomeRow::Item {
                    bucket,
                    item_index: idx,
                });
            }
            let more = indices.len().saturating_sub(shown);
            if more > 0 {
                rows.push(HomeRow::ShowMore {
                    bucket,
                    shown,
                    more,
                });
                row_keys.push(RowAnchor::ShowMore(bucket as u8));
            }
        }
        let note = if self.items.items.is_empty() && needs.is_empty() {
            Some(HomeNoteKind::Empty)
        } else if filter_on {
            (grouped_indices.iter().all(Vec::is_empty) && needs.is_empty())
                .then_some(HomeNoteKind::FilteredEmpty)
        } else {
            None
        };
        if let Some(kind) = note {
            rows.push(HomeRow::HomeNote(kind));
            row_keys.push(RowAnchor::HomeNote);
        }
        rows.push(HomeRow::Automations);
        row_keys.push(RowAnchor::Automations);
        if !self.soul_memory.suggestions.is_empty() || self.soul_memory.look_around_running {
            rows.push(HomeRow::Suggestions);
            row_keys.push(RowAnchor::Suggestions);
        }

        let old_keys = std::mem::replace(&mut self.items.home_list.row_keys, row_keys);
        reconcile_list_state(
            &mut self.items.home_list.list_state,
            &old_keys,
            &self.items.home_list.row_keys,
            reset_scroll,
        );
        self.items.home_list.rows = rows;
        self.items.home_list.number_chars = number_column_chars(max_number);
    }

    /// Orca's `BotPanelHeader` rendered inside Home's scroller (`BotActivityPanel.tsx` L188).
    fn render_home_header(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let meta = self.items.header_meta.clone().unwrap_or_default();
        let quiet_line = self.home_quiet_line();
        let automation_running = quiet_line.as_ref().is_some_and(|q| q.2);
        let quiet = !bot.paused && meta.needs_you_count == 0 && meta.working_count == 0;
        let status = if let Some(limit) = meta
            .usage_limit_note
            .as_ref()
            .filter(|_| !automation_running && quiet)
        {
            limit.clone()
        } else if quiet || meta.activity.trim().is_empty() {
            match quiet_line {
                Some((_, Some(short), _)) if meta.narrow => short,
                Some((line, ..)) => line,
                None => text::BP_IDLE.get().to_owned(),
            }
        } else {
            meta.activity.clone()
        };
        let state = if meta.needs_you_count > 0 {
            krill_proto::control::AgentState::Waiting
        } else if meta.working_count > 0 || (quiet && automation_running) {
            krill_proto::control::AgentState::Working
        } else {
            krill_proto::control::AgentState::Exited
        };
        let upload = self.settings.avatar_cache.get(&bot.id).cloned();
        let avatar_el = super::avatar::render(
            &super::avatar::Avatar::of(bot),
            upload,
            &bot.name,
            px(36.),
            px(9.),
            p,
        );
        let show_limit_mark =
            meta.usage_limit_note.is_some() && quiet && !automation_running && !bot.paused;
        let face = div()
            .relative()
            .flex_none()
            .child(avatar_el)
            .when(!bot.paused, |d| {
                d.child(
                    div()
                        .debug_selector(|| "bot-panel-header-state".into())
                        .absolute()
                        .bottom(px(-4.))
                        .right(px(-4.))
                        .size(px(14.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(p.sidebar)
                        .child(if show_limit_mark {
                            Icon::new(IconName::Gauge)
                                .size(px(10.))
                                .text_color(p.warning)
                                .into_any_element()
                        } else {
                            ui::status::status_glyph(state, false, ui::status::Size::Sm, cx)
                                .into_any_element()
                        }),
                )
            });
        let filter = self.home_filter().map(|f| {
            let this = cx.weak_entity();
            div()
                .relative()
                .flex_none()
                .child(
                    Button::new("bot-panel-filter")
                        .debug_selector(|| "bot-panel-filter".into())
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::ListFilter))
                        .tooltip(text::HM_FILTER_LABEL.get())
                        .accessibility_label(text::HM_FILTER_LABEL.get())
                        .dropdown_menu(move |menu, _, _| {
                            let this = this.clone();
                            let count = f.needs_you.to_string();
                            menu.item(
                                PopupMenuItem::element(move |_, cx| {
                                    let p = ui::palette(cx);
                                    div()
                                        .w_full()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            div().flex_1().child(text::HM_FILTER_NEEDS_YOU.get()),
                                        )
                                        .child(
                                            div()
                                                .text_size(ui::TEXT_XS)
                                                .text_color(p.muted_foreground)
                                                .child(count.clone()),
                                        )
                                })
                                .checked(f.needs_you_only)
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |v, cx| {
                                        v.set_needs_you_only(!f.needs_you_only, cx);
                                    })
                                    .ok();
                                }),
                            )
                        }),
                )
                .when(f.needs_you_only, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(4.))
                            .right(px(4.))
                            .size(px(6.))
                            .rounded_full()
                            .bg(p.foreground),
                    )
                })
        });
        let paused = bot.paused;
        let bot_id_pause = bot.id.clone();
        let bot_id_share = bot.id.clone();
        let folder = self.bot_home_folder(bot);
        let this = cx.weak_entity();
        div()
            .id("bot-panel-header")
            .debug_selector(|| "bot-panel-header".into())
            .w_full()
            .flex_none()
            .px_2()
            .pt_1()
            .pb(px(14.))
            .flex()
            .items_center()
            .gap(if meta.narrow { px(6.) } else { px(10.) })
            .child(face)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_MD)
                            .line_height(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.sidebar_foreground)
                            .child(bot.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .text_color(if meta.attention {
                                p.sidebar_foreground
                            } else if show_limit_mark {
                                p.warning
                            } else {
                                p.muted_foreground
                            })
                            .child(status),
                    ),
            )
            .children(filter)
            .child(
                Button::new("bot-panel-settings")
                    .debug_selector(|| "bot-panel-settings".into())
                    .ghost()
                    .xsmall()
                    .icon(IconName::Settings)
                    .tooltip(text::SB_BOT_SETTINGS.get())
                    .accessibility_label(text::SB_BOT_SETTINGS.get())
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(super::settings_root::OpenSettingsPage(
                            super::DetailTab::Settings,
                        ));
                    })),
            )
            .child(
                Button::new("bot-panel-more")
                    .debug_selector(|| "bot-panel-more".into())
                    .ghost()
                    .xsmall()
                    .icon(IconName::Ellipsis)
                    .tooltip(text::BP_MORE.get())
                    .accessibility_label(text::BP_MORE.get())
                    .dropdown_menu(move |menu, _, _| {
                        let this_pause = this.clone();
                        let id_pause = bot_id_pause.clone();
                        let this_share = this.clone();
                        let id_share = bot_id_share.clone();
                        let menu = menu
                            .item(
                                PopupMenuItem::new(if paused {
                                    text::SB_RESUME.get()
                                } else {
                                    text::SB_PAUSE.get()
                                })
                                .icon(if paused {
                                    IconName::Play
                                } else {
                                    IconName::Pause
                                })
                                .on_click(move |_, _, cx| {
                                    this_pause
                                        .update(cx, |v, cx| {
                                            v.toggle_bot_paused(id_pause.clone(), !paused, cx);
                                        })
                                        .ok();
                                }),
                            )
                            .item(
                                PopupMenuItem::new(text::BP_SHARE_BOT.get())
                                    .icon(Icon::new(IconName::Share2))
                                    .on_click(move |_, _, cx| {
                                        this_share
                                            .update(cx, |v, cx| {
                                                v.export_bot_zip(id_share.clone(), cx);
                                            })
                                            .ok();
                                    }),
                            );
                        match folder.clone() {
                            Some(path) => menu.separator().item(
                                PopupMenuItem::new(text::SB_COPY_PATH.get())
                                    .icon(IconName::Copy)
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            path.to_string_lossy().into_owned(),
                                        ));
                                    }),
                            ),
                            None => menu,
                        }
                    }),
            )
            .into_any_element()
    }

    fn toggle_bot_paused(&mut self, bot_id: BotId, paused: bool, cx: &mut Context<Self>) {
        use krill_proto::bots::{BotRpcRequest, FieldPatch, UpdateBotParams};
        let params = UpdateBotParams {
            bot_id,
            name: None,
            description: None,
            emoji: FieldPatch::Unchanged,
            avatar: FieldPatch::Unchanged,
            agent_id: None,
            model: FieldPatch::Unchanged,
            effort: FieldPatch::Unchanged,
            permission_mode: None,
            workspace: FieldPatch::Unchanged,
            paused: Some(paused),
            first_run_completed: None,
            notifications: None,
            schedules: None,
            look_around: None,
            memory_tidy: None,
            mcp_tools: None,
        };
        self.home_rpc(BotRpcRequest::UpdateBot(Box::new(params)), None, cx);
    }

    fn render_home_virtual_row(
        &mut self,
        bot: &BotConfig,
        row: &HomeRow,
        number_chars: usize,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            HomeRow::Header => self.render_home_header(bot, p, cx),
            HomeRow::FilterChip => div()
                .pb(px(14.))
                .child(Self::render_filter_chip(p, cx))
                .into_any_element(),
            // Orca `#47436`: left-aligned single plain line with no avatar.
            HomeRow::AllClear => div()
                .id("bot-home-all-clear")
                .debug_selector(|| "bot-home-all-clear".into())
                .role(gpui::Role::Status)
                .px_2()
                .py_1()
                .mb(px(6.))
                .text_size(ui::TEXT_MD)
                .line_height(px(20.))
                .text_color(p.muted_foreground)
                .child(text::HM_ALL_CLEAR.get())
                .into_any_element(),
            HomeRow::GroupHeader {
                bucket,
                first_group,
                count,
                fresh,
                needs_count,
                open,
            } => {
                let bucket = *bucket;
                let open = *open;
                let summary = (!open).then(|| {
                    if *fresh > 0 {
                        format!(
                            "{} · {}",
                            count,
                            hm::NEW_COUNT.format(&[("n", &fresh.to_string())])
                        )
                    } else if bucket == HomeBucket::NeedsYou {
                        (count + needs_count).to_string()
                    } else {
                        count.to_string()
                    }
                });
                let header = section_header(
                    &format!("bot-home-group-{}", bucket.id()),
                    bucket.title(),
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
                    .when(!first_group, gpui::Styled::pt_2)
                    .pb_0p5()
                    .flex()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(header))
                    .when(bucket == HomeBucket::Stale, |d| {
                        d.child(
                            ui::ghost("bot-home-stale-cancel", hm::CANCEL_ALL.get())
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.items.stale_confirm = Some(());
                                    cx.notify();
                                })),
                        )
                    })
                    .into_any_element()
            }
            HomeRow::StaleConfirm {
                stale_days,
                item_indices,
            } => {
                let stale: Vec<&BotItem> = item_indices
                    .iter()
                    .filter_map(|&idx| self.items.items.get(idx))
                    .collect();
                div()
                    .pb_0p5()
                    .child(Self::render_stale_confirm(
                        &bot.id,
                        &stale,
                        *stale_days,
                        p,
                        cx,
                    ))
                    .into_any_element()
            }
            HomeRow::Need(need) => div()
                .pb_0p5()
                .child(self.render_need_row(bot, need, p, cx))
                .into_any_element(),
            HomeRow::Item { bucket, item_index } => {
                let Some(item) = self.items.items.get(*item_index) else {
                    return div().into_any_element();
                };
                #[cfg(test)]
                {
                    let n = self.items.home_list.rendered_item_rows.get();
                    self.items.home_list.rendered_item_rows.set(n + 1);
                }
                div()
                    .pb_0p5()
                    .child(self.render_home_row(
                        bot,
                        item,
                        bucket.as_group(),
                        number_chars,
                        now,
                        p,
                        cx,
                    ))
                    .into_any_element()
            }
            HomeRow::ShowMore {
                bucket,
                shown,
                more,
            } => {
                let (bucket, shown, more) = (*bucket, *shown, *more);
                let sel = format!("bot-home-show-more-{}", bucket.id());
                let debug = sel.clone();
                div()
                    .pb_0p5()
                    .child(
                        div()
                            .id(SharedString::from(sel))
                            .debug_selector(move || debug.clone())
                            .role(gpui::Role::Button)
                            .tab_index(0)
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
            HomeRow::HomeNote(kind) => div()
                .pt(px(14.))
                .child(Self::render_home_note_kind(bot, *kind, p, cx))
                .into_any_element(),
            HomeRow::Automations => div()
                .pt(px(14.))
                .child(self.render_automations(bot, p, cx))
                .into_any_element(),
            HomeRow::Suggestions => div()
                .pt(px(14.))
                .children(self.render_suggestions(bot, p, cx))
                .into_any_element(),
        }
    }

    pub(super) fn render_home_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Stale waits the bot's Settings › Items days.
        self.ensure_settings_extras(&bot.id, cx);
        if let Some(item) = self
            .items
            .open_uid
            .as_ref()
            .and_then(|uid| self.items.items.iter().find(|i| &i.uid == uid))
            .cloned()
        {
            return self
                .render_home_item(bot, &item, p, window, cx)
                .into_any_element();
        }
        let now = now_unix_ms();
        self.sync_home_list_cache(bot, now, cx);
        #[cfg(test)]
        {
            self.items.home_list.rendered_item_rows.set(0);
        }

        let list_state = self.items.home_list.list_state.clone();
        let number_chars = self.items.home_list.number_chars;
        let bot_owned = bot.clone();
        let list = gpui::list(
            list_state,
            cx.processor(move |this, ix: usize, _, cx| {
                let p = ui::palette(cx);
                let Some(row) = this.items.home_list.rows.get(ix).cloned() else {
                    return div().into_any_element();
                };
                this.render_home_virtual_row(&bot_owned, &row, number_chars, now, p, cx)
            }),
        )
        .size_full()
        .pl(px(14.))
        .pr_2()
        .pt(px(14.))
        .pb_3();

        let body = div()
            .id("bots-tab-items-body")
            .debug_selector(|| "bots-tab-items-body".into())
            .flex_1()
            .min_h_0()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("bot-home-items")
                    .debug_selector(|| "bot-home-items".into())
                    .role(gpui::Role::List)
                    .flex_1()
                    .min_h_0()
                    .size_full()
                    .child(list),
            );
        let pill = self.render_selection_pill(&bot.id, p, cx);
        let snooze = self.render_snooze_pick(p, window, cx);
        let cancel = self.render_cancel_ask(p, window, cx);
        div()
            .id("bots-tab-items")
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

    /// Keeps the item seen mark Home opened with, and moves the bot's mark past what it shows.
    fn note_items_seen(&mut self, bot: &BotConfig, cx: &mut Context<Self>) {
        let Some(host_seen) = self.items.upcoming.as_ref().map(|u| u.items_seen_event_id) else {
            return;
        };
        let seen = *self
            .items
            .seen_at_open
            .entry(bot.id.clone())
            .or_insert(host_seen);
        let newest = self
            .items
            .items
            .iter()
            .map(others_last_event)
            .max()
            .unwrap_or(0);
        if newest > host_seen.max(seen) {
            if let Some(up) = self.items.upcoming.as_mut() {
                up.items_seen_event_id = newest;
            }
            self.mark_bot_items_opened_silently(bot.id.clone(), cx);
        }
    }

    /// The seen mark Home opened with for this bot.
    fn seen_mark(&self, bot: &BotConfig) -> u64 {
        self.items
            .seen_at_open
            .get(&bot.id)
            .copied()
            .unwrap_or(u64::MAX)
    }

    /// Orca's filter chip under the header while "Needs you only" is on.
    fn render_filter_chip(p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let name = text::HM_STATUS_NEEDS_YOU.get();
        let remove = text::HM_REMOVE_FILTER.format(&[("name", name)]);
        div().flex().flex_wrap().gap_1().px_2().child(
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
                .child(div().min_w_0().truncate().child(name))
                .child(
                    Button::new("bot-home-filter-chip-clear")
                        .debug_selector(|| "bot-home-filter-chip-clear".into())
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::X).size(px(12.)))
                        .accessibility_label(remove)
                        .on_click(cx.listener(|this, _, _, cx| this.set_needs_you_only(false, cx))),
                ),
        )
    }

    /// Orca's item notes: a new bot's empty state, nothing matching, or all clear.
    fn render_home_note_kind(
        bot: &BotConfig,
        kind: HomeNoteKind,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match kind {
            HomeNoteKind::Empty => div()
                .id("bot-home-empty")
                .debug_selector(|| "bot-home-empty".into())
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .px_6()
                .py_12()
                .text_center()
                .text_size(ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(
                    Icon::new(IconName::ListChecks)
                        .size(px(20.))
                        .text_color(p.muted_foreground),
                )
                .child(
                    div()
                        .text_size(ui::TEXT_MD)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.foreground)
                        .child(text::HM_EMPTY_TITLE.get()),
                )
                .child(text::HM_EMPTY_BODY.format(&[("bot", &bot.name)]))
                .into_any_element(),
            HomeNoteKind::FilteredEmpty => div()
                .id("bot-home-filtered-empty")
                .debug_selector(|| "bot-home-filtered-empty".into())
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .px_6()
                .py_10()
                .text_center()
                .text_size(ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(text::HM_NOTHING_NEEDS_YOU.get())
                .child(
                    ui::secondary("bot-home-clear-filters", text::HM_CLEAR_FILTERS.get())
                        .xsmall()
                        .on_click(cx.listener(|this, _, _, cx| this.set_needs_you_only(false, cx))),
                )
                .into_any_element(),
        }
    }

    /// Orca's `BotItemRow`: status glyph, `#N`, the title, its age; hovering an open row swaps
    /// the age for Done, Snooze and More over the row's end.
    #[expect(
        clippy::too_many_arguments,
        reason = "a render helper over the list's shared state"
    )]
    pub(super) fn render_home_row(
        &self,
        bot: &BotConfig,
        item: &BotItem,
        group: Group,
        number_chars: usize,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let uid = item.uid.clone();
        let closed = !item.status.is_open();
        let hovered = self.items.hover_uid.as_deref() == Some(uid.as_str());
        let quiet = matches!(group, Group::Stale | Group::Snoozed);
        let sel = format!("bot-home-item-{uid}");
        let debug = sel.clone();
        let age = match (group, item.snoozed_until_ms) {
            (Group::Snoozed, Some(until)) => {
                text::HM_ROW_UNTIL.format(&[("when", &super::home_time::snooze_time(until, now))])
            }
            _ => short_age(activity_at(item, now), now),
        };
        let selected = self.items.selected.contains(&uid);
        let checkbox = selected || !self.items.selected.is_empty();
        let unseen = others_last_event(item) > self.seen_mark(bot);
        let check_group: SharedString = format!("bot-home-check-{uid}").into();
        // Needs you's question or Waiting's note shows in the row's hover card.
        let question = (group == Group::NeedsYou)
            .then(|| super::home_item::waiting_question(item))
            .flatten()
            .cloned();
        let wait_text = (question.is_none() && group != Group::Snoozed)
            .then(|| waiting_note(item))
            .flatten()
            .map(str::to_owned);
        let open_uid = uid.clone();
        let hover_uid = uid.clone();
        let mut row = div()
            .id(SharedString::from(sel))
            .debug_selector(move || debug.clone())
            .role(gpui::Role::ListItem)
            .aria_label(SharedString::from(format!(
                "#{} {}",
                item.number, item.title
            )))
            .tab_index(0)
            .focus_visible(|d| d.bg(p.sidebar_accent))
            .relative()
            .min_w_0()
            .flex()
            .items_start()
            .gap_2()
            .rounded(ui::RADIUS_XS)
            .px_2()
            .py_1()
            .cursor_pointer()
            .hover(|d| d.bg(p.sidebar_accent))
            .when(selected, |d| d.bg(p.sidebar_accent))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let now_on = this.items.hover_uid.as_deref() == Some(hover_uid.as_str());
                if *hovered != now_on {
                    this.items.hover_uid = hovered.then(|| hover_uid.clone());
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_home_item(open_uid.clone(), cx);
            }))
            .when_some(question, |d, q| {
                let title = item.title.clone();
                d.tooltip(move |window, cx| question_card(&title, &q, window, cx))
            })
            .when_some(wait_text, |d, note| {
                let title = item.title.clone();
                d.tooltip(move |window, cx| wait_hover_card(&title, &note, window, cx))
            })
            .child(
                div()
                    .group(check_group.clone())
                    .relative()
                    .h(px(20.))
                    .w(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .when(checkbox, gpui::Styled::invisible)
                            .group_hover(check_group.clone(), gpui::Styled::invisible)
                            .child(item_status_icon(item, self.item_live(item), quiet, p, cx)),
                    )
                    .child(
                        div()
                            .absolute()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(!checkbox, |d| {
                                d.invisible()
                                    .group_hover(check_group, gpui::Styled::visible)
                            })
                            .child(Self::render_row_checkbox(
                                &uid,
                                &item.title,
                                selected,
                                p,
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .min_w(px(f32::from(
                        u16::try_from(number_chars).unwrap_or(u16::MAX),
                    ) * 7.5
                        + 6.))
                    .pr(px(6.))
                    .text_size(ui::TEXT_MD)
                    .line_height(px(20.))
                    .text_color(p.muted_foreground)
                    .child(format!("#{}", item.number)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui::TEXT_MD)
                    .line_height(px(20.))
                    .text_color(if closed {
                        p.muted_foreground
                    } else {
                        p.foreground
                    })
                    .child(item.title.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    // Only open rows have hover actions, so only they give the age up to them.
                    .when(hovered && !closed, gpui::Styled::invisible)
                    .when(unseen, |d| {
                        d.child(
                            div()
                                .debug_selector(|| "bot-home-item-unseen".into())
                                .size(px(6.))
                                .rounded_full()
                                .bg(p.foreground),
                        )
                    })
                    .child(age),
            );
        if hovered && !closed {
            row = row.child(Self::render_row_actions(bot, item, now, p, cx));
        }
        row
    }

    /// Done, Snooze and More over an open row's end.
    fn render_row_actions(
        bot: &BotConfig,
        item: &BotItem,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let this = cx.weak_entity();
        let (bot_id, uid) = (bot.id.clone(), item.uid.clone());
        let done = {
            let (bot_id, uid) = (bot_id.clone(), uid.clone());
            cx.listener(move |this, _, _, cx| {
                this.update_item_status(bot_id.clone(), uid.clone(), BotItemStatus::Done, cx);
            })
        };
        let snoozed = is_snoozed(item, now);
        let title = item.title.clone();
        let initial_wait_note = waiting_note(item).map(str::to_owned);
        let snooze_menu = {
            let (this, bot_id, uid) = (this.clone(), bot_id.clone(), uid.clone());
            move |menu: gpui_component::menu::PopupMenu, _: &mut Window, _: &mut Context<_>| {
                Self::snooze_menu_for(menu, &this, &bot_id, std::slice::from_ref(&uid), snoozed)
            }
        };
        let more_menu =
            move |menu: gpui_component::menu::PopupMenu, _: &mut Window, _: &mut Context<_>| {
                let done = {
                    let (this, bot_id, uid) = (this.clone(), bot_id.clone(), uid.clone());
                    PopupMenuItem::new(text::HM_DONE.get())
                        .icon(IconName::CircleCheck)
                        .on_click(move |_, _, cx| {
                            this.update(cx, |v, cx| {
                                v.update_item_status(
                                    bot_id.clone(),
                                    uid.clone(),
                                    BotItemStatus::Done,
                                    cx,
                                );
                            })
                            .ok();
                        })
                };
                let waiting_item = {
                    let (this, bot_id, uid, title, wait_note) = (
                        this.clone(),
                        bot_id.clone(),
                        uid.clone(),
                        title.clone(),
                        initial_wait_note.clone(),
                    );
                    PopupMenuItem::element(move |_, cx| {
                        let p = ui::palette(cx);
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(16.))
                                    .flex()
                                    .justify_center()
                                    .child(waiting_icon(p)),
                            )
                            .child(hm::STATUS_WAITING.get())
                    })
                    .on_click(move |_, _, cx| {
                        this.update(cx, |v, cx| {
                            v.ask_waiting(
                                bot_id.clone(),
                                vec![(uid.clone(), title.clone())],
                                wait_note.clone(),
                                cx,
                            );
                        })
                        .ok();
                    })
                };
                let cancel = {
                    let (this, bot_id, uid) = (this.clone(), bot_id.clone(), uid.clone());
                    PopupMenuItem::new(text::HM_CANCEL_ITEM.get())
                        .icon(IconName::CircleSlash)
                        .on_click(move |_, _, cx| {
                            // Orca #47934: Cancel from row menu takes effect immediately without dialog.
                            this.update(cx, |v, cx| {
                                v.update_item_status(
                                    bot_id.clone(),
                                    uid.clone(),
                                    BotItemStatus::Canceled,
                                    cx,
                                );
                            })
                            .ok();
                        })
                };
                Self::snooze_menu_for(
                    menu.item(done).item(waiting_item).separator(),
                    &this,
                    &bot_id,
                    std::slice::from_ref(&uid),
                    snoozed,
                )
                .separator()
                .item(cancel)
            };
        div()
            .absolute()
            .right(px(4.))
            .top(px(2.))
            .flex()
            .items_center()
            .gap_0p5()
            .pl(px(18.))
            .bg(p.sidebar_accent)
            .child(
                Button::new("bot-home-row-done")
                    .debug_selector(|| "bot-home-row-done".into())
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Check).size(px(14.)))
                    .tooltip(text::HM_DONE.get())
                    .accessibility_label(text::HM_DONE.get())
                    .on_click(done),
            )
            .child(
                Button::new("bot-home-row-snooze")
                    .debug_selector(|| "bot-home-row-snooze".into())
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Clock).size(px(14.)))
                    .tooltip(text::HM_SNOOZE.get())
                    .accessibility_label(text::HM_SNOOZE.get())
                    .dropdown_menu(snooze_menu),
            )
            .child(
                Button::new("bot-home-row-more")
                    .debug_selector(|| "bot-home-row-more".into())
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Ellipsis).size(px(14.)))
                    .tooltip(text::HM_MORE.get())
                    .accessibility_label(text::HM_MORE.get())
                    .dropdown_menu(more_menu),
            )
    }
}

/// The row's hover card while Waiting: title and what it waits on (Orca `BotItemRow.tsx` L196).
fn wait_hover_card(
    title: &str,
    note: &str,
    window: &mut Window,
    cx: &mut gpui::App,
) -> gpui::AnyView {
    let (title, note) = (title.to_owned(), note.to_owned());
    gpui_component::tooltip::Tooltip::element(move |_, cx| {
        let p = ui::palette(cx);
        div()
            .debug_selector(|| "bot-home-wait-card".into())
            .max_w(px(300.))
            .flex()
            .flex_col()
            .text_size(ui::TEXT_XS)
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.popover_foreground)
                    .child(title.clone()),
            )
            .child(
                div()
                    .mt_1()
                    .text_color(p.muted_foreground)
                    .child(note.clone()),
            )
    })
    .build(window, cx)
}

/// The row's hover card while it needs you: the title, then each question line with its quick
/// replies (read-only here; they answer on the item). Orca `BotItemQuestionCard`.
fn question_card(
    title: &str,
    q: &krill_proto::bots::BotItemEvent,
    window: &mut Window,
    cx: &mut gpui::App,
) -> gpui::AnyView {
    let title = title.to_owned();
    let gist = q
        .text
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(str::to_owned);
    let decisions = q.decisions.clone();
    gpui_component::tooltip::Tooltip::element(move |_, cx| {
        let p = ui::palette(cx);
        div()
            .debug_selector(|| "bot-home-question-card".into())
            .max_w(px(300.))
            .flex()
            .flex_col()
            .text_size(ui::TEXT_XS)
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.popover_foreground)
                    .child(title.clone()),
            )
            .children(gist.clone().map(|g| {
                div()
                    .mt_1()
                    .truncate()
                    .text_color(p.muted_foreground)
                    .child(g)
            }))
            .children(decisions.iter().map(|d| {
                div()
                    .mt_2()
                    .child(
                        div()
                            .text_color(p.popover_foreground)
                            .child(d.question.clone()),
                    )
                    .when(!d.options.is_empty(), |el| {
                        el.child(div().mt_1p5().flex().flex_wrap().gap_1().children(
                            d.options.iter().enumerate().map(|(i, o)| {
                                div()
                                    .rounded(px(10.))
                                    .border_1()
                                    .border_color(p.border)
                                    .px_2()
                                    .py_0p5()
                                    .text_size(ui::TEXT_2XS)
                                    .text_color(p.popover_foreground)
                                    .child(o.clone())
                                    .when(d.recommended == Some(i), |el| {
                                        el.child(
                                            div()
                                                .ml(px(5.))
                                                .text_color(p.muted_foreground)
                                                .child(text::HM_RECOMMENDED.get()),
                                        )
                                    })
                            }),
                        ))
                    })
            }))
    })
    .build(window, cx)
}

/// What the header's filter icon needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeFilter {
    pub needs_you_only: bool,
    /// Items that need you, the menu's count.
    pub needs_you: usize,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::{Modifiers, px};
    use krill_proto::bots::{
        BotConfig, BotId, BotItem, BotItemStatus, BotRpcRequest, BotRpcResponse,
    };

    use super::*;

    fn make_planned_items(bot_id: &BotId, count: usize, now: u64) -> Vec<BotItem> {
        (1..=count)
            .map(|n| {
                let n_u64 = u64::try_from(n).unwrap_or(0);
                let ts = now.saturating_sub(n_u64 * 1_000);
                BotItem {
                    uid: format!("it-{n}"),
                    number: n_u64,
                    owner_bot_id: bot_id.clone(),
                    title: format!("Planned item #{n}"),
                    description: None,
                    done_means: None,
                    status: BotItemStatus::Planned,
                    close_note: None,
                    links: Vec::new(),
                    events: Vec::new(),
                    snoozed_until_ms: None,
                    version: 1,
                    last_activity_at_ms: ts,
                    created_at_ms: ts,
                    updated_at_ms: ts,
                    closed_at_ms: None,
                }
            })
            .collect()
    }

    #[test]
    fn number_column_chars_matches_formatted_width() {
        assert_eq!(number_column_chars(0), 3);
        assert_eq!(number_column_chars(9), 3);
        assert_eq!(number_column_chars(99), 3);
        assert_eq!(number_column_chars(100), 4);
        assert_eq!(number_column_chars(500), 4);
        assert_eq!(number_column_chars(12345), 6);
    }

    #[gpui::test]
    fn home_panel_virtualizes_500_items_and_skips_regroup_on_hover(cx: &mut gpui::TestAppContext) {
        let now = now_unix_ms();
        let bot_id = BotId::new("perf-bot").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "Perf Bot", "agy", now).unwrap();
        let items = Arc::new(Mutex::new(make_planned_items(&bot_id, 500, now)));
        let bot_rpc = bot.clone();
        let items_rpc = items.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![bot_rpc.clone()])),
            BotRpcRequest::ListItems { .. } => {
                Ok(BotRpcResponse::Items(items_rpc.lock().unwrap().clone()))
            }
            BotRpcRequest::ListBotSessions { .. } => Ok(BotRpcResponse::BotSessions(vec![])),
            BotRpcRequest::ConnectorStatus { .. } => Ok(BotRpcResponse::ConnectorStatuses(vec![])),
            BotRpcRequest::ListAccessRequests { .. } => Ok(BotRpcResponse::AccessRequests(vec![])),
            BotRpcRequest::GetBotUpcoming { .. } => Ok(BotRpcResponse::Upcoming(
                krill_proto::bots::BotUpcoming::default(),
            )),
            BotRpcRequest::GetSoul { .. } | BotRpcRequest::GetMemory { .. } => {
                Ok(BotRpcResponse::Text(String::new()))
            }
            BotRpcRequest::ListMemoryTopics { .. } => Ok(BotRpcResponse::MemoryTopics(vec![])),
            BotRpcRequest::ListSuggestions { .. } => Ok(BotRpcResponse::Suggestions(vec![])),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.simulate_resize(gpui::size(px(360.), px(480.)));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, super::super::DetailTab::Items, cx);
            });
        });
        vcx.run_until_parked();

        // Expand Planned group and click "Show 495 more".
        let header = vcx
            .debug_bounds("bot-home-group-planned")
            .expect("planned header");
        vcx.simulate_click(header.center(), Modifiers::default());
        vcx.run_until_parked();
        let more = vcx
            .debug_bounds("bot-home-show-more-planned")
            .expect("show more");
        vcx.simulate_click(more.center(), Modifiers::default());
        vcx.run_until_parked();

        // Only the viewport + overscan rows mount, NOT all 500 items (Orca #45971).
        let rendered_top =
            vcx.update(|_, cx| view.read(cx).items.home_list.rendered_item_rows.get());
        assert!(rendered_top > 0, "visible items must render");
        assert!(
            rendered_top <= 50,
            "expected virtualized window of <= 50 rows for 500 items, rendered {rendered_top}"
        );
        assert!(vcx.debug_bounds("bot-home-item-it-1").is_some());
        assert!(
            vcx.debug_bounds("bot-home-item-it-450").is_none(),
            "far off-screen row must stay unmounted"
        );

        // Hovering a visible row does NOT rebuild/re-group the 500-item cache.
        let rebuilds_before_hover = vcx.update(|_, cx| view.read(cx).items.home_list.rebuild_count);
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.items.hover_uid = Some("it-1".to_owned());
                cx.notify();
            });
        });
        vcx.run_until_parked();
        let rebuilds_after_hover = vcx.update(|_, cx| view.read(cx).items.home_list.rebuild_count);
        assert_eq!(
            rebuilds_before_hover, rebuilds_after_hover,
            "hover must hit the cached Home projection without re-grouping 500 items"
        );

        // Scrolling down to item #300 mounts item #300 and unmounts item #1.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.items.home_list.list_state.scroll_to(gpui::ListOffset {
                    item_ix: 300,
                    offset_in_item: px(0.),
                });
                cx.notify();
            });
        });
        vcx.run_until_parked();
        let rendered_mid =
            vcx.update(|_, cx| view.read(cx).items.home_list.rendered_item_rows.get());
        assert!(
            rendered_mid > 0 && rendered_mid <= 50,
            "rendered_mid={rendered_mid}"
        );
        assert!(vcx.debug_bounds("bot-home-item-it-300").is_some());
        assert!(vcx.debug_bounds("bot-home-item-it-1").is_none());

        // Updating item #300 to newest moves it to top while staying virtualized.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                let mut updated = v
                    .items
                    .items
                    .iter()
                    .find(|i| i.uid == "it-300")
                    .unwrap()
                    .clone();
                updated.title = "Updated #300".to_owned();
                updated.version += 1;
                updated.last_activity_at_ms = now + 5_000;
                v.upsert_item(updated, &bot_id);
                v.items.home_list.list_state.scroll_to(gpui::ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.),
                });
                cx.notify();
            });
        });
        vcx.run_until_parked();
        assert!(vcx.debug_bounds("bot-home-item-it-300").is_some());
        let rendered_after =
            vcx.update(|_, cx| view.read(cx).items.home_list.rendered_item_rows.get());
        assert!(
            rendered_after > 0 && rendered_after <= 50,
            "rendered_after={rendered_after}"
        );
    }

    #[gpui::test]
    fn group_panel_virtualizes_300_items_across_bots(cx: &mut gpui::TestAppContext) {
        let now = now_unix_ms();
        let b1 = BotId::new("g-bot-1").unwrap();
        let b2 = BotId::new("g-bot-2").unwrap();
        let cfg1 = BotConfig::new(b1.clone(), "Alpha", "agy", now).unwrap();
        let cfg2 = BotConfig::new(b2.clone(), "Beta", "agy", now).unwrap();
        let mut all_items = make_planned_items(&b1, 150, now);
        for mut it in make_planned_items(&b2, 150, now) {
            it.uid = format!("b2-{}", it.uid);
            all_items.push(it);
        }
        let bots_rpc = vec![cfg1, cfg2];
        let items_rpc = all_items.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(bots_rpc.clone())),
            BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(items_rpc.clone())),
            BotRpcRequest::ListBotSessions { .. } => Ok(BotRpcResponse::BotSessions(vec![])),
            BotRpcRequest::ConnectorStatus { .. } => Ok(BotRpcResponse::ConnectorStatuses(vec![])),
            BotRpcRequest::ListAccessRequests { .. } => Ok(BotRpcResponse::AccessRequests(vec![])),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.simulate_resize(gpui::size(px(360.), px(480.)));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_group_in_panel("grp-perf", &[b1.clone(), b2.clone()], &[], cx);
                v.set_group_chat_scope(super::super::group_panel::GroupChatScope::AllTheirWork, cx);
            });
        });
        vcx.run_until_parked();

        let header = vcx
            .debug_bounds("bot-group-bucket-planned")
            .expect("group planned header");
        vcx.simulate_click(header.center(), Modifiers::default());
        vcx.run_until_parked();
        let more = vcx
            .debug_bounds("grp-more-planned")
            .expect("group show more");
        vcx.simulate_click(more.center(), Modifiers::default());
        vcx.run_until_parked();

        let mounted = vcx.update(|_, cx| {
            view.read(cx)
                .group_panel
                .list_cache
                .rendered_item_rows
                .get()
        });
        assert!(mounted > 0, "visible group items must mount");
        assert!(
            mounted <= 50,
            "expected virtualized group list of <= 50 rows for 300 items, mounted {mounted}"
        );
        assert!(vcx.debug_bounds("bot-home-item-it-1").is_some());
        assert!(vcx.debug_bounds("bot-home-item-b2-it-150").is_none());
    }
}
