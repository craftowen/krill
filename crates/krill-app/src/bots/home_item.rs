// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's open item in Home's place (`BotItemPane` + `BotItemView`): the back row, a header
//! (number and title, status menu, when it was made, snooze, links), then the description, close
//! note, Done means and the Activity (events and comment cards, the waiting question with its
//! quick replies), and the comment box at the bottom. A reply to the question is a comment.

use gpui::{
    AnyElement, Context, FontWeight, HighlightStyle, SharedString, StyledText, Window, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::bots::{
    BotConfig, BotItem, BotItemActor, BotItemDecision, BotItemEvent, BotItemEventKind,
    BotItemStatus,
};

use super::home::{clock_label, is_snoozed, status_icon, status_name};
use super::{OpenSession, View, avatar, now_unix_ms};
use crate::i18n::catalog::{bot_home as hm, bots as text};
use crate::ui;

const STATUSES: [BotItemStatus; 5] = [
    BotItemStatus::Planned,
    BotItemStatus::Working,
    BotItemStatus::NeedsYou,
    BotItemStatus::Done,
    BotItemStatus::Canceled,
];

/// The event whose decisions the item waits on: the latest one with decisions, while it needs you.
pub(super) fn waiting_question(item: &BotItem) -> Option<&BotItemEvent> {
    if item.status != BotItemStatus::NeedsYou {
        return None;
    }
    item.events.iter().rev().find(|e| !e.decisions.is_empty())
}

/// The status a `status -> x` event moved to (krilld's event text).
fn event_status(event: &BotItemEvent) -> Option<BotItemStatus> {
    let to = event.text.rsplit("-> ").next()?.trim();
    STATUSES.into_iter().find(|s| s.as_str() == to)
}

/// Orca's `BotItemClampedText` clamps by measuring; Krill guesses from the text instead.
/// ponytail: length heuristic, measure the laid-out height if long one-line text clamps wrong.
fn clamps(text: &str) -> bool {
    text.chars().count() > 180 || text.lines().count() > 3
}

impl View {
    /// A comment from the box: the waiting question's answer while it needs you, else a comment.
    pub(super) fn send_item_comment(
        &mut self,
        uid: String,
        body: &str,
        input: Option<gpui::Entity<gpui_component::input::InputState>>,
        cx: &mut Context<Self>,
    ) {
        // Attached files go in as their paths, which the bot reads on this computer.
        let files = self.items.attachments.remove(&uid).unwrap_or_default();
        let mut body = body.trim().to_owned();
        if !files.is_empty() {
            let list = files
                .iter()
                .map(|f| format!("- {}", f.display()))
                .collect::<Vec<_>>()
                .join("\n");
            format!("{body}\n\n{}\n{list}", hm::ATTACHED.get())
                .trim()
                .clone_into(&mut body);
        }
        if body.is_empty() {
            return;
        }
        let ask = self
            .items
            .items
            .iter()
            .find(|i| i.uid == uid)
            .and_then(|i| waiting_question(i).cloned());
        match ask {
            // Several questions: the box answers the line it is on, with the picks held so far.
            Some(ask) if ask.decisions.len() > 1 => {
                let key = (uid.clone(), ask.id);
                let line = self.items.ask_steps.get(&key).map_or(0, |s| s.current);
                self.send_held_answers(uid, &key, &ask.decisions, Some((line, body)), cx);
                if let Some(input) = input {
                    let mut slot = Some(input);
                    super::clear_input_on_window(&mut slot, self.window_handle, cx);
                }
            }
            Some(_) => self.answer_item_decision(uid, Some(body), None, input, cx),
            None => self.submit_item_comment(uid, &body, input, cx),
        }
    }

    pub(super) fn render_home_item(
        &mut self,
        bot: &BotConfig,
        item: &BotItem,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_per_item_inputs(&item.uid, window, cx);
        self.ensure_pr_reads(item, cx);
        let now = now_unix_ms();
        let back = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px(px(6.))
            .pb_1()
            .child(
                Button::new("bot-item-back")
                    .debug_selector(|| "bot-item-back".into())
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::ArrowLeft).size(px(14.)))
                    .label(text::BP_HOME.get())
                    .text_color(p.muted_foreground)
                    .on_click(cx.listener(|this, _, _, cx| this.close_home_item(cx))),
            );
        let mut content = div().flex().flex_col().gap_4().px(px(14.)).pt_2();
        if let Some(desc) = item.description.as_ref().filter(|d| !d.trim().is_empty()) {
            content = content.child(self.render_description(desc, p, cx));
        }
        if !item.status.is_open()
            && let Some(note) = item.close_note.as_ref().filter(|n| !n.trim().is_empty())
        {
            content = content.child(
                div()
                    .rounded(ui::RADIUS_SM)
                    .bg(p.muted)
                    .px(px(10.))
                    .py_2()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(format!(
                        "{} ",
                        if item.status == BotItemStatus::Canceled {
                            text::HM_CANCELED_NOTE.get()
                        } else {
                            text::HM_DONE_NOTE.get()
                        }
                    ))
                    .child(div().text_color(p.foreground).child(note.clone())),
            );
        }
        content = content.child(self.render_item_fields(bot, item, p, cx));
        content = content.child(self.render_activity(bot, item, now, p, cx));
        let scroller = div()
            .id("bot-item-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(self.render_item_header(bot, item, now, p, cx))
            .child(content);
        div()
            .id("bot-item-pane")
            .debug_selector(|| "bot-item-pane".into())
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .pt_2()
            .capture_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                this.item_key(ev, window, cx);
            }))
            .child(back)
            .child(scroller)
            .child(self.render_comment_box(bot, item, p, window, cx))
    }

    /// Orca's field grid: Chat (opens it), then Done means.
    fn render_item_fields(
        &self,
        bot: &BotConfig,
        item: &BotItem,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let label = |t: &'static str, h: f32| {
            div()
                .w(px(84.))
                .flex_none()
                .line_height(px(h))
                .text_color(p.muted_foreground)
                .child(t)
        };
        let chat = self.item_chat(bot, item);
        let (open_bot, open_sid) = (bot.id.clone(), chat.session);
        let mut grid = div()
            .debug_selector(|| "bot-item-fields".into())
            .flex()
            .flex_col()
            .gap_1()
            .text_size(ui::TEXT_XS)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(label(hm::CHAT.get(), 24.))
                    .child(
                        div()
                            .id("bot-item-chat")
                            .debug_selector(|| "bot-item-chat".into())
                            .h(px(24.))
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_color(p.foreground)
                            .cursor_pointer()
                            .hover(gpui::Styled::underline)
                            .child(
                                Icon::new(if chat.home {
                                    IconName::House
                                } else {
                                    IconName::MessageSquare
                                })
                                .size(px(12.))
                                .text_color(p.muted_foreground),
                            )
                            .child(div().min_w_0().truncate().child(chat.title))
                            .child(
                                Icon::new(IconName::ExternalLink)
                                    .size(px(12.))
                                    .text_color(p.muted_foreground),
                            )
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if let Some(session_id) = open_sid {
                                    cx.emit(OpenSession {
                                        bot_id: open_bot.clone(),
                                        session_id,
                                    });
                                }
                            })),
                    ),
            );
        if let Some(done) = item.done_means.as_ref().filter(|d| !d.trim().is_empty()) {
            grid = grid.child(
                div()
                    .debug_selector(|| "bot-item-done-means".into())
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(label(text::HM_DONE_MEANS.get(), 18.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .line_height(px(18.))
                            .text_color(p.foreground)
                            .child(done.clone()),
                    ),
            );
        }
        grid
    }

    /// Orca's `BotItemHeader`: `#N title`, the status menu and when it was made, the snooze line
    /// and the item's links.
    fn render_item_header(
        &self,
        bot: &BotConfig,
        item: &BotItem,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let number = format!("#{}", item.number);
        let title = format!("{number} {}", item.title);
        let title = StyledText::new(title).with_highlights([(
            0..number.len(),
            HighlightStyle {
                color: Some(p.muted_foreground),
                font_weight: Some(FontWeight::NORMAL),
                ..HighlightStyle::default()
            },
        )]);
        let this = cx.weak_entity();
        let (bot_id, uid, current) = (bot.id.clone(), item.uid.clone(), item.status);
        let snoozed = is_snoozed(item, now);
        let title_text = item.title.clone();
        let status_menu = Button::new("bot-item-status")
            .debug_selector(|| "bot-item-status".into())
            .ghost()
            .xsmall()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(status_icon(item.status, self.item_live(item), false, p, cx))
                    .child(status_name(item.status))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(12.))
                            .text_color(p.muted_foreground),
                    ),
            )
            .dropdown_menu(move |mut menu, _, _| {
                for status in STATUSES {
                    let (this, bot_id, uid) = (this.clone(), bot_id.clone(), uid.clone());
                    let name = if status == BotItemStatus::Canceled {
                        text::HM_CANCELED_ITEM.get()
                    } else {
                        status_name(status)
                    };
                    menu = menu.item(
                        PopupMenuItem::new(name)
                            .checked(status == current)
                            .disabled(status == current)
                            .on_click({
                                let title = title_text.clone();
                                move |_, _, cx| {
                                    this.update(cx, |v, cx| {
                                        if status == BotItemStatus::Canceled {
                                            v.ask_cancel(
                                                bot_id.clone(),
                                                vec![(uid.clone(), title.clone())],
                                                cx,
                                            );
                                        } else {
                                            v.update_item_status(
                                                bot_id.clone(),
                                                uid.clone(),
                                                status,
                                                cx,
                                            );
                                        }
                                    })
                                    .ok();
                                }
                            }),
                    );
                }
                if current.is_open() {
                    menu = Self::snooze_menu_for(
                        menu.separator(),
                        &this,
                        &bot_id,
                        std::slice::from_ref(&uid),
                        snoozed,
                    );
                }
                menu
            });
        let mut header = div()
            .debug_selector(|| "bot-item-header".into())
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(6.))
            .border_b_1()
            .border_color(p.border.opacity(0.6))
            .bg(p.sidebar)
            .px(px(14.))
            .pt_1()
            .pb_2()
            .child(
                div()
                    .line_clamp(2)
                    .text_size(ui::TEXT_LG)
                    .line_height(px(21.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(title),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_none().ml(px(-6.)).child(status_menu))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(format!(
                                "{} · {}",
                                hm::SUB_WHERE.format(&[
                                    ("n", &item.number.to_string()),
                                    ("chat", &self.item_chat(bot, item).title),
                                ]),
                                clock_label(item.created_at_ms, now)
                            )),
                    ),
            );
        if let Some(until) = item.snoozed_until_ms.filter(|_| is_snoozed(item, now)) {
            let (bot_id, uid) = (bot.id.clone(), item.uid.clone());
            header = header.child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(
                        Icon::new(IconName::AlarmClock)
                            .size(px(12.))
                            .text_color(p.muted_foreground),
                    )
                    .child(
                        div().flex_1().min_w_0().truncate().child(
                            text::HM_SNOOZED_UNTIL
                                .format(&[("when", &super::home_time::snooze_time(until, now))]),
                        ),
                    )
                    .child(
                        ui::secondary("bot-item-unsnooze", text::HM_UNSNOOZE.get())
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.snooze_item(bot_id.clone(), uid.clone(), None, cx);
                            })),
                    ),
            );
        }
        if !item.links.is_empty() {
            header = header.child(self.render_link_chips(bot, item, p, cx));
        }
        header
    }

    fn render_description(&self, desc: &str, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let open = self.items.desc_open;
        let clamped = clamps(desc);
        div()
            .debug_selector(|| "bot-item-description".into())
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(ui::TEXT_MD)
                    .line_height(px(19.))
                    .text_color(p.foreground)
                    .when(clamped && !open, |d| d.max_h(px(57.)).overflow_hidden())
                    .child(super::home_item_parts::markdown(
                        "bot-item-description-md",
                        desc,
                    )),
            )
            .when(clamped, |d| {
                d.child(
                    div()
                        .id("bot-item-description-toggle")
                        .self_start()
                        .cursor_pointer()
                        .text_size(ui::TEXT_SM)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.foreground)
                        .hover(gpui::Styled::underline)
                        .child(if open {
                            text::HM_SHOW_LESS.get()
                        } else {
                            text::HM_SHOW_MORE_TEXT.get()
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.items.desc_open = !this.items.desc_open;
                            cx.notify();
                        })),
                )
            })
    }

    /// Orca's Activity: a rail of events, comments as cards, the waiting question with its
    /// quick replies.
    fn render_activity(
        &self,
        bot: &BotConfig,
        item: &BotItem,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let waiting = waiting_question(item).map(|e| e.id);
        let mut list = div()
            .debug_selector(|| "bot-item-activity".into())
            .relative()
            .flex()
            .flex_col()
            .child(
                div()
                    .absolute()
                    .top(px(10.))
                    .bottom(px(10.))
                    .left(px(9.5))
                    .w(px(1.))
                    .bg(p.foreground.opacity(0.12)),
            );
        let comments = item
            .events
            .iter()
            .filter(|e| e.kind == BotItemEventKind::Commented || !e.decisions.is_empty())
            .count();
        let all = self.items.events_all.contains(&item.uid);
        let skip = if all {
            0
        } else {
            item.events
                .len()
                .saturating_sub(super::home_item_parts::ACTIVITY_PAGE)
        };
        for event in item.events.iter().skip(skip) {
            let card = event.kind == BotItemEventKind::Commented || !event.decisions.is_empty();
            if card {
                list = list.child(self.render_comment_card(
                    bot,
                    item,
                    event,
                    waiting == Some(event.id),
                    now,
                    p,
                    cx,
                ));
            } else {
                list = list.child(self.render_event_row(bot, item, event, now, p, cx));
            }
        }
        let mut section = div()
            .pb_2()
            .child(
                div()
                    .mb(px(6.))
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::HM_ACTIVITY.get()),
            )
            .when(skip > 0, |d| {
                let uid = item.uid.clone();
                d.child(
                    ui::ghost("bot-item-show-earlier", hm::SHOW_EARLIER.get())
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.items.events_all.insert(uid.clone());
                            cx.notify();
                        })),
                )
            })
            .child(list);
        if comments == 0 {
            section = section.child(
                div()
                    .pt_0p5()
                    .text_size(ui::TEXT_SM)
                    .line_height(px(18.))
                    .text_color(p.muted_foreground)
                    .child(text::HM_NO_COMMENTS.get()),
            );
        }
        section
    }

    fn actor_name(bot: &BotConfig, actor: &BotItemActor) -> String {
        match actor {
            BotItemActor::Owner { .. } => text::HM_YOU.get().to_owned(),
            BotItemActor::Bot { .. } => bot.name.clone(),
            BotItemActor::System { .. } => "Krill".to_owned(),
        }
    }

    /// One event on the rail: its glyph, who did what, and when.
    fn render_event_row(
        &self,
        bot: &BotConfig,
        _item: &BotItem,
        event: &BotItemEvent,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let bold_name = || {
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.foreground)
                .child(Self::actor_name(bot, &event.actor))
                .into_any_element()
        };
        let time = || {
            div()
                .flex_none()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(clock_label(event.created_at_ms, now))
        };
        let rail_icon = |icon: AnyElement| {
            div()
                .h(px(18.))
                .w(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .bg(p.sidebar)
                .child(icon)
        };
        let line = || {
            div()
                .relative()
                .min_w_0()
                .flex()
                .items_start()
                .gap_2()
                .py(px(3.))
                .text_size(ui::TEXT_SM)
                .line_height(px(18.))
                .text_color(p.muted_foreground)
        };
        let small = |name: IconName| {
            Icon::new(name)
                .size(px(12.))
                .text_color(p.muted_foreground)
                .into_any_element()
        };
        // An edit names what it changed, its before and after a click away.
        if event.kind == BotItemEventKind::Edited
            && let Some(changes) = self.render_field_changes(event, bold_name(), p, cx)
        {
            return line()
                .child(rail_icon(small(IconName::Pencil)))
                .child(changes)
                .child(time());
        }
        // A linked worker: started here, and finished once its session stops working.
        if event.kind == BotItemEventKind::Linked
            && let Some((title, finished)) = self.worker_lines(event)
        {
            let bold = |s: String| {
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.foreground)
                    .child(s)
            };
            let mut col = div().flex_1().min_w_0().flex().flex_col().child(
                line()
                    .child(rail_icon(small(IconName::Play)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_wrap()
                            .gap_x_1()
                            .child(hm::WORKER_STARTED.get())
                            .child(bold(title)),
                    )
                    .child(time()),
            );
            if let Some((stopped, title)) = finished {
                col = col.child(
                    line()
                        .child(rail_icon(if stopped {
                            small(IconName::CircleX)
                        } else {
                            small(IconName::CircleCheck)
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_wrap()
                                .gap_x_1()
                                .child(if stopped {
                                    hm::WORKER_STOPPED.get()
                                } else {
                                    hm::WORKER_FINISHED.get()
                                })
                                .child(bold(title)),
                        ),
                );
            }
            return col;
        }
        let glyph = |name: IconName| {
            Icon::new(name)
                .size(px(12.))
                .text_color(p.muted_foreground)
                .into_any_element()
        };
        let status = event_status(event);
        let (icon, verb, object): (Option<AnyElement>, String, Option<String>) = match event.kind {
            BotItemEventKind::Made => (
                Some(glyph(IconName::Plus)),
                text::HM_ACT_MADE.get().to_owned(),
                None,
            ),
            BotItemEventKind::Edited => (
                Some(glyph(IconName::Pencil)),
                text::HM_ACT_EDITED.get().to_owned(),
                None,
            ),
            BotItemEventKind::Linked => (
                Some(glyph(IconName::Link)),
                text::HM_ACT_LINKED.get().to_owned(),
                Some(
                    event
                        .text
                        .split_once(':')
                        .map_or(event.text.as_str(), |(_, t)| t)
                        .to_owned(),
                ),
            ),
            BotItemEventKind::Snoozed => {
                let until = event
                    .text
                    .rsplit(' ')
                    .next()
                    .and_then(|ms| ms.parse::<u64>().ok());
                (
                    Some(glyph(IconName::AlarmClock)),
                    until.map_or_else(
                        || text::HM_ACT_SNOOZED_BARE.get().to_owned(),
                        |ms| text::HM_ACT_SNOOZED.format(&[("when", &clock_label(ms, now))]),
                    ),
                    None,
                )
            }
            BotItemEventKind::Unsnoozed => (
                Some(glyph(IconName::AlarmClockOff)),
                text::HM_ACT_UNSNOOZED.get().to_owned(),
                None,
            ),
            BotItemEventKind::Status => (
                status.map(|s| status_icon(s, false, false, p, cx)),
                match status {
                    Some(BotItemStatus::Done) => text::HM_ACT_CLOSED.get().to_owned(),
                    Some(BotItemStatus::Canceled) => text::HM_ACT_CANCELED.get().to_owned(),
                    _ => text::HM_ACT_MOVED.get().to_owned(),
                },
                match status {
                    Some(BotItemStatus::Done | BotItemStatus::Canceled) | None => None,
                    Some(s) => Some(status_name(s).to_owned()),
                },
            ),
            BotItemEventKind::Commented => (None, String::new(), None),
        };
        let bold = |s: String| {
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.foreground)
                .child(s)
        };
        div()
            .relative()
            .min_w_0()
            .flex()
            .items_start()
            .gap_2()
            .py(px(3.))
            .text_size(ui::TEXT_SM)
            .line_height(px(18.))
            .text_color(p.muted_foreground)
            .child(
                div()
                    .h(px(18.))
                    .w(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(p.sidebar)
                    .children(icon),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .gap_x_1()
                    .child(bold(Self::actor_name(bot, &event.actor)))
                    .child(verb)
                    .children(object.map(bold)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(clock_label(event.created_at_ms, now)),
            )
    }

    /// Orca's `BotItemCommentCard`: face, name, time, the words, and its questions. The waiting
    /// question carries Needs you and its quick replies.
    #[expect(
        clippy::too_many_arguments,
        reason = "a render helper over the item's shared state"
    )]
    fn render_comment_card(
        &self,
        bot: &BotConfig,
        item: &BotItem,
        event: &BotItemEvent,
        waiting: bool,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let name = Self::actor_name(bot, &event.actor);
        let face = match event.actor {
            BotItemActor::Bot { .. } => avatar::Avatar::of(bot),
            _ => avatar::Avatar::from_fields(None, None, None, &name),
        };
        let upload = self.settings.avatar_cache.get(&bot.id).cloned();
        let words = (event.kind == BotItemEventKind::Commented)
            .then(|| event.text.trim().to_owned())
            .filter(|t| !t.is_empty());
        let mut body = div().pl_7().flex().flex_col().children(words.map(|w| {
            div()
                .text_size(ui::TEXT_MD)
                .line_height(px(19.))
                .text_color(p.foreground)
                .child(super::home_item_parts::markdown(
                    format!("bot-item-comment-md-{}", event.id),
                    &w,
                ))
        }));
        let several = event.decisions.len() > 1;
        let mut questions: gpui::Div = div()
            .flex()
            .flex_col()
            .when(several, gpui::Styled::gap_1p5)
            .when(!several, gpui::Styled::gap_3);
        for (d, decision) in event.decisions.iter().enumerate() {
            questions =
                questions.child(self.render_question(item, event, d, decision, waiting, p, cx));
        }
        if waiting && several {
            // An ask of several questions waits one line at a time.
            questions = self.render_question_steps(item, event, p, cx);
        }
        if !event.decisions.is_empty() {
            body = body.child(
                div()
                    .when(body_has_words(event), |d| {
                        d.mt_3().pt_3().border_t_1().border_color(p.border)
                    })
                    .child(questions),
            );
        }
        let needs_you_tag = waiting.then(|| {
            div()
                .ml_auto()
                .flex_none()
                .flex()
                .items_center()
                .gap_1p5()
                .rounded_full()
                .bg(p.agent_question.opacity(0.12))
                .px_2()
                .text_size(ui::TEXT_XS)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.agent_question)
                .child(div().size(px(6.)).rounded_full().bg(p.agent_question))
                .child(text::HM_STATUS_NEEDS_YOU.get())
        });
        div()
            .relative()
            .mx(px(-8.))
            .my_1()
            .rounded(ui::RADIUS_LG)
            .bg(p.sidebar)
            .child(
                div()
                    .debug_selector(move || format!("bot-item-comment-{}", event.id))
                    .rounded(ui::RADIUS_LG)
                    .bg(p.muted)
                    .border_1()
                    .border_color(p.border)
                    .px_2()
                    .pt(px(6.))
                    .when(event.decisions.is_empty(), gpui::Styled::pb_2)
                    .when(!event.decisions.is_empty(), gpui::Styled::pb_4)
                    .child(
                        div()
                            .mb_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(avatar::render(
                                &face,
                                upload,
                                &name,
                                px(20.),
                                ui::RADIUS_XS,
                                p,
                            ))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui::TEXT_MD)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.foreground)
                                    .child(name),
                            )
                            .children(needs_you_tag)
                            .child(
                                div()
                                    .when(!waiting, gpui::Styled::ml_auto)
                                    .flex_none()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(clock_label(event.created_at_ms, now)),
                            ),
                    )
                    .child(body),
            )
    }

    /// A question line: its words, and while it waits its quick replies and "Type your answer".
    #[expect(
        clippy::too_many_arguments,
        reason = "a render helper over the item's shared state"
    )]
    fn render_question(
        &self,
        item: &BotItem,
        event: &BotItemEvent,
        d: usize,
        decision: &BotItemDecision,
        waiting: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let words = div()
            .when(waiting, |w| {
                w.text_size(ui::TEXT_LG)
                    .line_height(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
            })
            .when(!waiting, |w| {
                w.text_size(ui::TEXT_MD)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.muted_foreground)
            })
            .child(decision.question.clone());
        let mut line = div().child(words);
        if !waiting || decision.options.is_empty() {
            return line;
        }
        let chip = |id: SharedString| {
            div()
                .id(id.clone())
                .debug_selector(move || id.to_string())
                .min_h(px(32.))
                .max_w_full()
                .rounded(px(16.))
                .border_1()
                .px(px(14.))
                .py(px(6.))
                .text_size(ui::TEXT_MD)
                .line_height(px(18.))
                .cursor_pointer()
        };
        let mut replies = div().mt(px(10.)).flex().flex_wrap().gap_2();
        for (o, option) in decision.options.iter().enumerate() {
            let recommended = decision.recommended == Some(o);
            let uid = item.uid.clone();
            let id: SharedString = format!("bot-item-reply-{}-{d}-{o}", event.id).into();
            replies = replies.child(
                chip(id)
                    .when(recommended, |c| {
                        c.border_color(p.foreground)
                            .bg(p.foreground)
                            .text_color(p.background)
                            .hover(|c| c.bg(p.foreground.opacity(0.9)))
                    })
                    .when(!recommended, |c| {
                        c.border_color(p.border)
                            .text_color(p.foreground)
                            .hover(|c| c.bg(p.foreground.opacity(0.05)))
                    })
                    .child(option.clone())
                    .when(recommended, |c| {
                        c.child(
                            div()
                                .ml(px(5.))
                                .text_size(ui::TEXT_XS)
                                .opacity(0.7)
                                .child(text::HM_RECOMMENDED.get()),
                        )
                        .flex()
                        .items_center()
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.answer_item_decision(
                            uid.clone(),
                            None,
                            u32::try_from(o).ok(),
                            None,
                            cx,
                        );
                    })),
            );
        }
        let input = self.items.comment_inputs.get(&item.uid).cloned();
        replies = replies.child(
            chip(format!("bot-item-type-answer-{}-{d}", event.id).into())
                .flex()
                .items_center()
                .gap_1p5()
                .border_color(p.border)
                .text_color(p.muted_foreground)
                .hover(|c| c.bg(p.foreground.opacity(0.05)))
                .child(Icon::new(IconName::Pencil).size(px(12.)))
                .child(text::HM_TYPE_ANSWER.get())
                .on_click(move |_, window, cx| {
                    if let Some(input) = &input {
                        input.update(cx, |i, cx| i.focus(window, cx));
                    }
                }),
        );
        line = line.child(replies);
        line
    }

    /// Orca's `BotItemCommentBox`: "Replying to" while a question waits, attached files, the @
    /// mention list, then attach, the text, the item's ↑ ↓ and send in one row.
    fn render_comment_box(
        &mut self,
        bot: &BotConfig,
        item: &BotItem,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let input = self.items.comment_inputs.get(&item.uid).cloned();
        let uid = item.uid.clone();
        let waiting = waiting_question(item).cloned();
        let quote = waiting
            .as_ref()
            .filter(|w| self.items.reply_hidden.get(&uid) != Some(&w.id))
            .map(|w| {
                let key = (uid.clone(), w.id);
                let line = self
                    .items
                    .ask_steps
                    .get(&key)
                    .map_or(0, |s| s.current)
                    .min(w.decisions.len().saturating_sub(1));
                w.decisions.get(line).map_or_else(
                    || super::home_item_parts::question_line(&w.text).to_owned(),
                    |d| super::home_item_parts::question_line(&d.question).to_owned(),
                )
            })
            .filter(|q| !q.is_empty());
        let placeholder = match &waiting {
            Some(w) if w.decisions.iter().any(|d| !d.options.is_empty()) => {
                hm::REPLY_OR_PICK.get().to_owned()
            }
            Some(_) => hm::REPLY_TO.format(&[("name", &bot.name)]),
            None => hm::ADD_COMMENT.get().to_owned(),
        };
        let value = input
            .as_ref()
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_default();
        if let Some(i) = &input
            && i.read(cx).presentation().placeholder().as_ref() != placeholder
        {
            i.update(cx, |s, cx| {
                s.set_placeholder(placeholder.clone(), window, cx);
            });
        }
        let mentions = super::home_item_parts::trailing_mention(&value)
            .map(str::to_owned)
            .map(|q| self.mention_matches(bot, &q))
            .unwrap_or_default();
        let files = self
            .items
            .attachments
            .get(&uid)
            .cloned()
            .unwrap_or_default();
        let empty = value.trim().is_empty() && files.is_empty();
        let replying = quote.is_some();
        let mut boxed = div()
            .debug_selector(|| "bot-item-comment-box".into())
            .flex_none()
            .mx_3()
            .mb_3()
            .mt_2()
            .flex()
            .flex_col()
            .rounded(ui::RADIUS_XL)
            .border_1()
            .border_color(p.border)
            .bg(p.background)
            .p(px(6.));
        if !mentions.is_empty() {
            let mut list = div()
                .debug_selector(|| "bot-item-mentions".into())
                .mb_1p5()
                .flex()
                .flex_col()
                .rounded(ui::RADIUS_SM)
                .border_1()
                .border_color(p.border)
                .bg(p.popover)
                .p_1();
            for (i, file) in mentions.into_iter().enumerate() {
                let (input, file_name) = (input.clone(), file.clone());
                list = list.child(
                    div()
                        .id(("bot-item-mention", i))
                        .debug_selector(move || format!("bot-item-mention-{i}"))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .rounded(ui::RADIUS_XS)
                        .cursor_pointer()
                        .text_size(ui::TEXT_SM)
                        .hover(|d| d.bg(p.accent))
                        .child(
                            Icon::new(IconName::File)
                                .size(px(14.))
                                .text_color(p.muted_foreground),
                        )
                        .child(div().min_w_0().truncate().child(file))
                        .on_click(move |_, window, cx| {
                            if let Some(input) = &input {
                                input.update(cx, |s, cx| {
                                    let text = s.value().to_string();
                                    let at = text.rfind('@').unwrap_or(text.len());
                                    s.set_value(
                                        format!(
                                            "{}@{file_name} ",
                                            text.get(..at).unwrap_or_default()
                                        ),
                                        window,
                                        cx,
                                    );
                                    s.focus(window, cx);
                                });
                            }
                        }),
                );
            }
            boxed = boxed.child(list);
        }
        if let (Some(q), Some(w)) = (quote, waiting.as_ref()) {
            let (hide_uid, hide_id) = (uid.clone(), w.id);
            let focus = input.clone();
            boxed = boxed.child(
                div()
                    .debug_selector(|| "bot-item-replying-to".into())
                    .mx_1()
                    .my_0p5()
                    .h(px(24.))
                    .max_w_full()
                    .min_w_0()
                    .self_start()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(ui::RADIUS_SM)
                    .bg(p.foreground.opacity(0.06))
                    .pr(px(3.))
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(
                        div()
                            .id("bot-item-replying-show")
                            .h_full()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .pl(px(10.))
                            .pr_1()
                            .rounded(ui::RADIUS_SM)
                            .cursor_pointer()
                            .hover(|d| d.bg(p.foreground.opacity(0.06)))
                            .child(
                                div()
                                    .size(px(6.))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(p.agent_question),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(format!("{} ", hm::REPLYING_TO.get()))
                                    .child(div().text_color(p.foreground).child(q))
                                    .flex(),
                            )
                            .tooltip(|w, cx| {
                                ui::tooltip_view(
                                    "bot-item-show-question",
                                    hm::SHOW_QUESTION.get(),
                                    w,
                                    cx,
                                )
                            })
                            .on_click(move |_, window, cx| {
                                if let Some(i) = &focus {
                                    i.update(cx, |s, cx| s.focus(window, cx));
                                }
                            }),
                    )
                    .child(
                        Button::new("bot-item-replying-hide")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::X).size(px(12.)))
                            .accessibility_label(hm::HIDE.get())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.items.reply_hidden.insert(hide_uid.clone(), hide_id);
                                cx.notify();
                            })),
                    ),
            );
        }
        if !files.is_empty() {
            let mut row = div().mb_1p5().px_1().pt_2().flex().flex_wrap().gap_2();
            for (i, path) in files.iter().enumerate() {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let remove_uid = uid.clone();
                row = row.child(
                    div()
                        .debug_selector(move || format!("bot-item-attachment-{i}"))
                        .h(px(24.))
                        .max_w(px(200.))
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pl_2()
                        .pr(px(2.))
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(p.border)
                        .text_size(ui::TEXT_XS)
                        .child(
                            Icon::new(IconName::File)
                                .size(px(12.))
                                .text_color(p.muted_foreground),
                        )
                        .child(div().min_w_0().truncate().child(name.clone()))
                        .child(
                            Button::new(("bot-item-attachment-remove", i))
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::X).size(px(10.)))
                                .accessibility_label(SharedString::from(
                                    hm::REMOVE_ATTACHMENT.format(&[("name", &name)]),
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(list) = this.items.attachments.get_mut(&remove_uid)
                                        && i < list.len()
                                    {
                                        list.remove(i);
                                    }
                                    cx.notify();
                                })),
                        ),
                );
            }
            boxed = boxed.child(row);
        }
        let attach_uid = uid.clone();
        let field = input.map(|i| {
            div()
                .min_w_0()
                .flex_1()
                // Replying to a question, the text has its own row, four lines tall.
                .when(replying, |d| {
                    d.w_full().flex_basis(gpui::relative(1.)).min_h(px(80.))
                })
                .child(Input::new(&i).appearance(false))
        });
        let send_input = self.items.comment_inputs.get(&uid).cloned();
        let mut tools = div().flex().flex_wrap().items_end().gap_0p5();
        if replying {
            tools = tools.children(field).child(div().w_full());
        }
        if self.host.is_local() {
            tools = tools.child(
                Button::new("bot-item-attach")
                    .debug_selector(|| "bot-item-attach".into())
                    .ghost()
                    .small()
                    .icon(Icon::new(IconName::Paperclip).size(px(16.)))
                    .tooltip(hm::ATTACH.get())
                    .accessibility_label(hm::ATTACH.get())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        Self::pick_attachments(attach_uid.clone(), cx);
                    })),
            );
        }
        if !replying {
            // `field` moved into the row above only while replying.
            tools = tools.children(self.items.comment_inputs.get(&uid).cloned().map(|i| {
                div()
                    .min_w_0()
                    .flex_1()
                    .child(Input::new(&i).appearance(false))
            }));
        }
        tools = tools.child(self.render_stepper(p, cx)).child(
            Button::new("bot-item-comment-send")
                .debug_selector(|| "bot-item-comment-send".into())
                .primary()
                .small()
                .rounded(ui::RADIUS_LG)
                .disabled(empty)
                .icon(Icon::new(IconName::ArrowUp).size(px(16.)))
                .tooltip(text::HM_COMMENT.get())
                .accessibility_label(text::HM_COMMENT.get())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let Some(input) = send_input.clone() else {
                        return;
                    };
                    let body = input.read(cx).value().to_string();
                    this.send_item_comment(uid.clone(), &body, Some(input), cx);
                })),
        );
        boxed.child(tools)
    }

    /// Picks files to attach to the comment (paths the bot reads on this computer).
    fn pick_attachments(uid: String, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(hm::ATTACH.get().into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await {
                this.update(cx, |this, cx| {
                    this.items.attachments.entry(uid).or_default().extend(paths);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}

fn body_has_words(event: &BotItemEvent) -> bool {
    event.kind == BotItemEventKind::Commented && !event.text.trim().is_empty()
}
