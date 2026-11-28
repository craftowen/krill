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
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::bots::{
    BotConfig, BotItem, BotItemActor, BotItemDecision, BotItemEvent, BotItemEventKind,
    BotItemStatus,
};

use super::home::{
    is_snoozed, item_status_icon, status_icon, status_name, waiting_icon, waiting_note,
};
use super::home_time::activity_timestamp;
use super::{OpenSession, View, avatar, now_unix_ms};
use crate::i18n::catalog::{bot_home as hm, bots as text};
use crate::ui;

const STATUSES: [BotItemStatus; 6] = BotItemStatus::ALL;

/// Latest non-owner ask event on `item`, used to mark older unanswered asks as superseded
/// (Orca `#47869`).
fn latest_ask_id(item: &BotItem) -> Option<u64> {
    item.events
        .iter()
        .rev()
        .find(|e| !matches!(e.actor, BotItemActor::Owner { .. }) && !e.decisions.is_empty())
        .map(|e| e.id)
}

/// The waiting question excluding asks explicitly skipped via "send anyway" (Orca `#47904`).
pub(super) fn waiting_question_with_skipped<'a>(
    item: &'a BotItem,
    skipped: &std::collections::BTreeSet<(String, u64)>,
) -> Option<&'a BotItemEvent> {
    let ask = waiting_question(item)?;
    (!skipped.contains(&(item.uid.clone(), ask.id))).then_some(ask)
}

/// The event whose decisions the item waits on (Orca `botItemWaitingAsk`): the latest ask that
/// still has an open line while the item is open and either `NeedsYou` or already partially
/// answered (krilld moves the item to `Working` on the first reply, while later lines stay open).
pub(super) fn waiting_question(item: &BotItem) -> Option<&BotItemEvent> {
    if !item.status.is_open() {
        return None;
    }
    let mut current: Option<&BotItemEvent> = None;
    for (idx, event) in item.events.iter().enumerate() {
        if event.kind == BotItemEventKind::Status {
            let to = event_status(event);
            if matches!(to, Some(BotItemStatus::Done | BotItemStatus::Canceled)) {
                current = None;
            } else if !matches!(event.actor, BotItemActor::Owner { .. })
                && to == Some(BotItemStatus::NeedsYou)
            {
                if event.decisions.is_empty() {
                    let same_write_ask = current.is_some_and(|ask| {
                        item.events[..idx]
                            .iter()
                            .rev()
                            .find(|e| e.kind == BotItemEventKind::Commented)
                            .is_some_and(|c| {
                                c.id == ask.id && c.created_at_ms == event.created_at_ms
                            })
                    });
                    if !same_write_ask {
                        current = None;
                    }
                } else {
                    current = Some(event);
                }
            }
            continue;
        }
        if !matches!(event.actor, BotItemActor::Owner { .. }) && !event.decisions.is_empty() {
            current = Some(event);
        }
    }
    let ask = current?;
    let answered = super::home_item_parts::answered_lines(item, ask);
    let has_open = answered.iter().any(Option::is_none);
    let has_answered = answered.iter().any(Option::is_some);
    (has_open && (item.status == BotItemStatus::NeedsYou || has_answered)).then_some(ask)
}

/// The status a `status -> x` (or `status -> waiting: <reason>`) event moved to.
fn event_status(event: &BotItemEvent) -> Option<BotItemStatus> {
    let after = event.text.rsplit("-> ").next()?.trim();
    let token = after.split(':').next()?.trim();
    STATUSES.into_iter().find(|s| s.as_str() == token)
}

/// Optional waiting reason recorded inside a `status -> waiting: <reason>` event.
fn event_waiting_reason(event: &BotItemEvent) -> Option<&str> {
    let after = event.text.rsplit("-> ").next()?.trim();
    after
        .split_once(':')
        .map(|(_, reason)| reason.trim())
        .filter(|reason| !reason.is_empty())
}

