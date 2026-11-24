// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's sidebar "Bots" section: a collapsible header with search and "+", one row per bot
//! (36 px avatar, name, age, activity line), its Home Chat and other live chats underneath,
//! a right-click menu, and the "Meet your AI teammate" card when there are no bots.
//! Also hosts the New bot and Bot templates dialogs, and opens a bot's Home Chat.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, Image, KeyDownEvent, MouseButton,
    SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{IconName, Sizable as _};
use krill_app::bots::{SidebarBot, avatar, new_bot, templates};
use krill_app::hosts::HostId;
use krill_app::i18n::catalog::bots as text;
use krill_app::ui;
use krill_proto::bots::{BotId, BotRpcRequest, BotRpcResponse, BotTemplate};

use super::Shell;

mod chats;
mod group_rows;
mod header;
mod rows;
#[cfg(test)]
#[cfg(unix)]
mod tests;

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
    /// "Bot options" (sort and hide); read from settings on first use.
    pub view: Option<krill_app::bots::sidebar_view::BotSidebarView>,
    /// Chats whose workers the user explicitly folded (chats with workers start open).
    pub folded_chats: HashSet<(HostId, krill_proto::SessionId)>,
    /// "Show more" steps taken per chat.
    pub chat_steps: HashMap<(HostId, krill_proto::SessionId), usize>,
    /// The bot row being renamed in place.
    pub renaming: Option<(BotId, Entity<InputState>, Subscription)>,
    /// The group row being renamed in place.
    pub renaming_group: Option<(String, Entity<InputState>, Subscription)>,
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
    pub group_dialog: Option<(Entity<krill_app::bots::BotGroupMembersDialog>, Subscription)>,
    /// Bot waiting for "Delete bot" confirmation.
    pub delete: Option<SidebarBot>,
    /// Group waiting for "Delete group" confirmation.
    pub delete_group: Option<krill_proto::bots::BotGroupChat>,
    pub delete_focus: Option<FocusHandle>,
    pub spinners: ui::status::SpinnerSlots,
    /// The right sidebar of a bot page.
    pub panel: super::bot_panel::BotPanel,
    /// Full workspace Group Chat page state.
    pub group_chat: super::bot_group_chat::BotGroupUiState,
}

impl Shell {
    /// Bot RPC to the host bots run on (the launch host).
    pub(super) fn bot_rpc(&self) -> Option<krill_app::bots::BotRpcCall> {
        self.bot_rpc_on(&self.launch_host)
    }

