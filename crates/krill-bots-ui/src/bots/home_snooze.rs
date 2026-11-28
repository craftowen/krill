// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's Snooze until dialog (`BotItemSnoozeDialog`): the presets first, then a month grid and a
//! quarter-hour time for a custom wake. Also the batch item writes Home's selection uses.

use gpui::{Context, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::bots::{
    BotId, BotItemStatus, BotRpcRequest, BotRpcResponse, FieldPatch, UpdateBotItemParams,
};

use super::home::{local_offset_secs_at_epoch, local_wall_to_epoch_ms_with, snooze_presets_with};
use super::home_time::{clock_minutes as clock, snooze_time};
use super::rpc::call_checked;
use super::{View, now_unix_ms};
use crate::i18n::{
    Message,
    catalog::{bot_home as hm, bots as text},
};
use crate::local_time::civil_from_epoch;
use crate::ui;

const DAY_SECS: i64 = 86_400;
/// The latest a snooze may reach (Orca `BOT_ITEM_SNOOZE_MAX_MS`).
const MAX_MS: u64 = 366 * DAY_SECS as u64 * 1000;
const SLOT_MINUTES: u32 = 15;
const DEFAULT_MINUTES: u32 = 9 * 60;
const FULL_MONTHS: [Message; 12] = [
    hm::MONTH_FULL_01,
    hm::MONTH_FULL_02,
    hm::MONTH_FULL_03,
    hm::MONTH_FULL_04,
    hm::MONTH_FULL_05,
    hm::MONTH_FULL_06,
    hm::MONTH_FULL_07,
    hm::MONTH_FULL_08,
    hm::MONTH_FULL_09,
    hm::MONTH_FULL_10,
    hm::MONTH_FULL_11,
    hm::MONTH_FULL_12,
];
const CAL_DAYS: [Message; 7] = [
    hm::CAL_SUN,
    hm::CAL_MON,
    hm::CAL_TUE,
    hm::CAL_WED,
    hm::CAL_THU,
    hm::CAL_FRI,
    hm::CAL_SAT,
];

/// The Snooze until dialog's state.
#[derive(Debug, Clone)]
pub(super) struct SnoozePick {
    pub bot_id: BotId,
    pub uids: Vec<String>,
    /// On the month grid rather than the presets.
    pub picking: bool,
    /// The chosen local day (days since 1970-01-01 in the local offset).
    pub day: i64,
    pub minutes: u32,
    /// The first day of the month the grid shows.
    pub month_first: i64,
}

/// `minutes` past local midnight of `day`, using the local timezone offset at the target time.
fn at_with(day: i64, minutes: u32, offset_secs_at: impl Fn(i64) -> i64) -> u64 {
    local_wall_to_epoch_ms_with(day, i64::from(minutes), offset_secs_at)
}

fn at_local(day: i64, minutes: u32) -> u64 {
    at_with(day, minutes, local_offset_secs_at_epoch)
}

fn today_with(now: u64, offset_secs_at: impl Fn(i64) -> i64) -> i64 {
    let epoch = i64::try_from(now / 1000).unwrap_or(0);
    (epoch + offset_secs_at(epoch)).div_euclid(DAY_SECS)
}

fn today_local(now: u64) -> i64 {
    today_with(now, local_offset_secs_at_epoch)
}

#[cfg(test)]
fn today(now: u64, offset: i64) -> i64 {
    today_with(now, |_| offset)
}

/// Quarter hours on `day` still ahead of `now` and within the cap (Orca `botItemSnoozeSlots`).
fn slots_with(day: i64, now: u64, offset_secs_at: impl Fn(i64) -> i64) -> Vec<u32> {
    (0..24 * 60)
        .step_by(SLOT_MINUTES as usize)
        .filter(|&m| {
            let until = at_with(day, m, &offset_secs_at);
            until > now && until <= now + MAX_MS
        })
        .collect()
}

fn slots_local(day: i64, now: u64) -> Vec<u32> {
    slots_with(day, now, local_offset_secs_at_epoch)
}

#[cfg(test)]
pub(super) fn slots(day: i64, now: u64, offset: i64) -> Vec<u32> {
    slots_with(day, now, |_| offset)
}

/// `minutes` if still open on `day`, else the next open slot, else the day's last.
fn slot_on_with(
    day: i64,
    minutes: u32,
    now: u64,
    offset_secs_at: impl Fn(i64) -> i64,
) -> Option<u32> {
    let open = slots_with(day, now, offset_secs_at);
    open.iter()
        .copied()
        .find(|&s| s >= minutes)
        .or_else(|| open.last().copied())
}

fn slot_on_local(day: i64, minutes: u32, now: u64) -> Option<u32> {
    slot_on_with(day, minutes, now, local_offset_secs_at_epoch)
}

#[cfg(test)]
pub(super) fn slot_on(day: i64, minutes: u32, now: u64, offset: i64) -> Option<u32> {
    slot_on_with(day, minutes, now, |_| offset)
}

/// The first and last days with an open slot.
fn day_range_with(now: u64, offset_secs_at: impl Fn(i64) -> i64) -> (i64, i64) {
    let mut first = today_with(now, &offset_secs_at);
    if slots_with(first, now, &offset_secs_at).is_empty() {
        first += 1;
    }
    (first, today_with(now + MAX_MS, &offset_secs_at))
}

fn day_range_local(now: u64) -> (i64, i64) {
    day_range_with(now, local_offset_secs_at_epoch)
}

#[cfg(test)]
pub(super) fn day_range(now: u64, offset: i64) -> (i64, i64) {
    day_range_with(now, |_| offset)
}

/// The picker's presets: the menu's three, then 9 AM a week out (Orca
/// `botItemSnoozePickerPresets`).
fn picker_presets_with(now: u64, offset_secs_at: impl Fn(i64) -> i64) -> Vec<(String, u64)> {
    let week = at_with(
        today_with(now, &offset_secs_at) + 7,
        DEFAULT_MINUTES,
        &offset_secs_at,
    );
    let mut presets: Vec<_> = snooze_presets_with(now, &offset_secs_at)
        .into_iter()
        .filter(|(_, at)| *at != week)
        .collect();
    presets.push((hm::SNOOZE_WEEK.get().to_owned(), week));
    presets
}

fn picker_presets_local(now: u64) -> Vec<(String, u64)> {
    picker_presets_with(now, local_offset_secs_at_epoch)
}

fn civil(day: i64) -> (i32, u8, u8) {
    civil_from_epoch(day * DAY_SECS).map_or((1970, 1, 1), |c| (c.year, c.month, c.day))
}

fn month_first(day: i64) -> i64 {
    day - i64::from(civil(day).2) + 1
}

/// The first day of the month after the one `first` starts.
fn next_month(first: i64) -> i64 {
    month_first(first + 31)
}

fn prev_month(first: i64) -> i64 {
    month_first(first - 1)
}

impl View {
    /// Opens the Snooze until dialog for these items and focuses its modal scope when a window is provided.
    pub(super) fn open_snooze_pick_in(
        &mut self,
        bot_id: BotId,
        uids: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window, cx);
        self.open_snooze_pick(bot_id, uids, cx);
    }

    /// Opens the Snooze until dialog for these items.
    pub(super) fn open_snooze_pick(
        &mut self,
        bot_id: BotId,
        uids: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let now = now_unix_ms();
        let day = today_local(now) + 1;
        self.items.snooze_pick = Some(SnoozePick {
            bot_id,
            uids,
            picking: false,
            day,
            minutes: DEFAULT_MINUTES,
            month_first: month_first(day),
        });
        cx.notify();
    }

    /// Writes one change to each of these items, one after another, then clears the selection.
    pub(super) fn update_items_batch(
        &mut self,
        _bot_id: BotId,
        params: Vec<UpdateBotItemParams>,
        cx: &mut Context<Self>,
    ) {
        if params.is_empty() || self.items.busy {
            return;
        }
        let rpc = self.rpc.clone();
        let req_host = self.host.clone();
        let req_gen = self.items.rpc_gen;
        self.items.busy = true;
        self.error = None;
        self.notice = None;
        self.items.selected.clear();
        self.item_task = Some(cx.spawn(async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    params
                        .into_iter()
                        .map(|p| call_checked(&rpc, &BotRpcRequest::UpdateItem(Box::new(p))))
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host || this.items.rpc_gen != req_gen {
                    return;
                }
                this.items.busy = false;
                for res in results {
                    match res {
                        Ok(BotRpcResponse::Item(updated)) => {
                            let owner_id = updated.owner_bot_id.clone();
                            this.upsert_item(*updated, &owner_id);
                        }
                        Ok(BotRpcResponse::Error(err)) | Err(err) => {
                            this.error = Some(SharedString::from(err));
                        }
                        Ok(_) => {}
                    }
                }
                this.emit_badge(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Snoozes (or unsnoozes, `None`) these items.
    pub(super) fn snooze_items(
        &mut self,
        bot_id: BotId,
        uids: &[String],
        until: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let patch = until.map_or(FieldPatch::Clear, FieldPatch::Set);
        let params = uids
            .iter()
            .map(|uid| item_params(uid, None, patch.clone()))
            .collect();
        self.update_items_batch(bot_id, params, cx);
    }

    /// Marks these items with `status` (clearing any `waiting:` note when moving off Waiting).
    pub(super) fn set_items_status(
        &mut self,
        bot_id: BotId,
        uids: &[String],
        status: BotItemStatus,
        cx: &mut Context<Self>,
    ) {
        let params = uids
            .iter()
            .map(|uid| {
                let was_waiting = self
                    .items
                    .items
                    .iter()
                    .find(|i| &i.uid == uid)
                    .is_some_and(|i| super::home::waiting_note(i).is_some());
                let mut p = item_params(uid, Some(status), FieldPatch::Unchanged);
                if was_waiting {
                    p.close_note = Some(String::new());
                }
                p
            })
            .collect();
        self.update_items_batch(bot_id, params, cx);
    }

    /// The Snooze until dialog over the window, while open.
    pub(super) fn render_snooze_pick(
        &self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let pick = self.items.snooze_pick.clone()?;
        let now = now_unix_ms();
        let busy = self.items.busy;
        let close = cx.listener(|this, _, _, cx| {
            this.items.snooze_pick = None;
            cx.notify();
        });
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(hm::SNOOZE_UNTIL.get()),
            )
            .child(
                Button::new("bot-snooze-pick-close")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::X).size(px(14.)))
                    .accessibility_label(hm::CLOSE.get())
                    .on_click(close),
            );
        let body = if pick.picking {
            self.render_snooze_grid(&pick, now, p, cx)
                .into_any_element()
        } else {
            let mut rows = div().flex().flex_col().gap_0p5().mx(px(-8.));
            for (i, (label, until)) in picker_presets_local(now).into_iter().enumerate() {
                let (bot_id, uids) = (pick.bot_id.clone(), pick.uids.clone());
                rows = rows.child(
                    div()
                        .id(("bot-snooze-preset", i))
                        .debug_selector(move || format!("bot-snooze-preset-{i}"))
                        .role(gpui::Role::Button)
                        .tab_index(0)
                        .h(px(32.))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(ui::RADIUS_SM)
                        .px_2()
                        .text_size(ui::TEXT_SM)
                        .when(busy, |d| d.opacity(0.5))
                        .when(!busy, |d| {
                            d.cursor_pointer()
                                .hover(|d| d.bg(p.accent).text_color(p.accent_foreground))
                        })
                        .child(div().flex_1().child(label))
                        .child(
                            div()
                                .text_color(p.muted_foreground)
                                .child(snooze_time(until, now)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.items.busy {
                                return;
                            }
                            this.items.snooze_pick = None;
                            // Recompute preset timestamp at click time so a dialog left open
                            // past a preset boundary never sends a past timestamp (`BOT-47`).
                            let fresh_now = super::now_unix_ms();
                            let fresh_until = picker_presets_local(fresh_now)
                                .get(i)
                                .map_or(until, |(_, ts)| *ts)
                                .max(fresh_now.saturating_add(60_000));
                            this.snooze_items(bot_id.clone(), &uids, Some(fresh_until), cx);
                        })),
                );
            }
            rows.child(div().my_1().h(px(1.)).bg(p.border))
                .child(
                    div()
                        .id("bot-snooze-custom")
                        .debug_selector(|| "bot-snooze-custom".into())
                        .role(gpui::Role::Button)
                        .tab_index(0)
                        .h(px(32.))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(ui::RADIUS_SM)
                        .px_2()
                        .cursor_pointer()
                        .text_size(ui::TEXT_SM)
                        .hover(|d| d.bg(p.accent).text_color(p.accent_foreground))
                        .child(
                            Icon::new(IconName::CalendarClock)
                                .size(px(16.))
                                .text_color(p.muted_foreground),
                        )
                        .child(div().flex_1().child(hm::SNOOZE_CUSTOM.get()))
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .size(px(16.))
                                .text_color(p.muted_foreground),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(pick) = this.items.snooze_pick.as_mut() {
                                pick.picking = true;
                            }
                            cx.notify();
                        })),
                )
                .into_any_element()
        };
        let dialog = div()
            .id("bot-snooze-pick")
            .debug_selector(|| "bot-snooze-pick".into())
            .role(gpui::Role::Dialog)
            .aria_label(hm::SNOOZE_UNTIL.get())
            .w(px(340.))
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .rounded(ui::RADIUS_LG)
            .border_1()
            .border_color(p.border)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .font_family(ui::FONT_UI)
            .shadow_lg()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(header)
            .child(body);
        let backdrop = ui::modal_focus_scope(
            div()
                .id("bot-snooze-pick-backdrop")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" && !ev.keystroke.modifiers.modified() {
                        this.items.snooze_pick = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.items.snooze_pick = None;
                        cx.notify();
                    }),
                )
                .child(dialog),
            &self.focus,
        );
        Some(ui::modal_backdrop(window.viewport_size(), backdrop).into_any_element())
    }

    fn render_snooze_grid(
        &self,
        pick: &SnoozePick,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let (first, last) = day_range_local(now);
        let chosen = pick.day.clamp(first, last);
        let slot = slot_on_local(chosen, pick.minutes, now);
        let until = slot.map(|s| at_local(chosen, s));
        let (year, month, _) = civil(pick.month_first);
        let month_name = FULL_MONTHS[usize::from(month.clamp(1, 12)) - 1].get();
        let caption = hm::MONTH_YEAR.format(&[("year", &year.to_string()), ("month", month_name)]);
        let today = today_local(now);
        let can_prev = pick.month_first > month_first(first);
        let can_next = next_month(pick.month_first) <= last;
        let nav = div()
            .mb_2()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .child(caption),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        Button::new("bot-snooze-prev-month")
                            .ghost()
                            .xsmall()
                            .disabled(!can_prev)
                            .icon(Icon::new(IconName::ChevronLeft).size(px(16.)))
                            .accessibility_label(hm::PREV_MONTH.get())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(pick) = this.items.snooze_pick.as_mut() {
                                    pick.month_first = prev_month(pick.month_first);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("bot-snooze-next-month")
                            .debug_selector(|| "bot-snooze-next-month".into())
                            .ghost()
                            .xsmall()
                            .disabled(!can_next)
                            .icon(Icon::new(IconName::ChevronRight).size(px(16.)))
                            .accessibility_label(hm::NEXT_MONTH.get())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(pick) = this.items.snooze_pick.as_mut() {
                                    pick.month_first = next_month(pick.month_first);
                                }
                                cx.notify();
                            })),
                    ),
            );
        let mut grid = div().w_full().flex().flex_col().gap_1().child(
            div()
                .flex()
                .w_full()
                .justify_between()
                .children(CAL_DAYS.map(|d| {
                    div()
                        .size(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(d.get())
                })),
        );
        // 1970-01-01 was a Thursday: the grid starts on the Sunday on or before the 1st.
        let lead = (pick.month_first + 4).rem_euclid(7);
        let end = next_month(pick.month_first);
        let mut start = pick.month_first - lead;
        while start < end {
            let mut week = div().flex().w_full().justify_between();
            for d in start..start + 7 {
                if d < pick.month_first || d >= end {
                    week = week.child(div().size(px(32.)));
                    continue;
                }
                let out = d < first || d > last;
                let selected = d == chosen;
                week = week.child(
                    div()
                        .id(("bot-snooze-day", usize::try_from(d).unwrap_or(0)))
                        .debug_selector(move || format!("bot-snooze-day-{}", civil(d).2))
                        .role(gpui::Role::Button)
                        .aria_selected(selected)
                        .tab_index(if selected { 0 } else { -1 })
                        .size(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(ui::RADIUS_SM)
                        .text_size(ui::TEXT_SM)
                        .when(d == today && !selected, |el| {
                            el.border_1().border_color(p.muted_foreground.opacity(0.5))
                        })
                        .when(selected, |el| {
                            el.bg(p.primary).text_color(p.primary_foreground)
                        })
                        .when(out, |el| el.text_color(p.muted_foreground).opacity(0.5))
                        .when(!out && !selected, |el| {
                            el.cursor_pointer()
                                .hover(|h| h.bg(p.accent).text_color(p.accent_foreground))
                        })
                        .child(civil(d).2.to_string())
                        .when(!out, |el| {
                            el.on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(pick) = this.items.snooze_pick.as_mut() {
                                    pick.day = d;
                                }
                                cx.notify();
                            }))
                        }),
                );
            }
            grid = grid.child(week);
            start += 7;
        }
        let slot_list = slots_local(chosen, now);
        let this = cx.weak_entity();
        let time = div()
            .h(px(32.))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(div().text_size(ui::TEXT_SM).child(hm::SNOOZE_TIME.get()))
            .child(
                Button::new("bot-snooze-time")
                    .debug_selector(|| "bot-snooze-time".into())
                    .outline()
                    .small()
                    .w(px(128.))
                    .label(slot.map(clock).unwrap_or_default())
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu.max_h(px(256.)).scrollable(true);
                        for s in slot_list.clone() {
                            let this = this.clone();
                            menu = menu.item(
                                PopupMenuItem::new(clock(s))
                                    .checked(Some(s) == slot)
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |v, cx| {
                                            if let Some(pick) = v.items.snooze_pick.as_mut() {
                                                pick.minutes = s;
                                            }
                                            cx.notify();
                                        })
                                        .ok();
                                    }),
                            );
                        }
                        menu
                    }),
            );
        let (bot_id, uids) = (pick.bot_id.clone(), pick.uids.clone());
        let footer = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .children(until.map(|u| snooze_time(u, now))),
            )
            .child(
                ui::secondary("bot-snooze-back", hm::BACK.get())
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(pick) = this.items.snooze_pick.as_mut() {
                            pick.picking = false;
                        }
                        cx.notify();
                    })),
            )
            .child(
                ui::primary("bot-snooze-submit", text::HM_SNOOZE.get())
                    .debug_selector(|| "bot-snooze-submit".into())
                    .small()
                    .disabled(until.is_none() || self.items.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.items.busy {
                            return;
                        }
                        if let Some(until) = until {
                            this.items.snooze_pick = None;
                            this.snooze_items(bot_id.clone(), &uids, Some(until), cx);
                        }
                    })),
            );
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().w_full().child(nav).child(grid))
            .child(time)
            .child(footer)
    }
}

