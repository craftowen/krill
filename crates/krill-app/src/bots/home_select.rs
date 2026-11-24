// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Home's row selection and its batch moves: a row's checkbox (shift-click checks a range), the
//! pill at the list's foot while rows are checked (`BotItemSelectionPill`), the Cancel dialog
//! that asks why (`BotItemCancelDialog`), and Stale's "Cancel all" confirmation.

use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::bots::{BotId, BotItem, BotItemStatus, FieldPatch, UpdateBotItemParams};

use super::home::{is_snoozed, status_icon, status_name};
use super::{View, now_unix_ms};
use crate::i18n::catalog::{bot_home as hm, bots as text};
use crate::ui;

/// Orca's `BOT_ITEM_STATUSES`, in the Move to menu's order.
const STATUSES: [BotItemStatus; 5] = [
    BotItemStatus::Planned,
    BotItemStatus::Working,
    BotItemStatus::NeedsYou,
    BotItemStatus::Done,
    BotItemStatus::Canceled,
];

/// Checks (`on`) or unchecks a row; with `range`, every row from the last one clicked to it, in
/// the list's order.
pub(super) fn select_rows(
    selected: &mut std::collections::BTreeSet<String>,
    anchor: Option<&str>,
    order: &[String],
    uid: &str,
    on: bool,
    range: bool,
) {
    let from = anchor.and_then(|a| order.iter().position(|u| u == a));
    let to = order.iter().position(|u| u == uid);
    let uids: Vec<&String> = match (range, from, to) {
        (true, Some(from), Some(to)) => order[from.min(to)..=from.max(to)].iter().collect(),
        _ => order.iter().filter(|u| *u == uid).collect(),
    };
    for u in uids {
        if on {
            selected.insert(u.clone());
        } else {
            selected.remove(u);
        }
    }
    if !order.iter().any(|u| u == uid) {
        if on {
            selected.insert(uid.to_owned());
        } else {
            selected.remove(uid);
        }
    }
}

/// The Waiting status note dialog's state (Orca `BotItemStatusNoteDialog`, `#47308`/`#47309`).
/// Cancel happens immediately without a dialog (`#47934`); only Waiting asks what it waits on.
pub(super) struct WaitAsk {
    pub bot_id: BotId,
    pub uids: Vec<String>,
    pub titles: Vec<String>,
    pub initial_note: Option<String>,
    pub input: Option<gpui::Entity<InputState>>,
}

