// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's bot page right sidebar. While the focused chat runs in a bot's folder, the right
//! panel trades Changes, Files, Search and Checks for the bot's own tabs:
//! Home (header, then its items; Bot settings opens over it), Connectors, Chats and Files
//! (the usual file explorer, rooted at the bot folder).

use gpui::{
    AnyElement, Context, Entity, FontWeight, SharedString, Subscription, Window, div, prelude::*,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use krill_app::bots::{DetailTab, SidebarBot};
use krill_app::i18n::catalog::bots as text;
use krill_app::ui;
use krill_proto::bots::{BotId, BotRpcRequest, BotSessionSummary};

use super::Shell;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum BotPanelTab {
    #[default]
    Home,
    Connectors,
    Chats,
    Files,
}

impl BotPanelTab {
    const ALL: [Self; 4] = [Self::Home, Self::Connectors, Self::Chats, Self::Files];

    fn label(self) -> &'static str {
        match self {
            Self::Home => text::BP_HOME.get(),
            Self::Connectors => text::BP_CONNECTORS.get(),
            Self::Chats => text::BP_CHATS.get(),
            Self::Files => text::BP_FILES.get(),
        }
    }

    fn icon(self) -> &'static [u8] {
        match self {
            Self::Home => ui::icons::HOUSE_SVG,
            Self::Connectors => ui::icons::PLUG_SVG,
            Self::Chats => ui::icons::MESSAGE_SQUARE_SVG,
            Self::Files => ui::icons::FILES_SVG,
        }
    }

    const fn selector(self) -> &'static str {
        match self {
            Self::Home => "bot-panel-tab-home",
            Self::Connectors => "bot-panel-tab-connectors",
            Self::Chats => "bot-panel-tab-chats",
            Self::Files => "bot-panel-tab-files",
        }
    }
}

/// Below this panel width only the current tab keeps its name (Orca hides labels when narrow).
const LABELLED_TABS_MIN_WIDTH: f32 = 360.;

/// Bot settings sub-pages, in Orca's order (identity and agent, soul and memory, access).
const SETTINGS_TABS: [(DetailTab, &str); 3] = [
    (DetailTab::Settings, "bot-settings-tab-settings"),
    (DetailTab::SoulMemory, "bot-settings-tab-soul"),
    (DetailTab::ToolsSharing, "bot-settings-tab-tools"),
];

/// Right-sidebar state for bot pages.
#[derive(Default)]
pub(super) struct BotPanel {
    pub tab: BotPanelTab,
    /// Bot settings is open over Home, on this page.
    pub settings: Option<DetailTab>,
    /// The Bots view drawn as the Home, Connectors and settings bodies.
    pub view: Option<Entity<krill_app::bots::View>>,
    /// What `view` was last told to show, so a render doesn't repeat it.
    pub shown: Option<(BotId, DetailTab)>,
    pub chat_search: Option<(Entity<InputState>, Subscription)>,
    pub chat_query: String,
}

/// How a bot chat is named: Home Chat for the Home Chat (krilld lists it first), else its own
/// label (krilld's default label is just the bot's name), else its thread, else Orca's "New chat".
pub(super) fn chat_title(bot: &SidebarBot, s: &BotSessionSummary) -> SharedString {
    if bot
        .home_session()
        .is_some_and(|h| h.session_id == s.session_id)
    {
        return text::SB_HOME_CHAT.get().into();
    }
    let label = s.label.trim();
    if !label.is_empty() && label != bot.name.trim() {
        return label.to_owned().into();
    }
    match &s.link_id {
        Some(link) => link.clone().into(),
        None => text::BP_NEW_CHAT.get().into(),
    }
}

impl Shell {
    /// The bot whose folder the focused chat runs in (Orca's bot page).
    pub(super) fn focused_bot(&self) -> Option<&SidebarBot> {
        let pane = self.layout.focused()?;
        if self.layout.file(pane).is_some() || self.layout.host(pane) != &self.launch_host {
            return None;
        }
        let session = self.layout.session(pane);
        let cwd = self.folder();
        self.sidebar_bots.iter().find(|b| {
            session.is_some_and(|s| b.sessions.iter().any(|x| x.session_id == s))
                || self.bot_folder(b).is_some_and(|f| f == cwd)
        })
    }