fn item_params(
    uid: &str,
    status: Option<BotItemStatus>,
    snooze: FieldPatch<u64>,
) -> UpdateBotItemParams {
    UpdateBotItemParams {
        item_id: uid.to_owned(),
        title: None,
        description: None,
        done_means: None,
        status,
        comment: None,
        decisions: Vec::new(),
        close_note: None,
        snoozed_until_ms: snooze,
        waiting_reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_quarter_hours_still_ahead() {
        // 2026-11-23 10:07 UTC, offset 0.
        let now = 1_795_428_420_000;
        let day = today(now, 0);
        let open = slots(day, now, 0);
        assert_eq!(open.first(), Some(&(10 * 60 + 15)));
        assert_eq!(open.last(), Some(&(23 * 60 + 45)));
        assert_eq!(slot_on(day, 9 * 60, now, 0), Some(10 * 60 + 15));
        assert_eq!(slots(day + 1, now, 0).len(), 96);
        let (first, last) = day_range(now, 0);
        assert_eq!(first, day);
        assert_eq!(last, day + 366);
    }

    #[test]
    fn months_step_from_their_first_day() {
        // 2026-11-23.
        let day = 20_780;
        assert_eq!(civil(day), (2026, 11, 23));
        let first = month_first(day);
        assert_eq!(civil(first), (2026, 11, 1));
        assert_eq!(civil(next_month(first)), (2026, 12, 1));
        assert_eq!(civil(prev_month(first)), (2026, 10, 1));
    }

    #[test]
    fn calendar_captions_and_headers_localize_via_catalog() {
        use crate::i18n::Language;
        let nov = FULL_MONTHS[10];
        assert_eq!(
            hm::MONTH_YEAR.format_in(
                Language::En,
                &[("year", "2026"), ("month", nov.in_language(Language::En))]
            ),
            "November 2026"
        );
        assert_eq!(
            hm::MONTH_YEAR.format_in(
                Language::Ko,
                &[("year", "2026"), ("month", nov.in_language(Language::Ko))]
            ),
            "2026년 11월"
        );
        assert_eq!(
            CAL_DAYS.map(|d| d.in_language(Language::En)),
            ["S", "M", "T", "W", "T", "F", "S"]
        );
        assert_eq!(
            CAL_DAYS.map(|d| d.in_language(Language::Ko)),
            ["일", "월", "화", "수", "목", "금", "토"]
        );
    }

    #[test]
    fn dst_transition_presets_and_custom_slots_use_target_offset() {
        use crate::local_time::{CivilTime, epoch_from_civil};

        let utc_ms = |y, m, d, h, min| {
            u64::try_from(
                epoch_from_civil(CivilTime {
                    year: y,
                    month: m,
                    day: d,
                    hour: h,
                    minute: min,
                    second: 0,
                })
                .unwrap()
                    * 1000,
            )
            .unwrap()
        };

        // Synthetic America/New_York spring-forward: 2026-03-08 07:00 UTC (-5h -> -4h).
        let transition_secs = epoch_from_civil(CivilTime {
            year: 2026,
            month: 3,
            day: 8,
            hour: 7,
            minute: 0,
            second: 0,
        })
        .unwrap();
        let ny_offset = |epoch_secs: i64| {
            if epoch_secs < transition_secs {
                -5 * 3_600
            } else {
                -4 * 3_600
            }
        };

        // Sat 2026-03-07 10:00 EST (15:00 UTC).
        let now = utc_ms(2026, 3, 7, 15, 0);
        let presets = picker_presets_with(now, ny_offset);
        // Tomorrow 9:00 AM EDT = 2026-03-08 13:00 UTC (NOT 14:00 UTC).
        assert_eq!(presets[1].1, utc_ms(2026, 3, 8, 13, 0));
        // Monday 9:00 AM EDT = 2026-03-09 13:00 UTC.
        assert_eq!(presets[2].1, utc_ms(2026, 3, 9, 13, 0));
        // Next week (Sat 2026-03-14) 9:00 AM EDT = 13:00 UTC.
        assert_eq!(presets[3].1, utc_ms(2026, 3, 14, 13, 0));

        // Custom date 116 days out (2026-07-01 09:00 EDT = 13:00 UTC).
        let july_1_day = today_with(utc_ms(2026, 7, 1, 12, 0), ny_offset);
        assert_eq!(
            at_with(july_1_day, 9 * 60, ny_offset),
            utc_ms(2026, 7, 1, 13, 0)
        );
    }

    #[gpui::test]
    fn snooze_submit_keeps_dialog_open_when_busy(cx: &mut gpui::TestAppContext) {
        use gpui::Modifiers;
        use krill_proto::bots::{BotConfig, BotRpcRequest, BotRpcResponse};
        use std::sync::Arc;

        let bot_id = BotId::new("bot-snooze-busy").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "Snooze Bot", "agy", 1_700_000_000_000).unwrap();
        let bot_rpc = bot.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![bot_rpc.clone()])),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.simulate_resize(gpui::size(px(400.), px(700.)));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, super::super::DetailTab::Items, cx);
                v.open_snooze_pick(bot_id.clone(), vec!["u1".to_owned()], cx);
                v.items.busy = true;
                cx.notify();
            });
        });
        vcx.run_until_parked();

        let preset = vcx
            .debug_bounds("bot-snooze-preset-0")
            .expect("snooze preset 0");
        vcx.simulate_click(preset.center(), Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert!(
                view.read(cx).items.snooze_pick.is_some(),
                "preset click must not clear snooze_pick while items.busy is true"
            );
        });

        // Switch to custom grid and verify submit stays disabled/preserves modal while busy.
        let custom = vcx
            .debug_bounds("bot-snooze-custom")
            .expect("snooze custom");
        vcx.simulate_click(custom.center(), Modifiers::default());
        vcx.run_until_parked();
        let submit = vcx
            .debug_bounds("bot-snooze-submit")
            .expect("snooze submit");
        vcx.simulate_click(submit.center(), Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert!(
                view.read(cx).items.snooze_pick.is_some(),
                "custom submit must not clear snooze_pick while items.busy is true"
            );
        });
    }

    #[gpui::test]
    fn panel_overlay_dialogs_focus_on_open_so_escape_closes_them(cx: &mut gpui::TestAppContext) {
        use krill_proto::bots::{BotConfig, BotRpcRequest, BotRpcResponse};
        use std::sync::Arc;

        let bot_id = BotId::new("bot-overlay-focus").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "Overlay Bot", "agy", 1_700_000_000_000).unwrap();
        let bot_rpc = bot.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![bot_rpc.clone()])),
            _ => Ok(BotRpcResponse::Ok),
        });
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.simulate_resize(gpui::size(px(420.), px(720.)));
        vcx.run_until_parked();

        // Snooze dialog focuses view.focus so Escape closes it even if focus started unfocused.
        vcx.update(|window, cx| {
            window.blur(cx);
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, super::super::DetailTab::Items, cx);
                v.open_snooze_pick_in(bot_id.clone(), vec!["u1".to_owned()], window, cx);
            });
        });
        vcx.run_until_parked();
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert!(
                view.read(cx).items.snooze_pick.is_none(),
                "Escape must close snooze dialog opened via open_snooze_pick_in"
            );
        });

        // Add connector dialog focuses add_search so Escape closes it.
        vcx.update(|window, cx| {
            window.blur(cx);
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, super::super::DetailTab::Connectors, cx);
                v.open_connector_add_dialog(window, cx);
            });
        });
        vcx.run_until_parked();
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert!(
                view.read(cx).connectors.adding.is_none(),
                "Escape must close connector add dialog opened via open_connector_add_dialog"
            );
        });
    }
}
