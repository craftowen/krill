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
use krill_app::hosts::HostId;
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

    fn icon(self) -> gpui_kit_assets::IconName {
        match self {
            Self::Home => gpui_kit_assets::IconName::House,
            Self::Connectors => gpui_kit_assets::IconName::Plug,
            Self::Chats => gpui_kit_assets::IconName::MessagesSquare,
            Self::Files => gpui_kit_assets::IconName::Files,
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

/// At or below this width the header says the short "Next 8:00 PM" (Orca `@max-[284px]`).
const NARROW_HEADER_WIDTH: f32 = 284.;

/// Right-sidebar state for bot pages.
#[derive(Default)]
pub(super) struct BotPanel {
    pub tab: BotPanelTab,
    /// Bot settings is open over Home, on this page.
    pub settings: Option<DetailTab>,
    /// The Bots view drawn as the Home, Connectors and settings bodies.
    pub view: Option<Entity<krill_app::bots::View>>,
    /// What `view` was last told to show, so a render doesn't repeat it.
    pub shown: Option<(HostId, BotId, DetailTab)>,
    /// The bot list changed since `view` last loaded it.
    pub stale: bool,
    pub chat_search: Option<(Entity<InputState>, Subscription)>,
    pub chat_search_lang: Option<krill_app::i18n::Language>,
    pub chat_query: String,
    /// Chats the user explicitly folded (chats with workers start open).
    pub chat_folded: std::collections::BTreeSet<(krill_app::hosts::HostId, krill_proto::SessionId)>,
    /// What the header last drew from `view`: its filter, whether an item is open, the
    /// automations' line ("Idle · next 8:00 PM · Look around", with its narrow form), the
    /// settings subpage crumb, and the group panel filter state/counts.
    pub home_chrome: HomeChrome,
    /// A draft for this bot's Home Chat composer, put in once its chat is on screen.
    pub pending_draft: Option<(HostId, BotId, String)>,
}

#[derive(Default, PartialEq, Eq)]
pub(super) struct HomeChrome {
    pub filter: Option<krill_app::bots::HomeFilter>,
    pub item_open: bool,
    pub quiet_line: Option<(String, Option<String>, bool)>,
    pub settings_subpage: Option<&'static str>,
    pub group_filter: krill_app::bots::GroupItemsFilter,
    pub group_filter_counts: (Vec<(BotId, usize)>, usize),
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

fn same_bot_folder(bot_dir: &std::path::Path, candidate: &std::path::Path) -> bool {
    if krill_proto::path::equivalent(bot_dir, candidate) {
        return true;
    }
    matches!(
        (std::fs::canonicalize(bot_dir), std::fs::canonicalize(candidate)),
        (Ok(a), Ok(b)) if krill_proto::path::equivalent(&a, &b)
    )
}

fn path_within_bot_folder(file: &std::path::Path, bot_dir: &std::path::Path) -> bool {
    if file.starts_with(bot_dir) {
        return true;
    }
    matches!(
        (std::fs::canonicalize(file), std::fs::canonicalize(bot_dir)),
        (Ok(f), Ok(b)) if f.starts_with(&b)
    )
}

impl Shell {
    /// The bot owning `pane`: its explicit split launcher binding, one of its live chats,
    /// a session/terminal running in its folder, or a file inside its folder.
    pub(super) fn bot_for_pane(&self, pane: krill_app::layout::PaneId) -> Option<&SidebarBot> {
        let pane_host = self.layout.host(pane);
        if let Some(bot_id) = self
            .split_launchers
            .get(&pane)
            .and_then(|l| l.bot_id.as_ref())
        {
            return self
                .sidebar_bots
                .iter()
                .find(|b| &b.host == pane_host && &b.id == bot_id);
        }
        if let Some(file) = self.layout.file(pane) {
            return self.sidebar_bots.iter().find(|b| {
                &b.host == pane_host
                    && self
                        .bot_folder(b)
                        .is_some_and(|f| path_within_bot_folder(file, &f))
            });
        }
        let session = self.layout.session(pane);
        // Session ownership takes precedence over `launch.cwd`: a newly opened bot chat tab may
        // briefly carry the previous tab's inherited `cwd`, and a bot may run in a shared
        // workspace, so another bot earlier in `sidebar_bots` must never steal the right panel.
        if let Some(session_id) = session
            && let Some(bot) = self.sidebar_bots.iter().find(|b| {
                &b.host == pane_host && b.sessions.iter().any(|x| x.session_id == session_id)
            })
        {
            return Some(bot);
        }
        let cwd = {
            use krill_proto::os::ffi::OsStringExt as _;
            std::path::PathBuf::from(std::ffi::OsString::from_vec(self.launch_for(pane).cwd))
        };
        self.sidebar_bots.iter().find(|b| {
            &b.host == pane_host
                && self
                    .bot_folder(b)
                    .is_some_and(|f| same_bot_folder(&f, &cwd))
        })
    }

    /// The bot whose page is on screen: the focused chat is one of its chats or runs in its
    /// folder, or the focused file is in its folder (Orca's bot workspace).
    pub(super) fn focused_bot(&self) -> Option<&SidebarBot> {
        let pane = self.layout.focused()?;
        self.bot_for_pane(pane)
    }

    /// Opens Bot settings in the right sidebar when `(host, bot_id)` is the bot page on screen.
    pub(super) fn open_bot_settings_in_panel_on(
        &mut self,
        host: &HostId,
        bot_id: &BotId,
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .focused_bot()
            .is_none_or(|b| &b.host != host || &b.id != bot_id)
        {
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

    /// The Bots view behind the panel bodies on `launch_host`.
    #[cfg(test)]
    pub(super) fn bot_panel_view(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<Entity<krill_app::bots::View>> {
        let host = self.launch_host.clone();
        self.bot_panel_view_on(&host, cx)
    }

    /// The Bots view behind the panel bodies, built once per host (once its connection is up).
    pub(super) fn bot_panel_view_on(
        &mut self,
        host: &HostId,
        cx: &mut Context<Self>,
    ) -> Option<Entity<krill_app::bots::View>> {
        if let Some(view) = &self.bot_ui.panel.view
            && view.read(cx).host() == host
        {
            return Some(view.clone());
        }
        #[cfg(test)]
        let view = {
            let rpc = self.bot_rpc_on(host)?;
            let host_owned = host.clone();
            cx.new(|cx| {
                krill_app::bots::View::with_rpc_on_host(host_owned, rpc, None, cx).into_panel()
            })
        };
        #[cfg(not(test))]
        let view = {
            let host = host.clone();
            let target = self.target_for_host(&host);
            // A remote host still connecting has no target yet; don't fall back to this Mac.
            if !host.is_local() && target.is_none() {
                return None;
            }
            cx.new(|cx| krill_app::bots::View::new_on_host(host, target, None, cx).into_panel())
        };
        Self::subscribe_bots_view(&view, false, cx);
        // The header (drawn here) shows the view's filter and hides over an open item: redraw
        // when those change, not on every change inside the view.
        cx.subscribe(
            &view,
            |this, view, ev: &krill_app::bots::DraftInHomeChat, cx| {
                let host = view.read(cx).host().clone();
                this.bot_ui.panel.pending_draft =
                    Some((host.clone(), ev.bot_id.clone(), ev.text.clone()));
                this.open_bot_home_on(&host, ev.bot_id.clone(), None, cx);
            },
        )
        .detach();
        cx.observe(&view, |this, view, cx| {
            let v = view.read(cx);
            let chrome = HomeChrome {
                filter: v.home_filter(),
                item_open: v.home_item_open(),
                quiet_line: v.home_quiet_line(),
                settings_subpage: v.settings_subpage(),
                group_filter: v.group_items_filter(),
                group_filter_counts: v.group_filter_counts(),
            };
            if this.bot_ui.panel.home_chrome != chrome {
                this.bot_ui.panel.home_chrome = chrome;
                cx.notify();
            }
        })
        .detach();
        self.bot_ui.panel.view = Some(view.clone());
        self.bot_ui.panel.shown = None;
        self.bot_ui.panel.stale = false;
        Some(view)
    }

    /// While a bot page shows Files, the diff panel is its file explorer and nothing else:
    /// its own tab bar hidden and its tab held on Files.
    pub(super) fn sync_bot_files_chrome(&self, bot_page: bool, cx: &mut Context<Self>) {
        let embedded = bot_page && self.bot_ui.panel.tab == BotPanelTab::Files;
        let off_files = self.diff.read(cx).active_tab() != krill_app::diff::PanelTab::Files;
        if self.diff.read(cx).is_embedded() != embedded || (embedded && off_files) {
            self.diff.update(cx, |d, cx| {
                d.set_embedded(embedded, cx);
                if embedded && off_files {
                    d.set_tab(krill_app::diff::PanelTab::Files, cx);
                    d.ensure_explorer_loaded_async(cx);
                }
            });
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

    /// Puts a pending draft in the bot's Home Chat composer once that chat is the focused pane.
    fn apply_pending_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((host, bot_id, _)) = &self.bot_ui.panel.pending_draft else {
            return;
        };
        let Some(pane) = self.layout.focused() else {
            return;
        };
        if self.layout.host(pane) != host {
            return;
        }
        let home = self
            .sidebar_bots
            .iter()
            .find(|b| &b.host == host && &b.id == bot_id)
            .and_then(SidebarBot::home_session)
            .map(|h| h.session_id);
        if home.is_none() || self.layout.session(pane) != home {
            return;
        }
        let Some(key) = self.chat_key(pane) else {
            return;
        };
        let Some((_, _, draft)) = self.bot_ui.panel.pending_draft.take() else {
            return;
        };
        let input = self.ensure_chat_input(key, window, cx);
        input.update(cx, |state, cx| {
            state.set_value(draft, window, cx);
            state.focus(window, cx);
        });
    }

    pub(super) fn render_bot_panel(
        &mut self,
        bot: &SidebarBot,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = ui::palette(cx);
        self.apply_pending_draft(window, cx);
        let tab = self.bot_ui.panel.tab;
        if tab == BotPanelTab::Chats {
            self.ensure_bot_chat_search(window, cx);
        }
        let host = bot.host.clone();
        let detail = match (tab, self.bot_ui.panel.settings) {
            (BotPanelTab::Home, Some(page)) => Some(page),
            (BotPanelTab::Home, None) => Some(DetailTab::Items),
            (BotPanelTab::Connectors, _) => Some(DetailTab::Connectors),
            _ => None,
        };
        let view = detail.and_then(|page| {
            let view = self.bot_panel_view_on(&host, cx)?;
            if std::mem::take(&mut self.bot_ui.panel.stale) {
                view.update(cx, krill_app::bots::View::reload);
            }
            // The bot's chats waiting on the owner are Home's asks.
            let states: std::collections::BTreeMap<_, _> = bot
                .sessions
                .iter()
                .filter_map(|s| {
                    self.sidebar
                        .read(cx)
                        .session_on(&host, s.session_id)
                        .map(|x| (s.session_id, x.status.state))
                })
                .collect();
            view.update(cx, |v, cx| v.set_session_states(states, cx));
            let want = (host.clone(), bot.id.clone(), page);
            if self.bot_ui.panel.shown.as_ref() != Some(&want) {
                // Held back by unsaved Soul edits: ask again next time instead of showing the
                // other bot under this one's page.
                if view.update(cx, |v, cx| v.show_in_panel(&bot.id, page, cx)) {
                    self.bot_ui.panel.shown = Some(want);
                }
            }
            Some(view)
        });
        // Switching bots while on the Chats tab still needs `view` synced so `chat_workers(&bot.id)`
        // reflects the focused bot rather than a previous bot or group panel.
        if tab == BotPanelTab::Chats
            && let Some(view) = self.bot_panel_view_on(&host, cx)
        {
            if std::mem::take(&mut self.bot_ui.panel.stale) {
                view.update(cx, krill_app::bots::View::reload);
            }
            let same_bot = self
                .bot_ui
                .panel
                .shown
                .as_ref()
                .is_some_and(|(h, id, _)| h == &host && id == &bot.id);
            if !same_bot && view.update(cx, |v, cx| v.show_in_panel(&bot.id, DetailTab::Items, cx))
            {
                self.bot_ui.panel.shown = Some((host, bot.id.clone(), DetailTab::Items));
            }
        }
        let body: AnyElement = match tab {
            BotPanelTab::Files => self.diff.clone().into_any_element(),
            BotPanelTab::Chats => self.render_bot_chats(bot, p, cx).into_any_element(),
            BotPanelTab::Connectors => div().size_full().children(view).into_any_element(),
            BotPanelTab::Home => {
                if let Some(page) = self.bot_ui.panel.settings {
                    Self::render_bot_settings_page(bot, page, view, p, cx).into_any_element()
                } else {
                    // An open item takes Home's place, header and all (Orca's item pane).
                    let item_open = view.as_ref().is_some_and(|v| v.read(cx).home_item_open());
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .when(!item_open, |d| {
                            d.child(self.render_bot_panel_header(
                                bot,
                                view.as_ref(),
                                width <= NARROW_HEADER_WIDTH,
                                p,
                                cx,
                            ))
                        })
                        .child(div().flex_1().min_h_0().children(view))
                        .into_any_element()
                }
            }
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
            .child(self.render_bot_panel_tabs(bot, p, cx))
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// Orca's labelled bot tabs (`activity-bar-buttons.tsx` + `BotActivityPanel.tsx`),
    /// always showing icon + text label on all 4 tabs, with the white needs-you circle on Home.
    fn render_bot_panel_tabs(
        &self,
        bot: &SidebarBot,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let current = self.bot_ui.panel.tab;
        let mut tabs = div()
            .id("bot-panel-tabs")
            .role(gpui::Role::TabList)
            .aria_label(text::BP_TABS.get())
            .tab_group()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_0p5()
            .overflow_x_hidden();
        for tab in BotPanelTab::ALL {
            let active = tab == current;
            let selector = tab.selector();
            // Orca's count pill: 16px filled white circle with dark count text.
            let badge = (tab == BotPanelTab::Home && bot.attention > 0).then(|| {
                div()
                    .debug_selector(|| "bot-panel-home-badge".into())
                    .h(px(16.))
                    .min_w(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_1()
                    .rounded_full()
                    .bg(p.foreground)
                    .text_size(ui::TEXT_3XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.background)
                    .child(bot.attention.to_string())
            });
            let fg = if active {
                p.foreground
            } else {
                p.muted_foreground
            };
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
                    .h(px(26.))
                    .px_1p5()
                    .min_w_0()
                    .flex_shrink_1()
                    .rounded(ui::RADIUS_SM)
                    .cursor_pointer()
                    .when(active, |d| d.bg(p.foreground.opacity(0.10)))
                    .when(!active, |d| {
                        d.hover(|d| d.bg(p.foreground.opacity(0.05)).text_color(p.foreground))
                    })
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(fg)
                    .child(
                        Icon::new(tab.icon())
                            .size(px(ui::ICON_SM))
                            .flex_none()
                            .text_color(fg),
                    )
                    .child(div().min_w_0().truncate().child(tab.label()))
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

    /// Orca's `BotPanelHeader`: face (with its state in the corner), name and what it is doing,
    /// the Items filter, Bot settings and More.
    fn render_bot_panel_header(
        &self,
        bot: &SidebarBot,
        view: Option<&Entity<krill_app::bots::View>>,
        narrow: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (activity, attention) = Self::bot_activity(bot);
        // Orca says what its automations do while nothing waits or runs ("Running …", "Idle ·
        // next 8:00 PM · Look around"); the bot's job is the sidebar's line only.
        let quiet = !bot.paused && bot.attention == 0 && bot.working() == 0;
        let quiet_line = self.bot_ui.panel.home_chrome.quiet_line.clone();
        let automation_running = quiet_line.as_ref().is_some_and(|q| q.2);
        let status = if quiet || activity.trim().is_empty() {
            match quiet_line {
                Some((_, Some(short), _)) if narrow => short,
                Some((line, ..)) => line,
                None => text::BP_IDLE.get().to_owned(),
            }
        } else {
            activity
        };
        let state = if bot.attention > 0 {
            krill_proto::control::AgentState::Waiting
        } else if bot.working() > 0 || (quiet && automation_running) {
            krill_proto::control::AgentState::Working
        } else {
            krill_proto::control::AgentState::Exited
        };
        let face = div()
            .relative()
            .flex_none()
            .child(self.bot_avatar(bot, 36., px(9.), p))
            .when(!bot.paused, |d| {
                d.child(
                    div()
                        .debug_selector(|| "bot-panel-header-state".into())
                        .absolute()
                        .bottom(px(-4.))
                        .right(px(-4.))
                        .size(px(14.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(p.sidebar)
                        .child(ui::status::status_glyph(
                            state,
                            false,
                            ui::status::Size::Sm,
                            cx,
                        )),
                )
            });
        let filter = view.and_then(|v| {
            let f = v.read(cx).home_filter()?;
            let v = v.clone();
            Some(
                div()
                    .relative()
                    .flex_none()
                    .child(
                        Button::new("bot-panel-filter")
                            .debug_selector(|| "bot-panel-filter".into())
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(gpui_kit_assets::IconName::ListFilter))
                            .tooltip(text::HM_FILTER_LABEL.get())
                            .accessibility_label(text::HM_FILTER_LABEL.get())
                            .dropdown_menu(move |menu, _, _| {
                                let v = v.clone();
                                let count = f.needs_you.to_string();
                                menu.item(
                                    PopupMenuItem::element(move |_, cx| {
                                        let p = ui::palette(cx);
                                        div()
                                            .w_full()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .child(text::HM_FILTER_NEEDS_YOU.get()),
                                            )
                                            .child(
                                                div()
                                                    .text_size(ui::TEXT_XS)
                                                    .text_color(p.muted_foreground)
                                                    .child(count.clone()),
                                            )
                                    })
                                    .checked(f.needs_you_only)
                                    .on_click(
                                        move |_, _, cx| {
                                            v.update(cx, |v, cx| {
                                                v.set_needs_you_only(!f.needs_you_only, cx);
                                            });
                                        },
                                    ),
                                )
                            }),
                    )
                    // Orca's dot on the icon while a filter is on.
                    .when(f.needs_you_only, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(4.))
                                .right(px(4.))
                                .size(px(6.))
                                .rounded_full()
                                .bg(p.foreground),
                        )
                    }),
            )
        });
        let paused = bot.paused;
        let pause_host = bot.host.clone();
        let pause_id = bot.id.clone();
        let folder = self.bot_folder(bot);
        let this = cx.weak_entity();
        let share = view.cloned().map(|v| (v, bot.id.clone()));
        div()
            .id("bot-panel-header")
            .debug_selector(|| "bot-panel-header".into())
            .flex_none()
            // Orca's panel padding (pl-3.5 pr-2 pt-3) plus the header's own (px-2 pt-1 pb-1).
            .pl(px(22.))
            .pr(px(16.))
            .pt(px(16.))
            .pb_1()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(face)
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
            .children(filter)
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
                        let host = pause_host.clone();
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
                                this.update(cx, |s, cx| {
                                    s.set_bot_paused_on(&host, id.clone(), !paused, cx);
                                })
                                .ok();
                            }),
                        );
                        // Orca's Share bot… publishes a link through its cloud; Krill shares the
                        // bot as its package file.
                        let menu = match share.clone() {
                            Some((view, id)) => menu.item(
                                PopupMenuItem::new(text::BP_SHARE_BOT.get())
                                    .icon(Icon::new(gpui_kit_assets::IconName::Share2))
                                    .on_click(move |_, _, cx| {
                                        view.update(cx, |v, cx| v.export_bot_zip(id.clone(), cx));
                                    }),
                            ),
                            None => menu,
                        };
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
    /// A settings row opened another page: Connectors is its own tab, the rest open over Home.
    pub(super) fn open_bot_settings_page(&mut self, page: DetailTab, cx: &mut Context<Self>) {
        if page == DetailTab::Connectors {
            self.bot_ui.panel.tab = BotPanelTab::Connectors;
            self.bot_ui.panel.settings = None;
        } else {
            self.bot_ui.panel.tab = BotPanelTab::Home;
            self.bot_ui.panel.settings = Some(page);
        }
        cx.notify();
    }

    /// Orca's settings view crumb: `< {bot} / Settings` over the list, `< {bot} / Settings /
    /// {page}` over a page it opened. Back climbs one level; the bot's name leaves Settings.
    fn render_bot_settings_page(
        bot: &SidebarBot,
        page: DetailTab,
        view: Option<Entity<krill_app::bots::View>>,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let subpage = match page {
            DetailTab::Settings => view.as_ref().and_then(|v| v.read(cx).settings_subpage()),
            DetailTab::SoulMemory => Some(text::SV_SOUL_MEMORY.get()),
            DetailTab::ToolsSharing => Some(text::TAB_TOOLS_SHARING.get()),
            _ => None,
        };
        let crumb = |id: &'static str, label: SharedString| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .min_w_0()
                .truncate()
                .px_1()
                .rounded(ui::RADIUS_XS)
                .text_color(p.muted_foreground)
                .cursor_pointer()
                .hover(|d| d.text_color(p.sidebar_foreground))
                .child(label)
        };
        let to_root = view.clone();
        let back_view = view.clone();
        let back_label = if subpage.is_some() {
            text::SV_BACK_SETTINGS.get()
        } else {
            text::BP_BACK_HOME.get()
        };
        let sep = || div().flex_none().text_color(p.muted_foreground).child("/");
        div()
            .id("bot-settings-page")
            .debug_selector(|| "bot-settings-page".into())
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(44.))
                    .pl(px(6.))
                    .pr(px(10.))
                    .border_b_1()
                    .border_color(p.border)
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(ui::TEXT_MD)
                    .child(
                        Button::new("bot-settings-back")
                            .debug_selector(|| "bot-settings-back".into())
                            .ghost()
                            .small()
                            .icon(IconName::ChevronLeft)
                            .tooltip(back_label)
                            .accessibility_label(back_label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                match (page, subpage) {
                                    (DetailTab::Settings, Some(_)) => {
                                        if let Some(v) = &back_view {
                                            v.update(
                                                cx,
                                                krill_app::bots::View::close_settings_subpage,
                                            );
                                        }
                                    }
                                    (DetailTab::Settings, None) => {
                                        this.bot_ui.panel.settings = None;
                                    }
                                    _ => this.bot_ui.panel.settings = Some(DetailTab::Settings),
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        crumb("bot-settings-crumb-bot", bot.name.clone().into()).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.bot_ui.panel.settings = None;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(sep())
                    .map(|d| match subpage {
                        Some(sub) => d
                            .child(
                                crumb(
                                    "bot-settings-crumb-settings",
                                    text::SV_SETTINGS.get().into(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        match (&to_root, page) {
                                            (Some(v), DetailTab::Settings) => v.update(
                                                cx,
                                                krill_app::bots::View::close_settings_subpage,
                                            ),
                                            _ => {
                                                this.bot_ui.panel.settings =
                                                    Some(DetailTab::Settings);
                                            }
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(sep())
                            .child(
                                div()
                                    .flex_none()
                                    .whitespace_nowrap()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.sidebar_foreground)
                                    .child(sub),
                            ),
                        None => d.child(
                            div()
                                .flex_none()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.sidebar_foreground)
                                .child(text::SV_SETTINGS.get()),
                        ),
                    }),
            )
            .child(div().flex_1().min_h_0().flex().flex_col().children(view))
    }

    /// Orca's Chats tab: search, New chat, then the bot's live chats (newest first after the
    /// Home Chat). Right-click a chat to stop it, run it again or take over its workspace.
    /// Orca's `BotChatTreeGroup` row: its source icon (the fold chevron on hover while it has
    /// workers), the title, its age (⋯ in its place on hover), then a preview line ending in
    /// what it is doing and "N running".
    #[expect(
        clippy::too_many_arguments,
        reason = "a render helper over the chat's shared state"
    )]
    fn render_bot_chat_row(
        &self,
        bot: &SidebarBot,
        s: &BotSessionSummary,
        title: &SharedString,
        state: Option<krill_proto::control::AgentState>,
        running: usize,
        foldable: bool,
        open: bool,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use krill_proto::control::AgentState;
        let focused = self
            .layout
            .focused()
            .and_then(|pane| Some((self.layout.host(pane), self.layout.session(pane)?)));
        let active = focused == Some((&bot.host, s.session_id));
        let session = s.session_id;
        let live = s.live;
        let home = s.link_id.is_none();
        let id = SharedString::from(format!("bot-panel-chat-{}", s.session_id.0));
        let group: SharedString = format!("bot-chat-{}", s.session_id.0).into();
        let open_host = bot.host.clone();
        let fold_host = bot.host.clone();
        let open_id = bot.id.clone();
        let menu = {
            let this = cx.weak_entity();
            let menu_host = bot.host.clone();
            let menu_bot = bot.id.clone();
            move |menu: gpui_component::menu::PopupMenu| {
                let action = |req: fn(BotId, krill_proto::SessionId) -> BotRpcRequest| {
                    let this = this.clone();
                    let host = menu_host.clone();
                    let bot = menu_bot.clone();
                    move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                        this.update(cx, |s, cx| {
                            s.run_bot_chat_action_on(&host, req(bot.clone(), session), cx);
                        })
                        .ok();
                    }
                };
                let menu = if live {
                    menu.item(
                        PopupMenuItem::new(text::CHAT_STOP_BTN.get())
                            .icon(IconName::CircleX)
                            .on_click(action(|bot_id, session_id| BotRpcRequest::StopBotSession {
                                bot_id,
                                session_id,
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
            }
        };
        let menu2 = menu.clone();
        let (live_text, live_strong) = match state {
            _ if s.working => (Some(text::BP_REPLYING.get()), false),
            Some(AgentState::Waiting | AgentState::Blocked) => (Some(text::BP_WAITING.get()), true),
            _ => (None, false),
        };
        let preview: SharedString = if home {
            text::BP_SAY_HI.format(&[("name", &bot.name)]).into()
        } else {
            s.link_id.clone().unwrap_or_default().into()
        };
        let age = ui::format_short_time_ago_secs(s.created_at_ms / 1000, now);
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .group(group.clone())
            .role(gpui::Role::Button)
            .tab_index(0)
            .focus_visible(|d| d.border_1().border_color(p.ring))
            .aria_label(title.clone())
            .aria_expanded(open)
            .relative()
            .w_full()
            .min_w_0()
            .flex()
            .items_start()
            .gap_1()
            .rounded(ui::RADIUS_SM)
            .py_1p5()
            .px_2()
            .cursor_pointer()
            .when(active, |d| d.bg(p.sidebar_accent))
            .hover(|d| d.bg(p.sidebar_accent))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_bot_session_on(&open_host, &open_id, session, cx);
            }))
            .context_menu(move |m, _, _| menu(m))
            .child(
                div()
                    .relative()
                    .h(px(20.))
                    .w(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .when(open, gpui::Styled::invisible)
                            .when(foldable, |d| {
                                d.group_hover(group.clone(), gpui::Styled::invisible)
                            })
                            .child(ui::icons::svg_icon(
                                if home {
                                    ui::icons::BOT_HOME_SVG
                                } else {
                                    ui::icons::MESSAGE_SQUARE_SVG
                                },
                                ui::ICON_XS,
                                p.muted_foreground,
                            )),
                    )
                    .when(foldable, |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!(
                                    "bot-panel-chat-fold-{}",
                                    session.0
                                )))
                                .debug_selector(move || {
                                    format!("bot-panel-chat-fold-{}", session.0)
                                })
                                .role(gpui::Role::Button)
                                .aria_expanded(open)
                                .aria_label(if open {
                                    text::BP_FOLD.format(&[("name", title)])
                                } else {
                                    text::BP_UNFOLD.format(&[("name", title)])
                                })
                                .absolute()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(ui::RADIUS_XS)
                                .hover(|d| d.bg(p.sidebar_accent.opacity(0.7)))
                                .when(!open, |d| {
                                    d.invisible()
                                        .group_hover(group.clone(), gpui::Styled::visible)
                                })
                                .child(
                                    Icon::new(if open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .size(px(14.))
                                    .text_color(p.muted_foreground),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let key = (fold_host.clone(), session);
                                    if !this.bot_ui.panel.chat_folded.remove(&key) {
                                        this.bot_ui.panel.chat_folded.insert(key);
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui::TEXT_MD)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.sidebar_foreground)
                                    .child(title.clone()),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .relative()
                                    .h(px(20.))
                                    .min_w(px(24.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_2XS)
                                            .text_color(p.muted_foreground)
                                            .group_hover(group.clone(), gpui::Styled::invisible)
                                            .child(age),
                                    )
                                    .child(
                                        div()
                                            .absolute()
                                            .right_0()
                                            .invisible()
                                            .group_hover(group.clone(), gpui::Styled::visible)
                                            .child(
                                                Button::new(SharedString::from(format!(
                                                    "bot-chat-more-{}",
                                                    session.0
                                                )))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Ellipsis)
                                                .accessibility_label(text::BP_MORE.get())
                                                .dropdown_menu(move |m, _, _| menu2(m)),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(div().flex_1().min_w_0().truncate().child(preview))
                            .children(live_text.map(|t| {
                                div()
                                    .flex_none()
                                    .when(live_strong, |d| d.text_color(p.sidebar_foreground))
                                    .child(t)
                            }))
                            .when(running > 0, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .child(ui::status::status_glyph(
                                            krill_proto::control::AgentState::Working,
                                            false,
                                            ui::status::Size::Sm,
                                            cx,
                                        ))
                                        .child(
                                            text::BP_RUNNING_COUNT
                                                .format(&[("n", &running.to_string())]),
                                        ),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    /// Orca's `BotChatTreeWorkerRow`: the worker's state, its title and age; a click opens it.
    #[expect(clippy::too_many_arguments, reason = "worker row render inputs")]
    fn render_bot_chat_worker(
        &self,
        host: &HostId,
        worker: krill_proto::SessionId,
        fallback_name: Option<String>,
        at_ms: u64,
        now: u64,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let host = host.clone();
        let sidebar = self.sidebar.read(cx);
        let session = sidebar.session_on(&host, worker);
        let state = session.map_or(krill_proto::control::AgentState::Exited, |s| s.status.state);
        let title = session
            .and_then(|s| s.status.title.clone())
            .filter(|t| !t.trim().is_empty())
            .or(fallback_name)
            .unwrap_or_else(|| format!("#{}", worker.0));
        let id = SharedString::from(format!("bot-chat-worker-{}", worker.0));
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .role(gpui::Role::Button)
            .tab_index(0)
            .h(px(28.))
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .rounded(ui::RADIUS_SM)
            .px_2()
            .cursor_pointer()
            .hover(|d| d.bg(p.sidebar_accent))
            .child(div().w(px(16.)).flex_none().flex().justify_center().child(
                if let Some(s) = session {
                    ui::status::status_glyph_for(&s.status, false, ui::status::Size::Sm, None, cx)
                } else {
                    ui::status::status_glyph(state, false, ui::status::Size::Sm, cx)
                },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.sidebar_foreground)
                    .child(title),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(ui::format_short_time_ago_secs(at_ms / 1000, now)),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_session_on(host.clone(), worker, cx);
            }))
    }

    fn render_bot_chats(
        &self,
        bot: &SidebarBot,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let q = self.bot_ui.panel.chat_query.trim().to_lowercase();
        let new_host = bot.host.clone();
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
                    .disabled(
                        bot.paused
                            || self
                                .bot_ui
                                .opening
                                .contains(&(bot.host.clone(), bot.id.clone())),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.start_bot_session_on(&new_host, new_id.clone(), None, cx);
                    })),
            );
        let now = super::bot_section::now_secs();
        let host = bot.host.clone();
        let orch = self.orch_snapshot_for(&host);
        let linked_workers = self
            .bot_ui
            .panel
            .view
            .as_ref()
            .map(|v| v.read(cx).chat_worker_links(&bot.id))
            .unwrap_or_default();
        let mut list = div()
            .id("bot-chats-list")
            .debug_selector(|| "bot-chats-list".into())
            .role(gpui::Role::List)
            .flex()
            .flex_col();
        let mut shown = 0;
        for s in &bot.sessions {
            let title = chat_title(bot, s);
            if !q.is_empty() && !title.to_lowercase().contains(&q) {
                continue;
            }
            shown += 1;
            let session = s.session_id;
            let state = self
                .sidebar
                .read(cx)
                .session_on(&host, session)
                .map(|x| x.status.state);
            let mut by_session: std::collections::HashMap<
                krill_proto::SessionId,
                (u64, Option<String>),
            > = std::collections::HashMap::new();
            let mut record = |worker: krill_proto::SessionId, at: u64, name: Option<String>| {
                by_session
                    .entry(worker)
                    .and_modify(|(prev_at, prev_name)| {
                        *prev_at = (*prev_at).max(at);
                        if prev_name.is_none() {
                            prev_name.clone_from(&name);
                        }
                    })
                    .or_insert((at, name));
            };
            if let Some(orch) = orch {
                for run in orch
                    .runs
                    .iter()
                    .filter(|r| r.coordinator_session_id == Some(session.0))
                {
                    for d in orch.dispatches.iter().filter(|d| d.run_id == run.id) {
                        let Some(wsid) = d.session_id.map(krill_proto::SessionId) else {
                            continue;
                        };
                        let at_ms = self
                            .sidebar
                            .read(cx)
                            .session_on(&host, wsid)
                            .map(|x| x.status.since_unix_ns / 1_000_000)
                            .filter(|&at| at > 0)
                            .unwrap_or_else(|| d.ended_at_ms.unwrap_or(d.started_at_ms));
                        let name =
                            (!d.worker_name.trim().is_empty()).then(|| d.worker_name.clone());
                        record(wsid, at_ms, name);
                    }
                }
            }
            for (chat, target, at) in &linked_workers {
                if *chat != session {
                    continue;
                }
                match target {
                    krill_app::bots::LinkedDispatchRef::Session(wsid) => {
                        record(*wsid, *at, None);
                    }
                    krill_app::bots::LinkedDispatchRef::Dispatch(disp_id) => {
                        if let Some(d) =
                            orch.and_then(|o| o.dispatches.iter().find(|d| d.id == *disp_id))
                            && let Some(wsid) = d.session_id.map(krill_proto::SessionId)
                        {
                            let at_ms = (*at).max(d.ended_at_ms.unwrap_or(d.started_at_ms));
                            let name =
                                (!d.worker_name.trim().is_empty()).then(|| d.worker_name.clone());
                            record(wsid, at_ms, name);
                        }
                    }
                }
            }
            let mut mine: Vec<(krill_proto::SessionId, u64, Option<String>)> = by_session
                .into_iter()
                .map(|(wsid, (at, name))| (wsid, at, name))
                .collect();
            mine.sort_by_key(|(wsid, at, _)| (std::cmp::Reverse(*at), std::cmp::Reverse(wsid.0)));
            let running = mine
                .iter()
                .filter(|(w, ..)| {
                    self.sidebar
                        .read(cx)
                        .session_on(&host, *w)
                        .is_some_and(|x| {
                            !x.is_exited()
                                && matches!(
                                    x.status.state,
                                    krill_proto::control::AgentState::Working
                                )
                        })
                })
                .count();
            let foldable = !mine.is_empty();
            let open = foldable
                && !self
                    .bot_ui
                    .panel
                    .chat_folded
                    .contains(&(host.clone(), session));
            list =
                list.child(self.render_bot_chat_row(
                    bot, s, &title, state, running, foldable, open, now, p, cx,
                ));
            if open {
                let mut tree = div().mb_1().ml_6().flex().flex_col();
                for (worker, at, name) in mine {
                    tree = tree
                        .child(self.render_bot_chat_worker(&host, worker, name, at, now, p, cx));
                }
                list = list.child(tree);
            }
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
        let lang = krill_app::i18n::language();
        if let Some((input, _)) = &self.bot_ui.panel.chat_search {
            if self.bot_ui.panel.chat_search_lang != Some(lang) {
                input.update(cx, |s, cx| {
                    s.set_placeholder(text::BP_SEARCH_CHATS.get(), window, cx);
                });
                self.bot_ui.panel.chat_search_lang = Some(lang);
            }
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
        self.bot_ui.panel.chat_search_lang = Some(lang);
    }

    /// Stop, run again or take over: one krilld call, then the sidebar poll catches up.
    fn run_bot_chat_action_on(
        &mut self,
        host: &HostId,
        request: BotRpcRequest,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(host) else {
            return;
        };
        let host_owned = host.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { krill_app::bots::rpc::call_checked(&rpc, &request) })
                .await;
            this.update(cx, |this, cx| {
                if let Err(err) = res {
                    this.notice = Some(err);
                }
                this.refresh_bots_for_host(&host_owned, true, cx);
            })
            .ok();
        })
        .detach();
    }
}
