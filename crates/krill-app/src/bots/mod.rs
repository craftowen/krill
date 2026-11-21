// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Krill Bots modal UI (Phases 1-3 complete, STYLEGUIDE-compliant).
//!
//! Opens as a single modal over the main workspace window via `ui::modal_backdrop` +
//! `ui::page_modal_size`, matching `Automations` and `Artifacts`. Active bot chats open
//! as regular workspace `[Chat | Term]` tabs via [`OpenSession`].

mod connectors;
mod create;
mod items;
pub mod rpc;
mod soul_memory;
mod tools_sharing;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, FontWeight, KeyDownEvent, SharedString, Task,
    Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_proto::SessionId;
use krill_proto::bots::{
    BotAccessRequest, BotAccessRequestStatus, BotConfig, BotConnectorStatus, BotId, BotItem,
    BotItemStatus, BotRpcRequest, BotRpcResponse, ConnectorHealthState, ExternalSource,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

pub use rpc::{
    BOT_RUNTIMES, BotRpcCall, BotRuntimeOption, SidebarBot, SidebarSnapshot, daemon_bot_rpc,
    fetch_sidebar_snapshot,
};

/// Request the host `Shell` to close the Bots modal.
#[derive(Debug)]
pub struct Close;

/// Request the host `Shell` to open/focus a bot's live session in a workspace `[Chat | Term]` tab.
#[derive(Debug, Clone)]
pub struct OpenSession {
    pub bot_id: BotId,
    pub session_id: SessionId,
}

/// Notify the host `Shell` when bots or their attention counts (`needs_you` + pending access) change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgeChanged {
    pub attention_count: usize,
    /// Sidebar Bots section rows, built once per change.
    pub bots: Vec<SidebarBot>,
}

/// Active sub-tab inside a bot's detail view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Items,
    SoulMemory,
    Connectors,
    ToolsSharing,
}

/// Active right-pane screen inside the Bots modal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Screen {
    #[default]
    Detail,
    Create,
    Import,
}

pub struct View {
    focus: FocusHandle,
    rpc: BotRpcCall,
    default_cwd: Option<PathBuf>,
    bots: Vec<BotConfig>,
    selected_bot_id: Option<BotId>,
    screen: Screen,
    detail_tab: DetailTab,
    loading: bool,
    busy: bool,
    opening_chat: bool,
    error: Option<SharedString>,
    notice: Option<SharedString>,
    armed_delete_bot: Option<BotId>,
    all_open_items: Vec<BotItem>,
    all_connector_statuses: Vec<BotConnectorStatus>,
    all_access_requests: Vec<BotAccessRequest>,
    create: create::CreateState,
    items: items::ItemsState,
    soul_memory: soul_memory::SoulMemoryState,
    connectors: connectors::ConnectorsState,
    tools: tools_sharing::ToolsState,
    import_state: tools_sharing::ImportState,
    load_gen: u64,
    detail_gen: u64,
    load_task: Option<Task<()>>,
    detail_task: Option<Task<()>>,
    action_task: Option<Task<()>>,
    language: crate::i18n::Language,
}

impl EventEmitter<Close> for View {}
impl EventEmitter<OpenSession> for View {}
impl EventEmitter<BadgeChanged> for View {}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotsView")
            .field("bots", &self.bots.len())
            .field("selected_bot_id", &self.selected_bot_id)
            .field("screen", &self.screen)
            .field("detail_tab", &self.detail_tab)
            .finish_non_exhaustive()
    }
}

impl View {
    /// Creates a new Bots modal connected to the local Krill daemon.
    pub fn new(
        target: Option<crate::hosts::HostTarget>,
        default_cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_rpc(daemon_bot_rpc(target), default_cwd, cx)
    }

