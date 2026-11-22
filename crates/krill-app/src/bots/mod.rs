// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Krill Bots modal UI (Phases 1-3 + Orca v2 parity, STYLEGUIDE-compliant).
//!
//! Opens as a single modal over the main workspace window via `ui::modal_backdrop` +
//! `ui::page_modal_size`, matching `Automations` and `Artifacts`. Active bot chats open
//! as regular workspace `[Chat | Term]` tabs via [`OpenSession`].

pub mod avatar;
mod connector_logos;
mod connectors;
mod connectors_panel;
mod groups;
mod home;
mod home_item;
mod home_item_parts;
mod home_needs;
mod home_select;
mod home_snooze;
mod home_time;
mod home_upcoming;
mod items;
pub mod new_bot;
mod panel;
pub mod rpc;
mod settings;
mod settings_pages;
mod settings_root;
mod settings_soul;
pub mod sidebar_view;
mod soul_memory;
pub mod templates;
mod tools_sharing;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui::{
    AnyWindowHandle, Context, Entity, EntityInputHandler as _, EventEmitter, FocusHandle,
    Focusable as _, FontWeight, KeyDownEvent, SharedString, Task, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::InputState;
use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_proto::SessionId;
use krill_proto::bots::{
    BotAccessRequest, BotAccessRequestStatus, BotConfig, BotConnectorStatus, BotId, BotItem,
    BotItemStatus, BotRpcRequest, BotRpcResponse, BotSessionSummary, ConnectorHealthState,
    ExternalSource,
};

use crate::hosts::{HostId, HostTarget};
use crate::i18n::catalog::bots as text;
use crate::ui;

pub use home::HomeFilter;

pub use rpc::{
    BOT_RUNTIMES, BotRpcCall, BotRuntimeOption, BotSessionMeta, SidebarBot, SidebarSnapshot,
    SidebarState, daemon_bot_rpc, fetch_sidebar_state,
};
use rpc::{call_checked, decode_png_avatar, join_errors, unsupported_version};
pub use settings_root::{OpenSettingsPage, RequestDeleteBot};

/// Request the host `Shell` to close the Bots modal.
#[derive(Debug)]
pub struct Close;

/// Request the host `Shell` to open Settings > Remote, where an outdated computer's Krill updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenRemoteSettings;

/// Request the host `Shell` to open the New bot dialog (Orca's single creation path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestNewBot;

/// Request the host `Shell` to open/focus a bot's live session in a workspace `[Chat | Term]` tab.
#[derive(Debug, Clone)]
pub struct OpenSession {
    pub bot_id: BotId,
    pub session_id: SessionId,
}

/// Open the bot's Home Chat with `text` in its composer, to finish and send (Orca's "New
/// automation" draft).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftInHomeChat {
    pub bot_id: BotId,
    pub text: String,
}

/// Notify the host `Shell` when bots or their attention counts (`needs_you` + pending access) change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgeChanged {
    pub attention_count: usize,
    /// Sidebar Bots section rows, built once per change.
    pub bots: Vec<SidebarBot>,
}

/// Notify the host `Shell` when cached `SessionId -> BotSessionMeta` mappings update.
#[derive(Debug, Clone)]
pub struct SessionsChanged {
    pub host: HostId,
    pub sessions: BTreeMap<SessionId, BotSessionMeta>,
}

/// Active sub-tab inside a bot's detail view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Items,
    SoulMemory,
    Connectors,
    ToolsSharing,
    Settings,
}

/// Active right-pane screen inside the Bots modal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Screen {
    #[default]
    Detail,
    Import,
    Groups,
}

#[derive(Default)]
struct ChatsState {
    expanded: bool,
    loading: bool,
    opening: bool,
}

pub struct View {
    focus: FocusHandle,
    window_handle: Option<AnyWindowHandle>,
    rpc: BotRpcCall,
    rpc_is_mock: bool,
    host: HostId,
    target: Option<HostTarget>,
    available_hosts: Vec<(HostId, String, Option<HostTarget>)>,
    default_cwd: Option<PathBuf>,
    bots: Vec<BotConfig>,
    /// When `ListBots` last answered. Until then this view knows nothing about the bot list
    /// and must not report one (an empty list would wipe the sidebar's bots).
    bots_loaded_at_ms: Option<u64>,
    selected_bot_id: Option<BotId>,
    pending_switch_bot: Option<BotId>,
    screen: Screen,
    detail_tab: DetailTab,
    loading: bool,
    busy: bool,
    chats: ChatsState,
    error: Option<SharedString>,
    /// This host's krilld predates bots: its version (`""` when it didn't say).
    unsupported: Option<SharedString>,
    notice: Option<SharedString>,
    armed_delete_bot: Option<BotId>,
    all_open_items: Vec<BotItem>,
    all_connector_statuses: Vec<BotConnectorStatus>,
    all_access_requests: Vec<BotAccessRequest>,
    bot_sessions: Vec<BotSessionSummary>,
    items: items::ItemsState,
    soul_memory: soul_memory::SoulMemoryState,
    connectors: connectors::ConnectorsState,
    tools: tools_sharing::ToolsState,
    import_state: tools_sharing::ImportState,
    settings: settings::SettingsState,
    groups: groups::GroupsState,
    load_gen: u64,
    detail_gen: u64,
    load_task: Option<Task<()>>,
    detail_task: Option<Task<()>>,
    open_chat_task: Option<Task<()>>,
    tidy_task: Option<Task<()>>,
    look_task: Option<Task<()>>,
    import_task: Option<Task<()>>,
    mutation_task: Option<Task<()>>,
    sessions_task: Option<Task<()>>,
    item_task: Option<Task<()>>,
    memory_task: Option<Task<()>>,
    connector_task: Option<Task<()>>,
    tools_task: Option<Task<()>>,
    group_task: Option<Task<()>>,
    group_poll_task: Option<Task<()>>,
    oauth_poll_task: Option<Task<()>>,
    avatar_task: Option<Task<()>>,
    language: crate::i18n::Language,
    /// Drawn as the modal or as a bot page's right-sidebar body (see `panel.rs`).
    mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Modal,
    Panel,
}

impl EventEmitter<Close> for View {}
impl EventEmitter<RequestNewBot> for View {}
impl EventEmitter<OpenRemoteSettings> for View {}
impl EventEmitter<OpenSession> for View {}
impl EventEmitter<DraftInHomeChat> for View {}
impl EventEmitter<BadgeChanged> for View {}
impl EventEmitter<SessionsChanged> for View {}
impl EventEmitter<OpenSettingsPage> for View {}
impl EventEmitter<RequestDeleteBot> for View {}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotsView")
            .field("host", &self.host)
            .field("bots", &self.bots.len())
            .field("selected_bot_id", &self.selected_bot_id)
            .field("screen", &self.screen)
            .field("detail_tab", &self.detail_tab)
            .finish_non_exhaustive()
    }
}

impl View {
    /// Creates a new Bots modal connected to the local Krill daemon (or custom target).
    pub fn new(
        target: Option<HostTarget>,
        default_cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_on_host(HostId::Local, target, default_cwd, cx)
    }

    /// Creates a new Bots modal bound to a specific [`HostId`] and [`HostTarget`].
    pub fn new_on_host(
        host: HostId,
        target: Option<HostTarget>,
        default_cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::build_with_rpc(
            daemon_bot_rpc(target.clone()),
            false,
            host,
            target,
            default_cwd,
            cx,
        );
        view.reload(cx);
        view
    }

    /// Creates a Bots modal backed by an injectable RPC handler (used by tests).
    pub fn with_rpc(rpc: BotRpcCall, default_cwd: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut view = Self::build_with_rpc(rpc, true, HostId::Local, None, default_cwd, cx);
        view.reload(cx);
        view
    }