    /// Opens Bot settings in the right sidebar when `bot_id` is the bot page on screen.
    pub(super) fn open_bot_settings_in_panel(
        &mut self,
        bot_id: &BotId,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.focused_bot().is_none_or(|b| &b.id != bot_id) {
            return false;
        }
        if self.panel_collapsed {
            self.toggle_panel(cx);
        }
        self.bot_ui.panel.tab = BotPanelTab::Home;
        self.bot_ui.panel.settings = Some(DetailTab::Settings);
        cx.notify();
        true
    }

    /// The Bots view behind the panel bodies, built once per host.
    #[cfg_attr(
        not(test),
        expect(
            clippy::unnecessary_wraps,
            reason = "tests have no daemon without an override"
        )
    )]
    fn bot_panel_view(&mut self, cx: &mut Context<Self>) -> Option<Entity<krill_app::bots::View>> {
        if let Some(view) = &self.bot_ui.panel.view
            && view.read(cx).host() == &self.launch_host
        {
            return Some(view.clone());
        }
        #[cfg(test)]
        let view = {
            let rpc = self.bot_rpc()?;
            cx.new(|cx| krill_app::bots::View::with_rpc(rpc, None, cx).into_panel())
        };
        #[cfg(not(test))]
        let view = {
            let host = self.launch_host.clone();
            let target = self.target_for_host(&host);
            cx.new(|cx| krill_app::bots::View::new_on_host(host, target, None, cx).into_panel())
        };
        Self::subscribe_bots_view(&view, cx);
        self.bot_ui.panel.view = Some(view.clone());
        self.bot_ui.panel.shown = None;
        Some(view)
    }

    /// Keeps the diff panel's own tab bar hidden only while it is a bot page's Files tab.
    pub(super) fn sync_bot_files_chrome(&self, bot_page: bool, cx: &mut Context<Self>) {
        let embedded = bot_page && self.bot_ui.panel.tab == BotPanelTab::Files;
        if self.diff.read(cx).is_embedded() != embedded {
            self.diff.update(cx, |d, cx| d.set_embedded(embedded, cx));
        }
    }

    fn set_bot_panel_tab(&mut self, tab: BotPanelTab, window: &mut Window, cx: &mut Context<Self>) {
        self.bot_ui.panel.tab = tab;
        match tab {
            BotPanelTab::Files => self.diff.update(cx, |d, cx| d.show_files(window, cx)),
            BotPanelTab::Home | BotPanelTab::Connectors | BotPanelTab::Chats => {}
        }
        cx.notify();
    }

    pub(super) fn render_bot_panel(
        &mut self,
        bot: &SidebarBot,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = ui::palette(cx);
        let tab = self.bot_ui.panel.tab;
        if tab == BotPanelTab::Chats {
            self.ensure_bot_chat_search(window, cx);
        }
        let detail = match (tab, self.bot_ui.panel.settings) {
            (BotPanelTab::Home, Some(page)) => Some(page),
            (BotPanelTab::Home, None) => Some(DetailTab::Items),
            (BotPanelTab::Connectors, _) => Some(DetailTab::Connectors),
            _ => None,
        };
        let view = detail.and_then(|page| {
            let view = self.bot_panel_view(cx)?;
            let want = (bot.id.clone(), page);
            if self.bot_ui.panel.shown.as_ref() != Some(&want) {
                view.update(cx, |v, cx| v.show_in_panel(&bot.id, page, cx));
                self.bot_ui.panel.shown = Some(want);
            }
            Some(view)
        });
        let body: AnyElement = match tab {
            BotPanelTab::Files => self.diff.clone().into_any_element(),
            BotPanelTab::Chats => self.render_bot_chats(bot, p, cx).into_any_element(),
            BotPanelTab::Connectors => div().size_full().children(view).into_any_element(),
            BotPanelTab::Home => match self.bot_ui.panel.settings {
                Some(page) => {
                    Self::render_bot_settings_page(bot, page, view, p, cx).into_any_element()
                }
                None => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(self.render_bot_panel_header(bot, p, cx))
                    .child(div().flex_1().min_h_0().children(view))
                    .into_any_element(),
            },
        };
        div()
            .id("bot-panel")
            .debug_selector(|| "bot-panel".into())
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(p.sidebar)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_SM)
            .text_color(p.sidebar_foreground)
            .child(self.render_bot_panel_tabs(bot, width < LABELLED_TABS_MIN_WIDTH, p, cx))
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// Orca's labelled bot tabs, with the needs-you count on Home.
    fn render_bot_panel_tabs(
        &self,
        bot: &SidebarBot,
        compact: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let current = self.bot_ui.panel.tab;
        let mut tabs = div()
            .id("bot-panel-tabs")
            .role(gpui::Role::TabList)
            .aria_label(text::BP_TABS.get())
            .tab_group()
            .flex()
            .items_center()
            .gap_0p5()
            .min_w_0()
            .overflow_x_hidden();
        for tab in BotPanelTab::ALL {
            let active = tab == current;
            let selector = tab.selector();
            let badge = (tab == BotPanelTab::Home && bot.attention > 0).then(|| {
                div()
                    .px_1()
                    .rounded_full()
                    .bg(p.sidebar_accent)
                    .text_size(ui::TEXT_2XS)
                    .text_color(p.sidebar_foreground)
                    .child(bot.attention.to_string())
            });
            tabs = tabs.child(
                div()
                    .id(selector)
                    .debug_selector(move || selector.into())
                    .role(gpui::Role::Tab)
                    .aria_label(tab.label())
                    .aria_selected(active)
                    .tab_index(0)
                    .tab_stop(active)
                    .focus_visible(|d| d.border_1().border_color(p.ring))
                    .relative()
                    .h(px(24.))
                    .px_1p5()
                    .flex_shrink_0()
                    .rounded(ui::RADIUS_XS)
                    .cursor_pointer()
                    .when(active, |d| d.bg(p.sidebar_accent))
                    .hover(|d| d.bg(p.sidebar_accent))
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(ui::TEXT_SM)
                    .text_color(if active {
                        p.sidebar_accent_foreground
                    } else {
                        p.muted_foreground
                    })
                    .child(
                        Icon::default()
                            .data(tab.icon())
                            .size(px(ui::ICON_SM))
                            .text_color(if active {
                                p.sidebar_accent_foreground
                            } else {
                                p.muted_foreground
                            }),
                    )
                    .when(active || !compact, |d| d.child(tab.label()))
                    .children(badge)
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tab.label()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_bot_panel_tab(tab, window, cx);
                    })),
            );
        }
        div()
            .h(ui::TAB_BAR_HEIGHT)
            .px_1p5()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap_1()
            .border_b_1()
            .border_color(p.sidebar_border)
            .child(tabs)
            .child(
                Button::new("bot-panel-collapse")
                    .debug_selector(|| "bot-panel-collapse".into())
                    .ghost()
                    .xsmall()
                    .icon(IconName::PanelRightClose)
                    .tooltip(krill_app::i18n::catalog::diff::COLLAPSE.get())
                    .accessibility_label(krill_app::i18n::catalog::diff::COLLAPSE.get())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_panel(cx))),
            )
    }

    /// Orca's `BotPanelHeader`: face, name and what it is doing, Bot settings and More.
    fn render_bot_panel_header(
        &self,
        bot: &SidebarBot,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (activity, attention) = Self::bot_activity(bot);
        let status = if activity.trim().is_empty() {
            text::BP_IDLE.get().to_owned()
        } else {
            activity
        };
        let paused = bot.paused;
        let pause_id = bot.id.clone();
        let folder = self.bot_folder(bot);
        let this = cx.weak_entity();
        div()
            .id("bot-panel-header")
            .debug_selector(|| "bot-panel-header".into())
            .flex_none()
            .px_2()
            .pt_2()
            .pb_1()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(self.bot_avatar(bot, 36., ui::RADIUS_MD, p))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.sidebar_foreground)
                            .child(bot.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .text_color(if attention {
                                p.sidebar_foreground
                            } else {
                                p.muted_foreground
                            })
                            .child(status),
                    ),
            )
            .child(
                Button::new("bot-panel-settings")
                    .debug_selector(|| "bot-panel-settings".into())
                    .ghost()
                    .xsmall()
                    .icon(IconName::Settings)
                    .tooltip(text::SB_BOT_SETTINGS.get())
                    .accessibility_label(text::SB_BOT_SETTINGS.get())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.bot_ui.panel.settings = Some(DetailTab::Settings);
                        cx.notify();
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
                        let this = this.clone();
                        let id = pause_id.clone();
                        let menu = menu.item(
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
                                this.update(cx, |s, cx| s.set_bot_paused(id.clone(), !paused, cx))
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
    }

    /// Bot settings over Home: back to Home, the three settings pages, Delete bot at the end.
    fn render_bot_settings_page(
        bot: &SidebarBot,
        page: DetailTab,
        view: Option<Entity<krill_app::bots::View>>,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let deleting = bot.clone();
        div()
            .id("bot-settings-page")
            .debug_selector(|| "bot-settings-page".into())
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("bot-settings-back")
                            .debug_selector(|| "bot-settings-back".into())
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronLeft)
                            .tooltip(text::BP_BACK_HOME.get())
                            .accessibility_label(text::BP_BACK_HOME.get())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.bot_ui.panel.settings = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text::BP_SETTINGS_TITLE.get()),
                    ),
            )
            .child(div().flex_none().px_2().pb_1().child(ui::segmented_control(
                p,
                SETTINGS_TABS.into_iter().map(|(tab, id)| {
                    let label = match tab {
                        DetailTab::SoulMemory => text::TAB_SOUL_MEMORY.get(),
                        DetailTab::ToolsSharing => text::TAB_TOOLS_SHARING.get(),
                        _ => text::TAB_SETTINGS.get(),
                    };
                    ui::segment(p, id, label, tab == page)
                        .debug_selector(move || id.into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.bot_ui.panel.settings = Some(tab);
                            cx.notify();
                        }))
                }),
            )))
            .child(div().flex_1().min_h_0().children(view))
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py_2()
                    .border_t_1()
                    .border_color(p.sidebar_border)
                    .child(
                        ui::ghost("bot-settings-delete", text::BP_DELETE.get())
                            .debug_selector(|| "bot-settings-delete".into())
                            .icon(IconName::Delete)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.bot_ui.delete = Some(deleting.clone());
                                cx.notify();
                            })),
                    ),
            )
    }

    /// Orca's Chats tab: search, New chat, then the bot's live chats (newest first after the
    /// Home Chat). Right-click a chat to stop it, run it again or take over its workspace.
    fn render_bot_chats(
        &self,
        bot: &SidebarBot,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let q = self.bot_ui.panel.chat_query.trim().to_lowercase();
        let focused = self
            .layout
            .focused()
            .and_then(|pane| self.layout.session(pane));
        let new_id = bot.id.clone();
        let search = self.bot_ui.panel.chat_search.as_ref().map(|(input, _)| {
            div()
                .flex_1()
                .min_w_0()
                .child(Input::new(input).small().cleanable(true))
        });
        let toolbar = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1p5()
            .px_0p5()
            .pb_2()
            .children(search)
            .child(
                ui::secondary("bot-chats-new", text::BP_NEW_CHAT.get())
                    .debug_selector(|| "bot-chats-new".into())
                    .icon(IconName::Plus)
                    .disabled(bot.paused || self.bot_ui.opening.contains(&bot.id))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_bot_session(new_id.clone(), None, cx);
                    })),
            );
        let now = super::bot_section::now_secs();
        let mut list = div()
            .id("bot-chats-list")
            .debug_selector(|| "bot-chats-list".into())
            .role(gpui::Role::List)
            .flex()
            .flex_col()
            .gap_0p5();
        let mut shown = 0;
        for s in &bot.sessions {
            let title = chat_title(bot, s);
            if !q.is_empty() && !title.to_lowercase().contains(&q) {
                continue;
            }
            shown += 1;
            let active = focused == Some(s.session_id);
            let session = s.session_id;
            let live = s.live;
            let id = SharedString::from(format!("bot-panel-chat-{}", s.session_id.0));
            let open_id = bot.id.clone();
            let this = cx.weak_entity();
            let menu_bot = bot.id.clone();
            list = list.child(
                div()
                    .id(id.clone())
                    .debug_selector(move || id.to_string())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .focus_visible(|d| d.border_1().border_color(p.ring))
                    .aria_label(title.clone())
                    .h(px(28.))
                    .px_1p5()
                    .rounded(ui::RADIUS_SM)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .cursor_pointer()
                    .when(active, |d| d.bg(p.sidebar_accent))
                    .when(!active, |d| {
                        d.hover(|d| d.bg(p.sidebar_accent.opacity(0.6)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_bot_session(&open_id, session, cx);
                    }))
                    .child(ui::icons::svg_icon(
                        if s.link_id.is_none() {
                            ui::icons::BOT_HOME_SVG
                        } else {
                            ui::icons::MESSAGE_SQUARE_SVG
                        },
                        ui::ICON_XS,
                        p.muted_foreground,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(p.sidebar_foreground)
                            .child(title),
                    )
                    .when(s.working, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::CHAT_SESSION_WORKING.get()),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(krill_app::jump::format_age_secs(
                                s.created_at_ms / 1000,
                                now,
                            )),
                    )
                    .context_menu(move |menu, _, _| {
                        let action = |req: fn(BotId, krill_proto::SessionId) -> BotRpcRequest| {
                            let this = this.clone();
                            let bot = menu_bot.clone();
                            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                                this.update(cx, |s, cx| {
                                    s.run_bot_chat_action(req(bot.clone(), session), cx);
                                })
                                .ok();
                            }
                        };
                        let menu = if live {
                            menu.item(
                                PopupMenuItem::new(text::CHAT_STOP_BTN.get())
                                    .icon(IconName::CircleX)
                                    .on_click(action(|bot_id, session_id| {
                                        BotRpcRequest::StopBotSession { bot_id, session_id }
                                    })),
                            )
                        } else {
                            menu
                        };
                        menu.item(
                            PopupMenuItem::new(text::CHAT_RUN_AGAIN_BTN.get())
                                .icon(IconName::Redo)
                                .on_click(action(|bot_id, session_id| {
                                    BotRpcRequest::SendBotRunAgain { bot_id, session_id }
                                })),
                        )
                        .item(
                            PopupMenuItem::new(text::CHAT_TAKEOVER_BTN.get())
                                .icon(IconName::ArrowRight)
                                .on_click(action(|bot_id, session_id| {
                                    BotRpcRequest::TakeOverWorkspace { bot_id, session_id }
                                })),
                        )
                    }),
            );
        }
        if shown == 0 {
            list = list.child(
                div()
                    .px_1p5()
                    .py_2()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(if q.is_empty() {
                        text::BP_NO_CHATS.get()
                    } else {
                        text::BP_NO_CHAT_MATCH.get()
                    }),
            );
        }
        div()
            .id("bot-chats")
            .debug_selector(|| "bot-chats".into())
            .size_full()
            .overflow_y_scroll()
            .px_2()
            .pt(px(10.))
            .pb_3()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(list)
    }

    fn ensure_bot_chat_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bot_ui.panel.chat_search.is_some() {
            return;
        }
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder(text::BP_SEARCH_CHATS.get()));
        let sub = cx.subscribe(&input, |this, input, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.bot_ui.panel.chat_query = input.read(cx).value().to_string();
                cx.notify();
            }
        });
        self.bot_ui.panel.chat_search = Some((input, sub));
    }

    /// Stop, run again or take over: one krilld call, then the sidebar poll catches up.
    fn run_bot_chat_action(&mut self, request: BotRpcRequest, cx: &mut Context<Self>) {
        let Some(rpc) = self.bot_rpc() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { krill_app::bots::rpc::call_checked(&rpc, &request) })
                .await;
            this.update(cx, |this, cx| {
                if let Err(err) = res {
                    this.notice = Some(err);
                }
                this.refresh_bots_badge(cx);
            })
            .ok();
        })
        .detach();
    }
}
