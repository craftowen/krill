// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License):
// src/renderer/src/components/settings/bots/BotSoulEditor.tsx, BotMemoryPanel.tsx,
// BotMemoryList.tsx, BotMemoryHistory.tsx, bots/settings-view/BotMemoryTopicFiles.tsx and
// shared/bots/bot-memory-history-copy.ts.
//! Settings › Soul (SOUL.md read or edit), Settings › Memory (what it knows, its History and
//! topic files) and Krill's Settings › Scheduled tasks.

use gpui::{
    AnyElement, AppContext as _, Context, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, Textarea};
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use krill_proto::bots::{
    BOT_MEMORY_INDEX_LINE_CAP, BotConfig, BotMemoryHistoryEntry, append_memory_fact,
    count_memory_lines, parse_memory_facts,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::View;
use super::settings_root::{Open, Row, row_switch};

const SOUL_FILE: &str = "SOUL.md";

/// Orca's `BotMemoryHistoryKind`: the row's mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HistoryKind {
    TidyUp,
    Undo,
    Edit,
    Forget,
    Add,
}

pub(super) fn history_kind(e: &BotMemoryHistoryEntry) -> HistoryKind {
    if e.actor == "system:tidy" || e.action == "tidy" {
        HistoryKind::TidyUp
    } else if e.action.contains("undo") {
        HistoryKind::Undo
    } else if !e.added.is_empty() && !e.removed.is_empty() {
        HistoryKind::Edit
    } else if e.removed.is_empty() {
        HistoryKind::Add
    } else {
        HistoryKind::Forget
    }
}

fn line_text(line: &str) -> String {
    parse_memory_facts(line).first().map_or_else(
        || line.trim().trim_start_matches(['-', '*']).trim().to_owned(),
        |f| f.text.clone(),
    )
}

/// What changed, in a few words (Orca's `botMemoryHistorySummary`).
pub(super) fn history_summary(e: &BotMemoryHistoryEntry) -> String {
    let (added, removed) = (&e.added, &e.removed);
    let n = |n: usize| n.to_string();
    if history_kind(e) == HistoryKind::TidyUp {
        return text::SV_HIST_TIDIED.format(&[("n", &n(added.len() + removed.len()))]);
    }
    match (added.as_slice(), removed.as_slice()) {
        ([one], []) => text::SV_HIST_ADDED.format(&[("fact", &line_text(one))]),
        ([one], [_]) => text::SV_HIST_EDITED.format(&[("fact", &line_text(one))]),
        ([], [one]) => text::SV_HIST_FORGOT.format(&[("fact", &line_text(one))]),
        (a, []) => text::SV_HIST_ADDED_MANY.format(&[("n", &n(a.len()))]),
        ([], r) => text::SV_HIST_FORGOT_MANY.format(&[("n", &n(r.len()))]),
        (a, r) => text::SV_HIST_CHANGED_MANY.format(&[("n", &n(a.len() + r.len()))]),
    }
}

/// Who made it (Orca's `botMemoryHistoryAuthor`).
fn history_author(e: &BotMemoryHistoryEntry) -> String {
    let author = match history_kind(e) {
        HistoryKind::TidyUp => text::SV_HIST_BY_TIDY.get(),
        HistoryKind::Undo => text::SV_HIST_BY_UNDO.get(),
        _ if e.actor == "owner" => text::SV_HIST_BY_OWNER.get(),
        _ => text::SV_HIST_BY_CHAT.get(),
    };
    if e.undoable || e.undone {
        author.to_owned()
    } else {
        text::SV_HIST_SECRET.format(&[("author", author)])
    }
}

/// "Today", "Yesterday", then the date, on this computer's clock.
fn day_label(at_ms: u64, now_ms: u64) -> String {
    let day = |ms: u64| {
        crate::local_time::format_timestamp(i64::try_from(ms / 1_000).unwrap_or(0))
            .map(|s| s.chars().take(10).collect::<String>())
    };
    let (d, today) = (day(at_ms), day(now_ms));
    if d == today {
        text::SV_TODAY.get().to_owned()
    } else if d == day(now_ms.saturating_sub(86_400_000)) {
        text::SV_YESTERDAY.get().to_owned()
    } else {
        d.map(|d| super::settings_pages::short_date(&d))
            .unwrap_or_default()
    }
}

