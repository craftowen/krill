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
use gpui_component::{Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::bots::{
    BotConfig, BotItem, BotItemActor, BotItemDecision, BotItemEvent, BotItemEventKind, BotItemLink,
    BotItemStatus,
};

use super::home::{clock_label, is_snoozed, status_icon, status_name};
use super::{OpenSession, View, avatar, now_unix_ms};
use crate::i18n::catalog::bots as text;
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
        let waiting = self
            .items
            .items
            .iter()
            .find(|i| i.uid == uid)
            .is_some_and(|i| waiting_question(i).is_some());
        if waiting {
            self.answer_item_decision(uid, Some(body.to_owned()), None, input, cx);
        } else {
            self.submit_item_comment(uid, body, input, cx);
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
        if let Some(done) = item.done_means.as_ref().filter(|d| !d.trim().is_empty()) {
            content = content.child(
                div()
                    .debug_selector(|| "bot-item-done-means".into())
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_size(ui::TEXT_SM)
                    .line_height(px(18.))
                    .child(
                        div()
                            .w(px(84.))
                            .flex_none()
                            .text_color(p.muted_foreground)
                            .child(text::HM_DONE_MEANS.get()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(p.foreground)
                            .child(done.clone()),
                    ),
            );
        }
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
            .child(back)
            .child(scroller)
            .child(self.render_comment_box(item, p, cx))
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
                            .on_click(move |_, _, cx| {
                                this.update(cx, |v, cx| {
                                    v.update_item_status(bot_id.clone(), uid.clone(), status, cx);
                                })
                                .ok();
                            }),
                    );
                }
                if current.is_open() {
                    menu = Self::snooze_menu_items(menu.separator(), &this, &bot_id, &uid);
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
                                text::HM_ITEM_SUB.format(&[("n", &item.number.to_string())]),
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
                    .child(div().flex_1().min_w_0().truncate().child(
                        text::HM_SNOOZED_UNTIL.format(&[("when", &clock_label(until, now))]),
                    ))
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
            header = header.child(Self::render_link_chips(bot, item, p, cx));
        }
        header
    }

    /// Orca's `BotItemLinkChips`: one chip per link; a chip opens it.
    fn render_link_chips(
        bot: &BotConfig,
        item: &BotItem,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut row = div().flex().flex_wrap().gap_1();
        for (i, link) in item.links.iter().enumerate() {
            let (icon, target) = match link {
                BotItemLink::Pr(v) => (IconName::GitPullRequest, v),
                BotItemLink::Url(v) => (IconName::Link, v),
                BotItemLink::File(v) => (IconName::File, v),
                BotItemLink::Dispatch(v) => (IconName::Bot, v),
            };
            let label = match link {
                BotItemLink::Pr(v) => v
                    .rsplit('/')
                    .next()
                    .filter(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
                    .map_or_else(|| v.clone(), |n| format!("#{n}")),
                BotItemLink::File(v) => v.rsplit('/').next().unwrap_or(v).to_owned(),
                _ => target.clone(),
            };
            let link = link.clone();
            let bot_id = bot.id.clone();
            row = row.child(
                div()
                    .id(("bot-item-link", i))
                    .h(px(22.))
                    .max_w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1p5()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.border)
                    .text_size(ui::TEXT_SM)
                    .text_color(p.foreground)
                    .cursor_pointer()
                    .hover(|d| d.bg(p.sidebar_accent))
                    .child(Icon::new(icon).size(px(12.)).text_color(p.muted_foreground))
                    .child(div().min_w_0().truncate().child(label))
                    .on_click(cx.listener(move |_, _, _, cx| match &link {
                        BotItemLink::Pr(v) | BotItemLink::Url(v)
                            if v.starts_with("https://") || v.starts_with("http://") =>
                        {
                            cx.open_url(v);
                        }
                        BotItemLink::File(v) if !v.trim().is_empty() => {
                            cx.reveal_path(std::path::Path::new(v.trim()));
                        }
                        BotItemLink::Dispatch(v) => {
                            if let Ok(n) = v.trim().parse::<u64>() {
                                cx.emit(OpenSession {
                                    bot_id: bot_id.clone(),
                                    session_id: krill_proto::SessionId(n),
                                });
                            }
                        }
                        _ => {}
                    })),
            );
        }
        row
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
                    .child(desc.to_owned()),
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
        let mut comments = 0;
        for event in &item.events {
            let card = event.kind == BotItemEventKind::Commented || !event.decisions.is_empty();
            if card {
                comments += 1;
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
                list = list.child(Self::render_event_row(bot, item, event, now, p, cx));
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
        bot: &BotConfig,
        _item: &BotItem,
        event: &BotItemEvent,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
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
                .child(w)
        }));
        let several = event.decisions.len() > 1;
        let mut questions = div()
            .flex()
            .flex_col()
            .when(several, gpui::Styled::gap_1p5)
            .when(!several, gpui::Styled::gap_3);
        for (d, decision) in event.decisions.iter().enumerate() {
            questions =
                questions.child(self.render_question(item, event, d, decision, waiting, p, cx));
        }
        if !event.decisions.is_empty() {
            body = body.child(
                div()
                    .when(body_has_words(event), gpui::Styled::mt_4)
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

    /// Orca's comment box: the field and the send arrow, always at the bottom.
    fn render_comment_box(
        &self,
        item: &BotItem,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let input = self.items.comment_inputs.get(&item.uid).cloned();
        let uid = item.uid.clone();
        let send_input = input.clone();
        div()
            .debug_selector(|| "bot-item-comment-box".into())
            .flex_none()
            .mx_3()
            .mb_3()
            .mt_2()
            .flex()
            .items_end()
            .gap_0p5()
            .rounded(ui::RADIUS_XL)
            .border_1()
            .border_color(p.border)
            .bg(p.background)
            .p(px(6.))
            .children(input.map(|i| {
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&i).appearance(false))
            }))
            .child(
                Button::new("bot-item-comment-send")
                    .debug_selector(|| "bot-item-comment-send".into())
                    .ghost()
                    .small()
                    .icon(Icon::new(IconName::ArrowUp).size(px(16.)))
                    .tooltip(text::HM_COMMENT.get())
                    .accessibility_label(text::HM_COMMENT.get())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let Some(input) = send_input.clone() else {
                            return;
                        };
                        let body = input.read(cx).value().to_string();
                        if !body.trim().is_empty() {
                            this.send_item_comment(uid.clone(), &body, Some(input), cx);
                        }
                    })),
            )
    }
}

fn body_has_words(event: &BotItemEvent) -> bool {
    event.kind == BotItemEventKind::Commented && !event.text.trim().is_empty()
}