    fn build_with_rpc(
        rpc: BotRpcCall,
        rpc_is_mock: bool,
        host: HostId,
        target: Option<HostTarget>,
        default_cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            window_handle: None,
            rpc,
            rpc_is_mock,
            host,
            target,
            available_hosts: Vec::new(),
            default_cwd,
            bots: Vec::new(),
            bots_loaded_at_ms: None,
            selected_bot_id: None,
            pending_switch_bot: None,
            screen: Screen::Detail,
            detail_tab: DetailTab::Items,
            loading: false,
            busy: false,
            chats: ChatsState::default(),
            error: None,
            unsupported: None,
            notice: None,
            armed_delete_bot: None,
            all_open_items: Vec::new(),
            all_connector_statuses: Vec::new(),
            all_access_requests: Vec::new(),
            bot_sessions: Vec::new(),
            items: items::ItemsState::default(),
            soul_memory: soul_memory::SoulMemoryState::default(),
            connectors: connectors::ConnectorsState::default(),
            tools: tools_sharing::ToolsState::default(),
            import_state: tools_sharing::ImportState::default(),
            settings: settings::SettingsState::default(),
            groups: groups::GroupsState::default(),
            load_gen: 0,
            detail_gen: 0,
            load_task: None,
            detail_task: None,
            open_chat_task: None,
            tidy_task: None,
            look_task: None,
            import_task: None,
            mutation_task: None,
            sessions_task: None,
            item_task: None,
            memory_task: None,
            connector_task: None,
            tools_task: None,
            group_task: None,
            group_poll_task: None,
            oauth_poll_task: None,
            avatar_task: None,
            language: crate::i18n::language(),
            mode: Mode::Modal,
        }
    }

    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    #[must_use]
    pub const fn screen(&self) -> Screen {
        self.screen
    }

    #[must_use]
    pub const fn detail_tab(&self) -> DetailTab {
        self.detail_tab
    }

    #[must_use]
    pub fn bots(&self) -> &[BotConfig] {
        &self.bots
    }

    #[must_use]
    pub fn host(&self) -> &HostId {
        &self.host
    }

    #[must_use]
    pub fn selected_bot(&self) -> Option<&BotConfig> {
        let id = self.selected_bot_id.as_ref()?;
        self.bots.iter().find(|b| &b.id == id)
    }

    #[must_use]
    pub fn session_meta_for(&self, bot_id: &BotId) -> Option<BotSessionMeta> {
        self.bots
            .iter()
            .find(|b| &b.id == bot_id)
            .map(BotSessionMeta::from_bot)
    }

    /// Updates available hosts and switches the daemon RPC target when viewing a different host.
    pub fn set_host_target(
        &mut self,
        host: HostId,
        target: Option<HostTarget>,
        hosts: Vec<(HostId, String, Option<HostTarget>)>,
        cx: &mut Context<Self>,
    ) {
        self.available_hosts = hosts;
        if self.rpc_is_mock {
            cx.notify();
            return;
        }
        let target_changed =
            self.host != host || (target.is_some() && self.target.as_ref() != target.as_ref());
        self.host = host;
        self.target.clone_from(&target);
        if target_changed {
            self.rpc = daemon_bot_rpc(target);
            self.unsupported = None;
            self.selected_bot_id = None;
            self.bots.clear();
            self.bots_loaded_at_ms = None;
            self.bot_sessions.clear();
            self.settings.avatar_cache.clear();
            self.reload(cx);
        } else {
            cx.notify();
        }
    }

    /// The current host's name as the host list shows it.
    fn host_label(&self) -> String {
        self.available_hosts
            .iter()
            .find(|(id, _, _)| id == &self.host)
            .map(|(_, label, _)| label.clone())
            .or_else(|| self.host.remote_id().map(str::to_owned))
            .unwrap_or_else(|| text::HOST_LOCAL.get().to_owned())
    }

    /// Banner text for failed loads: each message once, naming the host when there are several.
    fn load_error(&self, errors: &[String]) -> SharedString {
        let joined = join_errors(errors);
        if self.available_hosts.len() > 1 {
            format!("{}: {joined}", self.host_label()).into()
        } else {
            joined.into()
        }
    }

    #[must_use]
    pub fn attention_count(&self) -> usize {
        self.sidebar_snapshot().attention
    }

    pub fn on_hide(&mut self, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.soul_memory.armed_forget = None;
        self.pending_switch_bot = None;
        self.group_poll_task = None;
        self.oauth_poll_task = None;
        cx.notify();
    }

    pub fn reopen(&mut self, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.soul_memory.armed_forget = None;
        self.pending_switch_bot = None;
        self.error = None;
        self.notice = None;
        if !self.bots.is_empty() {
            self.screen = Screen::Detail;
        }
        self.reload(cx);
    }

    /// Rows and attention total for the main window sidebar's Bots section.
    #[must_use]
    pub fn sidebar_snapshot(&self) -> SidebarSnapshot {
        rpc::sidebar_snapshot(
            &self.bots,
            &self.all_open_items,
            &self.all_access_requests,
            &self.bot_sessions,
        )
    }

    pub(super) fn emit_badge(&self, cx: &mut Context<Self>) {
        if self.bots_loaded_at_ms.is_none() {
            return;
        }
        let snapshot = self.sidebar_snapshot();
        cx.emit(BadgeChanged {
            attention_count: snapshot.attention,
            bots: snapshot.bots,
        });
    }

    pub(super) fn emit_sessions_changed(&self, cx: &mut Context<Self>) {
        if self.bots_loaded_at_ms.is_none() {
            return;
        }
        let mut sessions = BTreeMap::new();
        for summary in &self.bot_sessions {
            if let Some(meta) = self.session_meta_for(&summary.bot_id) {
                sessions.insert(summary.session_id, meta);
            }
        }
        cx.emit(SessionsChanged {
            host: self.host.clone(),
            sessions,
        });
    }

    #[must_use]
    pub(super) fn sessions_for_bot(&self, bot_id: &BotId) -> Vec<&BotSessionSummary> {
        self.bot_sessions
            .iter()
            .filter(|s| &s.bot_id == bot_id)
            .collect()
    }

    /// Loads bots list, open items, connector statuses, access requests, tool relay status, and sessions.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load_gen = self.load_gen.wrapping_add(1);
        let generation = self.load_gen;
        let rpc = self.rpc.clone();
        self.loading = true;
        self.error = None;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let bots = match call_checked(&rpc, &BotRpcRequest::ListBots)? {
                        BotRpcResponse::Bots(list) => list,
                        other => return Err(format!("unexpected ListBots response: {other:?}")),
                    };
                    let mut warnings = Vec::new();
                    let open_items = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListItems {
                            bot_id: None,
                            include_closed: false,
                        },
                    ) {
                        Ok(BotRpcResponse::Items(list)) => list,
                        Ok(BotRpcResponse::Ok) => Vec::new(),
                        Ok(other) => {
                            warnings.push(format!("ListItems: {other:?}"));
                            Vec::new()
                        }
                        Err(e) => {
                            warnings.push(e);
                            Vec::new()
                        }
                    };
                    let statuses = match call_checked(
                        &rpc,
                        &BotRpcRequest::ConnectorStatus { bot_id: None },
                    ) {
                        Ok(BotRpcResponse::ConnectorStatuses(list)) => list,
                        Ok(BotRpcResponse::Ok) => Vec::new(),
                        Ok(other) => {
                            warnings.push(format!("ConnectorStatus: {other:?}"));
                            Vec::new()
                        }
                        Err(e) => {
                            warnings.push(e);
                            Vec::new()
                        }
                    };
                    let access = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListAccessRequests { bot_id: None },
                    ) {
                        Ok(BotRpcResponse::AccessRequests(list)) => list,
                        Ok(BotRpcResponse::Ok) => Vec::new(),
                        Ok(other) => {
                            warnings.push(format!("ListAccessRequests: {other:?}"));
                            Vec::new()
                        }
                        Err(e) => {
                            warnings.push(e);
                            Vec::new()
                        }
                    };
                    let relay = match call_checked(&rpc, &BotRpcRequest::ToolRelayStatus) {
                        Ok(BotRpcResponse::ToolRelayStatus(st)) => Some(st),
                        Ok(_) => None,
                        Err(e) => {
                            warnings.push(e);
                            None
                        }
                    };
                    let sessions = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListBotSessions { bot_id: None },
                    ) {
                        Ok(BotRpcResponse::BotSessions(list)) => Some(list),
                        Ok(_) => None,
                        Err(e) => {
                            warnings.push(e);
                            None
                        }
                    };
                    Ok::<_, String>((
                        bots, open_items, statuses, access, relay, sessions, warnings,
                    ))
                })
                .await;

            this.update(cx, |this, cx| {
                if this.load_gen != generation {
                    return;
                }
                this.loading = false;
                match outcome {
                    Ok((bots, open_items, statuses, access, relay, sessions, warnings)) => {
                        this.unsupported = None;
                        this.bots = bots;
                        this.bots_loaded_at_ms = Some(now_unix_ms());
                        this.all_open_items = open_items;
                        this.all_connector_statuses = statuses;
                        this.all_access_requests = access;
                        if let Some(st) = relay {
                            this.tools.relay_status = Some(st);
                        }
                        if let Some(list) = sessions {
                            this.bot_sessions = list;
                        }
                        if !warnings.is_empty() {
                            this.error = Some(this.load_error(&warnings));
                        }
                        let still_valid = this
                            .selected_bot_id
                            .as_ref()
                            .is_some_and(|id| this.bots.iter().any(|b| &b.id == id));
                        // A bot page keeps its bot (an empty body beats another bot's).
                        if !still_valid && this.mode == Mode::Modal {
                            this.selected_bot_id = this.bots.first().map(|b| b.id.clone());
                        }
                        this.emit_badge(cx);
                        this.emit_sessions_changed(cx);
                        if let Some(bot_id) = this.selected_bot_id.clone() {
                            this.load_selected_bot_detail(bot_id, cx);
                        }
                    }
                    // An older krilld without bots is a state to explain, not an error.
                    Err(err) => match unsupported_version(&err) {
                        Some(version) => {
                            this.unsupported = Some(version.to_owned().into());
                            this.bots.clear();
                        }
                        None => this.error = Some(this.load_error(&[err])),
                    },
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn select_bot(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.selected_bot_id.as_ref() != Some(&bot_id) && self.soul_memory.has_unsaved_edits(cx)
        {
            self.pending_switch_bot = Some(bot_id);
            cx.notify();
            return;
        }
        self.confirm_select_bot(bot_id, cx);
    }

    pub fn confirm_select_bot(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.soul_memory.armed_forget = None;
        self.soul_memory.discard_unsaved_edits();
        self.soul_memory.selected_topic = None;
        self.pending_switch_bot = None;
        self.error = None;
        self.notice = None;
        if self.selected_bot_id.as_ref() != Some(&bot_id) {
            // Home's open item, filter and folds are the last bot's.
            self.items.open_uid = None;
            self.items.needs_you_only = false;
            self.items.folds.clear();
            self.items.group_shown.clear();
        }
        self.selected_bot_id = Some(bot_id.clone());
        self.screen = Screen::Detail;
        self.connectors.needs_sync_from_bot = true;
        self.settings.needs_sync_from_bot = true;
        self.load_selected_bot_detail(bot_id, cx);
        cx.notify();
    }

    pub fn set_detail_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.soul_memory.armed_forget = None;
        self.error = None;
        self.notice = None;
        self.screen = Screen::Detail;
        self.detail_tab = tab;
        if tab == DetailTab::Connectors {
            self.connectors.needs_sync_from_bot = true;
        }
        if tab == DetailTab::Settings {
            self.settings.needs_sync_from_bot = true;
        }
        if tab == DetailTab::ToolsSharing {
            self.refresh_tool_relay_status(cx);
        }
        cx.notify();
    }

    /// New bot always opens Orca's dialog over the workspace.
    #[expect(
        clippy::unused_self,
        reason = "kept as a method for the modal's buttons"
    )]
    pub fn open_create(&mut self, cx: &mut Context<Self>) {
        cx.emit(RequestNewBot);
    }

    pub fn open_import(&mut self, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.pending_switch_bot = None;
        self.error = None;
        self.notice = None;
        self.screen = Screen::Import;
        cx.notify();
    }

    pub(super) fn load_selected_bot_detail(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.unsupported.is_some() {
            return;
        }
        self.detail_gen = self.detail_gen.wrapping_add(1);
        let generation = self.detail_gen;
        let rpc = self.rpc.clone();
        let bot_cfg = self.bots.iter().find(|b| b.id == bot_id).cloned();
        let has_avatar = bot_cfg.as_ref().is_some_and(|b| b.avatar.is_some());
        let composio_enabled = bot_cfg
            .as_ref()
            .is_some_and(|b| b.connectors.composio.enabled);

        if let Some(cfg) = &bot_cfg {
            self.soul_memory.sync_config_from_bot(cfg);
            self.settings.sync_from_bot(cfg);
        }

        self.detail_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let items_res = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListItems {
                            bot_id: Some(bot_id_bg.clone()),
                            include_closed: true,
                        },
                    ) {
                        Ok(BotRpcResponse::Items(list)) => Ok(list),
                        Ok(BotRpcResponse::Ok) => Ok(Vec::new()),
                        Ok(other) => Err(format!("unexpected ListItems: {other:?}")),
                        Err(e) => Err(e),
                    };
                    let soul_res = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetSoul {
                            bot_id: bot_id_bg.clone(),
                        },
                    ) {
                        Ok(BotRpcResponse::Text(t)) => Ok(Some(t)),
                        Ok(BotRpcResponse::Ok) => Ok(None),
                        Ok(other) => Err(format!("unexpected GetSoul: {other:?}")),
                        Err(e) => Err(e),
                    };
                    let memory_res = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemory {
                            bot_id: bot_id_bg.clone(),
                        },
                    ) {
                        Ok(BotRpcResponse::Text(t)) => Ok(Some(t)),
                        Ok(BotRpcResponse::Ok) => Ok(None),
                        Ok(other) => Err(format!("unexpected GetMemory: {other:?}")),
                        Err(e) => Err(e),
                    };
                    let statuses_res = match call_checked(
                        &rpc,
                        &BotRpcRequest::ConnectorStatus {
                            bot_id: Some(bot_id_bg.clone()),
                        },
                    ) {
                        Ok(BotRpcResponse::ConnectorStatuses(list)) => Ok(list),
                        Ok(BotRpcResponse::Ok) => Ok(Vec::new()),
                        Ok(other) => Err(format!("unexpected ConnectorStatus: {other:?}")),
                        Err(e) => Err(e),
                    };
                    let access_res = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListAccessRequests {
                            bot_id: Some(bot_id_bg.clone()),
                        },
                    ) {
                        Ok(BotRpcResponse::AccessRequests(list)) => Ok(list),
                        Ok(BotRpcResponse::Ok) => Ok(Vec::new()),
                        Ok(other) => Err(format!("unexpected ListAccessRequests: {other:?}")),
                        Err(e) => Err(e),
                    };
                    let relay_res = match call_checked(&rpc, &BotRpcRequest::ToolRelayStatus) {
                        Ok(BotRpcResponse::ToolRelayStatus(st)) => Ok(Some(st)),
                        Ok(_) => Ok(None),
                        Err(e) => Err(e),
                    };
                    let sessions_res = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListBotSessions { bot_id: None },
                    ) {
                        Ok(BotRpcResponse::BotSessions(list)) => Ok(Some(list)),
                        Ok(_) => Ok(None),
                        Err(e) => Err(e),
                    };
                    let suggestions_res = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListSuggestions {
                            bot_id: Some(bot_id_bg.clone()),
                            include_dismissed: false,
                        },
                    ) {
                        Ok(BotRpcResponse::Suggestions(list)) => Ok(Some(list)),
                        Ok(_) => Ok(None),
                        Err(e) => Err(e),
                    };
                    let history_res = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryHistory {
                            bot_id: bot_id_bg.clone(),
                            limit: Some(25),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryHistory(list)) => Ok(Some(list)),
                        Ok(_) => Ok(None),
                        Err(e) => Err(e),
                    };
                    let tidy_status_res = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryTidyStatus {
                            bot_id: bot_id_bg.clone(),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryTidyStatus(st)) => Ok(Some(st)),
                        Ok(_) => Ok(None),
                        Err(e) => Err(e),
                    };
                    let topics_res = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListMemoryTopics {
                            bot_id: bot_id_bg.clone(),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryTopics(list)) => Ok(Some(list)),
                        Ok(_) => Ok(None),
                        Err(e) => Err(e),
                    };
                    let tracker_res =
                        match call_checked(&rpc, &BotRpcRequest::GetDefaultTrackerBots) {
                            Ok(BotRpcResponse::DefaultTrackerBots(d)) => Ok(Some(d)),
                            Ok(_) => Ok(None),
                            Err(e) => Err(e),
                        };
                    let composio_res = if composio_enabled {
                        match call_checked(
                            &rpc,
                            &BotRpcRequest::ListComposioConnections {
                                bot_id: bot_id_bg.clone(),
                            },
                        ) {
                            Ok(BotRpcResponse::ComposioConnections(list)) => Ok(Some(list)),
                            Ok(_) => Ok(None),
                            Err(e) => Err(e),
                        }
                    } else {
                        Ok(None)
                    };
                    let upcoming = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetBotUpcoming {
                            bot_id: bot_id_bg.clone(),
                        },
                    ) {
                        Ok(BotRpcResponse::Upcoming(up)) => Some(up),
                        _ => None,
                    };
                    let avatar_img = if has_avatar {
                        match call_checked(&rpc, &BotRpcRequest::GetBotAvatar { bot_id: bot_id_bg })
                        {
                            Ok(BotRpcResponse::BotAvatar(data)) => {
                                decode_png_avatar(&data.png_bytes).ok()
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };

                    DetailBundle {
                        items_res,
                        soul_res,
                        memory_res,
                        statuses_res,
                        access_res,
                        relay_res,
                        sessions_res,
                        suggestions_res,
                        history_res,
                        tidy_status_res,
                        topics_res,
                        tracker_res,
                        composio_res,
                        avatar_img,
                        upcoming,
                    }
                })
                .await;

            this.update(cx, |this, cx| {
                if this.detail_gen != generation || this.selected_bot_id.as_ref() != Some(&bot_id) {
                    return;
                }
                let mut errors = Vec::new();
                match loaded.items_res {
                    Ok(items) => this.items.items = items,
                    Err(e) => errors.push(e),
                }
                let soul_opt = match loaded.soul_res {
                    Ok(val) => val,
                    Err(e) => {
                        errors.push(e);
                        None
                    }
                };
                let mem_opt = match loaded.memory_res {
                    Ok(val) => val,
                    Err(e) => {
                        errors.push(e);
                        None
                    }
                };
                // Preserve existing SOUL/MEMORY text when either section failed to load.
                this.soul_memory
                    .apply_loaded_sections(&bot_id, soul_opt, mem_opt);

                match loaded.statuses_res {
                    Ok(statuses) => this.connectors.bot_statuses = statuses,
                    Err(e) => errors.push(e),
                }
                match loaded.access_res {
                    Ok(access) => this.connectors.bot_access_requests = access,
                    Err(e) => errors.push(e),
                }
                match loaded.relay_res {
                    Ok(Some(st)) => this.tools.relay_status = Some(st),
                    Ok(None) => {}
                    Err(e) => errors.push(e),
                }
                match loaded.sessions_res {
                    Ok(Some(list)) => {
                        this.bot_sessions = list;
                        this.emit_sessions_changed(cx);
                    }
                    Ok(None) => {}
                    Err(e) => errors.push(e),
                }
                match loaded.suggestions_res {
                    Ok(Some(list)) => this.soul_memory.suggestions = list,
                    Ok(None) => {}
                    Err(e) => errors.push(e),
                }
                match loaded.history_res {
                    Ok(Some(list)) => this.soul_memory.memory_history = list,
                    Ok(None) => {}
                    Err(e) => errors.push(e),
                }
                match loaded.tidy_status_res {
                    Ok(Some(st)) => this.soul_memory.tidy_status = Some(st),
                    Ok(None) => {}
                    Err(e) => errors.push(e),
                }
                match loaded.topics_res {
                    Ok(Some(list)) => this.soul_memory.memory_topics = list,
                    Ok(None) => {}
                    Err(e) => errors.push(e),
                }
                match loaded.tracker_res {
                    Ok(Some(d)) => this.connectors.default_tracker_bots = d,
                    Ok(None) => {}
                    Err(e) => errors.push(e),
                }
                match loaded.composio_res {
                    Ok(Some(c)) => this.connectors.composio_connections = c,
                    Ok(None) => {}
                    Err(e) => errors.push(e),
                }
                if let Some(img) = loaded.avatar_img {
                    this.settings.avatar_cache.insert(bot_id.clone(), img);
                }
                this.items.upcoming = loaded.upcoming;

                this.connectors.needs_sync_from_bot = true;
                if let Some(version) = errors.iter().find_map(|e| unsupported_version(e)) {
                    this.unsupported = Some(version.to_owned().into());
                } else if !errors.is_empty() {
                    this.error = Some(this.load_error(&errors));
                }
                this.sync_open_items_for_bot(&bot_id);
                this.emit_badge(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn sync_open_items_for_bot(&mut self, bot_id: &BotId) {
        self.all_open_items.retain(|i| &i.owner_bot_id != bot_id);
        for item in &self.items.items {
            if item.status.is_open() {
                self.all_open_items.push(item.clone());
            }
        }
    }

    pub(super) fn upsert_bot_config(&mut self, updated: BotConfig) {
        if let Some(slot) = self.bots.iter_mut().find(|b| b.id == updated.id) {
            *slot = updated;
        } else {
            self.bots.push(updated);
        }
    }

    /// Focuses an active live chat session if one already exists; otherwise spawns a new one.
    pub(super) fn start_bot_chat(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.chats.opening {
            return;
        }
        if self
            .selected_bot()
            .is_some_and(|b| b.id == bot_id && b.paused)
            || self.bots.iter().any(|b| b.id == bot_id && b.paused)
        {
            self.error = Some(SharedString::from(text::OPEN_CHAT_PAUSED_TOOLTIP.get()));
            cx.notify();
            return;
        }
        if let Some(live) = self
            .bot_sessions
            .iter()
            .find(|s| s.bot_id == bot_id && s.live)
        {
            cx.emit(OpenSession {
                bot_id,
                session_id: live.session_id,
            });
            return;
        }
        let cwd = self
            .bots
            .iter()
            .find(|b| b.id == bot_id)
            .and_then(|b| b.workspace.clone())
            .filter(|s| !s.trim().is_empty());
        let rpc = self.rpc.clone();
        self.chats.opening = true;
        self.error = None;
        self.open_chat_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::StartBotSession {
                            bot_id: bot_id_bg,
                            cwd,
                            prompt: None,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.chats.opening = false;
                match res {
                    Ok(BotRpcResponse::SessionStarted {
                        bot_id,
                        session_id,
                        pid,
                    }) => {
                        this.bot_sessions.push(BotSessionSummary {
                            session_id,
                            bot_id: bot_id.clone(),
                            link_id: None,
                            pid,
                            live: true,
                            working: false,
                            label: this
                                .session_meta_for(&bot_id)
                                .map(|m| m.bot_name)
                                .unwrap_or_default(),
                            created_at_ms: now_unix_ms(),
                        });
                        this.emit_sessions_changed(cx);
                        cx.emit(OpenSession { bot_id, session_id });
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected StartBotSession response: {other:?}"
                        )));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn refresh_bot_sessions(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.chats.loading {
            return;
        }
        let rpc = self.rpc.clone();
        self.chats.loading = true;
        self.sessions_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(&rpc, &BotRpcRequest::ListBotSessions { bot_id: None })
                })
                .await;
            this.update(cx, |this, cx| {
                this.chats.loading = false;
                match res {
                    Ok(BotRpcResponse::BotSessions(list)) => {
                        this.bot_sessions = list;
                        this.emit_sessions_changed(cx);
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected ListBotSessions response: {other:?}"
                        )));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                let _ = &bot_id;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn reset_bot_chat(
        &mut self,
        bot_id: BotId,
        session_id: Option<SessionId>,
        cx: &mut Context<Self>,
    ) {
        self.run_session_action(
            bot_id.clone(),
            BotRpcRequest::ResetBotChat { bot_id, session_id },
            true,
            cx,
        );
    }

    pub fn stop_bot_session(
        &mut self,
        bot_id: BotId,
        session_id: SessionId,
        cx: &mut Context<Self>,
    ) {
        self.run_session_action(
            bot_id.clone(),
            BotRpcRequest::StopBotSession { bot_id, session_id },
            false,
            cx,
        );
    }

    pub fn retry_bot_kickoff(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        self.run_session_action(
            bot_id.clone(),
            BotRpcRequest::RetryBotKickoff { bot_id },
            true,
            cx,
        );
    }

    pub fn send_bot_run_again(
        &mut self,
        bot_id: BotId,
        session_id: SessionId,
        cx: &mut Context<Self>,
    ) {
        self.run_session_action(
            bot_id.clone(),
            BotRpcRequest::SendBotRunAgain { bot_id, session_id },
            true,
            cx,
        );
    }

    pub fn take_over_workspace(
        &mut self,
        bot_id: BotId,
        session_id: SessionId,
        cx: &mut Context<Self>,
    ) {
        self.run_session_action(
            bot_id.clone(),
            BotRpcRequest::TakeOverWorkspace { bot_id, session_id },
            false,
            cx,
        );
    }

    fn run_session_action(
        &mut self,
        bot_id: BotId,
        req: BotRpcRequest,
        focus_on_start: bool,
        cx: &mut Context<Self>,
    ) {
        if self.chats.loading {
            return;
        }
        let rpc = self.rpc.clone();
        self.chats.loading = true;
        self.error = None;
        self.sessions_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let reply = call_checked(&rpc, &req)?;
                    let sessions = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListBotSessions { bot_id: None },
                    ) {
                        Ok(BotRpcResponse::BotSessions(list)) => Some(list),
                        _ => None,
                    };
                    Ok::<_, String>((reply, sessions))
                })
                .await;
            this.update(cx, |this, cx| {
                this.chats.loading = false;
                match res {
                    Ok((reply, sessions)) => {
                        if let Some(list) = sessions {
                            this.bot_sessions = list;
                            this.emit_sessions_changed(cx);
                        }
                        if focus_on_start
                            && let BotRpcResponse::SessionStarted {
                                bot_id: bid,
                                session_id,
                                ..
                            } = reply
                        {
                            cx.emit(OpenSession {
                                bot_id: bid,
                                session_id,
                            });
                        } else {
                            let _ = &bot_id;
                            this.notice = Some(SharedString::from(text::SAVED.get()));
                        }
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn toggle_pause_bot(&mut self, bot_id: BotId, paused: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.mutation_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
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
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.upsert_bot_config(*updated);
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                        this.emit_badge(cx);
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected UpdateBot response: {other:?}"
                        )));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn activate_delete_bot(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.armed_delete_bot.as_ref() == Some(&bot_id) {
            self.armed_delete_bot = None;
            self.delete_bot(bot_id, cx);
        } else {
            self.armed_delete_bot = Some(bot_id);
            cx.notify();
        }
    }

    pub(super) fn delete_bot(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.mutation_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::DeleteBot {
                            bot_id: bot_id_bg,
                            delete_home: true,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::Deleted { .. } | BotRpcResponse::Ok) => {
                        this.bots.retain(|b| b.id != bot_id);
                        this.all_open_items.retain(|i| i.owner_bot_id != bot_id);
                        this.all_access_requests.retain(|r| r.bot_id != bot_id);
                        this.all_connector_statuses.retain(|s| s.bot_id != bot_id);
                        this.bot_sessions.retain(|s| s.bot_id != bot_id);
                        this.settings.avatar_cache.remove(&bot_id);
                        if this.selected_bot_id.as_ref() == Some(&bot_id) {
                            this.selected_bot_id = this.bots.first().map(|b| b.id.clone());
                            if let Some(next_id) = this.selected_bot_id.clone() {
                                this.load_selected_bot_detail(next_id, cx);
                            } else {
                                this.items.items.clear();
                                this.soul_memory.set_loaded(String::new(), String::new());
                            }
                        }
                        this.emit_badge(cx);
                        this.emit_sessions_changed(cx);
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected DeleteBot response: {other:?}"
                        )));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(crate) fn input_is_composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let app: &mut gpui::App = cx;
        self.items.input_is_composing(window, app)
            || self.soul_memory.input_is_composing(window, app)
            || self.connectors.input_is_composing(window, app)
            || self.tools.input_is_composing(window, app)
            || self.import_state.input_is_composing(window, app)
            || self.settings.input_is_composing(window, app)
            || self.groups.input_is_composing(window, app)
    }

    pub fn handle_escape(&mut self, cx: &mut Context<Self>) {
        if self.pending_switch_bot.take().is_some() {
            cx.notify();
            return;
        }
        if self.armed_delete_bot.take().is_some() {
            cx.notify();
            return;
        }
        if self.soul_memory.armed_forget.take().is_some() {
            cx.notify();
            return;
        }
        if matches!(self.screen, Screen::Import | Screen::Groups) && !self.bots.is_empty() {
            self.screen = Screen::Detail;
            cx.notify();
            return;
        }
        cx.emit(Close);
    }

    fn connector_dot_color(state: ConnectorHealthState, p: &ui::Palette) -> gpui::Hsla {
        match state {
            ConnectorHealthState::Connected => p.success,
            ConnectorHealthState::Connecting => p.warning,
            ConnectorHealthState::Error => p.destructive,
            ConnectorHealthState::Disabled | ConnectorHealthState::Disconnected => {
                p.muted_foreground.opacity(0.45)
            }
        }
    }

    fn render_error_with_retry(
        &self,
        err: SharedString,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let selected = self.selected_bot_id.clone();
        div().px_5().pt_3().child(
            div()
                .px_3()
                .py_2()
                .rounded(ui::RADIUS_SM)
                .border_1()
                .border_color(p.destructive.opacity(0.3))
                .bg(p.destructive.opacity(0.1))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.destructive)
                        .child(err),
                )
                .child(
                    ui::secondary("bots-error-retry", text::RETRY.get())
                        .debug_selector(|| "bots-error-retry".into())
                        .disabled(self.loading)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.error = None;
                            if let Some(bid) = selected.clone() {
                                this.load_selected_bot_detail(bid, cx);
                            } else {
                                this.reload(cx);
                            }
                        })),
                ),
        )
    }

    fn render_left_rail(
        &self,
        rail_width: gpui::Pixels,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut needs_by_bot: BTreeMap<&str, usize> = BTreeMap::new();
        for item in &self.all_open_items {
            if item.status == BotItemStatus::NeedsYou {
                *needs_by_bot.entry(item.owner_bot_id.as_str()).or_insert(0) += 1;
            }
        }
        let mut access_by_bot: BTreeMap<&str, usize> = BTreeMap::new();
        for req in &self.all_access_requests {
            if req.status == BotAccessRequestStatus::Pending {
                *access_by_bot.entry(req.bot_id.as_str()).or_insert(0) += 1;
            }
        }

        let mut list = div()
            .id("bots-rail-list")
            .debug_selector(|| "bots-rail-list".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_2()
            .py_2()
            .flex()
            .flex_col()
            .gap_1();

        for bot in &self.bots {
            let bot_id = bot.id.clone();
            let bot_id_sel = bot.id.as_str().to_owned();
            let is_selected =
                self.screen == Screen::Detail && self.selected_bot_id.as_ref() == Some(&bot.id);
            let needs_count = needs_by_bot.get(bot.id.as_str()).copied().unwrap_or(0);
            let pending_count = access_by_bot.get(bot.id.as_str()).copied().unwrap_or(0);
            let attention = needs_count.saturating_add(pending_count);
            let enabled_sources = bot.connectors.enabled_sources();
            let statuses_for_bot: Vec<ConnectorHealthState> = enabled_sources
                .iter()
                .map(|src| {
                    self.all_connector_statuses
                        .iter()
                        .find(|st| st.bot_id == bot.id && st.source == *src)
                        .map_or(ConnectorHealthState::Disconnected, |st| st.state)
                })
                .collect();
            let avatar_img = self.settings.avatar_cache.get(&bot.id).cloned();
            let runtime_label = rpc::runtime_display_label(&bot.agent_id);
            let row_id = format!("bots-rail-item-{bot_id_sel}");
            let row_sel = row_id.clone();

            list = list.child(
                div()
                    .id(SharedString::from(row_id))
                    .debug_selector(move || row_sel.clone())
                    .w_full()
                    .px_2p5()
                    .py_2()
                    .rounded(ui::RADIUS_MD)
                    .cursor_pointer()
                    .when(is_selected, |d| {
                        d.bg(p.sidebar_accent)
                            .text_color(p.sidebar_accent_foreground)
                    })
                    .when(!is_selected, |d| {
                        d.text_color(p.sidebar_foreground)
                            .hover(|s| s.bg(p.sidebar_accent.opacity(0.6)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_bot(bot_id.clone(), cx);
                    }))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_1p5()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    // Same face as the sidebar row (mascot, picture, emoji or initial).
                                    .child(avatar::render(
                                        &avatar::Avatar::of(bot),
                                        avatar_img,
                                        &bot.name,
                                        px(20.),
                                        ui::RADIUS_XS,
                                        p,
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(ui::TEXT_SM)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(bot.name.clone()),
                                    ),
                            )
                            .when(attention > 0, |d| {
                                d.child(ui::badge(p, ui::BadgeTone::Warning, attention.to_string()))
                            }),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_1p5()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .flex_wrap()
                                    .gap_1()
                                    .child(ui::icons::agent_brand_icon(
                                        &bot.agent_id,
                                        ui::ICON_XS,
                                        p.muted_foreground,
                                    ))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(ui::TEXT_2XS)
                                            .text_color(p.muted_foreground)
                                            .child(runtime_label),
                                    )
                                    .when(bot.paused, |d| {
                                        d.child(ui::badge(
                                            p,
                                            ui::BadgeTone::Muted,
                                            text::PAUSED_BADGE.get(),
                                        ))
                                    })
                                    .when(!bot.paused && needs_count > 0, |d| {
                                        d.child(ui::badge(
                                            p,
                                            ui::BadgeTone::Warning,
                                            text::NEEDS_YOU_BADGE
                                                .format(&[("n", &needs_count.to_string())]),
                                        ))
                                    })
                                    .when(pending_count > 0, |d| {
                                        d.child(ui::badge(
                                            p,
                                            ui::BadgeTone::Neutral,
                                            text::PENDING_ACCESS_BADGE
                                                .format(&[("n", &pending_count.to_string())]),
                                        ))
                                    }),
                            )
                            .when(!statuses_for_bot.is_empty(), |d| {
                                let mut dots = div().flex_none().flex().items_center().gap_1();
                                for st in statuses_for_bot {
                                    dots = dots.child(
                                        div()
                                            .size(ui::STATE_DOT)
                                            .rounded_full()
                                            .bg(Self::connector_dot_color(st, p)),
                                    );
                                }
                                d.child(dots)
                            }),
                    ),
            );
        }

        div()
            .id("bots-left-rail")
            .debug_selector(|| "bots-left-rail".into())
            .w(rail_width)
            .flex_none()
            .h_full()
            .min_h_0()
            .bg(p.sidebar)
            .border_r_1()
            .border_color(p.sidebar_border)
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_3()
                    .border_b_1()
                    .border_color(p.sidebar_border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ui::icons::svg_icon(
                                ui::icons::CUSTOM_AGENT_SVG,
                                ui::ICON_MD,
                                p.sidebar_foreground,
                            ))
                            .child(
                                div()
                                    .text_size(ui::TEXT_LG)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.sidebar_foreground)
                                    .child(text::TITLE.get()),
                            )
                            .when(self.loading, |d| {
                                d.child(ui::badge(p, ui::BadgeTone::Muted, text::LOADING.get()))
                            }),
                    )
                    .child(ui::button_with_tooltip(
                        Button::new("bots-refresh")
                            .xsmall()
                            .ghost()
                            .debug_selector(|| "bots-refresh".into())
                            .icon(IconName::Undo2)
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                        "bots-refresh",
                        text::REFRESH.get(),
                    )),
            )
            .when(self.available_hosts.len() > 1, |d| {
                let mut host_pills = div()
                    .px_2()
                    .py_1p5()
                    .border_b_1()
                    .border_color(p.sidebar_border)
                    .flex()
                    .flex_wrap()
                    .gap_1();
                for (hidx, (hid, label, target)) in self.available_hosts.iter().enumerate() {
                    let h_active = &self.host == hid;
                    let hid_c = hid.clone();
                    let target_c = target.clone();
                    let all_c = self.available_hosts.clone();
                    host_pills = host_pills.child(
                        ui::segment(p, format!("bots-host-{hidx}"), label.clone(), h_active)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_host_target(
                                    hid_c.clone(),
                                    target_c.clone(),
                                    all_c.clone(),
                                    cx,
                                );
                            })),
                    );
                }
                d.child(host_pills)
            })
            .child(list)
            .child(
                div()
                    .flex_none()
                    .p_2p5()
                    .border_t_1()
                    .border_color(p.sidebar_border)
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        ui::segment(
                            p,
                            "bots-nav-groups",
                            text::GROUP_CHATS_HEADING.get(),
                            self.screen == Screen::Groups,
                        )
                        .debug_selector(|| "bots-nav-groups".into())
                        .w_full()
                        .on_click(cx.listener(|this, _, _, cx| this.open_groups(cx))),
                    )
                    .child(
                        ui::primary("bots-new-btn", text::NEW_BOT.get())
                            .debug_selector(|| "bots-new-btn".into())
                            .w_full()
                            .on_click(cx.listener(|this, _, _, cx| this.open_create(cx))),
                    )
                    .child(
                        ui::secondary("bots-import-btn", text::IMPORT.get())
                            .debug_selector(|| "bots-import-btn".into())
                            .w_full()
                            .on_click(cx.listener(|this, _, _, cx| this.open_import(cx))),
                    ),
            )
    }

    fn render_compact_switcher(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let mut bar = div()
            .w_full()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(p.border)
            .bg(p.sidebar)
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5();
        for bot in &self.bots {
            let bid = bot.id.clone();
            let active =
                self.screen == Screen::Detail && self.selected_bot_id.as_ref() == Some(&bid);
            bar = bar.child(
                ui::segment(
                    p,
                    SharedString::from(format!("bots-compact-select-{}", bid.as_str())),
                    bot.name.clone(),
                    active,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_bot(bid.clone(), cx);
                })),
            );
        }
        bar.child(
            ui::segment(
                p,
                "bots-compact-groups",
                text::GROUP_CHATS_HEADING.get(),
                self.screen == Screen::Groups,
            )
            .on_click(cx.listener(|this, _, _, cx| this.open_groups(cx))),
        )
        .child(
            ui::primary("bots-new-btn", text::NEW_BOT.get())
                .debug_selector(|| "bots-new-btn".into())
                .on_click(cx.listener(|this, _, _, cx| this.open_create(cx))),
        )
        .child(
            ui::secondary("bots-import-btn", text::IMPORT.get())
                .debug_selector(|| "bots-import-btn".into())
                .on_click(cx.listener(|this, _, _, cx| this.open_import(cx))),
        )
    }

    /// This host's krilld predates bots: say so and point at the update instead of an error.
    fn render_unsupported(
        &self,
        version: &str,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let host = self.host_label();
        let desc = if version.is_empty() {
            text::UNSUPPORTED_DESC_NO_VERSION.format(&[("host", &host)])
        } else {
            text::UNSUPPORTED_DESC.format(&[("host", &host), ("version", version)])
        };
        div()
            .id("bots-unsupported")
            .debug_selector(|| "bots-unsupported".into())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .text_center()
            .child(ui::icons::svg_box(
                ui::icons::CUSTOM_AGENT_SVG,
                32.0,
                p.muted_foreground,
            ))
            .child(
                div()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(text::UNSUPPORTED_TITLE.get()),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(desc),
            )
            .when(self.host.is_remote(), |d| {
                d.child(
                    ui::primary(
                        "bots-unsupported-update",
                        text::UNSUPPORTED_OPEN_UPDATE.get(),
                    )
                    .debug_selector(|| "bots-unsupported-update".into())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenRemoteSettings))),
                )
            })
    }

    fn render_empty_state(p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id("bots-empty")
            .debug_selector(|| "bots-empty".into())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .text_center()
            .child(ui::icons::svg_box(
                ui::icons::CUSTOM_AGENT_SVG,
                32.0,
                p.muted_foreground,
            ))
            .child(
                div()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(text::NO_BOTS_TITLE.get()),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::NO_BOTS_DESC.get()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        ui::primary("bots-empty-create", text::CREATE_FIRST_BOT.get())
                            .debug_selector(|| "bots-empty-create".into())
                            .on_click(cx.listener(|this, _, _, cx| this.open_create(cx))),
                    )
                    .child(
                        ui::secondary("bots-empty-import", text::IMPORT.get())
                            .debug_selector(|| "bots-empty-import".into())
                            .on_click(cx.listener(|this, _, _, cx| this.open_import(cx))),
                    ),
            )
    }

    fn render_chats_panel(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let sessions: Vec<&BotSessionSummary> = self.sessions_for_bot(&bot.id);
        let heading = text::CHATS_HEADING.format(&[("n", &sessions.len().to_string())]);
        let bot_reset = bot.id.clone();
        let bot_kickoff = bot.id.clone();

        let mut panel = ui::card(p)
            .debug_selector(|| "bots-chats-panel".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(heading),
                    )
                    .child(
                        ui::controls()
                            .child(
                                ui::ghost("bots-chats-kickoff", text::CHAT_RETRY_KICKOFF_BTN.get())
                                    .disabled(self.chats.loading || bot.paused)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.retry_bot_kickoff(bot_kickoff.clone(), cx);
                                    })),
                            )
                            .child(
                                ui::secondary("bots-chats-reset", text::CHAT_RESET_BTN.get())
                                    .disabled(self.chats.loading || bot.paused)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.reset_bot_chat(bot_reset.clone(), None, cx);
                                    })),
                            ),
                    ),
            );

        if sessions.is_empty() {
            panel = panel.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::CHATS_EMPTY.get()),
            );
        } else {
            for (idx, sess) in sessions.into_iter().enumerate() {
                let sid = sess.session_id;
                let bid_focus = bot.id.clone();
                let bid_stop = bot.id.clone();
                let bid_again = bot.id.clone();
                let bid_take = bot.id.clone();
                let bid_reset = bot.id.clone();
                let label = if sess.label.trim().is_empty() {
                    format!("Session #{}", sid.0)
                } else {
                    sess.label.clone()
                };
                panel = panel.child(
                    div()
                        .w_full()
                        .px_2p5()
                        .py_1p5()
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(p.border)
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(ui::badge(
                                    p,
                                    if sess.live {
                                        ui::BadgeTone::Success
                                    } else {
                                        ui::BadgeTone::Muted
                                    },
                                    if sess.live {
                                        text::CHAT_SESSION_LIVE.get()
                                    } else {
                                        text::CHAT_SESSION_STOPPED.get()
                                    },
                                ))
                                .when(sess.working, |d| {
                                    d.child(ui::badge(
                                        p,
                                        ui::BadgeTone::Warning,
                                        text::CHAT_SESSION_WORKING.get(),
                                    ))
                                })
                                .child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.foreground)
                                        .child(label),
                                ),
                        )
                        .child(
                            ui::controls()
                                .child(
                                    ui::secondary(
                                        SharedString::from(format!("bots-chat-focus-{idx}")),
                                        text::CHAT_FOCUS_BTN.get(),
                                    )
                                    .on_click(cx.listener(
                                        move |_, _, _, cx| {
                                            cx.emit(OpenSession {
                                                bot_id: bid_focus.clone(),
                                                session_id: sid,
                                            });
                                        },
                                    )),
                                )
                                .when(sess.live, |d| {
                                    d.child(
                                        ui::ghost(
                                            SharedString::from(format!("bots-chat-stop-{idx}")),
                                            text::CHAT_STOP_BTN.get(),
                                        )
                                        .disabled(self.chats.loading)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.stop_bot_session(bid_stop.clone(), sid, cx);
                                            }),
                                        ),
                                    )
                                })
                                .child(
                                    ui::ghost(
                                        SharedString::from(format!("bots-chat-again-{idx}")),
                                        text::CHAT_RUN_AGAIN_BTN.get(),
                                    )
                                    .disabled(self.chats.loading)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.send_bot_run_again(bid_again.clone(), sid, cx);
                                        },
                                    )),
                                )
                                .child(
                                    ui::ghost(
                                        SharedString::from(format!("bots-chat-takeover-{idx}")),
                                        text::CHAT_TAKEOVER_BTN.get(),
                                    )
                                    .disabled(self.chats.loading)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.take_over_workspace(bid_take.clone(), sid, cx);
                                        },
                                    )),
                                )
                                .child(
                                    ui::ghost(
                                        SharedString::from(format!("bots-chat-reset-{idx}")),
                                        text::CHAT_RESET_BTN.get(),
                                    )
                                    .disabled(self.chats.loading)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.reset_bot_chat(bid_reset.clone(), Some(sid), cx);
                                        },
                                    )),
                                ),
                        ),
                );
            }
        }
        panel
    }

    fn render_bot_detail_header(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let bot_id_chat = bot.id.clone();
        let bot_id_export = bot.id.clone();
        let bot_id_pause = bot.id.clone();
        let bot_id_delete = bot.id.clone();
        let paused = bot.paused;
        let armed_delete = self.armed_delete_bot.as_ref() == Some(&bot.id);
        let runtime_label = rpc::runtime_display_label(&bot.agent_id);
        let avatar_img = self.settings.avatar_cache.get(&bot.id).cloned();
        let bot_session_count = self.sessions_for_bot(&bot.id).len();

        let tabs = [
            (DetailTab::Items, "bots-tab-items", text::TAB_ITEMS.get()),
            (
                DetailTab::SoulMemory,
                "bots-tab-soul-memory",
                text::TAB_SOUL_MEMORY.get(),
            ),
            (
                DetailTab::Connectors,
                "bots-tab-connectors",
                text::TAB_CONNECTORS.get(),
            ),
            (
                DetailTab::ToolsSharing,
                "bots-tab-tools-sharing",
                text::TAB_TOOLS_SHARING.get(),
            ),
            (
                DetailTab::Settings,
                "bots-tab-settings",
                text::TAB_SETTINGS.get(),
            ),
        ];

        div()
            .flex_none()
            .border_b_1()
            .border_color(p.border)
            .px_5()
            .pt_4()
            .pb_3()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .flex_wrap()
                                    .gap_2()
                                    .child(avatar::render(
                                        &avatar::Avatar::of(bot),
                                        avatar_img,
                                        &bot.name,
                                        px(36.),
                                        ui::RADIUS_SM,
                                        p,
                                    ))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(ui::TEXT_LG)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(bot.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(p.muted)
                                            .border_1()
                                            .border_color(p.border)
                                            .child(ui::icons::agent_brand_icon(
                                                &bot.agent_id,
                                                ui::ICON_XS,
                                                p.foreground,
                                            ))
                                            .child(
                                                div()
                                                    .text_size(ui::TEXT_2XS)
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(p.foreground)
                                                    .child(runtime_label),
                                            ),
                                    )
                                    .child(ui::badge(
                                        p,
                                        if paused {
                                            ui::BadgeTone::Muted
                                        } else {
                                            ui::BadgeTone::Success
                                        },
                                        if paused {
                                            text::PAUSED_BADGE.get()
                                        } else {
                                            text::ACTIVE_BADGE.get()
                                        },
                                    )),
                            )
                            .when(!bot.description.trim().is_empty(), |d| {
                                d.child(
                                    div()
                                        .min_w_0()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(bot.description.clone()),
                                )
                            }),
                    )
                    .child(
                        ui::controls()
                            .child(ui::button_with_tooltip_when(
                                paused,
                                ui::button_with_reserved_label(
                                    "bots-open-chat",
                                    text::OPEN_CHAT.get(),
                                    text::OPENING_CHAT.get(),
                                    self.chats.opening,
                                )
                                .small()
                                .primary()
                                .debug_selector(|| "bots-open-chat".into())
                                .disabled(self.chats.opening || paused)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.start_bot_chat(bot_id_chat.clone(), cx);
                                    },
                                )),
                                "bots-open-chat",
                                text::OPEN_CHAT_PAUSED_TOOLTIP.get(),
                            ))
                            .child(
                                ui::secondary(
                                    "bots-chats-toggle",
                                    text::CHAT_TOGGLE_BTN
                                        .format(&[("n", &bot_session_count.to_string())]),
                                )
                                .debug_selector(|| "bots-chats-toggle".into())
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.chats.expanded = !this.chats.expanded;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                ui::secondary(
                                    "bots-toggle-pause",
                                    if paused {
                                        text::RESUME_BOT.get()
                                    } else {
                                        text::PAUSE_BOT.get()
                                    },
                                )
                                .debug_selector(|| "bots-toggle-pause".into())
                                .disabled(self.busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.toggle_pause_bot(bot_id_pause.clone(), !paused, cx);
                                    },
                                )),
                            )
                            .child(
                                ui::secondary("bots-export-zip-header", text::EXPORT_ZIP.get())
                                    .debug_selector(|| "bots-export-zip-header".into())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.export_bot_zip(bot_id_export.clone(), cx);
                                    })),
                            )
                            .child(
                                ui::danger_confirm(
                                    "bots-delete-bot",
                                    text::DELETE_BOT.get(),
                                    text::DELETE_BOT_CONFIRM.get(),
                                    armed_delete,
                                )
                                .debug_selector(|| "bots-delete-bot".into())
                                .disabled(self.busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.activate_delete_bot(bot_id_delete.clone(), cx);
                                    },
                                )),
                            )
                            .child(ui::button_with_tooltip(
                                Button::new("bots-close")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Close)
                                    .debug_selector(|| "bots-close".into())
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Close))),
                                "bots-close",
                                text::CLOSE.get(),
                            )),
                    ),
            )
            .when(self.chats.expanded, |d| {
                d.child(self.render_chats_panel(bot, p, cx))
            })
            .child(ui::segmented_control(
                p,
                tabs.into_iter().map(|(tab, id, label)| {
                    ui::segment(p, id, label, self.detail_tab == tab)
                        .debug_selector(move || id.into())
                        .on_click(cx.listener(move |this, _, _, cx| this.set_detail_tab(tab, cx)))
                }),
            ))
    }

    /// "Leave without saving?" while a bot switch waits on unsaved soul edits.
    pub(super) fn render_pending_switch(
        &self,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        self.pending_switch_bot.clone().map(|target_id| {
            div().px_5().pt_3().child(
                div()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.warning.opacity(0.4))
                    .bg(p.warning.opacity(0.1))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.foreground)
                            .child(format!(
                                "{} — {}",
                                text::DIRTY_LEAVE_TITLE.get(),
                                text::DIRTY_LEAVE_DESC.get()
                            )),
                    )
                    .child(
                        ui::controls()
                            .child(ui::ghost("bots-dirty-cancel", text::CANCEL.get()).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.pending_switch_bot = None;
                                    cx.notify();
                                }),
                            ))
                            .child(
                                ui::primary("bots-dirty-discard", text::DIRTY_DISCARD_BTN.get())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.confirm_select_bot(target_id.clone(), cx);
                                    })),
                            ),
                    ),
            )
        })
    }

    pub(super) fn render_notice(&self, p: &ui::Palette) -> Option<gpui::Div> {
        self.notice.as_ref().map(|msg| {
            div().px_5().pt_3().child(
                div()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.success.opacity(0.3))
                    .bg(p.success.opacity(0.1))
                    .text_size(ui::TEXT_XS)
                    .text_color(p.success)
                    .child(msg.clone()),
            )
        })
    }

    pub(super) fn source_label(source: ExternalSource) -> &'static str {
        match source {
            ExternalSource::Telegram => text::CONN_TELEGRAM.get(),
            ExternalSource::Slack => text::CONN_SLACK.get(),
            ExternalSource::Discord => text::CONN_DISCORD.get(),
            ExternalSource::Google => text::CONN_GOOGLE.get(),
            ExternalSource::GitHub => text::CONN_GITHUB.get(),
            ExternalSource::Linear => text::CONN_LINEAR.get(),
            ExternalSource::Composio => text::CONN_COMPOSIO.get(),
        }
    }
}

