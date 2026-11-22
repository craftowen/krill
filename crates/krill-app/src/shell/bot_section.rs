// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's sidebar "Bots" section: a collapsible header with search and "+", one row per bot
//! (36 px avatar, name, age, activity line), its Home Chat and other live chats underneath,
//! a right-click menu, and the "Meet your AI teammate" card when there are no bots.
//! Also hosts the New bot and Bot templates dialogs, and opens a bot's Home Chat.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, Image, KeyDownEvent, MouseButton,
    SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_component::{IconName, Sizable as _};
use krill_app::bots::{SidebarBot, avatar, new_bot, templates};
use krill_app::i18n::catalog::bots as text;
use krill_app::ui;
use krill_proto::bots::{BotId, BotRpcRequest, BotRpcResponse, BotTemplate};

use super::Shell;

/// A right-click menu action on the Shell.
type ShellAction = Box<dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>)>;

/// A dialog to build at the start of the next render (it needs the window).
#[derive(Debug, Clone)]
pub(super) enum BotDialogRequest {
    NewBot {
        template: Option<&'static BotTemplate>,
        purpose: Option<String>,
    },
    Templates,
}

/// Sidebar Bots section state that lives on the `Shell`.
#[derive(Default)]
pub(super) struct BotUi {
    pub collapsed: bool,
    /// `Some` while the search field is open.
    pub search: Option<(Entity<InputState>, Subscription)>,
    pub query: String,
    pub folded: HashSet<BotId>,
    /// Decoded `avatar.png` per bot and the config time it belongs to (`None`: unreadable,
    /// so the poll doesn't fetch it again until the bot changes).
    pub avatars: HashMap<BotId, (u64, Option<Arc<Image>>)>,
    /// The bot list has arrived at least once (no "Meet your AI teammate" flash before it).
    pub loaded: bool,
    /// Bots whose Home Chat is being started, so a double click starts one, not two.
    pub opening: HashSet<BotId>,
    /// New bot came from the templates gallery: closing it goes back there, like Orca.
    pub back_to_templates: bool,
    pub pending: Option<BotDialogRequest>,
    pub new_bot: Option<(Entity<new_bot::NewBot>, Subscription)>,
    pub templates: Option<(Entity<templates::Templates>, Subscription)>,
    /// Bot waiting for "Delete bot" confirmation.
    pub delete: Option<SidebarBot>,
    pub delete_focus: Option<FocusHandle>,
    pub spinners: ui::status::SpinnerSlots,
    /// The right sidebar of a bot page.
    pub panel: super::bot_panel::BotPanel,
}

