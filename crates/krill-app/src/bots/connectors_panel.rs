// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! The bot page's Connectors tab as Orca draws it (`BotConnectorsPanel` → `BotConnectorsSection`):
//! "Connectors" with Add, the bot's connectors as rows in one card (logo, name, what it is
//! connected as; a click opens the connector's page in the list's place), or, with none, their
//! logos, what they are for and "Add a connector". Add opens "Add to {bot}" (`AddConnectorDialog`).

use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::bots::{BotConfig, ConnectorHealthState, ExternalSource};

use super::View;
use super::connector_logos as logo;
use crate::i18n::catalog::{bot_home as hm, bots as text};
use crate::ui;

const SOURCES: [ExternalSource; 7] = [
    ExternalSource::Slack,
    ExternalSource::GitHub,
    ExternalSource::Linear,
    ExternalSource::Google,
    ExternalSource::Discord,
    ExternalSource::Telegram,
    ExternalSource::Composio,
];

/// A connector's logo in Orca's 32 px tile.
fn tile(source: ExternalSource, size: f32, p: &ui::Palette) -> gpui::Div {
    let icon = match source {
        ExternalSource::Slack => Icon::default().data(logo::SLACK_SVG),
        ExternalSource::GitHub => Icon::default().data(logo::GITHUB_SVG),
        ExternalSource::Linear => Icon::default().data(logo::LINEAR_SVG),
        ExternalSource::Google => Icon::default().data(logo::GOOGLE_SVG),
        ExternalSource::Discord => Icon::default().data(logo::DISCORD_SVG),
        ExternalSource::Telegram => Icon::new(IconName::MessageSquare),
        ExternalSource::Composio => Icon::new(IconName::Plug),
    };
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(ui::RADIUS_SM)
        .bg(p.muted)
        .child(icon.size(px(size / 2.)).text_color(p.foreground))
}

/// What a connector is for, under its name in Add to {bot}.
fn about(source: ExternalSource) -> &'static str {
    match source {
        ExternalSource::Telegram => text::CONN_TELEGRAM_DESC.get(),
        ExternalSource::Slack => text::CONN_SLACK_DESC.get(),
        ExternalSource::Discord => text::CONN_DISCORD_DESC.get(),
        ExternalSource::Google => text::CONN_GOOGLE_DESC.get(),
        ExternalSource::GitHub => text::CONN_GITHUB_DESC.get(),
        ExternalSource::Linear => text::CONN_LINEAR_DESC.get(),
        ExternalSource::Composio => text::CONN_COMPOSIO_DESC.get(),
    }
}

impl View {
    /// The bot's connectors, in Orca's order of its list.
    pub(super) fn added_connectors(bot: &BotConfig) -> Vec<ExternalSource> {
        let on = bot.connectors.enabled_sources();
        SOURCES.into_iter().filter(|s| on.contains(s)).collect()
    }