struct DetailBundle {
    items_res: Result<Vec<BotItem>, String>,
    soul_res: Result<Option<String>, String>,
    memory_res: Result<Option<String>, String>,
    statuses_res: Result<Vec<BotConnectorStatus>, String>,
    access_res: Result<Vec<BotAccessRequest>, String>,
    relay_res: Result<Option<krill_proto::bots::BotToolRelayStatus>, String>,
    sessions_res: Result<Option<Vec<BotSessionSummary>>, String>,
    suggestions_res: Result<Option<Vec<krill_proto::bots::BotSuggestion>>, String>,
    history_res: Result<Option<Vec<krill_proto::bots::BotMemoryHistoryEntry>>, String>,
    tidy_status_res: Result<Option<krill_proto::bots::BotMemoryTidyStatus>, String>,
    topics_res: Result<Option<Vec<krill_proto::bots::BotMemoryTopicInfo>>, String>,
    tracker_res: Result<Option<krill_proto::bots::DefaultTrackerBots>, String>,
    composio_res: Result<Option<Vec<krill_proto::bots::ComposioConnectionInfo>>, String>,
    avatar_img: Option<std::sync::Arc<gpui::Image>>,
    /// `None` from a krilld that predates it, too: Home then leaves times out.
    upcoming: Option<krill_proto::bots::BotUpcoming>,
}

