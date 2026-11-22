// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Home's sections under the items: Orca's Automations (`BotUpcomingSection`) and Suggestions
//! (`BotUpNextSection`).

use gpui::{Context, SharedString, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::bots::BotConfig;

use super::home::section_header;
use super::settings_root::{OpenSettingsPage, SettingsPage};
use super::{DetailTab, View};
use crate::i18n::catalog::bots as text;
use crate::ui;

/// An Automations row: Tidy memory, Look around or one of the bot's schedules.
struct AutomationRow {
    key: String,
    icon: IconName,
    title: SharedString,
    on: bool,
    /// The settings page that holds it.
    page: SettingsPage,
}

/// Orca's `botAutomationCardRows` order (Tidy memory, Look around, then automations), paused
/// rows last.
fn automation_rows(bot: &BotConfig) -> Vec<AutomationRow> {
    let mut rows = vec![
        AutomationRow {
            key: "tidy".to_owned(),
            icon: IconName::Moon,
            title: text::HM_TIDY_MEMORY.get().into(),
            on: bot.memory_tidy.enabled,
            page: SettingsPage::Root,
        },
        AutomationRow {
            key: "look".to_owned(),
            icon: IconName::Eye,
            title: text::SV_LOOK_AROUND.get().into(),
            on: bot.look_around.enabled,
            page: SettingsPage::Root,
        },
    ];
    rows.extend(bot.schedules.iter().map(|s| AutomationRow {
        key: format!("schedule-{}", s.id),
        icon: IconName::CalendarClock,
        title: s.name.clone().into(),
        on: s.enabled,
        page: SettingsPage::Agent,
    }));
    rows.sort_by_key(|r| !r.on);
    rows
}

impl View {
    /// Orca's `BotUpcomingSection`: the bot's automations, and how to add one.
    pub(super) fn render_automations(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let open = !self.items.folds.contains("automations");
        let header = section_header(
            "bot-home-automations",
            text::HM_AUTOMATIONS.get(),
            open,
            None,
            p,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if !this.items.folds.remove("automations") {
                this.items.folds.insert("automations");
            }
            cx.notify();
        }));
        let mut section = div()
            .debug_selector(|| "bot-home-automations-section".into())
            .flex()
            .flex_col()
            .gap_0p5()
            .child(header);
        if !open {
            return section;
        }
        for row in automation_rows(bot) {
            let sel = format!("bot-home-automation-{}", row.key);
            let debug = sel.clone();
            let page = row.page;
            section = section.child(
                div()
                    .id(SharedString::from(sel))
                    .debug_selector(move || debug.clone())
                    .role(gpui::Role::Button)
                    .aria_label(if row.on {
                        row.title.clone()
                    } else {
                        format!("{}, {}", row.title, text::HM_PAUSED.get()).into()
                    })
                    .tab_index(0)
                    .focus_visible(|d| d.bg(p.sidebar_accent))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(ui::RADIUS_XS)
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|d| d.bg(p.sidebar_accent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings.page = page;
                        cx.emit(OpenSettingsPage(DetailTab::Settings));
                    }))
                    .child(
                        div()
                            .h(px(20.))
                            .w(px(16.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(!row.on, |d| d.opacity(0.6))
                            .child(
                                Icon::new(row.icon)
                                    .size(px(14.))
                                    .text_color(p.muted_foreground),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_MD)
                            .line_height(px(20.))
                            .text_color(if row.on {
                                p.foreground
                            } else {
                                p.muted_foreground
                            })
                            .child(row.title),
                    ),
            );
        }
        if bot.schedules.is_empty() {
            section = section.child(
                div()
                    .debug_selector(|| "bot-home-automations-empty".into())
                    .py_1()
                    .pr_2()
                    .pl_8()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::HM_AUTOMATIONS_EMPTY.format(&[("name", &bot.name)])),
            );
        }
        section
    }

    /// Orca's `BotUpNextSection` ("Suggestions"): what the last Look around suggests, shown while
    /// it has some. Hovering a row shows Start and Dismiss over its end; a click opens its link.
    pub(super) fn render_suggestions(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let rows: Vec<_> = self
            .soul_memory
            .suggestions
            .iter()
            .filter(|s| s.bot_id == bot.id && !s.dismissed)
            .cloned()
            .collect();
        if rows.is_empty() {
            return None;
        }
        let open = !self.items.folds.contains("suggestions");
        let header = section_header(
            "bot-home-suggestions",
            text::HM_SUGGESTIONS.get(),
            open,
            (!open).then(|| rows.len().to_string()),
            p,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if !this.items.folds.remove("suggestions") {
                this.items.folds.insert("suggestions");
            }
            cx.notify();
        }));
        let mut section = div().flex().flex_col().gap_0p5().child(header);
        if !open {
            return Some(section);
        }
        for s in rows {
            let key = format!("suggestion:{}", s.id);
            let hovered = self.items.hover_uid.as_deref() == Some(key.as_str());
            let sel = format!("bot-home-suggestion-{}", s.id);
            let debug = sel.clone();
            let link = s.link.clone();
            let (start_bot, start_id) = (bot.id.clone(), s.id.clone());
            let (dismiss_bot, dismiss_id) = (bot.id.clone(), s.id.clone());
            let mut row = div()
                .id(SharedString::from(sel))
                .debug_selector(move || debug.clone())
                .role(gpui::Role::Button)
                .aria_label(SharedString::from(s.title.clone()))
                .tab_index(0)
                .relative()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .rounded(ui::RADIUS_XS)
                .px_2()
                .py_1()
                .when(link.is_some(), gpui::Styled::cursor_pointer)
                .hover(|d| d.bg(p.sidebar_accent))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    let now_on = this.items.hover_uid.as_deref() == Some(key.as_str());
                    if *hovered != now_on {
                        this.items.hover_uid = hovered.then(|| key.clone());
                        cx.notify();
                    }
                }))
                .on_click(move |_, _, cx| {
                    if let Some(url) = link.as_deref().filter(|l| l.starts_with("http")) {
                        cx.open_url(url);
                    }
                })
                // The slot stays so every title in the panel starts at one edge.
                .child(div().h(px(20.)).w(px(16.)).flex_none())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(ui::TEXT_MD)
                        .line_height(px(20.))
                        .text_color(p.foreground)
                        .child(s.title.clone()),
                );
            if hovered {
                row = row.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pl_1()
                        .pr(px(6.))
                        .rounded(ui::RADIUS_XS)
                        .bg(p.sidebar_accent)
                        .child(
                            ui::secondary(
                                "bot-home-suggestion-start",
                                text::SUGGESTION_START_BTN.get(),
                            )
                            .xsmall()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.start_suggestion(start_bot.clone(), start_id.clone(), cx);
                                },
                            )),
                        )
                        .child(
                            Button::new("bot-home-suggestion-dismiss")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::X).size(px(14.)))
                                .tooltip(text::SUGGESTION_DISMISS_BTN.get())
                                .accessibility_label(text::SUGGESTION_DISMISS_BTN.get())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.dismiss_suggestion(
                                        dismiss_bot.clone(),
                                        dismiss_id.clone(),
                                        cx,
                                    );
                                })),
                        ),
                );
            }
            section = section.child(row);
        }
        Some(section)
    }
}