fn clock(at_ms: u64) -> String {
    crate::local_time::format_clock(i64::try_from(at_ms / 1_000).unwrap_or(0)).unwrap_or_default()
}

impl View {
    pub(super) fn render_soul_page(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_soul_inputs(bot, window, cx);
        let draft = self.soul_memory.soul_input.as_ref().map_or_else(
            || self.soul_memory.soul_loaded.clone(),
            |i| i.read(cx).value().to_string(),
        );
        let dirty = draft != self.soul_memory.soul_loaded;
        let editing = self
            .settings
            .soul_editing
            .unwrap_or_else(|| self.soul_memory.soul_loaded.trim().is_empty());
        let saving = self.soul_memory.busy;
        let status: AnyElement = if saving {
            text::SV_SAVING.get().into_any_element()
        } else if dirty {
            text::SV_UNSAVED.get().into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(Icon::new(IconName::Check).size(px(12.)))
                .child(text::SV_SAVED.get())
                .into_any_element()
        };
        let save_bot = bot.id.clone();
        let dark = cx.theme().mode.is_dark();
        let active_pill_bg = if dark {
            p.foreground.opacity(0.14)
        } else {
            p.background
        };
        let mode_tab = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(5.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .map(|d| {
                    if on {
                        d.bg(active_pill_bg).text_color(p.foreground)
                    } else {
                        d.text_color(p.muted_foreground)
                            .hover(|d| d.text_color(p.foreground))
                    }
                })
                .child(label)
        };
        let header = div()
            .h(px(40.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .border_b_1()
            .border_color(p.border.opacity(0.6))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .truncate()
                            .font_family(ui::FONT_MONO)
                            .text_size(ui::TEXT_SM)
                            .text_color(p.foreground.opacity(0.85))
                            .child(SOUL_FILE),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(status),
                    ),
            )
            .child(
                div()
                    .h(px(28.))
                    .p(px(2.))
                    .flex()
                    .items_center()
                    .rounded(ui::RADIUS_SM)
                    .bg(p.foreground.opacity(0.06))
                    .text_size(ui::TEXT_XS)
                    .child(
                        mode_tab("bot-soul-read", text::SV_SOUL_READ.get(), !editing).on_click(
                            cx.listener(move |this, _, _, cx| {
                                // Leaving Edit saves, as Orca's editor does on Read and blur.
                                if let Some(i) = &this.soul_memory.soul_input {
                                    let draft = i.read(cx).value().to_string();
                                    if draft != this.soul_memory.soul_loaded {
                                        this.save_soul(save_bot.clone(), draft, cx);
                                    }
                                }
                                this.settings.soul_editing = Some(false);
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        mode_tab("bot-soul-edit", text::SV_SOUL_EDIT.get(), editing).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.settings.soul_editing = Some(true);
                                if let Some(i) = &this.soul_memory.soul_input {
                                    i.update(cx, |s, cx| s.focus(window, cx));
                                }
                                cx.notify();
                            }),
                        ),
                    ),
            );
        let body: AnyElement = if editing {
            div()
                .flex_1()
                .min_h_0()
                .px_4()
                .py(px(14.))
                .flex()
                .children(self.soul_memory.soul_input.as_ref().map(|i| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Textarea::new(i).appearance(false))
                }))
                .into_any_element()
        } else {
            div()
                .id("bot-soul-preview")
                .debug_selector(|| "bot-soul-preview".into())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px_4()
                .py_4()
                .text_size(ui::TEXT_MD)
                .line_height(relative(1.55))
                .text_color(p.foreground)
                .whitespace_normal()
                .on_mouse_down(gpui::MouseButton::Left, |ev, _, cx| {
                    if ev.click_count < 2 {
                        cx.propagate();
                    }
                })
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    if ev.click_count() >= 2 {
                        this.settings.soul_editing = Some(true);
                        cx.notify();
                    }
                }))
                .children(draft.lines().map(|line| {
                    let t = line.trim_start();
                    if let Some(h) = t.strip_prefix("# ") {
                        div()
                            .pt_1()
                            .text_size(ui::TEXT_XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(h.to_owned())
                    } else if let Some(h) = t.strip_prefix("## ").or_else(|| t.strip_prefix("### "))
                    {
                        div()
                            .pt_2()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(h.to_owned())
                    } else if line.trim().is_empty() {
                        div().h(px(8.))
                    } else {
                        div().child(line.to_owned())
                    }
                }))
                .into_any_element()
        };
        let lines = if draft.is_empty() {
            0
        } else {
            draft.split('\n').count()
        };
        let words = draft.split_whitespace().count();
        let footer = div()
            .h(px(32.))
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(p.border.opacity(0.6))
            .text_size(ui::TEXT_XS)
            .text_color(p.muted_foreground)
            .child(
                text::SV_SOUL_STATS
                    .format(&[("lines", &lines.to_string()), ("words", &words.to_string())]),
            )
            .when(!editing, |d| d.child(text::SV_SOUL_HINT.get()));
        let card = div()
            .id("bot-soul-card")
            .debug_selector(|| "bot-soul-card".into())
            .flex_1()
            .min_h(px(360.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(ui::RADIUS_LG)
            .map(|d| {
                if editing {
                    d.border_1().border_color(p.border).bg(p.background)
                } else {
                    d.bg(p.foreground.opacity(0.035))
                }
            })
            .child(header)
            .child(body)
            .child(footer);
        Self::settings_screen("bot-settings-soul-page").child(card)
    }

    fn ensure_soul_inputs(&mut self, bot: &BotConfig, window: &mut Window, cx: &mut Context<Self>) {
        // Always delegate so pending SYNC_SOUL / SYNC_MEMORY / SYNC_TOPIC flags are
        // applied even after the inputs have already been created.
        self.ensure_soul_memory_inputs(bot, window, cx);
    }

    fn ensure_memory_add_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.settings.memory_add_input.clone() {
            crate::i18n::sync_input_placeholder(
                &input,
                text::SV_MEMORY_ADD_PLACEHOLDER.get(),
                window,
                cx,
            );
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(text::SV_MEMORY_ADD_PLACEHOLDER.get())
        });
        self.settings.memory_add_sub =
            Some(
                cx.subscribe_in(&input, window, |this, ent, ev: &InputEvent, window, cx| {
                    if matches!(ev, InputEvent::PressEnter { .. })
                        && !crate::jump::entity_input_is_composing(ent, window, cx)
                    {
                        let typed = ent.read(cx).value().trim().to_owned();
                        if this.remember_fact(&typed, cx) {
                            ent.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                    }
                    if matches!(ev, InputEvent::Change) {
                        cx.notify();
                    }
                }),
            );
        self.settings.memory_add_input = Some(input);
    }

    /// Quick-add: one new fact at the top of MEMORY.md, dated today.
    fn remember_fact(&mut self, typed: &str, cx: &mut Context<Self>) -> bool {
        let Some(bot_id) = self.selected_bot_id.clone() else {
            return false;
        };
        if typed.is_empty() || self.soul_memory.busy {
            return false;
        }
        let today = crate::local_time::format_timestamp(
            i64::try_from(super::now_unix_ms() / 1_000).unwrap_or(0),
        )
        .map(|s| s.chars().take(10).collect::<String>());
        let next = append_memory_fact(
            &self.soul_memory.memory_loaded,
            typed,
            today.as_deref(),
            None,
        );
        self.save_memory(bot_id, next, cx);
        true
    }

    pub(super) fn render_memory_page(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_soul_inputs(bot, window, cx);
        self.ensure_memory_add_input(window, cx);
        let facts = parse_memory_facts(&self.soul_memory.memory_loaded);
        let history = self.settings.memory_history_tab;
        let dark = cx.theme().mode.is_dark();
        let active_pill_bg = if dark {
            p.foreground.opacity(0.14)
        } else {
            p.background
        };
        let tab = |id: &'static str, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_1()
                .h(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .gap_1()
                .rounded(px(5.))
                .text_size(ui::TEXT_SM)
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .map(|d| {
                    if on {
                        d.bg(active_pill_bg).text_color(p.foreground)
                    } else {
                        d.text_color(p.muted_foreground)
                            .hover(|d| d.text_color(p.foreground))
                    }
                })
        };
        let tabs = div()
            .w_full()
            .h(px(28.))
            .p(px(2.))
            .flex()
            .rounded(ui::RADIUS_SM)
            .bg(p.foreground.opacity(0.06))
            .child(
                tab("bot-memory-tab-facts", !history)
                    .child(text::SV_MEMORY_FACTS.get())
                    .when(!facts.is_empty(), |d| {
                        d.child(
                            div()
                                .px(px(6.))
                                .rounded_full()
                                .bg(p.foreground.opacity(0.08))
                                .text_size(ui::TEXT_3XS)
                                .text_color(p.muted_foreground)
                                .child(facts.len().to_string()),
                        )
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.memory_history_tab = false;
                        cx.notify();
                    })),
            )
            .child(
                tab("bot-memory-tab-history", history)
                    .child(text::SV_MEMORY_HISTORY.get())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.memory_history_tab = true;
                        cx.notify();
                    })),
            );
        let screen = Self::settings_screen("bot-settings-memory-page").child(tabs);
        if history {
            return screen.child(self.render_memory_history(bot, p, cx));
        }
        screen
            .child(self.render_memory_facts(bot, p, cx))
            .children(self.render_topic_files(bot, p, cx))
    }

    fn render_memory_facts(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let typed = self
            .settings
            .memory_add_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        let quick_add = div()
            .h(px(36.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(p.border)
            .bg(p.background)
            .child(
                Icon::new(IconName::Plus)
                    .size(px(ui::ICON_MD))
                    .text_color(p.muted_foreground),
            )
            .children(self.settings.memory_add_input.as_ref().map(|i| {
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(i).appearance(false).small())
            }))
            .when(!typed.is_empty(), |d| {
                d.child(
                    ui::primary("bot-memory-add-save", text::SV_SAVE.get())
                        .xsmall()
                        .disabled(self.soul_memory.busy)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.remember_fact(&typed, cx)
                                && let Some(i) = &this.settings.memory_add_input
                            {
                                i.update(cx, |s, cx| s.set_value("", window, cx));
                            }
                        })),
                )
            });

        let lines = count_memory_lines(&self.soul_memory.memory_loaded);
        #[expect(
            clippy::cast_precision_loss,
            reason = "line counts stay far below f32's exact range"
        )]
        let pct = (lines as f32 / BOT_MEMORY_INDEX_LINE_CAP as f32).min(1.0);
        let title = div()
            .font_family(ui::FONT_MONO)
            .text_size(ui::TEXT_2XS)
            .child(text::SV_MEMORY_FILE.get())
            .into_any_element();
        let action = div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(ui::TEXT_2XS)
            .text_color(p.muted_foreground)
            .child(text::SV_MEMORY_LINES.format(&[
                ("n", &lines.to_string()),
                ("cap", &BOT_MEMORY_INDEX_LINE_CAP.to_string()),
            ]))
            .child(
                div()
                    .h(px(4.))
                    .w(px(40.))
                    .overflow_hidden()
                    .rounded_full()
                    .bg(p.foreground.opacity(0.1))
                    .child(
                        div()
                            .h_full()
                            .w(relative(pct))
                            .rounded_full()
                            .bg(p.foreground.opacity(0.6)),
                    ),
            )
            .into_any_element();

        let facts = parse_memory_facts(&self.soul_memory.memory_loaded);
        let mut card = div()
            .w_full()
            .rounded(ui::RADIUS_LG)
            .overflow_hidden()
            .bg(p.foreground.opacity(0.035))
            .flex()
            .flex_col();
        if facts.is_empty() {
            card = card.child(
                div()
                    .px_3()
                    .py_3()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::SV_MEMORY_EMPTY.get()),
            );
        }
        for (i, fact) in facts.into_iter().enumerate() {
            if i > 0 {
                card = card.child(div().pl_3().child(div().h(px(1.)).bg(p.border)));
            }
            let editing = self.soul_memory.editing_fact.as_deref() == Some(fact.text.as_str());
            if editing && let Some(input) = &self.soul_memory.fact_edit_input {
                let save_bot = bot.id.clone();
                let old = fact.text.clone();
                let read = input.clone();
                card = card.child(
                    div()
                        .p(px(10.))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .bg(p.background.opacity(0.8))
                        .child(Input::new(input).small())
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_1()
                                .child(
                                    ui::ghost("bot-memory-edit-cancel", text::CANCEL.get())
                                        .xsmall()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.soul_memory.editing_fact = None;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    ui::primary("bot-memory-edit-save", text::SV_SAVE.get())
                                        .xsmall()
                                        .disabled(self.soul_memory.busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let val = read.read(cx).value().to_string();
                                            this.edit_memory_fact(
                                                save_bot.clone(),
                                                old.clone(),
                                                &val,
                                                cx,
                                            );
                                        })),
                                ),
                        ),
                );
                continue;
            }
            let meta: Vec<String> = fact
                .date
                .iter()
                .map(|d| super::settings_pages::short_date(d))
                .chain(
                    fact.source
                        .iter()
                        .map(|s| text::SV_MEMORY_SOURCE.format(&[("source", s)])),
                )
                .collect();
            let group: SharedString = format!("bot-fact-{i}").into();
            let edit_text = fact.text.clone();
            let forget_text = fact.text.clone();
            let forget_bot = bot.id.clone();
            card = card.child(
                div()
                    .id(SharedString::from(format!("bot-memory-fact-{i}")))
                    .group(group.clone())
                    .relative()
                    .px_3()
                    .py_2()
                    .hover(|d| d.bg(p.accent.opacity(0.5)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_fact_edit(&edit_text, window, cx);
                    }))
                    .child(
                        div()
                            .text_size(ui::TEXT_MD)
                            .line_height(px(18.5))
                            .text_color(p.foreground)
                            .child(fact.text.clone()),
                    )
                    .when(!meta.is_empty(), |d| {
                        d.child(
                            div()
                                .mt_1()
                                .truncate()
                                .text_size(ui::TEXT_2XS)
                                .text_color(p.muted_foreground)
                                .child(meta.join(" · ")),
                        )
                    })
                    .child(
                        div()
                            .absolute()
                            .top(px(6.))
                            .right_2()
                            .flex()
                            .items_center()
                            .gap_0p5()
                            .rounded(ui::RADIUS_SM)
                            .bg(p.sidebar.opacity(0.9))
                            .pl(px(6.))
                            .invisible()
                            .group_hover(group, gpui::Styled::visible)
                            .child(
                                Button::new(SharedString::from(format!("bot-fact-edit-{i}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        Icon::default()
                                            .data(ui::icons::PENCIL_SVG)
                                            .size(px(ui::ICON_XS)),
                                    )
                                    .tooltip(text::SV_EDIT.get())
                                    .accessibility_label(text::SV_EDIT.get()),
                            )
                            .child(
                                Button::new(SharedString::from(format!("bot-fact-forget-{i}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(
                                        Icon::default()
                                            .data(ui::icons::TRASH_SVG)
                                            .size(px(ui::ICON_XS)),
                                    )
                                    .text_color(p.destructive)
                                    .tooltip(text::SV_FORGET.get())
                                    .accessibility_label(text::SV_FORGET.get())
                                    .disabled(self.soul_memory.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.forget_memory_fact(
                                            forget_bot.clone(),
                                            forget_text.clone(),
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            );
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(quick_add)
            .child(Self::inset_frame(Some(title), Some(action), card, None, p))
    }

    fn start_fact_edit(&mut self, fact: &str, window: &mut Window, cx: &mut Context<Self>) {
        let cur = fact.to_owned();
        let input = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            s.set_value(cur.clone(), window, cx);
            s
        });
        let old = fact.to_owned();
        self.soul_memory.fact_edit_sub = Some(cx.subscribe_in(
            &input,
            window,
            move |this, ent, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. })
                    && !crate::jump::entity_input_is_composing(ent, window, cx)
                    && let Some(bid) = this.selected_bot_id.clone()
                {
                    let val = ent.read(cx).value().to_string();
                    this.edit_memory_fact(bid, old.clone(), &val, cx);
                }
            },
        ));
        input.update(cx, |s, cx| s.focus(window, cx));
        self.soul_memory.fact_edit_input = Some(input);
        self.soul_memory.editing_fact = Some(fact.to_owned());
        cx.notify();
    }

    /// Orca's `BotMemoryHistory`: every change, newest first, grouped by day.
    fn render_memory_history(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let entries = &self.soul_memory.memory_history;
        if entries.is_empty() {
            return div()
                .px_1()
                .text_size(ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(text::SV_HIST_EMPTY.get());
        }
        let now = super::now_unix_ms();
        let mut sorted: Vec<&BotMemoryHistoryEntry> = entries.iter().collect();
        sorted.sort_by_key(|e| std::cmp::Reverse(e.timestamp_ms));
        let mut groups: Vec<(String, Vec<&BotMemoryHistoryEntry>)> = Vec::new();
        for e in sorted {
            let day = day_label(e.timestamp_ms, now);
            match groups.last_mut() {
                Some((d, rows)) if *d == day => rows.push(e),
                _ => groups.push((day, vec![e])),
            }
        }
        let mut out = div().w_full().flex().flex_col().gap(px(14.));
        for (day, rows) in groups {
            let mut card = div()
                .w_full()
                .rounded(ui::RADIUS_LG)
                .overflow_hidden()
                .bg(p.foreground.opacity(0.035))
                .flex()
                .flex_col();
            for (i, e) in rows.into_iter().enumerate() {
                if i > 0 {
                    card = card.child(div().pl(px(38.)).child(div().h(px(1.)).bg(p.border)));
                }
                card = card.child(self.render_history_row(bot, e, p, cx));
            }
            out = out.child(Self::inset_frame(
                Some(Self::group_title(&day)),
                None,
                card,
                None,
                p,
            ));
        }
        out
    }

    fn render_history_row(
        &self,
        bot: &BotConfig,
        e: &BotMemoryHistoryEntry,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.settings.history_open.as_deref() == Some(e.entry_id.as_str());
        let icon = match history_kind(e) {
            HistoryKind::TidyUp => Icon::default().data(ui::icons::SPARKLES_SVG),
            HistoryKind::Undo => Icon::default().data(ui::icons::ROTATE_CCW_SVG),
            HistoryKind::Edit => Icon::default().data(ui::icons::PENCIL_SVG),
            HistoryKind::Forget => Icon::new(IconName::Minus),
            HistoryKind::Add => Icon::new(IconName::Plus),
        };
        let id = e.entry_id.clone();
        let undo_id = e.entry_id.clone();
        let undo_bot = bot.id.clone();
        let line = |l: &String, sign: &'static str, fg| {
            div()
                .flex()
                .gap_1p5()
                .text_color(fg)
                .child(div().flex_none().child(sign))
                .child(
                    div()
                        .min_w_0()
                        .child(l.trim().trim_start_matches(['-', '*']).trim().to_owned()),
                )
        };
        let diff_bg = if cx.theme().mode.is_dark() {
            p.background.opacity(0.5)
        } else {
            p.foreground.opacity(0.045)
        };
        div()
            .id(SharedString::from(format!("bot-history-{}", e.entry_id)))
            .px_3()
            .py(px(10.))
            .hover(|d| d.bg(p.accent.opacity(0.4)))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "bot-history-toggle-{}",
                        e.entry_id
                    )))
                    .w_full()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings.history_open = (this.settings.history_open.as_deref()
                            != Some(id.as_str()))
                        .then(|| id.clone());
                        cx.notify();
                    }))
                    .child(
                        div()
                            .mt_0p5()
                            .size(px(16.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(ui::RADIUS_XS)
                            .bg(p.foreground.opacity(0.07))
                            .child(icon.size(px(10.)).text_color(p.muted_foreground)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui::TEXT_MD)
                                    .line_height(px(18.))
                                    .text_color(p.foreground)
                                    .child(history_summary(e)),
                            )
                            .child(
                                div()
                                    .mt_0p5()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .text_size(ui::TEXT_2XS)
                                    .text_color(p.muted_foreground)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(history_author(e)),
                                    )
                                    .child(div().flex_none().child(clock(e.timestamp_ms))),
                            ),
                    )
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .mt_1()
                        .size(px(ui::ICON_MD))
                        .text_color(p.muted_foreground.opacity(0.7)),
                    ),
            )
            .when(open, |d| {
                d.child(
                    div()
                        .mt(px(10.))
                        .ml(px(26.))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .rounded(ui::RADIUS_SM)
                                .bg(diff_bg)
                                .p_2()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .font_family(ui::FONT_MONO)
                                .text_size(ui::TEXT_2XS)
                                .line_height(px(17.))
                                .children(
                                    e.removed.iter().map(|l| line(l, "−", p.muted_foreground)),
                                )
                                .children(e.added.iter().map(|l| line(l, "+", p.foreground))),
                        )
                        .when(e.undoable && !e.undone, |d| {
                            d.child(
                                div().flex().justify_end().child(
                                    Button::new(SharedString::from(format!(
                                        "bot-history-undo-{}",
                                        e.entry_id
                                    )))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Undo2)
                                    .label(text::SV_HIST_UNDO.get())
                                    .disabled(self.soul_memory.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.undo_memory_change(
                                            undo_bot.clone(),
                                            undo_id.clone(),
                                            cx,
                                        );
                                    })),
                                ),
                            )
                        }),
                )
            })
            .into_any_element()
    }

    /// Orca's `BotMemoryTopicFiles`: the topic files, then the archive, each opening in place.
    fn render_topic_files(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::Div> {
        let (archive, topics): (Vec<_>, Vec<_>) = self
            .soul_memory
            .memory_topics
            .iter()
            .partition(|t| t.topic.starts_with("archive"));
        let row = |t: &krill_proto::bots::BotMemoryTopicInfo, icon: Option<&'static [u8]>| {
            let name = format!("{}.md", t.topic);
            let topic = t.topic.clone();
            let bot_id = bot.id.clone();
            let open = self.settings.topic_open.as_deref() == Some(t.topic.as_str());
            let row = Row::new(
                SharedString::from(format!("bot-memory-topic-{}", t.topic)),
                icon,
                name,
            );
            let row = if icon.is_none() {
                row.icon_name(IconName::FileText)
            } else {
                row
            };
            row.detail(text::SV_TOPIC_BYTES.format(&[("n", &t.bytes.to_string())]))
                .end(
                    Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ExternalLink
                    })
                    .size(px(ui::ICON_MD))
                    .text_color(p.muted_foreground),
                )
                .opens(Open::run(move |this, _, cx| {
                    if this.settings.topic_open.as_deref() == Some(topic.as_str()) {
                        this.settings.topic_open = None;
                    } else {
                        this.settings.topic_open = Some(topic.clone());
                        this.select_memory_topic(bot_id.clone(), &topic, cx);
                    }
                    cx.notify();
                }))
        };
        let mut out = Vec::new();
        if !topics.is_empty() {
            out.push(Self::render_inset_group(
                Some(Self::group_title(text::SV_TOPIC_FILES.get())),
                None,
                topics.iter().map(|t| row(t, None)).collect(),
                None,
                false,
                p,
                cx,
            ));
        }
        if !archive.is_empty() {
            out.push(Self::render_inset_group(
                None,
                None,
                archive
                    .iter()
                    .map(|t| row(t, Some(ui::icons::HISTORY_SVG)))
                    .collect(),
                None,
                false,
                p,
                cx,
            ));
        }
        if let Some(topic) = self.settings.topic_open.clone()
            && let Some(input) = &self.soul_memory.topic_content_input
        {
            let read = input.clone();
            let bot_id = bot.id.clone();
            out.push(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .px_3()
                            .font_family(ui::FONT_MONO)
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(text::SV_TOPIC_FILE.format(&[("topic", &topic)])),
                    )
                    .child(Textarea::new(input).small())
                    .child(
                        div().flex().justify_end().child(
                            ui::primary("bot-memory-topic-save", text::SV_SAVE.get())
                                .xsmall()
                                .disabled(self.soul_memory.busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let content = read.read(cx).value().to_string();
                                    this.save_memory_topic(bot_id.clone(), &topic, content, cx);
                                })),
                        ),
                    ),
            );
        }
        out
    }

    /// Krill's bot schedules, as rows with a switch and Remove, then Add.
    pub(super) fn render_schedules_page(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let saving = self.settings.saving;
        let rows: Vec<Row> = self
            .settings
            .schedules
            .iter()
            .enumerate()
            .map(|(idx, sched)| {
                let ent = cx.entity();
                let (bid_t, sid_t) = (bot.id.clone(), sched.id.clone());
                let (bid_d, sid_d) = (bot.id.clone(), sched.id.clone());
                Row::new(
                    SharedString::from(format!("bot-schedule-{idx}")),
                    Some(ui::icons::CALENDAR_CLOCK_SVG),
                    sched.name.clone(),
                )
                .detail(format!("{} · {}", sched.cron, sched.prompt))
                .end(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(row_switch(
                            p,
                            SharedString::from(format!("bots-sched-toggle-{idx}")),
                            format!("{} ({})", sched.name, text::ENABLE_LABEL.get()),
                            sched.enabled,
                            move |on, _, cx| {
                                let (b, s) = (bid_t.clone(), sid_t.clone());
                                ent.update(cx, |this, cx| {
                                    this.toggle_schedule_enabled(b, &s, on, cx);
                                });
                            },
                        ))
                        .child(
                            Button::new(SharedString::from(format!("bots-sched-del-{idx}")))
                                .ghost()
                                .xsmall()
                                .icon(
                                    Icon::default()
                                        .data(ui::icons::TRASH_SVG)
                                        .size(px(ui::ICON_XS)),
                                )
                                .tooltip(text::SV_REMOVE.get())
                                .accessibility_label(text::SV_REMOVE.get())
                                .disabled(saving)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_schedule(bid_d.clone(), &sid_d, cx);
                                })),
                        ),
                )
            })
            .collect();
        let list = if rows.is_empty() {
            div()
                .px_1()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(text::SCHEDULE_EMPTY.get())
        } else {
            Self::render_inset_group(None, None, rows, None, false, p, cx)
        };
        let add_bot = bot.id.clone();
        let form = div()
            .w_full()
            .rounded(ui::RADIUS_LG)
            .bg(p.foreground.opacity(0.035))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(
                        self.settings
                            .sched_name_input
                            .as_ref()
                            .map(|i| div().flex_1().min_w(px(140.)).child(Input::new(i).small())),
                    )
                    .children(
                        self.settings
                            .sched_cron_input
                            .as_ref()
                            .map(|i| div().w(px(140.)).child(Input::new(i).small())),
                    ),
            )
            .children(
                self.settings
                    .sched_prompt_input
                    .as_ref()
                    .map(|i| Input::new(i).small()),
            )
            .child(
                div().flex().justify_end().child(
                    ui::secondary("bots-sched-add", text::SCHEDULE_ADD_BTN.get())
                        .debug_selector(|| "bots-sched-add".into())
                        .small()
                        .disabled(saving)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_schedule(add_bot.clone(), cx);
                        })),
                ),
            );
        Self::settings_screen("bot-settings-schedules-page")
            .child(list)
            .child(Self::inset_frame(
                Some(Self::group_title(text::SCHEDULE_ADD_BTN.get())),
                None,
                form,
                None,
                p,
            ))
    }
}