impl Render for View {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window_handle = Some(window.window_handle());
        let p = ui::palette(cx);
        let lang_changed = self.language != crate::i18n::language();
        self.language = crate::i18n::language();
        if lang_changed {
            self.items.refresh_placeholders(window, cx);
            self.soul_memory.refresh_placeholders(window, cx);
            self.connectors.refresh_placeholders(window, cx);
            self.tools.refresh_placeholders(window, cx);
            self.import_state.refresh_placeholders(window, cx);
            self.settings.refresh_placeholders(window, cx);
            self.groups.refresh_placeholders(window, cx);
        }

        let rem = window.rem_size();
        let modal = ui::page_modal_size(window.viewport_size());
        let compact = modal.width < rem * 42.5;
        let narrow = modal.width < rem * 60.;
        let rail_width = if narrow {
            ui::SETTINGS_NAV_WIDTH_NARROW.to_pixels(rem)
        } else {
            ui::SETTINGS_NAV_WIDTH
        };

        if self.mode == Mode::Panel {
            return self.render_panel(window, cx).into_any_element();
        }
        let pending_switch_banner = self.render_pending_switch(p, cx);

        let selected_bot = self.selected_bot().cloned();
        let right_pane = match self.screen {
            Screen::Import => self.render_import_view(p, window, cx),
            Screen::Groups => self.render_groups_view(p, window, cx),
            Screen::Detail => {
                if let Some(bot) = selected_bot {
                    let tab_body = match self.detail_tab {
                        DetailTab::Items => self.render_items_tab(&bot, p, window, cx),
                        DetailTab::SoulMemory => self.render_soul_memory_tab(&bot, p, window, cx),
                        DetailTab::Connectors => self.render_connectors_tab(&bot, p, window, cx),
                        DetailTab::ToolsSharing => {
                            self.render_tools_sharing_tab(&bot, p, window, cx)
                        }
                        DetailTab::Settings => self.render_settings_tab(&bot, p, window, cx),
                    };
                    div()
                        .id("bots-detail-pane")
                        .debug_selector(|| "bots-detail-pane".into())
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(self.render_bot_detail_header(&bot, p, cx))
                        .children(pending_switch_banner)
                        .children(
                            self.error
                                .clone()
                                .map(|err| self.render_error_with_retry(err, p, cx)),
                        )
                        .children(self.render_notice(p))
                        .child(tab_body)
                } else {
                    div()
                        .id("bots-empty-pane")
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex_none()
                                .px_5()
                                .py_3()
                                .border_b_1()
                                .border_color(p.border)
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(text::SUBTITLE.get()),
                                )
                                .child(ui::button_with_tooltip(
                                    Button::new("bots-close")
                                        .small()
                                        .ghost()
                                        .icon(IconName::Close)
                                        .debug_selector(|| "bots-close".into())
                                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Close))),
                                    "bots-close",
                                    text::CLOSE.get(),
                                )),
                        )
                        .children(
                            self.error
                                .clone()
                                .map(|err| self.render_error_with_retry(err, p, cx)),
                        )
                        .map(|d| match self.unsupported.clone() {
                            Some(version) => d.child(self.render_unsupported(&version, p, cx)),
                            // While the first load runs, an empty pane instead of a "no bots" flash.
                            None => {
                                d.when(!self.loading, |d| d.child(Self::render_empty_state(p, cx)))
                            }
                        })
                }
            }
        };

        let layout = if compact {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.render_compact_switcher(p, cx))
                .child(right_pane)
        } else {
            div()
                .size_full()
                .flex()
                .child(self.render_left_rail(rail_width, p, cx))
                .child(right_pane)
        };

        ui::modal_focus_scope(
            div()
                .id("bots-page")
                .debug_selector(|| "bots-page".into())
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape"
                        && !event.keystroke.modifiers.modified()
                        && !this.input_is_composing(window, cx)
                    {
                        this.handle_escape(cx);
                        cx.stop_propagation();
                    }
                }))
                .size_full()
                .bg(p.background)
                .font_family(ui::FONT_UI)
                .text_size(ui::TEXT_MD)
                .text_color(p.foreground)
                .child(layout),
            &self.focus,
        )
        .into_any_element()
    }
}