    /// Creates a Bots modal backed by an injectable RPC handler (used by tests and custom targets).
    pub fn with_rpc(rpc: BotRpcCall, default_cwd: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            focus: cx.focus_handle(),
            rpc,
            default_cwd,
            bots: Vec::new(),
            selected_bot_id: None,
            screen: Screen::Detail,
            detail_tab: DetailTab::Items,
            loading: false,
            busy: false,
            opening_chat: false,
            error: None,
            notice: None,
            armed_delete_bot: None,
            all_open_items: Vec::new(),
            all_connector_statuses: Vec::new(),
            all_access_requests: Vec::new(),
            create: create::CreateState::default(),
            items: items::ItemsState::default(),
            soul_memory: soul_memory::SoulMemoryState::default(),
            connectors: connectors::ConnectorsState::default(),
            tools: tools_sharing::ToolsState::default(),
            import_state: tools_sharing::ImportState::default(),
            load_gen: 0,
            detail_gen: 0,
            load_task: None,
            detail_task: None,
            action_task: None,
            language: crate::i18n::language(),
        };
        view.reload(cx);
        view
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
    pub fn selected_bot(&self) -> Option<&BotConfig> {
        let id = self.selected_bot_id.as_ref()?;
        self.bots.iter().find(|b| &b.id == id)
    }

    #[must_use]
    pub fn attention_count(&self) -> usize {
        self.sidebar_snapshot().attention
    }

    pub fn on_hide(&mut self, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.soul_memory.armed_forget = None;
        cx.notify();
    }

    pub fn reopen(&mut self, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.soul_memory.armed_forget = None;
        self.error = None;
        self.notice = None;
        self.reload(cx);
    }

    /// Rows and attention total for the main window sidebar's Bots section.
    #[must_use]
    pub fn sidebar_snapshot(&self) -> SidebarSnapshot {
        rpc::sidebar_snapshot(&self.bots, &self.all_open_items, &self.all_access_requests)
    }

    pub(super) fn emit_badge(&self, cx: &mut Context<Self>) {
        let snapshot = self.sidebar_snapshot();
        cx.emit(BadgeChanged {
            attention_count: snapshot.attention,
            bots: snapshot.bots,
        });
    }