impl View {
    /// A row's checkbox, in place of its status glyph while hovered or while any row is checked.
    pub(super) fn render_row_checkbox(
        uid: &str,
        title: &str,
        checked: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let uid = uid.to_owned();
        let sel = format!("bot-home-item-check-{uid}");
        let debug = sel.clone();
        div()
            .id(SharedString::from(sel))
            .debug_selector(move || debug.clone())
            .role(gpui::Role::CheckBox)
            .aria_label(SharedString::from(
                hm::SELECT_ROW.format(&[("title", title)]),
            ))
            .aria_toggled(if checked {
                gpui::Toggled::True
            } else {
                gpui::Toggled::False
            })
            .tab_index(0)
            .size(px(14.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .border_1()
            .border_color(if checked { p.primary } else { p.input })
            .when(checked, |d| d.bg(p.primary))
            .cursor_pointer()
            .when(checked, |d| {
                d.child(
                    Icon::new(IconName::Check)
                        .size(px(10.))
                        .text_color(p.primary_foreground),
                )
            })
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                cx.stop_propagation();
                let order = this.items.shown_order.clone();
                select_rows(
                    &mut this.items.selected,
                    this.items.select_anchor.as_deref(),
                    &order,
                    &uid,
                    !checked,
                    ev.modifiers().shift,
                );
                this.items.select_anchor = Some(uid.clone());
                cx.notify();
            }))
    }

    /// Orca's selection pill: "N selected · Move to · ×", floating at the list's foot.
    pub(super) fn render_selection_pill(
        &self,
        bot_id: &BotId,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let items: Vec<BotItem> = self
            .items
            .items
            .iter()
            .filter(|i| self.items.selected.contains(&i.uid))
            .cloned()
            .collect();
        if items.is_empty() {
            return None;
        }
        let count = items.len();
        let this = cx.weak_entity();
        let bot = bot_id.clone();
        let now = now_unix_ms();
        let open: Vec<String> = items
            .iter()
            .filter(|i| i.status.is_open())
            .map(|i| i.uid.clone())
            .collect();
        let any_snoozed = items.iter().any(|i| is_snoozed(i, now));
        let all: Vec<(String, String)> = items
            .iter()
            .map(|i| (i.uid.clone(), i.title.clone()))
            .collect();
        let move_to = Button::new("bot-home-pill-move")
            .debug_selector(|| "bot-home-pill-move".into())
            .ghost()
            .xsmall()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(hm::MOVE_TO.get())
                    .child(Icon::new(IconName::ChevronUp).size(px(12.))),
            )
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.label(if count == 1 {
                    hm::MOVE_ONE.get().to_owned()
                } else {
                    hm::MOVE_MANY.format(&[("n", &count.to_string())])
                });
                let existing_wait = if items.len() == 1 {
                    items
                        .first()
                        .and_then(super::home::waiting_note)
                        .map(str::to_owned)
                } else {
                    None
                };
                for status in STATUSES {
                    if status == BotItemStatus::Planned {
                        let (this_w, bot_w, all_w, existing_w) = (
                            this.clone(),
                            bot.clone(),
                            all.clone(),
                            existing_wait.clone(),
                        );
                        let wait_glyph = PopupMenuItem::element(move |_, cx| {
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
                                        .child(super::home::waiting_icon(p)),
                                )
                                .child(hm::STATUS_WAITING.get())
                        });
                        menu = menu.item(wait_glyph.on_click(move |_, _, cx| {
                            this_w
                                .update(cx, |v, cx| {
                                    v.ask_waiting(
                                        bot_w.clone(),
                                        all_w.clone(),
                                        existing_w.clone(),
                                        cx,
                                    );
                                })
                                .ok();
                        }));
                    }
                    let (this, bot, all) = (this.clone(), bot.clone(), all.clone());
                    let name = if status == BotItemStatus::Canceled {
                        text::HM_CANCELED_ITEM.get()
                    } else {
                        status_name(status)
                    };
                    let glyph = PopupMenuItem::element(move |_, cx| {
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
                                    .child(status_icon(status, false, false, p, cx)),
                            )
                            .child(name)
                    });
                    menu = menu.item(glyph.on_click(move |_, _, cx| {
                        this.update(cx, |v, cx| {
                            let uids: Vec<String> = all.iter().map(|(u, _)| u.clone()).collect();
                            // Orca #47934: Cancel (and every non-Waiting move) happens at once.
                            v.set_items_status(bot.clone(), &uids, status, cx);
                        })
                        .ok();
                    }));
                }
                if !open.is_empty() {
                    menu = Self::snooze_menu_for(menu.separator(), &this, &bot, &open, any_snoozed);
                }
                menu
            });
        Some(
            div()
                .debug_selector(|| "bot-home-selection-pill".into())
                .absolute()
                .bottom(px(8.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_0p5()
                        .rounded_full()
                        .border_1()
                        .border_color(p.border)
                        .bg(p.popover)
                        .py_0p5()
                        .pl_3()
                        .pr_1()
                        .text_size(ui::TEXT_XS)
                        .shadow_lg()
                        .child(
                            div()
                                .whitespace_nowrap()
                                .font_weight(FontWeight::MEDIUM)
                                .child(hm::SELECTED_COUNT.format(&[("n", &count.to_string())])),
                        )
                        .child(move_to)
                        .child(
                            Button::new("bot-home-pill-clear")
                                .debug_selector(|| "bot-home-pill-clear".into())
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::X).size(px(14.)))
                                .accessibility_label(hm::CLEAR_SELECTION.get())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.items.selected.clear();
                                    cx.notify();
                                })),
                        ),
                ),
        )
    }

    /// The snooze presets, Pick a time… and (while any is snoozed) Unsnooze, for these items.
    pub(super) fn snooze_menu_for(
        mut menu: gpui_component::menu::PopupMenu,
        this: &gpui::WeakEntity<Self>,
        bot_id: &BotId,
        uids: &[String],
        any_snoozed: bool,
    ) -> gpui_component::menu::PopupMenu {
        let now = now_unix_ms();
        let offset = super::home::local_offset_secs(now);
        for (label, at) in super::home::snooze_presets(now, offset) {
            let (this, bot, uids) = (this.clone(), bot_id.clone(), uids.to_vec());
            menu = menu.item(
                PopupMenuItem::new(format!(
                    "{label}  {}",
                    super::home_time::snooze_time(at, now)
                ))
                .on_click(move |_, _, cx| {
                    this.update(cx, |v, cx| v.snooze_items(bot.clone(), &uids, Some(at), cx))
                        .ok();
                }),
            );
        }
        let (pick_this, pick_bot, pick_uids) = (this.clone(), bot_id.clone(), uids.to_vec());
        menu = menu.separator().item(
            PopupMenuItem::new(hm::SNOOZE_PICK.get())
                .icon(IconName::CalendarClock)
                .on_click(move |_, _, cx| {
                    pick_this
                        .update(cx, |v, cx| {
                            v.open_snooze_pick(pick_bot.clone(), pick_uids.clone(), cx);
                        })
                        .ok();
                }),
        );
        if any_snoozed {
            let (this, bot, uids) = (this.clone(), bot_id.clone(), uids.to_vec());
            menu = menu.item(
                PopupMenuItem::new(text::HM_UNSNOOZE.get())
                    .icon(IconName::AlarmClockOff)
                    .on_click(move |_, _, cx| {
                        this.update(cx, |v, cx| v.snooze_items(bot.clone(), &uids, None, cx))
                            .ok();
                    }),
            );
        }
        menu
    }

    /// Opens the Waiting note dialog for these items (`#47308`/`#47309`).
    pub(super) fn ask_waiting(
        &mut self,
        bot_id: BotId,
        items: Vec<(String, String)>,
        initial_note: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let (uids, titles) = items.into_iter().unzip();
        self.items.cancel_ask = Some(WaitAsk {
            bot_id,
            uids,
            titles,
            initial_note,
            input: None,
        });
        cx.notify();
    }

    fn submit_wait(&mut self, cx: &mut Context<Self>) {
        let Some(ask) = self.items.cancel_ask.as_ref() else {
            return;
        };
        let note = ask
            .input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        if note.is_empty() {
            return;
        }
        let Some(ask) = self.items.cancel_ask.take() else {
            return;
        };
        self.set_items_waiting(ask.bot_id, &ask.uids, &note, cx);
    }

    /// Marks these items as Waiting with what they wait on (`waiting:<note>`).
    pub(super) fn set_items_waiting(
        &mut self,
        bot_id: BotId,
        uids: &[String],
        note: &str,
        cx: &mut Context<Self>,
    ) {
        let encoded = super::home::encode_waiting_note(note);
        let params = uids
            .iter()
            .map(|uid| UpdateBotItemParams {
                item_id: uid.clone(),
                title: None,
                description: None,
                done_means: None,
                status: Some(BotItemStatus::Planned),
                comment: None,
                decisions: Vec::new(),
                close_note: Some(encoded.clone()),
                snoozed_until_ms: FieldPatch::Unchanged,
            })
            .collect();
        self.update_items_batch(bot_id, params, cx);
    }

    /// Cancels these items with `reason` as their close note (used by Stale's "Cancel all").
    pub(super) fn cancel_items(
        &mut self,
        bot_id: BotId,
        uids: &[String],
        reason: &str,
        cx: &mut Context<Self>,
    ) {
        let params = uids
            .iter()
            .map(|uid| UpdateBotItemParams {
                item_id: uid.clone(),
                title: None,
                description: None,
                done_means: None,
                status: Some(BotItemStatus::Canceled),
                comment: None,
                decisions: Vec::new(),
                close_note: Some(reason.to_owned()),
                snoozed_until_ms: FieldPatch::Unchanged,
            })
            .collect();
        self.update_items_batch(bot_id, params, cx);
    }

    /// Alias kept for `group_panel.rs` (Domain D) so the Waiting dialog renders there too.
    pub(super) fn render_cancel_ask(
        &mut self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.render_wait_ask(p, window, cx)
    }

    /// Orca's `BotItemStatusNoteDialog` over the window while setting Waiting (`#47308`/`#47309`).
    pub(super) fn render_wait_ask(
        &mut self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ask = self.items.cancel_ask.as_mut()?;
        let initial = ask.initial_note.clone();
        let input = ask
            .input
            .get_or_insert_with(|| {
                let input = cx.new(|cx| {
                    InputState::new(window, cx).placeholder(hm::WAIT_ON_PLACEHOLDER.get())
                });
                cx.observe(&input, |_, _, cx| cx.notify()).detach();
                input.update(cx, |s, cx| {
                    if let Some(text) = initial {
                        s.set_value(text, window, cx);
                    }
                    s.focus(window, cx);
                });
                input
            })
            .clone();
        let count = ask.uids.len();
        let title = if count == 1 {
            hm::WAIT_ON_TITLE_ONE
                .format(&[("title", ask.titles.first().map_or("", String::as_str))])
        } else {
            hm::WAIT_ON_TITLE_MANY.format(&[("n", &count.to_string())])
        };
        let ready = !input.read(cx).value().trim().is_empty();
        let close = cx.listener(|this, _, _, cx| {
            this.items.cancel_ask = None;
            cx.notify();
        });
        let dialog = div()
            .id("bot-wait-ask")
            .debug_selector(|| "bot-wait-ask".into())
            .role(gpui::Role::Dialog)
            .aria_label(SharedString::from(title.clone()))
            .w(px(384.))
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
            .on_action(
                cx.listener(|this, _: &gpui_component::input::Enter, _, cx| {
                    this.submit_wait(cx);
                }),
            )
            .child(
                div()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(Input::new(&input))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::ghost("bot-wait-cancel", hm::WAIT_ON_CANCEL.get())
                            .small()
                            .on_click(close),
                    )
                    .child(
                        ui::primary("bot-wait-confirm", hm::WAIT_ON_CONFIRM.get())
                            .small()
                            .disabled(!ready)
                            .on_click(cx.listener(|this, _, _, cx| this.submit_wait(cx))),
                    ),
            );
        let backdrop = ui::modal_focus_scope(
            div()
                .id("bot-wait-ask-backdrop")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                    if ev.keystroke.key == "escape"
                        && !ev.keystroke.modifiers.modified()
                        && !this.input_is_composing(window, cx)
                    {
                        this.items.cancel_ask = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.items.cancel_ask = None;
                        cx.notify();
                    }),
                )
                .child(dialog),
            &self.focus,
        );
        Some(ui::modal_backdrop(window.viewport_size(), backdrop).into_any_element())
    }

    /// Stale's "Cancel all" confirmation under its header.
    pub(super) fn render_stale_confirm(
        bot_id: &BotId,
        stale: &[&BotItem],
        stale_days: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = stale.len();
        let reason = hm::STALE_REASON.format(&[("n", &stale_days.to_string())]);
        let uids: Vec<String> = stale.iter().map(|i| i.uid.clone()).collect();
        let bot = bot_id.clone();
        let confirm_reason = reason.clone();
        div()
            .debug_selector(|| "bot-home-stale-confirm".into())
            .mx_1()
            .my_1()
            .flex()
            .flex_col()
            .gap_2()
            .rounded(ui::RADIUS_SM)
            .border_1()
            .border_color(p.border)
            .bg(p.popover)
            .p(px(10.))
            .text_size(ui::TEXT_XS)
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.foreground)
                    .child(if count == 1 {
                        hm::STALE_CONFIRM_ONE.get().to_owned()
                    } else {
                        hm::STALE_CONFIRM_MANY.format(&[("n", &count.to_string())])
                    }),
            )
            .child(
                div()
                    .text_color(p.muted_foreground)
                    .child(hm::STALE_CONFIRM_WHY.format(&[("reason", &reason)])),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        ui::ghost("bot-home-stale-keep", hm::KEEP.get())
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.items.stale_confirm = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        ui::primary("bot-home-stale-cancel-all", hm::CANCEL_ALL.get())
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.items.stale_confirm = None;
                                this.cancel_items(bot.clone(), &uids, &confirm_reason, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_click_checks_the_range_from_the_anchor() {
        let order: Vec<String> = ["a", "b", "c", "d"].map(str::to_owned).to_vec();
        let mut sel = std::collections::BTreeSet::new();
        select_rows(&mut sel, None, &order, "b", true, false);
        select_rows(&mut sel, Some("b"), &order, "d", true, true);
        assert_eq!(
            sel.iter().map(String::as_str).collect::<Vec<_>>(),
            ["b", "c", "d"]
        );
        select_rows(&mut sel, Some("d"), &order, "c", false, true);
        assert_eq!(sel.iter().map(String::as_str).collect::<Vec<_>>(), ["b"]);
    }
}