    #[cfg_attr(
        not(test),
        expect(
            clippy::unnecessary_wraps,
            reason = "tests have no daemon without an override"
        )
    )]
    pub(super) fn bot_rpc_on(&self, host: &HostId) -> Option<krill_app::bots::BotRpcCall> {
        #[cfg(test)]
        {
            self.bot_rpc_overrides
                .get(host)
                .or(self.bot_rpc_override.as_ref())
                .cloned()
        }
        #[cfg(not(test))]
        {
            Some(krill_app::bots::daemon_bot_rpc(self.target_for_host(host)))
        }
    }

    pub(super) fn bot_host_label(&self, cx: &App) -> SharedString {
        match &self.launch_host {
            HostId::Local => text::HOST_LOCAL
                .mac_or(krill_app::i18n::catalog::other_os::HOST_LOCAL)
                .get()
                .into(),
            HostId::Remote(id) => self.remote_host_name(id, cx).into(),
        }
    }

    /// Computers New bot offers: this one and every connected remote host, launch host first.
    fn new_bot_hosts(&self, cx: &App) -> Vec<new_bot::HostChoice> {
        let mut hosts: Vec<new_bot::HostChoice> = self
            .execution_locations(cx)
            .into_iter()
            .filter_map(|(id, label)| {
                Some(new_bot::HostChoice {
                    rpc: self.bot_rpc_on(&id)?,
                    id,
                    label: label.into(),
                })
            })
            .collect();
        if let Some(at) = hosts.iter().position(|h| h.id == self.launch_host) {
            let current = hosts.remove(at);
            hosts.insert(0, current);
        } else if let Some(rpc) = self.bot_rpc() {
            // The launch host isn't connected right now; it stays the default all the same.
            hosts.insert(
                0,
                new_bot::HostChoice {
                    id: self.launch_host.clone(),
                    label: self.bot_host_label(cx),
                    rpc,
                },
            );
        }
        hosts
    }

    /// `<state>/bots/<slug>` for bots on `host` (local only).
    pub(super) fn bot_folder_on(host: &HostId, bot: &SidebarBot) -> Option<std::path::PathBuf> {
        host.is_local()
            .then(krill_proto::daemon::state_dir)
            .flatten()
            .map(|state| state.join("bots").join(&bot.slug))
    }

    /// `<state>/bots/<slug>` for bots on this Mac.
    pub(super) fn bot_folder(&self, bot: &SidebarBot) -> Option<std::path::PathBuf> {
        Self::bot_folder_on(&self.launch_host, bot)
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
        if self.bot_rpc().is_none() {
            return;
        }
        // Dialogs open over the workspace like Orca's; the Bots modal makes room.
        self.close_settings(Some(window), cx);
        match request {
            BotDialogRequest::NewBot { template, purpose } => {
                let taken: Vec<String> = self.sidebar_bots.iter().map(|b| b.name.clone()).collect();
                let hosts = self.new_bot_hosts(cx);
                let dialog = cx.new(|cx| {
                    let mut d = new_bot::NewBot::new(hosts, taken, template, window, cx);
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

    pub(super) fn on_new_bot_event(
        &mut self,
        ev: &new_bot::Event,
        window: &mut Window,
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
            new_bot::Event::OpenImport => {
                self.bot_ui.new_bot = None;
                self.bot_ui.back_to_templates = false;
                self.open_bots_screen(BotsScreen::Import, Some(window), cx);
            }
            new_bot::Event::UpdateHost => {
                self.bot_ui.new_bot = None;
                self.bot_ui.back_to_templates = false;
                self.open_settings(krill_app::settings::AdminTab::Machines, None, cx);
            }
            new_bot::Event::Created { bot, warning, host } => {
                self.bot_ui.new_bot = None;
                self.bot_ui.back_to_templates = false;
                // The bot lives on the computer picked for it: bots, its chat and the sidebar's
                // Bots section follow that host from here.
                if *host != self.launch_host {
                    self.set_launch_host(host.clone(), cx);
                    self.sidebar_bots.clear();
                    self.sidebar_groups.clear();
                    self.bot_ui.avatars.clear();
                    self.bot_ui.renaming = None;
                    self.bot_ui.renaming_group = None;
                    self.bot_ui.panel.shown = None;
                    self.bot_ui.panel.settings = None;
                }
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
            if let Some(draft) = kickoff {
                self.bot_ui.panel.pending_draft = Some((bot_id.clone(), draft));
            }
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
            if let Some(pane) = target_pane {
                self.clear_split_launcher_launching(pane);
            }
            return;
        }
        let name = bot.map(|b| b.name.clone()).unwrap_or_default();
        let host = self.launch_host.clone();
        let bot_meta = bot.map(SidebarBot::session_meta);
        let bot_folder = bot.and_then(|b| Self::bot_folder_on(&host, b));
        let host_name: Option<String> = match &host {
            HostId::Local => Some(krill_app::bots::rpc::local_host_label().to_owned()),
            HostId::Remote(id) => {
                let label = self.remote_host_name(id, cx);
                (!label.trim().is_empty() && label != *id).then_some(label)
            }
        };
        let opening = bot_id.clone();
        cx.spawn(async move |this, cx| {
            let started = cx
                .background_executor()
                .spawn(async move {
                    krill_app::bots::rpc::call_checked_on(
                        &rpc,
                        &BotRpcRequest::StartBotSession {
                            bot_id,
                            cwd: None,
                            prompt: kickoff,
                        },
                        host_name.as_deref(),
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
                        if this.launch_host == host
                            && let Some(row) = this.sidebar_bots.iter_mut().find(|b| b.id == bot_id)
                        {
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
                        } else if let Some(meta) = bot_meta {
                            this.sidebar.update(cx, |sb, cx| {
                                sb.register_bot_session(host.clone(), session_id, meta, cx);
                            });
                        }
                        if let Some(pane) = target_pane.filter(|p| this.layout.pane_exists(*p)) {
                            this.open_bot_session_in_pane_on(
                                &host, &bot_id, session_id, pane, bot_folder, cx,
                            );
                        } else {
                            this.open_bot_session_on(host, &bot_id, session_id, cx);
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

    /// Replaces a launcher pane in place with a live bot chat session on `host`.
    fn open_bot_session_in_pane_on(
        &mut self,
        host: &HostId,
        bot_id: &BotId,
        session: krill_proto::SessionId,
        pane: krill_app::layout::PaneId,
        fallback_folder: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        use krill_proto::os::ffi::OsStrExt as _;
        let bot = (*host == self.launch_host)
            .then(|| self.sidebar_bots.iter().find(|b| &b.id == bot_id).cloned())
            .flatten();
        if let Some(meta) = bot.as_ref().map(SidebarBot::session_meta) {
            self.sidebar.update(cx, |sb, cx| {
                sb.register_bot_session(host.clone(), session, meta, cx);
            });
        }
        let folder = bot
            .as_ref()
            .and_then(|b| Self::bot_folder_on(host, b))
            .or(fallback_folder)
            .or_else(|| {
                self.sidebar
                    .read(cx)
                    .folder_on(host, session)
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
        self.open_bot_session_on(host, bot_id, session, cx);
    }

    pub(super) fn open_bot_session_on(
        &mut self,
        host: HostId,
        bot_id: &BotId,
        session: krill_proto::SessionId,
        cx: &mut Context<Self>,
    ) {
        if host == self.launch_host
            && let Some(meta) = self
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
    pub(super) fn delete_bot_keep_folder(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
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

    pub(super) fn toggle_bot_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn render_bots_empty(
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
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
        if let Some((d, _)) = &self.bot_ui.group_dialog {
            out.push(d.clone().into_any_element());
        }
        if let Some(bot) = self.bot_ui.delete.clone() {
            out.push(self.render_bot_delete(&bot, window, cx));
        }
        if let Some(group) = self.bot_ui.delete_group.clone() {
            out.push(self.render_group_delete(&group, window, cx));
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
}

impl Shell {
    pub(super) fn open_bots_screen(
        &mut self,
        screen: BotsScreen,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.open_bots(window, cx);
        if let Some(view) = self.bots_cache.clone() {
            view.update(cx, |v, cx| match screen {
                BotsScreen::Import => v.open_import(cx),
            });
        }
    }
}

pub(super) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