    /// Loads bots list, open items, connector statuses, access requests, and tool relay status.
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
                    let bots = match rpc(&BotRpcRequest::ListBots)? {
                        BotRpcResponse::Bots(list) => list,
                        BotRpcResponse::Error(msg) => return Err(msg),
                        other => return Err(format!("unexpected ListBots response: {other:?}")),
                    };
                    let open_items = match rpc(&BotRpcRequest::ListItems {
                        bot_id: None,
                        include_closed: false,
                    }) {
                        Ok(BotRpcResponse::Items(list)) => list,
                        _ => Vec::new(),
                    };
                    let statuses = match rpc(&BotRpcRequest::ConnectorStatus { bot_id: None }) {
                        Ok(BotRpcResponse::ConnectorStatuses(list)) => list,
                        _ => Vec::new(),
                    };
                    let access = match rpc(&BotRpcRequest::ListAccessRequests { bot_id: None }) {
                        Ok(BotRpcResponse::AccessRequests(list)) => list,
                        _ => Vec::new(),
                    };
                    let relay = match rpc(&BotRpcRequest::ToolRelayStatus) {
                        Ok(BotRpcResponse::ToolRelayStatus(st)) => Some(st),
                        _ => None,
                    };
                    Ok::<_, String>((bots, open_items, statuses, access, relay))
                })
                .await;

            this.update(cx, |this, cx| {
                if this.load_gen != generation {
                    return;
                }
                this.loading = false;
                match outcome {
                    Ok((bots, open_items, statuses, access, relay)) => {
                        this.bots = bots;
                        this.all_open_items = open_items;
                        this.all_connector_statuses = statuses;
                        this.all_access_requests = access;
                        if let Some(st) = relay {
                            this.tools.relay_status = Some(st);
                        }
                        let still_valid = this
                            .selected_bot_id
                            .as_ref()
                            .is_some_and(|id| this.bots.iter().any(|b| &b.id == id));
                        if !still_valid {
                            this.selected_bot_id = this.bots.first().map(|b| b.id.clone());
                        }
                        this.emit_badge(cx);
                        if let Some(bot_id) = this.selected_bot_id.clone() {
                            this.load_selected_bot_detail(bot_id, cx);
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

    pub fn select_bot(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.soul_memory.armed_forget = None;
        self.error = None;
        self.notice = None;
        self.selected_bot_id = Some(bot_id.clone());
        self.screen = Screen::Detail;
        self.connectors.needs_sync_from_bot = true;
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
        if tab == DetailTab::ToolsSharing {
            self.refresh_tool_relay_status(cx);
        }
        cx.notify();
    }

    pub fn open_create(&mut self, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.error = None;
        self.notice = None;
        self.screen = Screen::Create;
        self.create.opened_alone = false;
        cx.notify();
    }

    /// New bot as its own dialog (sidebar "+"): Escape closes the modal like Orca's.
    pub fn open_create_alone(&mut self, cx: &mut Context<Self>) {
        self.open_create(cx);
        self.create.opened_alone = true;
    }

    pub fn open_import(&mut self, cx: &mut Context<Self>) {
        self.armed_delete_bot = None;
        self.error = None;
        self.notice = None;
        self.screen = Screen::Import;
        cx.notify();
    }

    pub(super) fn load_selected_bot_detail(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        self.detail_gen = self.detail_gen.wrapping_add(1);
        let generation = self.detail_gen;
        let rpc = self.rpc.clone();
        self.detail_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let items = match rpc(&BotRpcRequest::ListItems {
                        bot_id: Some(bot_id_bg.clone()),
                        include_closed: true,
                    }) {
                        Ok(BotRpcResponse::Items(list)) => list,
                        _ => Vec::new(),
                    };
                    let soul = match rpc(&BotRpcRequest::GetSoul {
                        bot_id: bot_id_bg.clone(),
                    }) {
                        Ok(BotRpcResponse::Text(t)) => t,
                        _ => String::new(),
                    };
                    let memory = match rpc(&BotRpcRequest::GetMemory {
                        bot_id: bot_id_bg.clone(),
                    }) {
                        Ok(BotRpcResponse::Text(t)) => t,
                        _ => String::new(),
                    };
                    let statuses = match rpc(&BotRpcRequest::ConnectorStatus {
                        bot_id: Some(bot_id_bg.clone()),
                    }) {
                        Ok(BotRpcResponse::ConnectorStatuses(list)) => list,
                        _ => Vec::new(),
                    };
                    let access = match rpc(&BotRpcRequest::ListAccessRequests {
                        bot_id: Some(bot_id_bg),
                    }) {
                        Ok(BotRpcResponse::AccessRequests(list)) => list,
                        _ => Vec::new(),
                    };
                    let relay = match rpc(&BotRpcRequest::ToolRelayStatus) {
                        Ok(BotRpcResponse::ToolRelayStatus(st)) => Some(st),
                        _ => None,
                    };
                    (items, soul, memory, statuses, access, relay)
                })
                .await;

            this.update(cx, |this, cx| {
                if this.detail_gen != generation || this.selected_bot_id.as_ref() != Some(&bot_id) {
                    return;
                }
                let (items, soul, memory, statuses, access, relay) = loaded;
                this.items.items = items;
                this.soul_memory.set_loaded(soul, memory);
                this.connectors.bot_statuses = statuses;
                this.connectors.bot_access_requests = access;
                this.connectors.needs_sync_from_bot = true;
                if let Some(st) = relay {
                    this.tools.relay_status = Some(st);
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

    pub(super) fn start_bot_chat(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.opening_chat {
            return;
        }
        let cwd = self
            .bots
            .iter()
            .find(|b| b.id == bot_id)
            .and_then(|b| b.workspace.clone())
            .filter(|s| !s.trim().is_empty());
        let rpc = self.rpc.clone();
        self.opening_chat = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    rpc(&BotRpcRequest::StartBotSession {
                        bot_id: bot_id_bg,
                        cwd,
                        prompt: None,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.opening_chat = false;
                match res {
                    Ok(BotRpcResponse::SessionStarted {
                        bot_id, session_id, ..
                    }) => {
                        cx.emit(OpenSession { bot_id, session_id });
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
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

    pub(super) fn toggle_pause_bot(&mut self, bot_id: BotId, paused: bool, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    rpc(&BotRpcRequest::UpdateBot(Box::new(
                        krill_proto::bots::UpdateBotParams {
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
                        },
                    )))
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
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
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
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    rpc(&BotRpcRequest::DeleteBot {
                        bot_id: bot_id_bg,
                        delete_home: true,
                    })
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
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(crate) fn input_is_composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let app: &mut gpui::App = cx;
        self.create.input_is_composing(window, app)
            || self.items.input_is_composing(window, app)
            || self.connectors.input_is_composing(window, app)
            || self.tools.input_is_composing(window, app)
    }

    pub fn handle_escape(&mut self, cx: &mut Context<Self>) {
        if self.armed_delete_bot.take().is_some() {
            cx.notify();
            return;
        }
        if self.soul_memory.armed_forget.take().is_some() {
            cx.notify();
            return;
        }
        let alone = self.screen == Screen::Create && self.create.opened_alone;
        if matches!(self.screen, Screen::Create | Screen::Import) && !self.bots.is_empty() && !alone
        {
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

    fn render_left_rail(
        &self,
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
            let display_emoji = bot.emoji.clone().unwrap_or_default();
            let has_emoji = !display_emoji.is_empty();
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
                                    .when(has_emoji, |d| {
                                        d.child(
                                            div()
                                                .flex_none()
                                                .text_size(ui::TEXT_SM)
                                                .child(display_emoji),
                                        )
                                    })
                                    .when(!has_emoji, |d| {
                                        d.child(ui::icons::svg_icon(
                                            ui::icons::CUSTOM_AGENT_SVG,
                                            ui::ICON_SM,
                                            p.muted_foreground,
                                        ))
                                    })
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
            .w(px(236.))
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
                            ),
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
        let emoji = bot.emoji.clone().unwrap_or_default();

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
                                    .gap_2()
                                    .when(!emoji.is_empty(), |d| {
                                        d.child(div().text_size(ui::TEXT_LG).child(emoji))
                                    })
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
                                    .when(paused, |d| {
                                        d.child(ui::badge(
                                            p,
                                            ui::BadgeTone::Muted,
                                            text::PAUSED_BADGE.get(),
                                        ))
                                    }),
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
                            .child(
                                ui::primary(
                                    "bots-open-chat",
                                    if self.opening_chat {
                                        text::OPENING_CHAT.get()
                                    } else {
                                        text::OPEN_CHAT.get()
                                    },
                                )
                                .debug_selector(|| "bots-open-chat".into())
                                .disabled(self.opening_chat)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.start_bot_chat(bot_id_chat.clone(), cx);
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
            .child(ui::segmented_control(
                p,
                tabs.into_iter().map(|(tab, id, label)| {
                    ui::segment(p, id, label, self.detail_tab == tab)
                        .debug_selector(move || id.into())
                        .on_click(cx.listener(move |this, _, _, cx| this.set_detail_tab(tab, cx)))
                }),
            ))
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

impl Render for View {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let lang_changed = self.language != crate::i18n::language();
        self.language = crate::i18n::language();
        if lang_changed {
            self.create.refresh_placeholders(window, cx);
            self.items.refresh_placeholders(window, cx);
            self.soul_memory.refresh_placeholders(window, cx);
            self.connectors.refresh_placeholders(window, cx);
            self.tools.refresh_placeholders(window, cx);
        }

        let selected_bot = self.selected_bot().cloned();
        let right_pane = match self.screen {
            Screen::Create => self.render_create_view(p, window, cx),
            Screen::Import => self.render_import_view(p, window, cx),
            Screen::Detail => {
                if let Some(bot) = selected_bot {
                    let tab_body = match self.detail_tab {
                        DetailTab::Items => self.render_items_tab(&bot, p, window, cx),
                        DetailTab::SoulMemory => self.render_soul_memory_tab(&bot, p, window, cx),
                        DetailTab::Connectors => self.render_connectors_tab(&bot, p, window, cx),
                        DetailTab::ToolsSharing => {
                            self.render_tools_sharing_tab(&bot, p, window, cx)
                        }
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
                        .children(
                            self.error.as_ref().map(|err| {
                                div().px_5().pt_3().child(ui::error_banner(p, err.clone()))
                            }),
                        )
                        .children(self.notice.as_ref().map(|msg| {
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
                        }))
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
                            self.error.as_ref().map(|err| {
                                div().px_5().pt_3().child(ui::error_banner(p, err.clone()))
                            }),
                        )
                        // While the first load runs, an empty pane instead of a "no bots" flash.
                        .when(!self.loading, |d| d.child(Self::render_empty_state(p, cx)))
                }
            }
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
                .flex()
                .bg(p.background)
                .font_family(ui::FONT_UI)
                .text_size(ui::TEXT_MD)
                .text_color(p.foreground)
                .child(self.render_left_rail(p, cx))
                .child(right_pane),
            &self.focus,
        )
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
    input: Option<&Entity<gpui_component::input::InputState>>,
    window: &mut Window,
    app: &mut gpui::App,
) -> bool {
    input.is_some_and(|i| crate::jump::entity_input_is_composing(i, window, app))
}
