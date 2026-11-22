// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's bot Home in the right sidebar (`BotActivityTab`): the bot's items in folding groups
//! (Needs you and In progress open; Planned, Snoozed, Stale and Closed folded), five rows each
//! then "Show N more", then Automations (Tidy memory, Look around and the bot's schedules).
//! A row opens its item in the list's place (`home_item.rs`). The shell draws the tab bar and
//! the header (with the filter icon) above it.

use std::cmp::Reverse;

use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::SessionId;
use krill_proto::bots::{BotConfig, BotItem, BotItemLink, BotItemStatus};

use super::{View, now_unix_ms};
use crate::i18n::catalog::bots as text;
use crate::ui;

/// Rows a group shows before "Show N more" (Orca `BOT_ITEM_GROUP_ROWS`).
const GROUP_ROWS: usize = 5;
/// Orca's default stale setting. Krill has no per-bot setting yet, so every bot uses it.
const STALE_DAYS: u64 = 5;
const DAY_MS: u64 = 86_400_000;

/// Orca's item groups, in list order.
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
            Self::Planned => text::HM_STATUS_PLANNED.get(),
            Self::Snoozed => text::HM_GROUP_SNOOZED.get(),
            Self::Stale => text::HM_GROUP_STALE.get(),
            Self::Closed => text::HM_GROUP_CLOSED.get(),
        }
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

/// Orca's derived Stale: Needs you, or In progress with no live worker, untouched for the stale
/// days; Planned for three times that. A snoozed item is never stale.
fn is_stale(item: &BotItem, live: bool, now: u64) -> bool {
    if is_snoozed(item, now) {
        return false;
    }
    let idle = now.saturating_sub(activity_at(item, now));
    match item.status {
        BotItemStatus::NeedsYou => idle >= STALE_DAYS * DAY_MS,
        BotItemStatus::Working => !live && idle >= STALE_DAYS * DAY_MS,
        BotItemStatus::Planned => idle >= 3 * STALE_DAYS * DAY_MS,
        BotItemStatus::Done | BotItemStatus::Canceled => false,
    }
}