    pub(super) fn render_connectors_panel(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let body = div()
            .id("bots-tab-connectors-body")
            .debug_selector(|| "bots-tab-connectors-body".into())
            .flex_1()
            .min_h_0()
            .size_full()
            .overflow_y_scroll()
            .px_2()
            .py_3()
            .flex()
            .flex_col()
            .gap_0p5();
        let add = self.render_add_connector(bot, p, window, cx);
        if let Some(source) = self.connectors.page {
            return body
                .child(
                    div().pb_2().child(
                        Button::new("bot-connector-page-back")
                            .debug_selector(|| "bot-connector-page-back".into())
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::ArrowLeft).size(px(14.)))
                            .label(text::CONNECTORS_HEADING.get())
                            .text_color(p.muted_foreground)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.connectors.page = None;
                                cx.notify();
                            })),
                    ),
                )
                .child(self.render_connector_card(bot, source, p, window, cx))
                .children(add);
        }
        let added = Self::added_connectors(bot);
        if added.is_empty() {
            return body
                .child(
                    div()
                        .debug_selector(|| "bot-connectors-empty".into())
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_3()
                        .px_4()
                        .py_8()
                        .text_center()
                        .child(
                            div().flex().gap_2().children(
                                [
                                    ExternalSource::Slack,
                                    ExternalSource::GitHub,
                                    ExternalSource::Linear,
                                    ExternalSource::Google,
                                ]
                                .map(|s| tile(s, 32., p)),
                            ),
                        )
                        .child(
                            div()
                                .max_w(px(256.))
                                .text_size(ui::TEXT_XS)
                                .line_height(px(20.))
                                .text_color(p.muted_foreground)
                                .child(hm::CONNECTORS_EMPTY.format(&[("name", &bot.name)])),
                        )
                        .child(
                            ui::primary("bot-connectors-add-first", hm::ADD_CONNECTOR.get())
                                .small()
                                .icon(Icon::new(IconName::Plus))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.connectors.adding = Some(());
                                    cx.notify();
                                })),
                        ),
                )
                .children(add);
        }
        let mut list = div()
            .debug_selector(|| "bot-connectors-list".into())
            .rounded(px(10.))
            .border_1()
            .border_color(p.border)
            .bg(p.card)
            .p_1()
            .flex()
            .flex_col();
        for source in added {
            list = list.child(self.render_connector_row(source, p, cx));
        }
        body.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .pr_1()
                .pb_1()
                .pl(px(13.))
                .child(
                    div()
                        .text_size(ui::TEXT_SM)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(text::CONNECTORS_HEADING.get()),
                )
                .child(
                    Button::new("bot-connectors-add")
                        .debug_selector(|| "bot-connectors-add".into())
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::Plus).size(px(14.)))
                        .label(hm::ADD.get())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.connectors.adding = Some(());
                            cx.notify();
                        })),
                ),
        )
        .child(list)
        .children(add)
    }

    /// Orca's `BotConnectorPageRow`: logo, name and what it is connected as; its state only when
    /// something needs the owner; a click opens its page.
    fn render_connector_row(
        &self,
        source: ExternalSource,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let status = self
            .connectors
            .bot_statuses
            .iter()
            .find(|s| s.source == source);
        let state = status.map_or(ConnectorHealthState::Disconnected, |s| s.state);
        let description = match (state, status) {
            (ConnectorHealthState::Connected, Some(s)) => s
                .account_label
                .clone()
                .unwrap_or_else(|| text::HEALTH_CONNECTED.get().to_owned()),
            (ConnectorHealthState::Connecting, _) => text::HEALTH_CONNECTING.get().to_owned(),
            (ConnectorHealthState::Error, Some(s)) => s
                .last_error
                .clone()
                .unwrap_or_else(|| text::HEALTH_ERROR.get().to_owned()),
            _ => text::HEALTH_DISCONNECTED.get().to_owned(),
        };
        let trouble = state == ConnectorHealthState::Error;
        let sel = format!("bot-connector-row-{}", source.as_str());
        let debug = sel.clone();
        div()
            .id(SharedString::from(sel))
            .debug_selector(move || debug.clone())
            .role(gpui::Role::Button)
            .aria_label(Self::source_label(source))
            .tab_index(0)
            .flex()
            .items_center()
            .gap_3()
            .rounded(ui::RADIUS_LG)
            .px_2()
            .py_2()
            .cursor_pointer()
            .hover(|d| d.bg(p.accent.opacity(0.4)))
            .child(tile(source, 32., p))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::MEDIUM)
                            .child(Self::source_label(source)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(description),
                    ),
            )
            .when(trouble, |d| {
                d.child(
                    Icon::new(IconName::CircleAlert)
                        .size(px(14.))
                        .text_color(p.destructive),
                )
            })
            .child(
                Icon::new(IconName::ChevronRight)
                    .size(px(16.))
                    .text_color(p.muted_foreground),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.connectors.page = Some(source);
                cx.notify();
            }))
    }

    /// One connector's page: its setup card, as the modal shows it.
    fn render_connector_card(
        &mut self,
        bot: &BotConfig,
        source: ExternalSource,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_connector_inputs(bot, window, cx);
        let c = bot.connectors.clone();
        match source {
            ExternalSource::Telegram => {
                self.render_telegram_card(bot, &c, p, cx).into_any_element()
            }
            ExternalSource::Slack => self
                .render_slack_card(bot, &c, &bot.name, p, cx)
                .into_any_element(),
            ExternalSource::Discord => self.render_discord_card(bot, &c, p, cx).into_any_element(),
            ExternalSource::Google => self.render_google_card(bot, &c, p, cx).into_any_element(),
            ExternalSource::GitHub => self.render_github_card(bot, &c, p, cx).into_any_element(),
            ExternalSource::Linear => self.render_linear_card(bot, &c, p, cx).into_any_element(),
            ExternalSource::Composio => {
                self.render_composio_card(bot, &c, p, cx).into_any_element()
            }
        }
    }

    /// Orca's "Add to {bot}": every connector, a click opens its page to set it up.
    fn render_add_connector(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.connectors.adding?;
        let added = Self::added_connectors(bot);
        let title = hm::ADD_TO.format(&[("bot", &bot.name)]);
        let mut rows = div().flex().flex_col().gap_0p5().p_2();
        for source in SOURCES {
            let on = added.contains(&source);
            let sel = format!("bot-connector-add-{}", source.as_str());
            let debug = sel.clone();
            rows = rows.child(
                div()
                    .id(SharedString::from(sel))
                    .debug_selector(move || debug.clone())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded(ui::RADIUS_LG)
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .hover(|d| d.bg(p.accent))
                    .child(tile(source, 32., p))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(Self::source_label(source)),
                            )
                            .child(
                                div()
                                    .line_clamp(2)
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(about(source)),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(if on { hm::ADDED.get() } else { hm::ADD.get() }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.connectors.adding = None;
                        this.connectors.page = Some(source);
                        cx.notify();
                    })),
            );
        }
        let close = cx.listener(|this, _, _, cx| {
            this.connectors.adding = None;
            cx.notify();
        });
        let dialog = div()
            .id("bot-connector-add-dialog")
            .debug_selector(|| "bot-connector-add-dialog".into())
            .role(gpui::Role::Dialog)
            .aria_label(SharedString::from(title.clone()))
            .w(px(460.))
            .max_h(px(600.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(ui::RADIUS_LG)
            .border_1()
            .border_color(p.border)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .font_family(ui::FONT_UI)
            .shadow_lg()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(p.border)
                    .py(px(14.))
                    .pr(px(14.))
                    .pl_5()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        Button::new("bot-connector-add-close")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::X).size(px(14.)))
                            .accessibility_label(hm::CLOSE.get())
                            .on_click(close),
                    ),
            )
            .child(
                div()
                    .id("bot-connector-add-list")
                    .overflow_y_scroll()
                    .child(rows),
            );
        let backdrop = div()
            .id("bot-connector-add-backdrop")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.connectors.adding = None;
                    cx.notify();
                }),
            )
            .child(dialog);
        Some(ui::modal_backdrop(window.viewport_size(), backdrop).into_any_element())
    }
}
