// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's sidebar "Bots" section: a collapsible header with search and "+", one row per bot
//! (32 px avatar, name, age, activity line), its Home Chat and other live chats underneath,
//! a right-click menu, and the "Meet your AI teammate" card when there are no bots.
//! Also hosts the New bot and Bot templates dialogs, and opens a bot's Home Chat.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, FontWeight, Image, KeyDownEvent, MouseButton,
    SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{IconName, Sizable as _};
use krill_app::bots::{SidebarBot, avatar, new_bot, templates};
use krill_app::hosts::{HostId, HostTarget};
use krill_app::i18n::catalog::bots as text;
use krill_app::ui;
use krill_proto::bots::{BotGroupSummary, BotId, BotRpcRequest, BotRpcResponse, BotTemplate};

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

/// Decoded `avatar.png` cache keyed by `(host, bot_id)`.
type BotAvatarCache = HashMap<(HostId, BotId), (u64, Option<Arc<Image>>)>;

/// A multi-bot group entry owned by a specific Krill host (`Local` or `Remote`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::shell) struct SidebarGroup {
    pub host: HostId,
    pub summary: BotGroupSummary,
}

impl Deref for SidebarGroup {
    type Target = BotGroupSummary;

    fn deref(&self) -> &Self::Target {
        &self.summary
    }
}

impl DerefMut for SidebarGroup {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.summary
    }
}