/// Clears an `InputState` inside `Ok(...)` completion handlers using the cached window handle.
pub(super) fn clear_input_on_window(
    slot: &mut Option<Entity<InputState>>,
    window_handle: Option<AnyWindowHandle>,
    cx: &mut gpui::App,
) {
    if let Some(input) = slot.as_ref() {
        if let Some(handle) = window_handle {
            let input_c = input.clone();
            if handle
                .update(cx, |_, window, cx| {
                    input_c.update(cx, |s, cx| s.set_value(String::new(), window, cx));
                })
                .is_ok()
            {
                return;
            }
        }
        *slot = None;
    }
}

/// Returns current Unix timestamp in milliseconds.
pub(super) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Checks whether an `Option<Entity<InputState>>` is currently composing IME text.
pub(super) fn opt_input_composing(
    input: Option<&Entity<InputState>>,
    window: &mut Window,
    app: &mut gpui::App,
) -> bool {
    input.is_some_and(|i| crate::jump::entity_input_is_composing(i, window, app))
}

/// Checks whether an `Option<Entity<TextareaState>>` is currently composing IME text.
pub(super) fn opt_textarea_composing(
    input: Option<&Entity<gpui_component::input::TextareaState>>,
    window: &mut Window,
    app: &mut gpui::App,
) -> bool {
    input.is_some_and(|i| {
        i.update(app, |state, cx| {
            state.focus_handle(cx).is_focused(window)
                && state.marked_text_range(window, cx).is_some()
        })
    })
}