pub(super) fn group_of(item: &BotItem, live: bool, now: u64) -> Group {
    match item.status {
        BotItemStatus::Done | BotItemStatus::Canceled => Group::Closed,
        _ if is_snoozed(item, now) => Group::Snoozed,
        _ if is_stale(item, live, now) => Group::Stale,
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
    now: u64,
) -> [Vec<&'a BotItem>; 6] {
    let mut groups: [Vec<&BotItem>; 6] = Default::default();
    for item in items {
        groups[group_of(item, live(item), now) as usize].push(item);
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

/// Orca's `formatShortTimeAgo`: now, 6m, 22h, 3d.
pub(super) fn short_age(ts_ms: u64, now_ms: u64) -> String {
    let delta = now_ms.saturating_sub(ts_ms);
    if delta < 60_000 {
        return "now".to_owned();
    }
    let minutes = delta / 60_000;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
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

/// A wake or event time as the rows say it: `HH:MM` today, else `MM-DD HH:MM`.
pub(super) fn clock_label(ts_ms: u64, now_ms: u64) -> String {
    let offset = local_offset_secs(now_ms);
    let local = |ms: u64| i64::try_from(ms / 1000).unwrap_or(0) + offset;
    let (t, n) = (local(ts_ms), local(now_ms));
    let Some(c) = crate::local_time::civil_from_epoch(t) else {
        return String::new();
    };
    if t.div_euclid(86_400) == n.div_euclid(86_400) {
        format!("{:02}:{:02}", c.hour, c.minute)
    } else {
        format!("{:02}-{:02} {:02}:{:02}", c.month, c.day, c.hour, c.minute)
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
        BotItemStatus::Done => Icon::new(IconName::CircleCheck)
            .size(px(10.))
            .text_color(p.success)
            .into_any_element(),
        BotItemStatus::Canceled => Icon::new(IconName::CircleSlash)
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

    pub(super) fn render_home_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if let Some(item) = self
            .items
            .open_uid
            .as_ref()
            .and_then(|uid| self.items.items.iter().find(|i| &i.uid == uid))
            .cloned()
        {
            return self.render_home_item(bot, &item, p, window, cx);
        }
        let now = now_unix_ms();
        let filter_on = self.items.needs_you_only;
        let all = self.items.items.clone();
        let shown = all
            .iter()
            .filter(|i| !filter_on || i.status == BotItemStatus::NeedsYou);
        let groups = grouped(shown, |i| self.item_live(i), now);
        // Every row's number column fits the widest number, so titles start in one column.
        let number_chars = all
            .iter()
            .map(|i| format!("#{}", i.number).len())
            .max()
            .unwrap_or(3);

        let mut body = div()
            .id("bots-tab-items-body")
            .debug_selector(|| "bots-tab-items-body".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pl(px(14.))
            .pr_2()
            .pt(px(14.))
            .pb_3()
            .flex()
            .flex_col()
            .gap(px(14.));
        if filter_on {
            body = body.child(Self::render_filter_chip(p, cx));
        }
        let mut list = div()
            .id("bot-home-items")
            .debug_selector(|| "bot-home-items".into())
            .role(gpui::Role::List)
            .flex()
            .flex_col()
            .gap_2();
        for group in Group::ALL {
            let rows = &groups[group as usize];
            if rows.is_empty() {
                continue;
            }
            list = list.child(self.render_home_group(bot, group, rows, number_chars, now, p, cx));
        }
        body = body
            .child(list)
            .children(Self::render_home_note(bot, &all, &groups, filter_on, p, cx));
        body.child(self.render_automations(bot, p, cx))
            .children(self.render_suggestions(bot, p, cx))
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
    fn render_home_note(
        bot: &BotConfig,
        all: &[BotItem],
        groups: &[Vec<&BotItem>; 6],
        filter_on: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if all.is_empty() {
            return Some(
                div()
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
            );
        }
        if filter_on {
            if groups.iter().any(|g| !g.is_empty()) {
                return None;
            }
            return Some(
                div()
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
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_needs_you_only(false, cx)),
                            ),
                    )
                    .into_any_element(),
            );
        }
        let quiet = groups[Group::NeedsYou as usize].is_empty()
            && groups[Group::Working as usize].is_empty();
        quiet.then(|| {
            div()
                .debug_selector(|| "bot-home-all-clear".into())
                .px_6()
                .py_6()
                .text_center()
                .text_size(ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(text::HM_ALL_CLEAR.get())
                .into_any_element()
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a render helper over the list's shared state"
    )]
    fn render_home_group(
        &self,
        bot: &BotConfig,
        group: Group,
        rows: &[&BotItem],
        number_chars: usize,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let open = group.folded_by_default() == self.items.folds.contains(group.id());
        let header = section_header(
            &format!("bot-home-group-{}", group.id()),
            group.title(),
            open,
            (!open).then(|| rows.len().to_string()),
            p,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if !this.items.folds.remove(group.id()) {
                this.items.folds.insert(group.id());
            }
            cx.notify();
        }));
        let mut section = div().flex().flex_col().gap_0p5().child(header);
        if !open {
            return section;
        }
        let shown = self
            .items
            .group_shown
            .get(group.id())
            .copied()
            .unwrap_or(GROUP_ROWS);
        for item in rows.iter().take(shown) {
            section =
                section.child(self.render_home_row(bot, item, group, number_chars, now, p, cx));
        }
        let more = rows.len().saturating_sub(shown);
        if more > 0 {
            let sel = format!("bot-home-show-more-{}", group.id());
            let debug = sel.clone();
            section = section.child(
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
                            .insert(group.id(), shown + GROUP_ROWS.max(more));
                        cx.notify();
                    })),
            );
        }
        section
    }

    /// Orca's `BotItemRow`: status glyph, `#N`, the title, its age; hovering an open row swaps
    /// the age for Done, Snooze and More over the row's end.
    #[expect(
        clippy::too_many_arguments,
        reason = "a render helper over the list's shared state"
    )]
    fn render_home_row(
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
                text::HM_ROW_UNTIL.format(&[("when", &clock_label(until, now))])
            }
            _ => short_age(activity_at(item, now), now),
        };
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
            .child(
                div()
                    .h(px(20.))
                    .w(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(status_icon(item.status, self.item_live(item), quiet, p, cx)),
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
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    // Only open rows have hover actions, so only they give the age up to them.
                    .when(hovered && !closed, gpui::Styled::invisible)
                    .child(age),
            );
        if hovered && !closed {
            row = row.child(Self::render_row_actions(bot, item, p, cx));
        }
        row
    }

    /// Done, Snooze and More over an open row's end.
    fn render_row_actions(
        bot: &BotConfig,
        item: &BotItem,
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
        let snooze_menu = {
            let (this, bot_id, uid) = (this.clone(), bot_id.clone(), uid.clone());
            move |menu: gpui_component::menu::PopupMenu, _: &mut Window, _: &mut Context<_>| {
                Self::snooze_menu_items(menu, &this, &bot_id, &uid)
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
                let cancel = {
                    let (this, bot_id, uid) = (this.clone(), bot_id.clone(), uid.clone());
                    PopupMenuItem::new(text::HM_CANCEL_ITEM.get())
                        .icon(IconName::CircleSlash)
                        .on_click(move |_, _, cx| {
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
                Self::snooze_menu_items(menu.item(done).separator(), &this, &bot_id, &uid)
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

    /// Orca's three snooze presets, each with its wake time.
    pub(super) fn snooze_menu_items(
        mut menu: gpui_component::menu::PopupMenu,
        this: &gpui::WeakEntity<Self>,
        bot_id: &krill_proto::bots::BotId,
        uid: &str,
    ) -> gpui_component::menu::PopupMenu {
        let now = now_unix_ms();
        for (label, at) in snooze_presets(now, local_offset_secs(now)) {
            let (this, bot_id, uid) = (this.clone(), bot_id.clone(), uid.to_owned());
            menu = menu.item(
                PopupMenuItem::new(format!("{label}  {}", clock_label(at, now))).on_click(
                    move |_, _, cx| {
                        this.update(cx, |v, cx| {
                            v.snooze_item(bot_id.clone(), uid.clone(), Some(at), cx);
                        })
                        .ok();
                    },
                ),
            );
        }
        menu
    }
}

/// What the header's filter icon needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeFilter {
    pub needs_you_only: bool,
    /// Items that need you, the menu's count.
    pub needs_you: usize,
}