/// Per-host sidebar bots synchronization state (isolated retries, backoff, and capability cache).
#[derive(Default)]
pub(super) struct HostBotsState {
    pub target: Option<HostTarget>,
    pub transport_epoch: u64,
    pub generation: u64,
    pub in_flight: bool,
    pub dirty: bool,
    pub probed: bool,
    /// Cached daemon version when it lacks `FEAT_BOTS`, plus timestamp of check.
    pub unsupported: Option<(String, std::time::Instant)>,
    pub consecutive_failures: u32,
    pub next_retry_at: Option<std::time::Instant>,
    pub snapshot: krill_app::bots::SidebarSnapshot,
    pub groups: Vec<BotGroupSummary>,
    pub last_error: Option<String>,
    pub retry_task: Option<Task<()>>,
}

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
    /// Per-host fetched snapshots, backoff, and capability state.
    pub hosts: BTreeMap<HostId, HostBotsState>,
    /// Independent background poll task per host (no host cancels or starves another).
    pub host_tasks: HashMap<HostId, Task<()>>,
    /// Last observed list of connected non-suppressed remote hosts from `Sidebar`.
    pub last_observed_remotes: Vec<HostId>,
    /// Safety timer so an unreachable remote host never keeps `loaded == false` forever.
    pub loaded_fallback_task: Option<Task<()>>,
    /// Virtualized collapsed preview scroll offset (`BotCollapsedPreview` / `#47622`).
    pub preview_offset: usize,
    /// `Some` while the search field is open.
    pub search: Option<(Entity<InputState>, Subscription)>,
    /// Locale active when the search input was built, so language switches refresh its placeholder.
    pub search_lang: Option<krill_app::i18n::Language>,
    pub query: String,
    pub folded: HashSet<(HostId, BotId)>,
    /// Unseen Up Next suggestion counts per `(host, bot_id)` (`BotUpNextCount`).
    pub up_next_unseen: HashMap<(HostId, BotId), usize>,
    /// Snapshot key `(host, bot_id, paused)` for which unseen suggestion counts were requested.
    pub up_next_bots_key: Vec<(HostId, BotId, bool)>,
    /// Replaceable background fetch for `up_next_unseen` (cancels superseded requests).
    pub up_next_task: Option<Task<()>>,
    /// Generation counter guarding `up_next_unseen` async responses against stale writes.
    pub up_next_gen: u64,
    /// "Bot options" (sort and hide); read from settings on first use.
    pub view: Option<krill_app::bots::sidebar_view::BotSidebarView>,
    /// Chats whose workers the user explicitly folded (chats with workers start open).
    pub folded_chats: HashSet<(HostId, krill_proto::SessionId)>,
    /// "Show more" steps taken per chat.
    pub chat_steps: HashMap<(HostId, krill_proto::SessionId), usize>,
    /// The bot row being renamed in place.
    pub renaming: Option<(HostId, BotId, Entity<InputState>, Subscription)>,
    /// The group row being renamed in place.
    pub renaming_group: Option<(HostId, String, Entity<InputState>, Subscription)>,
    /// Decoded `avatar.png` per `(host, bot)` and the config time it belongs to (`None`: unreadable,
    /// so the poll doesn't fetch it again until the bot changes).
    pub avatars: BotAvatarCache,
    /// The bot list has arrived at least once (no "Meet your AI teammate" flash before it).
    pub loaded: bool,
    /// Bots whose Home Chat is being started, so a double click starts one, not two.
    pub opening: HashSet<(HostId, BotId)>,
    /// New bot came from the templates gallery: closing it goes back there, like Orca.
    pub back_to_templates: bool,
    pub pending: Option<BotDialogRequest>,
    pub new_bot: Option<(Entity<new_bot::NewBot>, Subscription)>,
    pub templates: Option<(Entity<templates::Templates>, Subscription)>,
    pub group_dialog: Option<(Entity<krill_app::bots::BotGroupMembersDialog>, Subscription)>,
    /// Which host the currently open New/Manage group dialog targets.
    pub group_dialog_host: Option<HostId>,
    /// Bot waiting for "Delete bot" confirmation.
    pub delete: Option<SidebarBot>,
    /// Group waiting for "Delete group" confirmation.
    pub delete_group: Option<(HostId, krill_proto::bots::BotGroupChat)>,
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
            let target = self.target_for_host(host);
            if host.is_remote() && target.is_none() {
                return None;
            }
            Some(krill_app::bots::daemon_bot_rpc_for_host(host, target))
        }
    }

    pub(super) fn host_display_label(&self, host: &HostId, cx: &App) -> SharedString {
        match host {
            HostId::Local => text::HOST_LOCAL
                .mac_or(krill_app::i18n::catalog::other_os::HOST_LOCAL)
                .get()
                .into(),
            HostId::Remote(id) => self.remote_host_name(id, cx).into(),
        }
    }

    pub(super) fn bot_host_label(&self, cx: &App) -> SharedString {
        self.host_display_label(&self.launch_host, cx)
    }

    /// `<state>/bots/<slug>` for bots on `bot.host` (local only).
    pub(super) fn bot_folder_on(host: &HostId, bot: &SidebarBot) -> Option<std::path::PathBuf> {
        host.is_local()
            .then(krill_proto::daemon::state_dir)
            .flatten()
            .map(|state| state.join("bots").join(&bot.slug))
    }

    /// `<state>/bots/<slug>` for `bot` (local only).
    #[expect(
        clippy::unused_self,
        reason = "keeps consistent shell method call sites across submodules"
    )]
    pub(super) fn bot_folder(&self, bot: &SidebarBot) -> Option<std::path::PathBuf> {
        Self::bot_folder_on(&bot.host, bot)
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
        self.sync_bot_sidebar_ephemeral(window, cx);
        let Some(request) = self.bot_ui.pending.take() else {
            return;
        };
        match request {
            BotDialogRequest::NewBot { template, purpose } => {
                let hosts = self.new_bot_hosts(cx);
                if hosts.is_empty() {
                    return;
                }
                // Dialogs open over the workspace like Orca's; the Bots modal makes room.
                self.close_settings(Some(window), cx);
                let taken: Vec<String> = self.sidebar_bots.iter().map(|b| b.name.clone()).collect();
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
                // Templates are static built-in metadata and do not require the launch host RPC.
                self.close_settings(Some(window), cx);
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
                if *host != self.launch_host {
                    self.set_launch_host(host.clone(), cx);
                }
                if let Some(err) = warning {
                    self.notice = Some(err.clone());
                }
                // Orca expands the section so the new bot's row is visible and active.
                self.bot_ui.collapsed = false;
                self.bot_ui.folded.remove(&(host.clone(), bot.id.clone()));
                self.bot_ui.panel.settings = None;
                self.bot_ui.panel.stale = true;
                let row = krill_app::bots::rpc::sidebar_snapshot_on(
                    host,
                    std::slice::from_ref(bot.as_ref()),
                    &[],
                    &[],
                    &[],
                )
                .bots;
                let entry = self.bot_ui.hosts.entry(host.clone()).or_default();
                entry.probed = true;
                for r in row {
                    if !entry.snapshot.bots.iter().any(|b| b.id == r.id) {
                        entry.snapshot.bots.insert(0, r.clone());
                    }
                    if !self
                        .sidebar_bots
                        .iter()
                        .any(|b| &b.host == host && b.id == r.id)
                    {
                        self.sidebar_bots.insert(0, r);
                    }
                }
                // No kickoff turn: the chat opens empty and the bot first answers its owner's
                // first message (its instructions ride in the agent's system prompt).
                self.open_bot_home_on(host, bot.id.clone(), None, cx);
                self.refresh_bots_for_host(host, true, cx);
                cx.notify();
            }
        }
    }

    /// Opens a bot's Home Chat on `host`: focuses its live session, or starts one (with `kickoff`
    /// as the first turn for a bot that was just made).
    pub(super) fn open_bot_home_on(
        &mut self,
        host: &HostId,
        bot_id: BotId,
        kickoff: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(live) = self
            .sidebar_bots
            .iter()
            .find(|b| &b.host == host && b.id == bot_id)
            .and_then(SidebarBot::home_session)
        {
            let session = live.session_id;
            if let Some(draft) = kickoff {
                self.bot_ui.panel.pending_draft = Some((host.clone(), bot_id.clone(), draft));
            }
            self.open_bot_session_on(host, &bot_id, session, cx);
            return;
        }
        self.start_bot_session_on(host, bot_id, kickoff, cx);
    }

    /// Starts a new chat for a bot on `host` (with `kickoff` as its first turn) and opens it.
    pub(super) fn start_bot_session_on(
        &mut self,
        host: &HostId,
        bot_id: BotId,
        kickoff: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.start_bot_session_in_on(host, bot_id, kickoff, None, cx);
    }

    /// Starts a new chat for a bot on `host`, filling `target_pane` in place when present.
    pub(super) fn start_bot_session_in_on(
        &mut self,
        host: &HostId,
        bot_id: BotId,
        kickoff: Option<String>,
        target_pane: Option<krill_app::layout::PaneId>,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(host) else {
            if let Some(pane) = target_pane {
                self.clear_split_launcher_launching(pane);
            }
            return;
        };
        let bot = self
            .sidebar_bots
            .iter()
            .find(|b| &b.host == host && b.id == bot_id);
        // krilld won't start a paused bot; say so in the app's words.
        if bot.is_some_and(|b| b.paused) && kickoff.is_none() {
            if let Some(pane) = target_pane {
                self.clear_split_launcher_launching(pane);
            }
            self.notice = Some(text::OPEN_CHAT_PAUSED_TOOLTIP.get().to_owned());
            cx.notify();
            return;
        }
        let opening_key = (host.clone(), bot_id.clone());
        if !self.bot_ui.opening.insert(opening_key.clone()) {
            if let Some(pane) = target_pane {
                self.clear_split_launcher_launching(pane);
            }
            return;
        }
        let name = bot.map(|b| b.name.clone()).unwrap_or_default();
        let host = host.clone();
        let bot_meta = bot.map(SidebarBot::session_meta);
        let bot_folder = bot.and_then(|b| Self::bot_folder_on(&host, b));
        let host_name: Option<String> = match &host {
            HostId::Local => Some(krill_app::bots::rpc::local_host_label().to_owned()),
            HostId::Remote(id) => {
                let label = self.remote_host_name(id, cx);
                (!label.trim().is_empty() && label != *id).then_some(label)
            }
        };
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
                this.bot_ui.opening.remove(&opening_key);
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
                        let new_summary = krill_proto::bots::BotSessionSummary {
                            session_id,
                            bot_id: bot_id.clone(),
                            link_id: None,
                            pid,
                            live: true,
                            working: false,
                            label: String::new(),
                            created_at_ms: now_secs().saturating_mul(1000),
                        };
                        if let Some(row) = this
                            .sidebar_bots
                            .iter_mut()
                            .find(|b| b.host == host && b.id == bot_id)
                        {
                            // krilld lists the Home Chat first, then newest.
                            let at = usize::from(row.home_session().is_some());
                            row.sessions.insert(at, new_summary.clone());
                        }
                        if let Some(entry) = this.bot_ui.hosts.get_mut(&host)
                            && let Some(row) =
                                entry.snapshot.bots.iter_mut().find(|b| b.id == bot_id)
                        {
                            let at = usize::from(row.home_session().is_some());
                            row.sessions.insert(at, new_summary);
                        }
                        if let Some(meta) = bot_meta {
                            this.sidebar.update(cx, |sb, cx| {
                                sb.register_bot_session(host.clone(), session_id, meta, cx);
                            });
                        }
                        if let Some(pane) = target_pane.filter(|p| this.layout.pane_exists(*p)) {
                            this.open_bot_session_in_pane_on(
                                &host, &bot_id, session_id, pane, bot_folder, cx,
                            );
                        } else {
                            this.open_bot_session_on(&host, &bot_id, session_id, cx);
                        }
                        this.refresh_bots_for_host(&host, true, cx);
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
        let bot = self
            .sidebar_bots
            .iter()
            .find(|b| &b.host == host && &b.id == bot_id)
            .cloned();
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
    #[cfg(all(test, unix))]
    pub(super) fn open_bot_session(
        &mut self,
        bot_id: &BotId,
        session: krill_proto::SessionId,
        cx: &mut Context<Self>,
    ) {
        let host = self.launch_host.clone();
        self.open_bot_session_on(&host, bot_id, session, cx);
    }

    pub(super) fn open_bot_session_on(
        &mut self,
        host: &HostId,
        bot_id: &BotId,
        session: krill_proto::SessionId,
        cx: &mut Context<Self>,
    ) {
        use krill_proto::os::ffi::OsStrExt as _;
        let bot = self
            .sidebar_bots
            .iter()
            .find(|b| &b.host == host && &b.id == bot_id)
            .cloned();
        if let Some(meta) = bot.as_ref().map(SidebarBot::session_meta) {
            self.sidebar.update(cx, |sb, cx| {
                sb.register_bot_session(host.clone(), session, meta, cx);
            });
        }
        let known_folder = self
            .sidebar
            .read(cx)
            .folder_on(host, session)
            .map(std::path::Path::to_path_buf);
        let bot_folder = bot
            .as_ref()
            .and_then(|b| Self::bot_folder_on(host, b))
            .or_else(|| {
                bot.as_ref()
                    .and_then(|b| b.workspace.as_deref())
                    .map(std::path::PathBuf::from)
            });
        let deferred = self.layout_loading;
        self.open_session_on(host.clone(), session, cx);
        if deferred {
            self.restore_session_chat = true;
        } else if let Some(pane) = self.layout.focused() {
            if known_folder.is_none()
                && let Some(folder) = bot_folder
                && self.layout.session(pane) == Some(session)
                && self.layout.host(pane) == host
            {
                self.change(true, cx, |l| {
                    let mut launch =
                        l.launch(pane)
                            .cloned()
                            .unwrap_or(krill_proto::control::Create {
                                account: None,
                                program: krill_proto::control::Program::Login,
                                cwd: Vec::new(),
                                cols: krill_app::pane::SIZE.0,
                                rows: krill_app::pane::SIZE.1,
                            });
                    launch.cwd = folder.as_os_str().as_bytes().to_vec();
                    l.set_launch(pane, launch);
                });
                self.sync_diff_workspace(cx);
            }
            self.set_pane_chat_view(pane, true, cx);
        }
    }

    pub(super) fn set_bot_paused_on(
        &mut self,
        host: &HostId,
        bot_id: BotId,
        paused: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(host) else {
            return;
        };
        let host_owned = host.clone();
        // Optimistic, like the rest of the sidebar; the poll below corrects a refusal.
        if let Some(b) = self
            .sidebar_bots
            .iter_mut()
            .find(|b| b.host == host_owned && b.id == bot_id)
        {
            b.paused = paused;
        }
        if let Some(entry) = self.bot_ui.hosts.get_mut(&host_owned)
            && let Some(b) = entry.snapshot.bots.iter_mut().find(|b| b.id == bot_id)
        {
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
                    cx.notify();
                }
                this.refresh_bots_for_host(&host_owned, true, cx);
            })
            .ok();
        })
        .detach();
    }

    fn close_panes_for_bot(
        &mut self,
        host: &HostId,
        sessions: &HashSet<krill_proto::SessionId>,
        cx: &mut Context<Self>,
    ) {
        if sessions.is_empty() {
            return;
        }
        let matching: Vec<krill_app::layout::PaneId> = self
            .layout
            .tabs()
            .iter()
            .flat_map(|t| {
                let mut leaves = Vec::new();
                t.root.leaves(&mut leaves);
                leaves
            })
            .filter(|p| {
                self.layout.host(*p) == host
                    && self
                        .layout
                        .session(*p)
                        .is_some_and(|sid| sessions.contains(&sid))
            })
            .collect();
        if matching.is_empty() {
            return;
        }
        self.change(true, cx, |l| {
            for pane in matching {
                if let Some(tab_idx) = l.tab_of_pane(pane) {
                    if l.tab_panes(tab_idx).len() <= 1 {
                        l.close_tab(tab_idx);
                    } else {
                        l.focus(pane);
                        l.close_pane();
                    }
                }
            }
        });
    }

    /// Orca's "Delete bot": removes the bot and keeps its folder (soul and memory stay).
    pub(super) fn delete_bot_keep_folder_on(
        &mut self,
        host: &HostId,
        bot_id: BotId,
        cx: &mut Context<Self>,
    ) {
        let Some(rpc) = self.bot_rpc_on(host) else {
            return;
        };
        let host_owned = host.clone();
        let mut sessions: HashSet<krill_proto::SessionId> = self
            .sidebar_bots
            .iter()
            .filter(|b| b.host == host_owned && b.id == bot_id)
            .flat_map(|b| b.sessions.iter().map(|s| s.session_id))
            .collect();
        for ((h, sid), cached) in &self.chat.cached_bots {
            if h == &host_owned && cached.id == bot_id {
                sessions.insert(*sid);
            }
        }
        let focused_deleted = self.layout.focused().is_some_and(|pane| {
            self.layout.host(pane) == &host_owned
                && self
                    .layout
                    .session(pane)
                    .is_some_and(|sid| sessions.contains(&sid))
        });

        self.bot_ui.delete = None;
        self.sidebar_bots
            .retain(|b| !(b.host == host_owned && b.id == bot_id));
        if let Some(entry) = self.bot_ui.hosts.get_mut(&host_owned) {
            entry.snapshot.bots.retain(|b| b.id != bot_id);
        }
        self.chat.cached_bots.retain(|(h, sid), b| {
            !(h == &host_owned && (b.id == bot_id || sessions.contains(sid)))
        });
        let key = (host_owned.clone(), bot_id.clone());
        self.bot_ui.avatars.remove(&key);
        self.bot_ui.folded.remove(&key);
        self.bot_ui.up_next_unseen.remove(&key);
        if self
            .bot_ui
            .renaming
            .as_ref()
            .is_some_and(|(h, id, _, _)| h == &host_owned && id == &bot_id)
        {
            self.bot_ui.renaming = None;
        }
        if focused_deleted
            || self
                .bot_ui
                .panel
                .shown
                .as_ref()
                .is_some_and(|(h, id, _)| h == &host_owned && id == &bot_id)
        {
            self.bot_ui.panel.shown = None;
            self.bot_ui.panel.settings = None;
        }
        if self
            .bot_ui
            .panel
            .pending_draft
            .as_ref()
            .is_some_and(|(h, id, _)| h == &host_owned && id == &bot_id)
        {
            self.bot_ui.panel.pending_draft = None;
        }
        self.bot_ui.panel.stale = true;
        self.close_panes_for_bot(&host_owned, &sessions, cx);
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
                    cx.notify();
                }
                this.refresh_bots_for_host(&host_owned, true, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn toggle_bot_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bot_ui.search.take().is_some() {
            self.bot_ui.query.clear();
            self.bot_ui.search_lang = None;
        } else {
            let lang = krill_app::i18n::language();
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(text::SB_SEARCH.get()));
            let sub = cx.subscribe(&input, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.bot_ui.query = input.read(cx).value().to_string();
                    cx.notify();
                }
            });
            input.update(cx, |s, cx| s.focus(window, cx));
            self.bot_ui.search = Some((input, sub));
            self.bot_ui.search_lang = Some(lang);
        }
        cx.notify();
    }

    /// Keeps the sidebar's search placeholder in sync after a live language switch (Orca #46226)
    /// and triggers a lightweight suggestion-count fetch for `BotUpNextCount`.
    pub(super) fn sync_bot_sidebar_ephemeral(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lang = krill_app::i18n::language();
        if let Some((input, _)) = &self.bot_ui.search
            && self.bot_ui.search_lang != Some(lang)
        {
            input.update(cx, |s, cx| {
                s.set_placeholder(text::SB_SEARCH.get(), window, cx);
            });
            self.bot_ui.search_lang = Some(lang);
        }
        let state_key: Vec<(HostId, BotId, bool)> = self
            .sidebar_bots
            .iter()
            .map(|b| (b.host.clone(), b.id.clone(), b.paused))
            .collect();
        if state_key != self.bot_ui.up_next_bots_key {
            self.bot_ui.up_next_unseen.retain(|(h, id), _| {
                state_key
                    .iter()
                    .any(|(kh, kid, paused)| kh == h && kid == id && !paused)
            });
            self.bot_ui.up_next_bots_key = state_key;
            self.refresh_bot_up_next(cx);
        }
    }

    /// Reads non-dismissed suggestions newer than `up_next_seen_ms` per bot (`BotUpNextCount`).
    pub(super) fn refresh_bot_up_next(&mut self, cx: &mut Context<Self>) {
        self.bot_ui.up_next_gen = self.bot_ui.up_next_gen.wrapping_add(1);
        let generation = self.bot_ui.up_next_gen;
        let mut by_host: Vec<(HostId, krill_app::bots::BotRpcCall, Vec<BotId>)> = Vec::new();
        for b in self.sidebar_bots.iter().filter(|b| !b.paused) {
            if let Some((_, _, ids)) = by_host.iter_mut().find(|(h, _, _)| h == &b.host) {
                ids.push(b.id.clone());
            } else if let Some(rpc) = self.bot_rpc_on(&b.host) {
                by_host.push((b.host.clone(), rpc, vec![b.id.clone()]));
            }
        }
        if by_host.is_empty() {
            self.bot_ui.up_next_task = None;
            let active_unpaused: HashSet<(&HostId, &BotId)> = self
                .sidebar_bots
                .iter()
                .filter(|b| !b.paused)
                .map(|b| (&b.host, &b.id))
                .collect();
            let before = self.bot_ui.up_next_unseen.len();
            self.bot_ui
                .up_next_unseen
                .retain(|(h, id), _| active_unpaused.contains(&(h, id)));
            if self.bot_ui.up_next_unseen.len() != before {
                cx.notify();
            }
            return;
        }
        self.bot_ui.up_next_task = Some(cx.spawn(async move |this, cx| {
            let counts: Vec<((HostId, BotId), Option<usize>)> = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::new();
                    for (host, rpc, bot_ids) in by_host {
                        for bot_id in bot_ids {
                            let seen_ms = match krill_app::bots::rpc::call_checked(
                                &rpc,
                                &BotRpcRequest::GetBotUpcoming {
                                    bot_id: bot_id.clone(),
                                },
                            ) {
                                Ok(BotRpcResponse::Upcoming(u)) => u.up_next_seen_ms,
                                _ => 0,
                            };
                            let unseen = match krill_app::bots::rpc::call_checked(
                                &rpc,
                                &BotRpcRequest::ListSuggestions {
                                    bot_id: Some(bot_id.clone()),
                                    include_dismissed: false,
                                },
                            ) {
                                Ok(BotRpcResponse::Suggestions(rows)) => Some(
                                    rows.iter()
                                        .filter(|s| !s.dismissed && s.created_at_ms > seen_ms)
                                        .count(),
                                ),
                                _ => None,
                            };
                            out.push(((host.clone(), bot_id), unseen));
                        }
                    }
                    out
                })
                .await;
            this.update(cx, |this, cx| {
                if this.bot_ui.up_next_gen != generation {
                    return;
                }
                this.bot_ui.up_next_task = None;
                let mut changed = false;
                for (key, count) in counts {
                    let live_unpaused = this
                        .sidebar_bots
                        .iter()
                        .any(|b| b.host == key.0 && b.id == key.1 && !b.paused);
                    if !live_unpaused {
                        if this.bot_ui.up_next_unseen.remove(&key).is_some() {
                            changed = true;
                        }
                        continue;
                    }
                    let Some(count) = count else {
                        continue;
                    };
                    if count == 0 {
                        if this.bot_ui.up_next_unseen.remove(&key).is_some() {
                            changed = true;
                        }
                    } else if this.bot_ui.up_next_unseen.insert(key, count) != Some(count) {
                        changed = true;
                    }
                }
                if changed {
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Decodes newly read `avatar.png` files for `host` (empty = unreadable; remembered so the
    /// poll doesn't fetch it again until the bot changes) and drops pictures of deleted bots.
    pub(super) fn store_bot_avatars_for_host(
        &mut self,
        host: &HostId,
        avatars: Vec<(BotId, u64, Vec<u8>)>,
    ) {
        for (id, at, png) in avatars {
            let img = krill_app::bots::rpc::decode_png_avatar(&png).ok();
            self.bot_ui.avatars.insert((host.clone(), id), (at, img));
        }
        let live: HashSet<&BotId> = self
            .bot_ui
            .hosts
            .get(host)
            .into_iter()
            .flat_map(|h| h.snapshot.bots.iter().map(|b| &b.id))
            .collect();
        self.bot_ui
            .avatars
            .retain(|(h, id), _| h != host || live.contains(id));
    }

    pub(super) fn known_bot_avatars_for_host(&self, host: &HostId) -> HashMap<BotId, u64> {
        self.bot_ui
            .avatars
            .iter()
            .filter(|((h, _), _)| h == host)
            .map(|((_, id), (at, _))| (id.clone(), *at))
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
            .get(&(bot.host.clone(), bot.id.clone()))
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
        if let Some((host, group)) = self.bot_ui.delete_group.clone() {
            out.push(self.render_group_delete(&host, &group, window, cx));
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
        let host = bot.host.clone();
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
                                this.delete_bot_keep_folder_on(&host, id.clone(), cx);
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