/// The latest `status -> waiting` event while the item is still `Waiting` with a reason;
/// suppressed from the plain event rail because it renders as the "Waiting on" card instead.
pub(super) fn active_wait_event_id(item: &BotItem) -> Option<u64> {
    if item.status != BotItemStatus::Waiting || item.waiting_reason.is_none() {
        return None;
    }
    item.events
        .iter()
        .rev()
        .find(|e| {
            e.kind == BotItemEventKind::Status && event_status(e) == Some(BotItemStatus::Waiting)
        })
        .map(|e| e.id)
}

/// Orca's `BotItemClampedText` clamps by measuring; Krill guesses from the text instead.
/// ponytail: length heuristic, measure the laid-out height if long one-line text clamps wrong.
fn clamps(text: &str) -> bool {
    text.chars().count() > 180 || text.lines().count() > 3
}

/// Orca `BotItemCommentCard` clamps settled comments at 8 lines (`152px`), while showing the
/// agent's asking comment whole while it waits on an open question (`d6cc2684` / `#47076`).
fn clamps_comment(text: &str) -> bool {
    text.chars().count() > 480 || text.lines().count() > 8
}

impl View {
    /// A comment from the box: the waiting question's answer while it needs you, else a comment.
    pub(super) fn send_item_comment(
        &mut self,
        uid: String,
        body: &str,
        input: Option<gpui::Entity<TextareaState>>,
        cx: &mut Context<Self>,
    ) {
        if self.items.busy {
            return;
        }
        // Attached files go in as their paths, which the bot reads on this computer.
        // Keep them until RPC succeeds so busy/network failures do not drop attachments.
        let files = self
            .items
            .attachments
            .get(&uid)
            .cloned()
            .unwrap_or_default();
        let has_files = !files.is_empty();
        let mut body = body.trim().to_owned();
        if has_files {
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
        let item_snapshot = self.items.items.iter().find(|i| i.uid == uid).cloned();
        let ask = item_snapshot
            .as_ref()
            .and_then(|i| waiting_question_with_skipped(i, &self.items.skipped_asks).cloned());
        match (item_snapshot.as_ref(), ask) {
            // Several questions: words from the box post at once (Orca #46693), answering the
            // open line the box names with any held picks, or riding along as a plain comment
            // when "Replying to" was dismissed.
            (Some(item), Some(ask)) if ask.decisions.len() > 1 => {
                let key = (uid.clone(), ask.id);
                let state = super::home_item_parts::question_step_state(
                    item,
                    &ask,
                    self.items.ask_steps.get(&key),
                );
                let reply_hidden = self.items.reply_hidden.get(&uid) == Some(&ask.id);
                if reply_hidden {
                    if state.kept.iter().any(Option::is_some) {
                        self.send_held_answers_with_attachments(
                            uid,
                            &key,
                            &ask.decisions,
                            state.kept,
                            None,
                            Some(body),
                            input,
                            has_files,
                            cx,
                        );
                    } else {
                        self.submit_item_comment_with_attachments(uid, &body, input, has_files, cx);
                    }
                } else {
                    self.send_held_answers_with_attachments(
                        uid,
                        &key,
                        &ask.decisions,
                        state.kept,
                        Some((state.current, body)),
                        None,
                        input,
                        has_files,
                        cx,
                    );
                }
            }
            (_, Some(ask)) => {
                if self.items.reply_hidden.get(&uid) == Some(&ask.id) {
                    self.submit_item_comment_with_attachments(uid, &body, input, has_files, cx);
                } else {
                    let decision_id = (!ask.decisions.is_empty()).then(|| format!("{}:0", ask.id));
                    self.answer_item_decision_with_cleanup(
                        uid,
                        Some(body),
                        None,
                        decision_id,
                        input,
                        has_files,
                        None,
                        cx,
                    );
                }
            }
            (_, None) => {
                self.submit_item_comment_with_attachments(uid, &body, input, has_files, cx);
            }
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
        // Orca `#47076`: keep an asking comment expanded even after it is answered.
        if let Some(ask) = waiting_question_with_skipped(item, &self.items.skipped_asks)
            && self.items.asked_here.insert(ask.id)
        {
            self.items.open_comments.insert(ask.id);
        }
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
                    .label(if self.group_panel.active {
                        text::GRP_BACK_ITEMS.get()
                    } else {
                        text::BP_HOME.get()
                    })
                    .text_color(p.muted_foreground)
                    .on_click(cx.listener(|this, _, _, cx| this.close_home_item(cx))),
            );
        let mut content = div().flex().flex_col().gap_4().px(px(14.)).pt_3();
        if let Some(desc) = item.description.as_ref().filter(|d| !d.trim().is_empty()) {
            content = content.child(self.render_description(bot, desc, p, cx));
        }
        if !item.status.is_open()
            && let Some(note) = item
                .close_note
                .as_ref()
                .filter(|n| !n.trim().is_empty() && waiting_note(item).is_none())
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
            .child(content);
        let wait_modal = self.render_wait_ask(p, window, cx);
        let snooze_modal = self.render_snooze_pick(p, window, cx);
        div()
            .id("bot-item-pane")
            .debug_selector(|| "bot-item-pane".into())
            .relative()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .pt_2()
            .capture_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                this.item_key(ev, window, cx);
            }))
            .child(back)
            .child(self.render_item_header(bot, item, now, p, cx))
            .child(scroller)
            .child(self.render_comment_box(bot, item, p, window, cx))
            .children(wait_modal)
            .children(snooze_modal)
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
        let is_waiting = item.status == BotItemStatus::Waiting
            || (item.status != BotItemStatus::NeedsYou && waiting_note(item).is_some());
        let current_wait = waiting_note(item).unwrap_or_default().to_owned();
        let title_text = item.title.clone();
        let status_label = if is_waiting {
            hm::STATUS_WAITING.get()
        } else {
            status_name(item.status)
        };
        let status_menu = Button::new("bot-item-status")
            .debug_selector(|| "bot-item-status".into())
            .ghost()
            .xsmall()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(item_status_icon(item, self.item_live(item), false, p, cx))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.foreground)
                            .child(status_label),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(12.))
                            .text_color(p.muted_foreground),
                    ),
            )
            .dropdown_menu(move |mut menu, _, _| {
                for status in STATUSES {
                    let (this, bot_id, uid) = (this.clone(), bot_id.clone(), uid.clone());
                    if status == BotItemStatus::Waiting {
                        let title = title_text.clone();
                        let wait_init = current_wait.clone();
                        let disabled = current == BotItemStatus::NeedsYou;
                        menu = menu.item(
                            PopupMenuItem::new(text::HM_WAITING_ITEM.get())
                                .checked(is_waiting)
                                .disabled(disabled)
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |v, cx| {
                                        v.ask_waiting(
                                            bot_id.clone(),
                                            vec![(uid.clone(), title.clone())],
                                            (!wait_init.is_empty()).then(|| wait_init.clone()),
                                            cx,
                                        );
                                    })
                                    .ok();
                                }),
                        );
                    } else {
                        let active = status == current && !is_waiting;
                        let name = if status == BotItemStatus::Canceled {
                            text::HM_CANCELED_ITEM.get()
                        } else {
                            status_name(status)
                        };
                        // Orca `#47934`: Cancel applies immediately without a reason dialog.
                        menu = menu.item(
                            PopupMenuItem::new(name)
                                .checked(active)
                                .disabled(active)
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |v, cx| {
                                        v.update_item_status(
                                            bot_id.clone(),
                                            uid.clone(),
                                            status,
                                            cx,
                                        );
                                    })
                                    .ok();
                                }),
                        );
                    }
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
            .pb_2p5()
            .child(
                div()
                    .line_clamp(2)
                    .text_size(ui::TEXT_HEADING)
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
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(hm::SUB_WHERE.format(&[
                                ("n", &item.number.to_string()),
                                ("chat", &self.item_chat(bot, item).title),
                            ])),
                    )
                    .child(
                        div()
                            .flex_none()
                            .whitespace_nowrap()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(activity_timestamp(item.created_at_ms, now)),
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

    fn render_description(
        &self,
        bot: &BotConfig,
        desc: &str,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let open = self.items.desc_open;
        let clamped = clamps(desc);
        let is_local = self.host.is_local();
        let base_dir = self.bot_home_folder(bot);
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
                        is_local,
                        base_dir,
                    )),
            )
            .when(clamped, |d| {
                d.child(
                    div()
                        .id("bot-item-description-toggle")
                        .self_start()
                        .flex()
                        .items_center()
                        .gap_1()
                        .cursor_pointer()
                        .text_size(ui::TEXT_XS)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.muted_foreground)
                        .hover(|d| d.text_color(p.foreground))
                        .child(if open {
                            text::HM_SHOW_LESS.get()
                        } else {
                            text::HM_SHOW_MORE_TEXT.get()
                        })
                        .child(
                            Icon::new(if open {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            })
                            .size(px(12.)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.items.desc_open = !this.items.desc_open;
                            cx.notify();
                        })),
                )
            })
    }

    /// Orca's Activity: a rail of events, comments as cards, the waiting question with its
    /// quick replies, plus wait card (`#47308`/`#47309`) and failed comment retry (`#47485`).
    fn render_activity(
        &self,
        bot: &BotConfig,
        item: &BotItem,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let waiting = waiting_question_with_skipped(item, &self.items.skipped_asks).map(|e| e.id);
        let latest_ask = latest_ask_id(item);
        let hidden_wait_event = active_wait_event_id(item);
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
            if hidden_wait_event == Some(event.id) {
                continue;
            }
            let card = event.kind == BotItemEventKind::Commented || !event.decisions.is_empty();
            if card {
                let superseded = !event.decisions.is_empty()
                    && waiting != Some(event.id)
                    && latest_ask.is_some_and(|id| id != event.id);
                let skipped = self
                    .items
                    .skipped_asks
                    .contains(&(item.uid.clone(), event.id));
                list = list.child(self.render_comment_card(
                    bot,
                    item,
                    event,
                    waiting == Some(event.id),
                    superseded,
                    skipped,
                    now,
                    p,
                    cx,
                ));
            } else {
                list = list.child(self.render_event_row(bot, item, event, now, p, cx));
            }
        }
        // Orca `#47308`/`#47309`: BotItemWaitCard at the bottom of Activity when Waiting.
        if let Some(wait_text) = waiting_note(item) {
            let (bot_id, uid, title, wait_owned) = (
                bot.id.clone(),
                item.uid.clone(),
                item.title.clone(),
                wait_text.to_owned(),
            );
            let since = item
                .waiting_since_ms
                .or_else(|| {
                    hidden_wait_event
                        .and_then(|id| item.events.iter().find(|e| e.id == id))
                        .map(|e| e.created_at_ms)
                })
                .unwrap_or(item.updated_at_ms);
            list = list.child(
                div()
                    .id("bot-item-wait-card")
                    .debug_selector(|| "bot-item-wait-card".into())
                    .relative()
                    .mx(px(-8.))
                    .my_1()
                    .rounded(ui::RADIUS_LG)
                    .bg(p.sidebar)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ask_waiting(
                            bot_id.clone(),
                            vec![(uid.clone(), title.clone())],
                            Some(wait_owned.clone()),
                            cx,
                        );
                    }))
                    .child(
                        div()
                            .rounded(ui::RADIUS_LG)
                            .bg(p.muted.opacity(0.75))
                            .border_1()
                            .border_color(p.border.opacity(0.6))
                            .px_3()
                            .py_2()
                            .hover(|d| d.bg(p.muted))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .mb_1()
                                    .child(waiting_icon(p))
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_SM)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.foreground)
                                            .child(hm::WAIT_ON_CARD_TITLE.get()),
                                    )
                                    .child(
                                        div()
                                            .ml_auto()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(activity_timestamp(since, now)),
                                    ),
                            )
                            .child(
                                div()
                                    .pl_5()
                                    .text_size(ui::TEXT_SM)
                                    .line_height(px(18.))
                                    .text_color(p.foreground.opacity(0.9))
                                    .child(wait_text.to_owned()),
                            ),
                    ),
            );
        }
        // Orca `#47485`: refused / failed comment cards with Retry & Discard.
        for fc in self
            .items
            .failed_comments
            .get(&item.uid)
            .into_iter()
            .flatten()
        {
            let (
                fid,
                uid,
                body,
                is_answer,
                option_index,
                decision_id,
                ask_step_key,
                skip_remaining,
            ) = (
                fc.id,
                item.uid.clone(),
                fc.text.clone(),
                fc.is_answer,
                fc.option_index,
                fc.decision_id.clone(),
                fc.ask_step_key.clone(),
                fc.skip_remaining,
            );
            let uid_discard = uid.clone();
            let body_retry = body.clone();
            list = list.child(
                div()
                    .id(("bot-item-failed-comment", usize::try_from(fid).unwrap_or(0)))
                    .debug_selector(move || format!("bot-item-failed-comment-{fid}"))
                    .relative()
                    .mx(px(-8.))
                    .my_1()
                    .rounded(ui::RADIUS_LG)
                    .bg(p.sidebar)
                    .child(
                        div()
                            .rounded(ui::RADIUS_LG)
                            .bg(p.muted.opacity(0.6))
                            .border_1()
                            .border_color(p.destructive.opacity(0.45))
                            .px_3()
                            .py_2()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .line_height(px(18.))
                                    .text_color(p.foreground.opacity(0.85))
                                    .child(body),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(ui::TEXT_XS)
                                    .child(
                                        div()
                                            .text_color(p.destructive)
                                            .child(hm::COMMENT_NOT_SENT.get()),
                                    )
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .id((
                                                "bot-item-failed-retry",
                                                usize::try_from(fid).unwrap_or(0),
                                            ))
                                            .cursor_pointer()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.foreground)
                                            .hover(gpui::Styled::underline)
                                            .child(hm::COMMENT_RETRY.get())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                // Guard against busy BEFORE removing the card so Retry
                                                // never silently drops the comment (`BOT-35`).
                                                if this.items.busy {
                                                    return;
                                                }
                                                if let Some(list) =
                                                    this.items.failed_comments.get_mut(&uid)
                                                {
                                                    list.retain(|f| f.id != fid);
                                                }
                                                let clear_ta =
                                                    this.items.comment_inputs.get(&uid).cloned();
                                                let has_files = this
                                                    .items
                                                    .attachments
                                                    .get(&uid)
                                                    .is_some_and(|f| !f.is_empty());
                                                if is_answer {
                                                    let retry_text = if option_index.is_some()
                                                        && !body_retry.contains(" → ")
                                                    {
                                                        None
                                                    } else {
                                                        Some(body_retry.clone())
                                                    };
                                                    this.answer_item_decision_with_options(
                                                        uid.clone(),
                                                        retry_text,
                                                        option_index,
                                                        decision_id.clone(),
                                                        None,
                                                        clear_ta,
                                                        has_files,
                                                        ask_step_key.clone(),
                                                        skip_remaining,
                                                        cx,
                                                    );
                                                } else {
                                                    // Submit directly without re-appending `Attached:` (`BOT-35`).
                                                    this.submit_item_comment_with_attachments(
                                                        uid.clone(),
                                                        &body_retry,
                                                        clear_ta,
                                                        has_files,
                                                        cx,
                                                    );
                                                }
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id((
                                                "bot-item-failed-discard",
                                                usize::try_from(fid).unwrap_or(0),
                                            ))
                                            .cursor_pointer()
                                            .text_color(p.muted_foreground)
                                            .hover(|d| d.text_color(p.foreground))
                                            .child(hm::COMMENT_DISCARD.get())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(list) =
                                                    this.items.failed_comments.get_mut(&uid_discard)
                                                {
                                                    list.retain(|f| f.id != fid);
                                                }
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    ),
            );
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
        let time_for = |at_ms: u64| {
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(activity_timestamp(at_ms, now))
        };
        let time = || time_for(event.created_at_ms);
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
        // A linked worker: started here, and finished/stopped once its session stops working.
        if event.kind == BotItemEventKind::Linked
            && let Some((title, finished)) = self.worker_lines(event)
        {
            let inline_worker = |prefix: &str, title: &str, reason: Option<&str>| {
                let mut full = format!("{prefix} {title}");
                let title_start = prefix.len() + 1;
                let title_end = title_start + title.len();
                if let Some(r) = reason {
                    full.push_str(" · ");
                    full.push_str(r);
                }
                StyledText::new(full).with_highlights([(
                    title_start..title_end,
                    HighlightStyle {
                        color: Some(p.foreground),
                        font_weight: Some(FontWeight::MEDIUM),
                        ..HighlightStyle::default()
                    },
                )])
            };
            let mut col = div().flex_1().min_w_0().flex().flex_col().child(
                line()
                    .child(rail_icon(small(IconName::Play)))
                    .child(div().flex_1().min_w_0().child(inline_worker(
                        hm::WORKER_STARTED.get(),
                        &title,
                        None,
                    )))
                    .child(time()),
            );
            if let Some((stopped, title, finished_at, reason)) = finished {
                let prefix = if stopped {
                    hm::WORKER_STOPPED.get()
                } else {
                    hm::WORKER_FINISHED.get()
                };
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
                                .child(inline_worker(prefix, &title, reason)),
                        )
                        .children(finished_at.map(time_for)),
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
                Some({
                    let raw = event.text.trim();
                    raw.strip_prefix("url:")
                        .or_else(|| raw.strip_prefix("file:"))
                        .unwrap_or(raw)
                        .to_owned()
                }),
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
                        |ms| text::HM_ACT_SNOOZED.format(&[("when", &activity_timestamp(ms, now))]),
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
                    Some(BotItemStatus::Waiting) => Some(match event_waiting_reason(event) {
                        Some(reason) => {
                            format!("{} ({reason})", status_name(BotItemStatus::Waiting))
                        }
                        None => status_name(BotItemStatus::Waiting).to_owned(),
                    }),
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
            .child(time())
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
        superseded: bool,
        skipped: bool,
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
        let event_id = event.id;
        let comment_open = self.items.open_comments.contains(&event_id);
        let is_local = self.host.is_local();
        let base_dir = self.bot_home_folder(bot);
        let quote_source = words.clone();
        let mut body = div().pl_7().flex().flex_col().children(words.map(|w| {
            // Orca `d6cc2684` / `#47076`: while waiting on an open question, show the agent's
            // asking comment whole; once answered or for regular comments, clamp past 8 lines.
            let clamped = !waiting && clamps_comment(&w);
            let md = div()
                .when(waiting, |d| {
                    d.debug_selector(|| "bot-item-comment-whole".into())
                })
                .when(clamped && !comment_open, |d| {
                    d.debug_selector(|| "bot-item-comment-clamped".into())
                        .max_h(px(152.))
                        .overflow_hidden()
                })
                .text_size(ui::TEXT_MD)
                .line_height(px(19.))
                .text_color(p.foreground)
                .child(super::home_item_parts::markdown(
                    format!("bot-item-comment-md-{event_id}"),
                    &w,
                    is_local,
                    base_dir,
                ));
            if clamped {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(md)
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "bot-item-comment-toggle-{event_id}"
                            )))
                            .debug_selector(move || format!("bot-item-comment-toggle-{event_id}"))
                            .self_start()
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .text_size(ui::TEXT_XS)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.muted_foreground)
                            .hover(|d| d.text_color(p.foreground))
                            .child(if comment_open {
                                text::HM_SHOW_LESS.get()
                            } else {
                                text::HM_SHOW_MORE_TEXT.get()
                            })
                            .child(
                                Icon::new(if comment_open {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .size(px(12.)),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.items.open_comments.remove(&event_id) {
                                    this.items.open_comments.insert(event_id);
                                }
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            } else {
                md.into_any_element()
            }
        }));
        let several = event.decisions.len() > 1;
        let answered = (!(event.decisions.is_empty() || (waiting && several)))
            .then(|| super::home_item_parts::answered_lines(item, event));
        let mut questions: gpui::Div = div()
            .flex()
            .flex_col()
            .when(several, gpui::Styled::gap_1p5)
            .when(!several, gpui::Styled::gap_3);
        if waiting && several {
            // An ask of several questions waits one line at a time.
            questions = self.render_question_steps(item, event, p, cx);
        } else {
            let mut has_unanswered = false;
            for (d, decision) in event.decisions.iter().enumerate() {
                if let Some(words) = answered
                    .as_ref()
                    .and_then(|a| a.get(d))
                    .and_then(Option::as_deref)
                {
                    let q = super::home_item_parts::question_line(&decision.question);
                    questions = questions.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .text_size(ui::TEXT_SM)
                            .line_height(px(19.))
                            .text_color(p.muted_foreground)
                            .child(format!("{q} → "))
                            .child(
                                div()
                                    .text_color(p.foreground.opacity(0.82))
                                    .child(words.to_owned()),
                            ),
                    );
                } else if skipped {
                    let q = super::home_item_parts::question_line(&decision.question);
                    questions = questions.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .text_size(ui::TEXT_SM)
                            .line_height(px(19.))
                            .text_color(p.muted_foreground)
                            .child(format!("{q} → "))
                            .child(
                                div()
                                    .text_color(p.muted_foreground.opacity(0.7))
                                    .child(hm::ASK_SKIPPED.get()),
                            ),
                    );
                } else {
                    has_unanswered = true;
                    questions = questions
                        .child(self.render_question(item, event, d, decision, waiting, p, cx));
                }
            }
            // Orca `#47869`: mark an older unanswered ask superseded by a newer question set.
            if superseded && has_unanswered {
                questions = questions.child(
                    div()
                        .debug_selector(move || format!("bot-item-ask-replaced-{event_id}"))
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(hm::ASK_REPLACED.get()),
                );
            }
        }
        if !event.decisions.is_empty() {
            body = body.child(
                div()
                    .when(body_has_words(event), |d| {
                        d.mt_2p5()
                            .pt_2p5()
                            .border_t_1()
                            .border_color(p.border.opacity(0.6))
                    })
                    .child(questions),
            );
        }
        let needs_you_tag = (waiting && item.status == BotItemStatus::NeedsYou).then(|| {
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
        let has_needs_you_tag = needs_you_tag.is_some();
        let card_group: SharedString = format!("bot-comment-card-{event_id}").into();
        let add_to_chat = quote_source.map(|src| {
            let input = self.items.comment_inputs.get(&item.uid).cloned();
            let snippet: String = src.lines().take(3).collect::<Vec<_>>().join("\n> ");
            div()
                .invisible()
                .group_hover(card_group.clone(), gpui::Styled::visible)
                .child(
                    Button::new(SharedString::from(format!(
                        "bot-item-quote-comment-{event_id}"
                    )))
                    .debug_selector(move || format!("bot-item-quote-comment-{event_id}"))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Quote).size(px(12.)))
                    .tooltip(hm::ADD_TO_CHAT.get())
                    .accessibility_label(hm::ADD_TO_CHAT.get())
                    .on_click(move |_, window, cx| {
                        if let Some(input) = &input {
                            input.update(cx, |s, cx| {
                                let prev = s.value().to_string();
                                let quoted = format!("> {snippet}\n\n");
                                let next = if prev.trim().is_empty() {
                                    quoted
                                } else {
                                    format!("{prev}\n{quoted}")
                                };
                                s.set_value(next, window, cx);
                                s.focus(window, cx);
                            });
                        }
                    }),
                )
        });
        div()
            .relative()
            .mx(px(-8.))
            .my_1p5()
            .rounded(ui::RADIUS_LG)
            .bg(p.sidebar)
            .child(
                div()
                    .id(SharedString::from(format!("bot-item-comment-{event_id}")))
                    .debug_selector(move || format!("bot-item-comment-{event_id}"))
                    .group(card_group)
                    .rounded(ui::RADIUS_LG)
                    .bg(p.muted)
                    .border_1()
                    .border_color(p.border.opacity(0.45))
                    .px_3()
                    .pt(px(8.))
                    .when(event.decisions.is_empty(), |d| d.pb(px(10.)))
                    .when(!event.decisions.is_empty(), gpui::Styled::pb_3)
                    .child(
                        div()
                            .mb_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(avatar::render(&face, upload, &name, px(20.), px(10.), p))
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
                                    .when(!has_needs_you_tag, gpui::Styled::ml_auto)
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .children(add_to_chat)
                                    .child(
                                        div()
                                            .whitespace_nowrap()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(activity_timestamp(event.created_at_ms, now)),
                                    ),
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
        let event_id = event.id;
        let mut replies = div().mt(px(10.)).flex().flex_wrap().gap_2();
        for (o, option) in decision.options.iter().enumerate() {
            let recommended = decision.recommended == Some(o);
            let uid = item.uid.clone();
            let id: SharedString = format!("bot-item-reply-{event_id}-{d}-{o}").into();
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
                        this.answer_item_decision_for(
                            uid.clone(),
                            None,
                            u32::try_from(o).ok(),
                            Some(format!("{event_id}:{d}")),
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
        let waiting = waiting_question_with_skipped(item, &self.items.skipped_asks).cloned();
        let quote = waiting
            .as_ref()
            .filter(|w| self.items.reply_hidden.get(&uid) != Some(&w.id))
            .map(|w| {
                let key = (uid.clone(), w.id);
                let line = if w.decisions.len() > 1 {
                    super::home_item_parts::question_step_state(
                        item,
                        w,
                        self.items.ask_steps.get(&key),
                    )
                    .current
                } else {
                    0
                };
                w.decisions.get(line).map_or_else(
                    || super::home_item_parts::question_line(&w.text),
                    |d| super::home_item_parts::question_line(&d.question),
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
            .map(|q| self.mention_matches(bot, &q, cx))
            .unwrap_or_default();
        let files = self
            .items
            .attachments
            .get(&uid)
            .cloned()
            .unwrap_or_default();
        let empty = value.trim().is_empty() && files.is_empty();
        let replying = quote.is_some();
        let pill = !replying
            && files.is_empty()
            && mentions.is_empty()
            && !value.contains('\n')
            && value.chars().count() < 48;
        let mut boxed = div()
            .debug_selector(|| "bot-item-comment-box".into())
            .flex_none()
            .mx_3()
            .mb_3()
            .mt_2()
            .flex()
            .flex_col()
            .when(pill, |d| d.rounded_full().px_2().py(px(5.)))
            .when(!pill, |d| d.rounded(ui::RADIUS_XL).p(px(6.)))
            .border_1()
            .border_color(p.chat_composer_border())
            .bg(p.chat_composer_surface());
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
                .child(Textarea::new(&i).appearance(false))
        });
        let send_input = self.items.comment_inputs.get(&uid).cloned();
        let mut tools = div()
            .flex()
            .when(replying, |d| d.flex_wrap().items_end())
            .when(!replying, gpui::Styled::items_center)
            .gap_1();
        if replying {
            tools = tools.children(field).child(div().w_full());
        }
        if self.host.is_local() {
            tools = tools.child(
                Button::new("bot-item-attach")
                    .debug_selector(|| "bot-item-attach".into())
                    .ghost()
                    .xsmall()
                    .icon(
                        Icon::new(IconName::Plus)
                            .size(px(16.))
                            .text_color(p.muted_foreground),
                    )
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
                    .child(Textarea::new(&i).appearance(false))
            }));
        }
        let send_btn = div()
            .id("bot-item-comment-send")
            .debug_selector(|| "bot-item-comment-send".into())
            .role(gpui::Role::Button)
            .aria_label(text::HM_COMMENT.get())
            .size(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(if empty {
                p.foreground.opacity(0.35)
            } else {
                p.foreground
            })
            .when(!empty, |d| {
                d.cursor_pointer()
                    .hover(|d| d.bg(p.foreground.opacity(0.9)))
            })
            .child(
                Icon::new(IconName::ArrowUp)
                    .size(px(15.))
                    .text_color(p.background),
            )
            .tooltip(|w, cx| ui::tooltip_view("bot-item-send-tip", text::HM_COMMENT.get(), w, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                if empty {
                    return;
                }
                let Some(input) = send_input.clone() else {
                    return;
                };
                let body = input.read(cx).value().to_string();
                this.send_item_comment(uid.clone(), &body, Some(input), cx);
            }));
        tools = tools
            .child(self.render_stepper(replying, p, cx))
            .child(send_btn);
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