impl Shell {
    /// Bot RPC to the host bots run on (the launch host).
    #[cfg_attr(
        not(test),
        expect(
            clippy::unnecessary_wraps,
            reason = "tests have no daemon without an override"
        )
    )]
    pub(super) fn bot_rpc(&self) -> Option<krill_app::bots::BotRpcCall> {
        #[cfg(test)]
        {
            self.bot_rpc_override.clone()
        }
        #[cfg(not(test))]
        {
            Some(krill_app::bots::daemon_bot_rpc(
                self.target_for_host(&self.launch_host),
            ))
        }
    }

    pub(super) fn bot_host_label(&self) -> SharedString {
        if self.launch_host.is_local() {
            return text::HOST_LOCAL
                .mac_or(krill_app::i18n::catalog::other_os::HOST_LOCAL)
                .get()
                .into();
        }
        self.execution_locations_for_bots()
    }

    fn execution_locations_for_bots(&self) -> SharedString {
        self.ssh_hosts
            .iter()
            .find(|h| krill_app::hosts::HostId::Remote(h.id.clone()) == self.launch_host)
            .map_or_else(|| SharedString::from("?"), |h| h.label.clone().into())
    }

    /// `<state>/bots/<slug>` for bots on this Mac.
    pub(super) fn bot_folder(&self, bot: &SidebarBot) -> Option<std::path::PathBuf> {
        self.launch_host
            .is_local()
            .then(krill_proto::daemon::state_dir)
            .flatten()
            .map(|state| state.join("bots").join(&bot.slug))
    }

    /// Opens New bot (optionally from a template) on the next render.
    pub(super) fn request_new_bot(
        &mut self,
        template: Option<&'static BotTemplate>,
        purpose: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.bot_ui.templates = None;
        self.bot_ui.back_to_templates = false;
        self.bot_ui.pending = Some(BotDialogRequest::NewBot { template, purpose });
        cx.notify();
    }

    pub(super) fn request_bot_templates(&mut self, cx: &mut Context<Self>) {
        self.bot_ui.new_bot = None;
        self.bot_ui.pending = Some(BotDialogRequest::Templates);
        cx.notify();
    }

    /// Builds a requested dialog. Called at the start of `render`, where the window exists.
    pub(super) fn open_pending_bot_dialogs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.bot_ui.pending.take() else {
            return;
        };
        let Some(rpc) = self.bot_rpc() else {
            return;
        };
        // Dialogs open over the workspace like Orca's; the Bots modal makes room.
        self.close_settings(Some(window), cx);
        match request {
            BotDialogRequest::NewBot { template, purpose } => {
                let taken: Vec<String> = self.sidebar_bots.iter().map(|b| b.name.clone()).collect();
                let host_label = self.bot_host_label();
                let local = self.launch_host.is_local();
                let dialog = cx.new(|cx| {
                    let mut d =
                        new_bot::NewBot::new(rpc, host_label, local, taken, template, window, cx);
                    if let Some(purpose) = purpose {
                        d.set_purpose(purpose, window, cx);
                    }
                    d
                });
                let sub = cx.subscribe_in(&dialog, window, move |this, _, ev, window, cx| {
                    this.on_new_bot_event(ev, window, cx);
                });
                dialog.update(cx, |d, cx| d.focus_name(window, cx));
                self.bot_ui.new_bot = Some((dialog, sub));
            }
            BotDialogRequest::Templates => {
                let dialog = cx.new(|cx| templates::Templates::new(window, cx));
                let sub = cx.subscribe_in(&dialog, window, |this, _, ev, _, cx| match ev {
                    templates::Event::Use(t) => {
                        this.request_new_bot(Some(*t), None, cx);
                        this.bot_ui.back_to_templates = true;
                    }
                    templates::Event::Blank(purpose) => {
                        this.request_new_bot(None, purpose.clone(), cx);
                        this.bot_ui.back_to_templates = true;
                    }
                    templates::Event::Close => {
                        this.bot_ui.templates = None;
                        cx.notify();
                    }
                });
                self.bot_ui.templates = Some((dialog, sub));
            }
        }
        cx.notify();
    }

    fn on_new_bot_event(
        &mut self,
        ev: &new_bot::Event,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            new_bot::Event::Close => {
                self.bot_ui.new_bot = None;
                if std::mem::take(&mut self.bot_ui.back_to_templates) {
                    self.request_bot_templates(cx);
                }
                cx.notify();
            }
            new_bot::Event::BrowseTemplates => self.request_bot_templates(cx),
            new_bot::Event::Created { bot, warning } => {
                self.bot_ui.new_bot = None;
                self.bot_ui.back_to_templates = false;
                if let Some(err) = warning {
                    self.notice = Some(err.clone());
                }
                // Orca expands the section so the new bot's row is visible and active.
                self.bot_ui.collapsed = false;
                self.bot_ui.folded.remove(&bot.id);
                let row = krill_app::bots::rpc::sidebar_snapshot(
                    std::slice::from_ref(bot.as_ref()),
                    &[],
                    &[],
                    &[],
                )
                .bots;
                for r in row {
                    if !self.sidebar_bots.iter().any(|b| b.id == r.id) {
                        self.sidebar_bots.insert(0, r);
                    }
                }
                // No kickoff turn: the chat opens empty and the bot first answers its owner's
                // first message (its instructions ride in the agent's system prompt).
                self.open_bot_home(bot.id.clone(), None, cx);
                self.refresh_bots_badge(cx);
                cx.notify();
            }
        }
    }

    /// Opens a bot's Home Chat: focuses its live session, or starts one (with `kickoff` as
    /// the first turn for a bot that was just made).
    pub(super) fn open_bot_home(
        &mut self,
        bot_id: BotId,
        kickoff: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(live) = self
            .sidebar_bots
            .iter()
            .find(|b| b.id == bot_id)
            .and_then(SidebarBot::home_session)
        {
            let session = live.session_id;
            self.open_bot_session(&bot_id, session, cx);
            return;
        }
        self.start_bot_session(bot_id, kickoff, cx);
    }

    /// Starts a new chat for a bot (with `kickoff` as its first turn) and opens it.
    pub(super) fn start_bot_session(
        &mut self,
        bot_id: BotId,
        kickoff: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.start_bot_session_in(bot_id, kickoff, None, cx);
    }

    /// Starts a new chat for a bot, filling `target_pane` in place when present (split/tab launcher).
    pub(super) fn start_bot_session_in(
        &mut self,
        bot_id: BotId,
        kickoff: Option<String>,
        target_pane: Option<krill_app::layout::PaneId>,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc() else {
            if let Some(pane) = target_pane {
                self.clear_split_launcher_launching(pane);
            }
            return;
        };
        let bot = self.sidebar_bots.iter().find(|b| b.id == bot_id);
        // krilld won't start a paused bot; say so in the app's words.
        if bot.is_some_and(|b| b.paused) && kickoff.is_none() {
            if let Some(pane) = target_pane {
                self.clear_split_launcher_launching(pane);
            }
            self.notice = Some(text::OPEN_CHAT_PAUSED_TOOLTIP.get().to_owned());
            cx.notify();
            return;
        }
        if !self.bot_ui.opening.insert(bot_id.clone()) {
            return;
        }
        let name = bot.map(|b| b.name.clone()).unwrap_or_default();
        let opening = bot_id.clone();
        cx.spawn(async move |this, cx| {
            let started = cx
                .background_executor()
                .spawn(async move {
                    krill_app::bots::rpc::call_checked(
                        &rpc,
                        &BotRpcRequest::StartBotSession {
                            bot_id,
                            cwd: None,
                            prompt: kickoff,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.bot_ui.opening.remove(&opening);
                if let Some(pane) = target_pane {
                    this.clear_split_launcher_launching(pane);
                }
                match started {
                    Ok(BotRpcResponse::SessionStarted {
                        bot_id,
                        session_id,
                        pid,
                    }) => {
                        // Show the new Home Chat under the bot now; the poll fills in the rest.
                        if let Some(row) = this.sidebar_bots.iter_mut().find(|b| b.id == bot_id) {
                            // krilld lists the Home Chat first, then newest.
                            let at = usize::from(row.home_session().is_some());
                            row.sessions.insert(
                                at,
                                krill_proto::bots::BotSessionSummary {
                                    session_id,
                                    bot_id: bot_id.clone(),
                                    link_id: None,
                                    pid,
                                    live: true,
                                    working: false,
                                    label: String::new(),
                                    created_at_ms: now_secs().saturating_mul(1000),
                                },
                            );
                        }
                        if let Some(pane) = target_pane.filter(|p| this.layout.pane_exists(*p)) {
                            this.open_bot_session_in_pane(&bot_id, session_id, pane, cx);
                        } else {
                            this.open_bot_session(&bot_id, session_id, cx);
                        }
                        this.refresh_bots_badge(cx);
                    }
                    Ok(_) => {
                        this.notice = Some(text::NB_CREATED_NOT_OPENED.format(&[("name", &name)]));
                    }
                    Err(err) => this.notice = Some(err),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Replaces a launcher pane in place with a live bot chat session.
    pub(super) fn open_bot_session_in_pane(
        &mut self,
        bot_id: &BotId,
        session: krill_proto::SessionId,
        pane: krill_app::layout::PaneId,
        cx: &mut Context<Self>,
    ) {
        use krill_proto::os::ffi::OsStrExt as _;
        let host = self.launch_host.clone();
        let bot = self.sidebar_bots.iter().find(|b| &b.id == bot_id).cloned();
        if let Some(meta) = bot.as_ref().map(SidebarBot::session_meta) {
            self.sidebar.update(cx, |sb, cx| {
                sb.register_bot_session(host.clone(), session, meta, cx);
            });
        }
        let folder = bot.as_ref().and_then(|b| self.bot_folder(b)).or_else(|| {
            self.sidebar
                .read(cx)
                .folder_on(&host, session)
                .map(std::path::Path::to_path_buf)
        });
        self.split_launchers.remove(&pane);
        self.workspace_closing.remove(&(host.clone(), session));
        self.change(true, cx, |l| {
            if let Some(tab) = l.tab_of_pane(pane) {
                l.activate(tab);
            }
            l.focus(pane);
            l.set_host(pane, host.clone());
            l.set_session(pane, session);
            if let Some(folder) = folder {
                l.set_launch(
                    pane,
                    krill_proto::control::Create {
                        account: None,
                        program: krill_proto::control::Program::Login,
                        cwd: folder.as_os_str().as_bytes().to_vec(),
                        cols: krill_app::pane::SIZE.0,
                        rows: krill_app::pane::SIZE.1,
                    },
                );
            }
        });
        self.set_pane_chat_view(pane, true, cx);
        self.files.refocus = true;
    }

    /// Shows one of a bot's sessions as a chat tab, labelled with the bot's identity.
    pub(super) fn open_bot_session(
        &mut self,
        bot_id: &BotId,
        session: krill_proto::SessionId,
        cx: &mut Context<Self>,
    ) {
        let host = self.launch_host.clone();
        if let Some(meta) = self
            .sidebar_bots
            .iter()
            .find(|b| &b.id == bot_id)
            .map(SidebarBot::session_meta)
        {
            self.sidebar.update(cx, |sb, cx| {
                sb.register_bot_session(host.clone(), session, meta, cx);
            });
        }
        let deferred = self.layout_loading;
        self.open_session_on(host, session, cx);
        if deferred {
            self.restore_session_chat = true;
        } else if let Some(pane) = self.layout.focused() {
            self.set_pane_chat_view(pane, true, cx);
        }
    }

    pub(super) fn set_bot_paused(&mut self, bot_id: BotId, paused: bool, cx: &mut Context<Self>) {
        let Some(rpc) = self.bot_rpc() else {
            return;
        };
        // Optimistic, like the rest of the sidebar; the poll below corrects a refusal.
        if let Some(b) = self.sidebar_bots.iter_mut().find(|b| b.id == bot_id) {
            b.paused = paused;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    krill_app::bots::rpc::call_checked(
                        &rpc,
                        &BotRpcRequest::UpdateBot(Box::new(krill_proto::bots::UpdateBotParams {
                            bot_id,
                            name: None,
                            description: None,
                            emoji: krill_proto::bots::FieldPatch::Unchanged,
                            avatar: krill_proto::bots::FieldPatch::Unchanged,
                            agent_id: None,
                            model: krill_proto::bots::FieldPatch::Unchanged,
                            effort: krill_proto::bots::FieldPatch::Unchanged,
                            permission_mode: None,
                            workspace: krill_proto::bots::FieldPatch::Unchanged,
                            paused: Some(paused),
                            first_run_completed: None,
                            notifications: None,
                            schedules: None,
                            look_around: None,
                            memory_tidy: None,
                            mcp_tools: None,
                        })),
                    )
                })
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

    /// Orca's "Delete bot": removes the bot and keeps its folder (soul and memory stay).
    fn delete_bot_keep_folder(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        let Some(rpc) = self.bot_rpc() else {
            return;
        };
        self.bot_ui.delete = None;
        self.sidebar_bots.retain(|b| b.id != bot_id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    krill_app::bots::rpc::call_checked(
                        &rpc,
                        &BotRpcRequest::DeleteBot {
                            bot_id,
                            delete_home: false,
                        },
                    )
                })
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

    fn toggle_bot_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bot_ui.search.take().is_some() {
            self.bot_ui.query.clear();
        } else {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(text::SB_SEARCH.get()));
            let sub = cx.subscribe(&input, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.bot_ui.query = input.read(cx).value().to_string();
                    cx.notify();
                }
            });
            input.update(cx, |s, cx| s.focus(window, cx));
            self.bot_ui.search = Some((input, sub));
        }
        cx.notify();
    }

    /// Decodes newly read `avatar.png` files (empty = unreadable; remembered so the poll
    /// doesn't fetch it again until the bot changes) and drops pictures of deleted bots.
    pub(super) fn store_bot_avatars(&mut self, avatars: Vec<(BotId, u64, Vec<u8>)>) {
        for (id, at, png) in avatars {
            let img = krill_app::bots::rpc::decode_png_avatar(&png).ok();
            self.bot_ui.avatars.insert(id, (at, img));
        }
        let live: HashSet<&BotId> = self.sidebar_bots.iter().map(|b| &b.id).collect();
        self.bot_ui.avatars.retain(|id, _| live.contains(id));
    }

    pub(super) fn known_bot_avatars(&self) -> HashMap<BotId, u64> {
        self.bot_ui
            .avatars
            .iter()
            .map(|(id, (at, _))| (id.clone(), *at))
            .collect()
    }

    pub(super) fn bot_avatar(
        &self,
        bot: &SidebarBot,
        size: f32,
        radius: gpui::Pixels,
        p: &ui::Palette,
    ) -> AnyElement {
        let upload = self
            .bot_ui
            .avatars
            .get(&bot.id)
            .and_then(|(_, img)| img.clone());
        avatar::render(&bot.avatar, upload, &bot.name, px(size), radius, p)
    }

    /// Orca's activity line: paused, then needs you, then working, then the bot's job.
    pub(super) fn bot_activity(bot: &SidebarBot) -> (String, bool) {
        if bot.paused {
            return (text::PAUSED_BADGE.get().to_owned(), false);
        }
        if bot.attention > 0 {
            let n = bot.attention.to_string();
            let msg = if bot.attention == 1 {
                text::SB_NEEDS_YOU_ONE.format(&[("n", &n)])
            } else {
                text::SB_NEEDS_YOU_MANY.format(&[("n", &n)])
            };
            return (msg, true);
        }
        let working = bot.working();
        if working > 0 {
            return (
                text::SB_WORKING.format(&[("n", &working.to_string())]),
                false,
            );
        }
        (bot.description.clone(), false)
    }

    fn render_bots_header(
        &self,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let collapsed = self.bot_ui.collapsed;
        let has_bots = !self.sidebar_bots.is_empty();
        let groups_ok = self.sidebar_bots.len() >= 2;
        let this = cx.weak_entity();
        let new_menu = Button::new("bots-section-new")
            .ghost()
            .xsmall()
            .icon(IconName::Plus)
            .accessibility_label(text::SB_NEW_MENU.get())
            .tooltip(text::SB_NEW_MENU.get())
            .debug_selector(|| "bots-section-new".into())
            .dropdown_menu(move |menu, _, _| {
                let act = |f: fn(&mut Shell, &mut Window, &mut Context<Shell>)| {
                    let this = this.clone();
                    move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                        this.update(cx, |s, cx| f(s, window, cx)).ok();
                    }
                };
                menu.item(
                    PopupMenuItem::new(text::SB_NEW_BOT.get())
                        .icon(IconName::Plus)
                        .on_click(act(|s, _, cx| s.request_new_bot(None, None, cx))),
                )
                .item(
                    PopupMenuItem::new(text::SB_BROWSE_TEMPLATES.get())
                        .icon(gpui_component::Icon::default().data(ui::icons::LAYOUT_TEMPLATE_SVG))
                        .on_click(act(|s, _, cx| s.request_bot_templates(cx))),
                )
                .item(
                    PopupMenuItem::new(text::SB_IMPORT.get())
                        .icon(gpui_component::Icon::default().data(ui::icons::DOWNLOAD_SVG))
                        .on_click(act(|s, window, cx| {
                            s.open_bots_screen(BotsScreen::Import, window, cx);
                        })),
                )
                .separator()
                .item(
                    PopupMenuItem::new(text::SB_NEW_GROUP.get())
                        .disabled(!groups_ok)
                        .on_click(act(|s, window, cx| {
                            s.open_bots_screen(BotsScreen::Groups, window, cx);
                        })),
                )
            });

        let mut header = div()
            .id("bots-section-header")
            .debug_selector(|| "bots-section-header".into())
            // Orca SidebarSectionHeader: `h-8 px-2`, a fold chevron before the title.
            .h(ui::SIDEBAR_HEADER_HEIGHT)
            .px_2()
            .flex()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .id("bots-section-title")
                    .debug_selector(|| "bots-section-title".into())
                    .role(gpui::Role::Button)
                    .aria_label(if collapsed {
                        text::SB_EXPAND.get()
                    } else {
                        text::SB_COLLAPSE.get()
                    })
                    .aria_expanded(!collapsed)
                    .group("bots-section-title")
                    .cursor_pointer()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.bot_ui.collapsed = !this.bot_ui.collapsed;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .ml_0p5()
                            .size_5()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(ui::RADIUS_SM)
                            .text_color(p.muted_foreground)
                            .group_hover("bots-section-title", |d| {
                                d.bg(p.accent.opacity(0.7)).text_color(p.foreground)
                            })
                            .child(
                                gpui_component::Icon::new(if collapsed {
                                    IconName::ChevronRight
                                } else {
                                    IconName::ChevronDown
                                })
                                .size(px(14.)),
                            ),
                    )
                    .child(
                        div()
                            .pr_0p5()
                            .text_size(ui::TEXT_XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.muted_foreground.opacity(0.8))
                            .child(krill_app::i18n::catalog::sidebar::NAV_BOTS.get()),
                    ),
            );
        if collapsed {
            header = header.child(self.render_collapsed_faces(p, cx));
        }
        header
            .child(div().flex_1())
            .when(!collapsed && has_bots, |d| {
                d.child(
                    Button::new("bots-section-search")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Search)
                        .accessibility_label(text::SB_SEARCH.get())
                        .tooltip(text::SB_SEARCH.get())
                        .debug_selector(|| "bots-section-search".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_bot_search(window, cx)),
                        ),
                )
            })
            .when(!collapsed, |d| d.child(new_menu))
    }

    /// Collapsed section: up to five round avatars, then `+N` (attention first, like Orca).
    fn render_collapsed_faces(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let mut bots: Vec<&SidebarBot> = self.sidebar_bots.iter().collect();
        bots.sort_by_key(|b| (b.attention == 0, b.working() == 0));
        let shown = bots.len().min(5);
        let mut row = div().flex().items_center();
        for (i, bot) in bots.iter().take(shown).enumerate() {
            let id = bot.id.clone();
            row = row.child(
                div()
                    .id(SharedString::from(format!(
                        "bots-collapsed-{}",
                        bot.id.as_str()
                    )))
                    .debug_selector({
                        let sel = format!("bots-collapsed-{}", bot.id.as_str());
                        move || sel.clone()
                    })
                    .when(i > 0, |d| d.ml(px(-4.)))
                    .rounded_full()
                    .border_2()
                    .border_color(p.sidebar)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.bot_ui.collapsed = false;
                        this.open_bot_home(id.clone(), None, cx);
                    }))
                    .child(self.bot_avatar(bot, 20., px(10.), p)),
            );
        }
        if bots.len() > shown {
            row = row.child(
                div()
                    .ml_1()
                    .text_size(ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(format!("+{}", bots.len() - shown)),
            );
        }
        row
    }

    fn render_bot_children(
        &self,
        bot: &SidebarBot,
        focused: Option<krill_proto::SessionId>,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let now = now_secs();
        let mut col = div().pl(px(21.)).pr_1().pb_1().flex().flex_col().gap_1();
        for s in &bot.sessions {
            let active = focused == Some(s.session_id);
            let home = s.link_id.is_none();
            let title = super::bot_panel::chat_title(bot, s);
            let bot_id = bot.id.clone();
            let session = s.session_id;
            let id = SharedString::from(format!("bot-chat-{}", s.session_id.0));
            col = col.child(
                div()
                    .id(id.clone())
                    .debug_selector(move || id.to_string())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .focus_visible(|d| d.border_1().border_color(p.ring))
                    .aria_label(title.clone())
                    .h(px(24.))
                    .pl_1()
                    .pr_0p5()
                    .rounded(ui::RADIUS_SM)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .cursor_pointer()
                    .when(active, |d| d.bg(p.sidebar_accent))
                    .when(!active, |d| d.hover(|d| d.bg(p.sidebar_accent)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_bot_session(&bot_id, session, cx);
                    }))
                    .child(ui::icons::svg_icon(
                        if home {
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
                            .text_color(p.foreground)
                            .when(active, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(title),
                    )
                    .when(s.working, |d| {
                        d.child(ui::status::status_glyph_slot(
                            krill_proto::control::AgentState::Working,
                            false,
                            ui::status::Size::Sm,
                            Some(&self.bot_ui.spinners),
                            cx,
                        ))
                    })
                    .child(div().flex_none().text_size(ui::TEXT_2XS).child(
                        krill_app::jump::format_age_secs(s.created_at_ms / 1000, now),
                    )),
            );
        }
        col
    }

    fn render_bot_row(
        &self,
        bot: &SidebarBot,
        focused: Option<krill_proto::SessionId>,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let has_children = !bot.sessions.is_empty();
        let folded = self.bot_ui.folded.contains(&bot.id);
        let active = focused.is_some_and(|f| bot.sessions.iter().any(|s| s.session_id == f));
        let (activity, attention) = Self::bot_activity(bot);
        let age = bot
            .sessions
            .iter()
            .map(|s| s.created_at_ms)
            .max()
            .unwrap_or(bot.updated_at_ms);
        let row_id = format!("bot-row-{}", bot.id.as_str());
        let row_sel = row_id.clone();
        let group = SharedString::from(format!("bot-row-group-{}", bot.id.as_str()));
        let open_id = bot.id.clone();
        let fold_id = bot.id.clone();
        let menu_bot = bot.clone();
        let folder = self.bot_folder(bot);
        let this = cx.weak_entity();

        let row =
            div()
                .id(SharedString::from(row_id))
                .debug_selector(move || row_sel.clone())
                .group(group.clone())
                .role(gpui::Role::Button)
                .tab_index(0)
                .focus_visible(|d| d.border_1().border_color(p.ring))
                .aria_label(bot.name.clone())
                // Orca BotSidebarRow sits on the workspace card surface: `ml-1 pr-1.5`, a
                // transparent border that the active row fills like an active workspace card.
                .ml_1()
                .pl(px(5.))
                .pr_1p5()
                .py(px(5.))
                .rounded(ui::RADIUS_MD)
                .border_1()
                .flex()
                .items_center()
                .cursor_pointer()
                .when(bot.paused, |d| d.opacity(0.6))
                .when(active, |d| {
                    d.bg(p.worktree_sidebar)
                        .border_color(p.worktree_sidebar_border)
                })
                .when(!active, |d| {
                    d.border_color(ui::TRANSPARENT)
                        .hover(|d| d.bg(p.sidebar_accent.opacity(0.45)))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_bot_home(open_id.clone(), None, cx);
                }))
                .child(
                    div()
                        .mr_2()
                        .child(self.bot_avatar(bot, 32., ui::RADIUS_SM, p)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(ui::TEXT_MD)
                                        .text_color(p.sidebar_foreground)
                                        .child(bot.name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(ui::TEXT_SM)
                                        .text_color(p.muted_foreground)
                                        .child(krill_app::jump::format_age_secs(
                                            age / 1000,
                                            now_secs(),
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(ui::TEXT_SM)
                                .text_color(if attention {
                                    p.sidebar_foreground
                                } else {
                                    p.muted_foreground
                                })
                                .child(activity),
                        ),
                )
                .child(div().flex_none().w(px(28.)).flex().justify_center().when(
                    has_children,
                    |d| {
                        let name = bot.name.clone();
                        d.child(
                            div()
                                .when(!folded, |d| {
                                    d.invisible()
                                        .group_hover(group.clone(), gpui::Styled::visible)
                                })
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "bot-fold-{}",
                                        bot.id.as_str()
                                    )))
                                    .ghost()
                                    .xsmall()
                                    .icon(if folded {
                                        IconName::ChevronRight
                                    } else {
                                        IconName::ChevronDown
                                    })
                                    .accessibility_label(if folded {
                                        text::SB_UNFOLD.format(&[("name", &name)])
                                    } else {
                                        text::SB_FOLD.format(&[("name", &name)])
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        if !this.bot_ui.folded.remove(&fold_id) {
                                            this.bot_ui.folded.insert(fold_id.clone());
                                        }
                                        cx.notify();
                                    })),
                                ),
                        )
                    },
                ))
                .context_menu(move |menu, _, _| {
                    let bot = menu_bot.clone();
                    let act = |f: ShellAction| {
                        let this = this.clone();
                        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                            this.update(cx, |s, cx| f(s, window, cx)).ok();
                        }
                    };
                    let id = bot.id.clone();
                    let settings = act(Box::new(move |s, window, cx| {
                        if !s.open_bot_settings_in_panel(&id, cx) {
                            s.open_bot(Some(id.clone()), Some(window), cx);
                        }
                    }));
                    let id = bot.id.clone();
                    let paused = bot.paused;
                    let pause = act(Box::new(move |s, _, cx| {
                        s.set_bot_paused(id.clone(), !paused, cx);
                    }));
                    let deleting = bot.clone();
                    let delete = act(Box::new(move |s, _, cx| {
                        s.bot_ui.delete = Some(deleting.clone());
                        cx.notify();
                    }));
                    let mut menu = menu
                        .item(
                            PopupMenuItem::new(text::SB_BOT_SETTINGS.get())
                                .icon(IconName::Settings)
                                .on_click(settings),
                        )
                        .item(
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
                            .on_click(pause),
                        );
                    if let Some(path) = folder.clone() {
                        menu = menu.separator().item(
                            PopupMenuItem::new(text::SB_COPY_PATH.get())
                                .icon(IconName::Copy)
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        path.to_string_lossy().into_owned(),
                                    ));
                                }),
                        );
                    }
                    menu.separator().item(
                        PopupMenuItem::new(text::SB_DELETE.get())
                            .icon(IconName::Delete)
                            .on_click(delete),
                    )
                });

        div()
            .w_full()
            .flex()
            .flex_col()
            .child(row)
            .when(has_children && !folded, |d| {
                d.child(
                    div()
                        .when(bot.paused, |d| d.opacity(0.6))
                        .child(self.render_bot_children(bot, focused, p, cx)),
                )
            })
            .into_any_element()
    }

    fn render_bots_empty(p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let mut faces = div().flex().items_center();
        for (i, id) in ["A1", "E1", "M1"].into_iter().enumerate() {
            let face = avatar::Avatar::from_fields(Some(id), None, None, id);
            faces = faces.child(
                div()
                    .when(i > 0, |d| d.ml(px(-6.)))
                    .rounded(ui::RADIUS_SM)
                    .border_2()
                    .border_color(p.sidebar)
                    .child(avatar::render(&face, None, id, px(24.), ui::RADIUS_SM, p)),
            );
        }
        div()
            .id("bots-empty-card")
            .debug_selector(|| "bots-empty-card".into())
            .mx_2()
            .p_3()
            .rounded(ui::RADIUS_LG)
            .border_1()
            .border_color(p.border.opacity(0.7))
            .bg(p.muted.opacity(0.4))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div().flex().items_center().gap_2().child(faces).child(
                    div()
                        .text_size(ui::TEXT_SM)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.sidebar_foreground)
                        .child(text::SB_EMPTY_TITLE.get()),
                ),
            )
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::SB_EMPTY_BODY.get()),
            )
            .child(
                div()
                    .flex()
                    .gap_1p5()
                    .child(
                        div().flex_1().child(
                            ui::primary("bots-empty-create", text::SB_CREATE_A_BOT.get())
                                .icon(IconName::Plus)
                                .w_full()
                                .debug_selector(|| "bots-empty-create".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.request_new_bot(None, None, cx);
                                })),
                        ),
                    )
                    .child(
                        div().flex_1().child(
                            ui::secondary("bots-empty-templates", text::SB_TEMPLATES.get())
                                .icon(
                                    gpui_component::Icon::default()
                                        .data(ui::icons::LAYOUT_TEMPLATE_SVG),
                                )
                                .w_full()
                                .debug_selector(|| "bots-empty-templates".into())
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.request_bot_templates(cx)),
                                ),
                        ),
                    ),
            )
    }

    /// The whole section, placed between the top navigation and Projects.
    pub(super) fn render_bots_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = ui::palette(cx);
        self.bot_ui.spinners.clear();
        let mut section = div()
            .id("bots-section")
            .debug_selector(|| "bots-section".into())
            .role(gpui::Role::Group)
            .aria_label(krill_app::i18n::catalog::sidebar::NAV_BOTS.get())
            .flex_none()
            // Orca caps the section at 40% of the sidebar; the bot list scrolls inside it.
            .max_h(gpui::relative(0.4))
            // Orca SidebarBotsSplit: `border-b pb-2`, the Bots / Projects divider.
            .border_b_1()
            .border_color(p.worktree_sidebar_border)
            .pb_2()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .child(self.render_bots_header(p, cx));
        if self.bot_ui.collapsed {
            return section.into_any_element();
        }
        if self.sidebar_bots.is_empty() {
            // Until the list arrives, show nothing rather than an invitation to make a first bot.
            if self.bot_ui.loaded {
                section = section.child(Self::render_bots_empty(p, cx));
            }
            return section.into_any_element();
        }
        if let Some((input, _)) = &self.bot_ui.search {
            section = section.child(
                div()
                    .px_2()
                    .pb_1()
                    .child(Input::new(input).small().cleanable(true)),
            );
        }
        let q = self.bot_ui.query.trim().to_lowercase();
        let focused = self
            .focused_session()
            .filter(|(host, _)| *host == self.launch_host)
            .map(|(_, s)| s);
        let mut list = div()
            .id("bots-section-list")
            .debug_selector(|| "bots-section-list".into())
            .role(gpui::Role::List)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb_1()
            .pl_1()
            .pr_1()
            .flex()
            .flex_col();
        let mut shown = 0;
        for bot in &self.sidebar_bots {
            if !q.is_empty()
                && !bot.name.to_lowercase().contains(&q)
                && !bot.description.to_lowercase().contains(&q)
            {
                continue;
            }
            shown += 1;
            list = list.child(self.render_bot_row(bot, focused, p, cx));
        }
        if shown == 0 {
            list = list.child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::SB_NO_MATCHES.get()),
            );
        }
        section.child(list).into_any_element()
    }

    /// Spinners for working bot chats (drawn above the section so it isn't re-laid out per tick).
    pub(super) fn bots_spinner_overlay(&self) -> impl IntoElement {
        ui::status::spinner_overlay(&self.bot_ui.spinners)
    }

    /// Open New bot / Bot templates dialogs, and the delete confirmation.
    pub(super) fn bot_dialogs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();
        if let Some((d, _)) = &self.bot_ui.new_bot {
            out.push(d.clone().into_any_element());
        }
        if let Some((d, _)) = &self.bot_ui.templates {
            out.push(d.clone().into_any_element());
        }
        if let Some(bot) = self.bot_ui.delete.clone() {
            out.push(self.render_bot_delete(&bot, window, cx));
        }
        out
    }

    fn render_bot_delete(
        &mut self,
        bot: &SidebarBot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = ui::palette(cx);
        let focus = self
            .bot_ui
            .delete_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        if !focus.contains_focused(window, cx) {
            window.focus(&focus, cx);
        }
        let path = self
            .bot_folder(bot)
            .map_or_else(|| bot.slug.clone(), |p| p.to_string_lossy().into_owned());
        let id = bot.id.clone();
        let card = div()
            .id("bot-delete-dialog")
            .debug_selector(|| "bot-delete-dialog".into())
            .role(gpui::Role::AlertDialog)
            .aria_label(text::SB_DELETE_TITLE.format(&[("name", &bot.name)]))
            .occlude()
            .w(ui::DIALOG_WIDTH)
            .p_6()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.bot_ui.delete = None;
                    cx.notify();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text::SB_DELETE_TITLE.format(&[("name", &bot.name)])),
            )
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::SB_DELETE_BODY.format(&[("path", &path)])),
            )
            .child(
                ui::controls()
                    .justify_end()
                    .child(ui::ghost("bot-delete-cancel", text::CANCEL.get()).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.bot_ui.delete = None;
                            cx.notify();
                        }),
                    ))
                    .child(
                        Button::new("bot-delete-confirm")
                            .small()
                            .danger()
                            .label(text::SB_DELETE.get())
                            .debug_selector(|| "bot-delete-confirm".into())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_bot_keep_folder(id.clone(), cx);
                            })),
                    ),
            );
        ui::modal_backdrop(
            window.viewport_size(),
            div()
                .id("bot-delete-backdrop")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.bot_ui.delete = None;
                        cx.notify();
                    }),
                )
                .child(ui::modal_focus_scope(card, &focus)),
        )
        .into_any_element()
    }
}

/// Bots modal screens the sidebar menu jumps to.
#[derive(Debug, Clone, Copy)]
pub(super) enum BotsScreen {
    Import,
    Groups,
}

impl Shell {
    fn open_bots_screen(
        &mut self,
        screen: BotsScreen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_bots(Some(window), cx);
        if let Some(view) = self.bots_cache.clone() {
            view.update(cx, |v, cx| match screen {
                BotsScreen::Import => v.open_import(cx),
                BotsScreen::Groups => v.open_groups(cx),
            });
        }
    }
}

pub(super) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
