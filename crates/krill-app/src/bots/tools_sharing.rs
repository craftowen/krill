// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Tab 4 — Tools & Sharing (13 curated MCP apps with BrowserOAuth/Bearer flow,
//! HTTP & stdio custom MCP endpoints, `BotToolRelay` per-tool health/counts, sanitized .zip
//! export/import with v2 overrides) + dedicated Import screen with path picker & checkboxes.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use gpui::{App, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use krill_proto::bots::{
    BOT_MCP_APPS, BotConfig, BotId, BotMcpAuthKind, BotMcpOAuthState, BotMcpToolConfig,
    BotPackageExportOptions, BotRpcRequest, BotRpcResponse, BotSlug, BotToolRelayStatus,
    ExternalBotCandidate, ExternalBotSourceApp, FieldPatch, MAX_RPC_ZIP_BYTES, UpdateBotParams,
    bot_slug_from_name, validate_bot_slug,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::rpc::{call_checked, mcp_secret_ref};
use super::{DetailTab, Screen, View, clear_input_on_window, opt_input_composing};

/// The Tools and sharing tab's cards.
pub(super) struct ToolsCards {
    pub(super) relay: gpui::AnyElement,
    pub(super) curated: gpui::AnyElement,
    pub(super) custom: gpui::AnyElement,
    pub(super) sharing: gpui::AnyElement,
}

#[derive(Default)]
pub(super) struct ToolsState {
    pub(super) relay_status: Option<BotToolRelayStatus>,
    pub(super) app_secret_inputs: BTreeMap<String, Entity<InputState>>,
    pub(super) mcp_oauth_states: BTreeMap<(BotId, String), BotMcpOAuthState>,
    pub(super) custom_name: Option<Entity<InputState>>,
    pub(super) custom_url: Option<Entity<InputState>>,
    pub(super) custom_cmd: Option<Entity<InputState>>,
    pub(super) custom_header: Option<Entity<InputState>>,
    pub(super) custom_secret: Option<Entity<InputState>>,
    pub(super) custom_is_stdio: bool,
    pub(super) export_include_memory: bool,
    pub(super) busy: bool,
}

impl ToolsState {
    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        opt_input_composing(self.custom_name.as_ref(), window, app)
            || opt_input_composing(self.custom_url.as_ref(), window, app)
            || opt_input_composing(self.custom_cmd.as_ref(), window, app)
            || opt_input_composing(self.custom_header.as_ref(), window, app)
            || opt_input_composing(self.custom_secret.as_ref(), window, app)
            || self
                .app_secret_inputs
                .values()
                .any(|i| crate::jump::entity_input_is_composing(i, window, app))
    }

    pub(super) fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<View>) {
        if let Some(i) = &self.custom_name {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::CUSTOM_MCP_NAME_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.custom_url {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::CUSTOM_MCP_URL_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.custom_cmd {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::CUSTOM_MCP_CMD_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.custom_header {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::CUSTOM_MCP_HEADER_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.custom_secret {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::MCP_SECRET_PLACEHOLDER.get(), window, cx);
            });
        }
        for i in self.app_secret_inputs.values() {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::MCP_SECRET_PLACEHOLDER.get(), window, cx);
            });
        }
    }
}

#[derive(Default)]
pub(super) struct ImportState {
    pub(super) source_filter: Option<ExternalBotSourceApp>,
    pub(super) candidates: Vec<ExternalBotCandidate>,
    pub(super) selected_paths: BTreeSet<String>,
    pub(super) custom_path_input: Option<Entity<InputState>>,
    pub(super) name_override_input: Option<Entity<InputState>>,
    pub(super) slug_override_input: Option<Entity<InputState>>,
    pub(super) agent_override_input: Option<Entity<InputState>>,
    pub(super) scanning: bool,
}

impl ImportState {
    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        opt_input_composing(self.custom_path_input.as_ref(), window, app)
            || opt_input_composing(self.name_override_input.as_ref(), window, app)
            || opt_input_composing(self.slug_override_input.as_ref(), window, app)
            || opt_input_composing(self.agent_override_input.as_ref(), window, app)
    }

    pub(super) fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<View>) {
        let updates = [
            (&self.custom_path_input, text::IMPORT_PATH_PLACEHOLDER.get()),
            (
                &self.name_override_input,
                text::IMPORT_NAME_OVERRIDE_PLACEHOLDER.get(),
            ),
            (
                &self.slug_override_input,
                text::IMPORT_SLUG_OVERRIDE_PLACEHOLDER.get(),
            ),
            (
                &self.agent_override_input,
                text::IMPORT_AGENT_OVERRIDE_PLACEHOLDER.get(),
            ),
        ];
        for (slot, ph) in updates {
            if let Some(i) = slot {
                i.update(cx, |s, cx| s.set_placeholder(ph, window, cx));
            }
        }
    }
}

fn auth_kind_label(kind: BotMcpAuthKind) -> &'static str {
    match kind {
        BotMcpAuthKind::BrowserOAuth => text::MCP_AUTH_OAUTH.get(),
        BotMcpAuthKind::BearerHeader => text::MCP_AUTH_BEARER.get(),
        BotMcpAuthKind::None => text::MCP_AUTH_NONE.get(),
    }
}

impl View {
    fn ensure_tools_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        if self.tools.custom_name.is_none() {
            self.tools.custom_name = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::CUSTOM_MCP_NAME_PLACEHOLDER.get())
            }));
        }
        if self.tools.custom_url.is_none() {
            self.tools.custom_url = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::CUSTOM_MCP_URL_PLACEHOLDER.get())
            }));
        }
        if self.tools.custom_cmd.is_none() {
            self.tools.custom_cmd = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::CUSTOM_MCP_CMD_PLACEHOLDER.get())
            }));
        }
        if self.tools.custom_header.is_none() {
            self.tools.custom_header = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::CUSTOM_MCP_HEADER_PLACEHOLDER.get())
            }));
        }
        if self.tools.custom_secret.is_none() {
            self.tools.custom_secret = Some(cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(text::MCP_SECRET_PLACEHOLDER.get())
                    .masked(true)
            }));
        }
    }

    fn ensure_mcp_secret_input(
        &mut self,
        app_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<InputState>> {
        if !cx.has_global::<gpui_component::Theme>() {
            return None;
        }
        if let Some(existing) = self.tools.app_secret_inputs.get(app_id) {
            return Some(existing.clone());
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(text::MCP_SECRET_PLACEHOLDER.get())
                .masked(true)
        });
        self.tools
            .app_secret_inputs
            .insert(app_id.to_owned(), input.clone());
        Some(input)
    }

    pub fn refresh_tool_relay_status(&mut self, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        self.tools_status_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { call_checked(&rpc, &BotRpcRequest::ToolRelayStatus) })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::ToolRelayStatus(st)) => {
                        this.tools.relay_status = Some(st);
                    }
                    Ok(BotRpcResponse::Ok) => {}
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected ToolRelayStatus response: {other:?}"
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
    }

    pub fn save_mcp_tools(
        &mut self,
        bot_id: BotId,
        tools: Vec<BotMcpToolConfig>,
        secret_to_store: Option<(String, String)>,
        clear_inputs: Vec<Option<Entity<InputState>>>,
        cx: &mut Context<Self>,
    ) {
        if self.tools.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.tools.busy = true;
        self.error = None;
        self.notice = None;
        self.tools_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    if let Some((secret_ref, value)) = secret_to_store {
                        call_checked(
                            &rpc,
                            &BotRpcRequest::SetSecret {
                                secret_ref: secret_ref.clone(),
                                value,
                            },
                        )?;
                        let update = call_checked(
                            &rpc,
                            &BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                                bot_id,
                                name: None,
                                description: None,
                                emoji: FieldPatch::Unchanged,
                                avatar: FieldPatch::Unchanged,
                                agent_id: None,
                                model: FieldPatch::Unchanged,
                                effort: FieldPatch::Unchanged,
                                permission_mode: None,
                                workspace: FieldPatch::Unchanged,
                                paused: None,
                                first_run_completed: None,
                                notifications: None,
                                schedules: None,
                                look_around: None,
                                memory_tidy: None,
                                mcp_tools: Some(tools),
                            })),
                        );
                        if let Err(err) = &update {
                            drop(rpc(&BotRpcRequest::DeleteSecret { secret_ref }));
                            return Err(text::ROLLBACK_NOTICE.format(&[("err", err)]));
                        }
                        update
                    } else {
                        call_checked(
                            &rpc,
                            &BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                                bot_id,
                                name: None,
                                description: None,
                                emoji: FieldPatch::Unchanged,
                                avatar: FieldPatch::Unchanged,
                                agent_id: None,
                                model: FieldPatch::Unchanged,
                                effort: FieldPatch::Unchanged,
                                permission_mode: None,
                                workspace: FieldPatch::Unchanged,
                                paused: None,
                                first_run_completed: None,
                                notifications: None,
                                schedules: None,
                                look_around: None,
                                memory_tidy: None,
                                mcp_tools: Some(tools),
                            })),
                        )
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.tools.busy = false;
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.upsert_bot_config(*updated);
                        for slot in clear_inputs.into_iter().flatten() {
                            let mut s = Some(slot);
                            clear_input_on_window(&mut s, this.window_handle, cx);
                        }
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                        this.refresh_tool_relay_status(cx);
                    }
                    Ok(_) => {
                        for slot in clear_inputs.into_iter().flatten() {
                            let mut s = Some(slot);
                            clear_input_on_window(&mut s, this.window_handle, cx);
                        }
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                        this.refresh_tool_relay_status(cx);
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

    pub fn delete_mcp_secret(
        &mut self,
        bot_id: BotId,
        tool_id: &str,
        mut tools: Vec<BotMcpToolConfig>,
        secret_ref: String,
        cx: &mut Context<Self>,
    ) {
        if self.tools.busy {
            return;
        }
        if let Some(slot) = tools.iter_mut().find(|t| t.id == tool_id) {
            slot.secret_ref = None;
        }
        let rpc = self.rpc.clone();
        self.tools.busy = true;
        self.error = None;
        self.tools_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(&rpc, &BotRpcRequest::DeleteSecret { secret_ref })?;
                    call_checked(
                        &rpc,
                        &BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                            bot_id,
                            name: None,
                            description: None,
                            emoji: FieldPatch::Unchanged,
                            avatar: FieldPatch::Unchanged,
                            agent_id: None,
                            model: FieldPatch::Unchanged,
                            effort: FieldPatch::Unchanged,
                            permission_mode: None,
                            workspace: FieldPatch::Unchanged,
                            paused: None,
                            first_run_completed: None,
                            notifications: None,
                            schedules: None,
                            look_around: None,
                            memory_tidy: None,
                            mcp_tools: Some(tools),
                        })),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.tools.busy = false;
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.upsert_bot_config(*updated);
                        this.notice = Some(SharedString::from(text::SECRET_DELETED.get()));
                        this.refresh_tool_relay_status(cx);
                    }
                    Ok(_) => {
                        this.notice = Some(SharedString::from(text::SECRET_DELETED.get()));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn start_mcp_oauth(&mut self, bot_id: BotId, tool_id: String, cx: &mut Context<Self>) {
        if self.tools.busy {
            return;
        }
        let rpc = self.rpc.clone();
        self.tools.busy = true;
        self.error = None;
        let bid_bg = bot_id.clone();
        let tid_bg = tool_id.clone();
        self.tools_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::StartMcpOAuth {
                            bot_id: bid_bg,
                            tool_id: tid_bg,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.tools.busy = false;
                match res {
                    Ok(BotRpcResponse::McpOAuth(st)) => {
                        if let Some(url) = &st.auth_url
                            && (url.starts_with("https://") || url.starts_with("http://"))
                        {
                            cx.open_url(url);
                        }
                        let should_poll = !matches!(
                            st.status.as_str(),
                            "connected" | "authorized" | "cancelled" | "canceled" | "failed"
                        );
                        this.tools
                            .mcp_oauth_states
                            .insert((bot_id.clone(), tool_id.clone()), st);
                        if should_poll {
                            this.poll_mcp_oauth(bot_id, tool_id, cx);
                        }
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected StartMcpOAuth response: {other:?}"
                        )));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn check_mcp_oauth_status(
        &mut self,
        bot_id: BotId,
        tool_id: String,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        let bid_bg = bot_id.clone();
        let tid_bg = tool_id.clone();
        self.tools_status_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::McpOAuthStatus {
                            bot_id: bid_bg,
                            tool_id: tid_bg,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::McpOAuth(st)) => {
                        this.tools.mcp_oauth_states.insert((bot_id, tool_id), st);
                        this.refresh_tool_relay_status(cx);
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected McpOAuthStatus response: {other:?}"
                        )));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn cancel_mcp_oauth(&mut self, bot_id: BotId, tool_id: String, cx: &mut Context<Self>) {
        self.oauth_poll_task = None;
        let rpc = self.rpc.clone();
        let bid_bg = bot_id.clone();
        let tid_bg = tool_id.clone();
        self.tools_cancel_oauth_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::CancelMcpOAuth {
                            bot_id: bid_bg,
                            tool_id: tid_bg,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::McpOAuth(st)) => {
                        this.tools.mcp_oauth_states.insert((bot_id, tool_id), st);
                    }
                    Ok(_) => {
                        this.tools.mcp_oauth_states.remove(&(bot_id, tool_id));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn poll_mcp_oauth(&mut self, bot_id: BotId, tool_id: String, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        let bid_bg = bot_id.clone();
        let tid_bg = tool_id.clone();
        self.oauth_poll_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::McpOAuthStatus {
                            bot_id: bid_bg,
                            tool_id: tid_bg,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::McpOAuth(st)) => {
                        let keep_polling =
                            matches!(st.status.as_str(), "pending" | "authorizing" | "started");
                        if let Some(err) = &st.error {
                            this.error = Some(SharedString::from(err.clone()));
                        }
                        this.tools
                            .mcp_oauth_states
                            .insert((bot_id.clone(), tool_id.clone()), st);
                        if keep_polling && this.selected_bot_id.as_ref() == Some(&bot_id) {
                            this.poll_mcp_oauth(bot_id, tool_id, cx);
                        } else {
                            this.refresh_tool_relay_status(cx);
                        }
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected McpOAuthStatus response: {other:?}"
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
    }

    pub fn export_bot_zip(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let include_memory = self.tools.export_include_memory;
        let default_name = self
            .bots
            .iter()
            .find(|b| b.id == bot_id)
            .map_or_else(|| "bot.zip".to_owned(), |b| format!("{}.zip", b.slug));
        let start_dir = self
            .default_cwd
            .clone()
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(std::path::PathBuf::from)
            })
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let save_dialog = cx.prompt_for_new_path(&start_dir, Some(&default_name));
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.export_task = Some(cx.spawn(async move |this, cx| {
            let chosen = save_dialog.await;
            let Ok(Ok(Some(dest_path))) = chosen else {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    cx.notify();
                })
                .ok();
                return;
            };
            let res = cx
                .background_executor()
                .spawn(async move {
                    let resp = call_checked(
                        &rpc,
                        &BotRpcRequest::ExportBotZip {
                            bot_id,
                            options: BotPackageExportOptions {
                                include_memory,
                                include_avatar: true,
                            },
                        },
                    )?;
                    match resp {
                        BotRpcResponse::ExportedZip { zip_bytes, .. } => {
                            std::fs::write(&dest_path, &zip_bytes).map_err(|e| e.to_string())?;
                            Ok(dest_path.display().to_string())
                        }
                        other => Err(format!("unexpected ExportBotZip response: {other:?}")),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(path) => {
                        this.notice = Some(SharedString::from(
                            text::EXPORT_SUCCESS.format(&[("path", &path)]),
                        ));
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

    pub fn import_bot_zip_from_dialog(&mut self, cx: &mut Context<Self>) {
        if self.import_state.scanning {
            return;
        }
        let name_override = self
            .import_state
            .name_override_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty());
        let raw_slug = self
            .import_state
            .slug_override_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty());
        let slug_override = if let Some(raw) = raw_slug {
            if validate_bot_slug(&raw).is_err() {
                self.error = Some(SharedString::from(text::ERR_INVALID_SLUG.get()));
                cx.notify();
                return;
            }
            Some(BotSlug::from_name(&raw))
        } else {
            None
        };
        let agent_override = self
            .import_state
            .agent_override_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty());

        let open_dialog = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(text::IMPORT_ZIP_BTN.get().into()),
        });
        let rpc = self.rpc.clone();
        self.import_state.scanning = true;
        self.error = None;
        self.notice = None;
        self.import_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = open_dialog.await else {
                this.update(cx, |this, cx| {
                    this.import_state.scanning = false;
                    cx.notify();
                })
                .ok();
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                this.update(cx, |this, cx| {
                    this.import_state.scanning = false;
                    cx.notify();
                })
                .ok();
                return;
            };
            let res = cx
                .background_executor()
                .spawn(async move {
                    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                    if bytes.len() > MAX_RPC_ZIP_BYTES {
                        return Err(text::IMPORT_ZIP_TOO_LARGE
                            .format(&[("max", &(MAX_RPC_ZIP_BYTES / 1024).to_string())]));
                    }
                    call_checked(
                        &rpc,
                        &BotRpcRequest::ImportBotZipV2 {
                            zip_bytes: bytes,
                            name_override,
                            slug_override,
                            agent_override,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.import_state.scanning = false;
                match res {
                    Ok(BotRpcResponse::Bot(imported)) => {
                        let id = imported.id.clone();
                        let handle = this.window_handle;
                        clear_input_on_window(
                            &mut this.import_state.name_override_input,
                            handle,
                            cx,
                        );
                        clear_input_on_window(
                            &mut this.import_state.slug_override_input,
                            handle,
                            cx,
                        );
                        clear_input_on_window(
                            &mut this.import_state.agent_override_input,
                            handle,
                            cx,
                        );
                        this.upsert_bot_config(*imported);
                        this.emit_badge(cx);
                        if this.mode == super::Mode::Panel {
                            cx.emit(super::OpenBotHome {
                                bot_id: id,
                                kickoff: None,
                                host: this.host.clone(),
                            });
                        } else {
                            this.selected_bot_id = Some(id.clone());
                            this.screen = Screen::Detail;
                            this.detail_tab = DetailTab::Items;
                            this.notice = Some(SharedString::from(
                                text::IMPORT_SUCCESS.format(&[("n", "1")]),
                            ));
                            this.load_selected_bot_detail(id, cx);
                        }
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected ImportBotZipV2 response: {other:?}"
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

    pub fn run_external_import(&mut self, dry_run: bool, cx: &mut Context<Self>) {
        if self.import_state.scanning {
            return;
        }
        let source = self.import_state.source_filter;
        let custom_path = self
            .import_state
            .custom_path_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty());
        let selected_subset: Vec<ExternalBotCandidate> = if dry_run {
            Vec::new()
        } else {
            self.import_state
                .candidates
                .iter()
                .filter(|c| self.import_state.selected_paths.contains(&c.path))
                .cloned()
                .collect()
        };
        let import_all_at_once = dry_run
            || self.import_state.candidates.is_empty()
            || selected_subset.len() == self.import_state.candidates.len();

        let rpc = self.rpc.clone();
        self.import_state.scanning = true;
        self.error = None;
        self.notice = None;
        self.import_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    if import_all_at_once {
                        call_checked(
                            &rpc,
                            &BotRpcRequest::ImportExternalBots {
                                source,
                                path: custom_path,
                                dry_run,
                            },
                        )
                    } else {
                        let mut all_imported = Vec::new();
                        for cand in selected_subset {
                            if let BotRpcResponse::ImportedExternalBots { imported, .. } =
                                call_checked(
                                    &rpc,
                                    &BotRpcRequest::ImportExternalBots {
                                        source: Some(cand.app),
                                        path: Some(cand.path),
                                        dry_run: false,
                                    },
                                )?
                            {
                                all_imported.extend(imported);
                            }
                        }
                        Ok(BotRpcResponse::ImportedExternalBots {
                            candidates: Vec::new(),
                            imported: all_imported,
                        })
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.import_state.scanning = false;
                match res {
                    Ok(BotRpcResponse::ImportedExternalBots {
                        candidates,
                        imported,
                    }) => {
                        if dry_run {
                            this.import_state.selected_paths =
                                candidates.iter().map(|c| c.path.clone()).collect();
                            this.import_state.candidates = candidates;
                            let msg = text::IMPORT_SCAN_FOUND
                                .format(&[("n", &this.import_state.candidates.len().to_string())]);
                            this.notice = Some(SharedString::from(msg));
                        } else {
                            let count = imported.len();
                            let first_id = imported.first().map(|b| b.id.clone());
                            for b in imported {
                                this.upsert_bot_config(b);
                            }
                            this.emit_badge(cx);
                            let msg = text::IMPORT_SUCCESS.format(&[("n", &count.to_string())]);
                            this.notice = Some(SharedString::from(msg));
                            if let Some(id) = first_id {
                                if this.mode == super::Mode::Panel {
                                    cx.emit(super::OpenBotHome {
                                        bot_id: id,
                                        kickoff: None,
                                        host: this.host.clone(),
                                    });
                                } else {
                                    this.selected_bot_id = Some(id.clone());
                                    this.screen = Screen::Detail;
                                    this.load_selected_bot_detail(id, cx);
                                }
                            }
                        }
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected ImportExternalBots response: {other:?}"
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

    pub(super) fn render_tools_sharing_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let cards = self.tools_cards(bot, p, window, cx);
        div()
            .id("bots-tab-tools-body")
            .debug_selector(|| "bots-tab-tools-body".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(cards.relay)
            .child(cards.curated)
            .child(cards.custom)
            .child(cards.sharing)
    }

    /// The tab's cards, also shown one page each in the panel's Settings › Tools and Sharing.
    pub(super) fn tools_cards(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolsCards {
        self.ensure_tools_inputs(window, cx);
        let bot_id = bot.id.clone();
        let busy = self.tools.busy;
        let relay = self.tools.relay_status.clone().unwrap_or_default();

        let relay_card = ui::card(p)
            .id("bots-tool-relay-card")
            .debug_selector(|| "bots-tool-relay-card".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(ui::label(
                        p,
                        text::TOOL_RELAY_HEADING.get(),
                        Some(text::TOOL_RELAY_DESC.get()),
                    ))
                    .child(
                        ui::controls()
                            .child(ui::badge(
                                p,
                                if relay.running {
                                    ui::BadgeTone::Success
                                } else {
                                    ui::BadgeTone::Muted
                                },
                                if relay.running {
                                    text::TOOL_RELAY_RUNNING.get()
                                } else {
                                    text::TOOL_RELAY_STOPPED.get()
                                },
                            ))
                            .child(
                                ui::secondary(
                                    "bots-relay-refresh",
                                    text::TOOL_RELAY_REFRESH_BTN.get(),
                                )
                                .debug_selector(|| "bots-relay-refresh".into())
                                .disabled(busy)
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.refresh_tool_relay_status(cx);
                                    },
                                )),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(
                        text::TOOL_RELAY_TOOLS_COUNT
                            .format(&[("n", &relay.connected_tools.len().to_string())]),
                    )
                    .when_some(relay.endpoint_url.clone(), |d, url| {
                        d.child(ui::badge(p, ui::BadgeTone::Neutral, url))
                    }),
            )
            .when_some(relay.last_error.clone(), |d, err| {
                d.child(ui::error_banner(p, err))
            });

        // 2. 13 Curated MCP Apps
        let enabled_curated = BOT_MCP_APPS
            .iter()
            .filter(|app| bot.mcp_tools.iter().any(|t| t.id == app.id && t.enabled))
            .count();
        let curated_heading = text::CURATED_MCP_HEADING.format(&[
            ("enabled", &enabled_curated.to_string()),
            ("total", &BOT_MCP_APPS.len().to_string()),
        ]);
        let mut curated_list = div()
            .id("bots-curated-mcp-list")
            .debug_selector(|| "bots-curated-mcp-list".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(ui::label(
                p,
                curated_heading,
                Some(text::CURATED_MCP_DESC.get()),
            ));

        for app in BOT_MCP_APPS {
            let configured = bot.mcp_tools.iter().find(|t| t.id == app.id);
            let is_enabled = configured.is_some_and(|t| t.enabled);
            let has_secret = configured.is_some_and(|t| t.secret_ref.is_some());
            let srv_status = relay
                .connected_tools
                .iter()
                .find(|s| s.id == app.id)
                .cloned();
            let oauth_state = self
                .tools
                .mcp_oauth_states
                .get(&(bot_id.clone(), app.id.to_owned()))
                .cloned();
            let secret_input = (app.auth_kind == BotMcpAuthKind::BearerHeader)
                .then(|| self.ensure_mcp_secret_input(app.id, window, cx))
                .flatten();

            let entity = cx.entity();
            let app_id = app.id;
            let app_name = app.name;
            let app_url = app.endpoint_url;
            let app_auth = app.auth_kind;
            let app_header = app.header_name;
            let docs_url = app.docs_url;
            let bot_toggle = bot_id.clone();
            let bot_secret = bot_id.clone();
            let bot_clear_sec = bot_id.clone();
            let bot_oauth_start = bot_id.clone();
            let bot_oauth_check = bot_id.clone();
            let bot_oauth_cancel = bot_id.clone();
            let current_tools = bot.mcp_tools.clone();
            let tools_secret = bot.mcp_tools.clone();
            let tools_clear = bot.mcp_tools.clone();
            let secret_in_clone = secret_input.clone();
            let clear_secret_ref = configured.and_then(|t| t.secret_ref.clone());

            curated_list = curated_list.child(
                ui::card(p)
                    .id(SharedString::from(format!("bots-mcp-app-{app_id}")))
                    .debug_selector(move || format!("bots-mcp-app-{app_id}"))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(ui::label(p, app.name, Some(app.description)))
                            .child(
                                ui::controls()
                                    .child(ui::badge(
                                        p,
                                        ui::BadgeTone::Muted,
                                        auth_kind_label(app.auth_kind),
                                    ))
                                    .when_some(srv_status.as_ref(), |d, st| {
                                        d.child(ui::badge(
                                            p,
                                            if st.connected {
                                                ui::BadgeTone::Success
                                            } else {
                                                ui::BadgeTone::Warning
                                            },
                                            text::MCP_TOOL_CONNECTED
                                                .format(&[("n", &st.tool_count.to_string())]),
                                        ))
                                    })
                                    .when(has_secret, |d| {
                                        d.child(ui::badge(
                                            p,
                                            ui::BadgeTone::Success,
                                            text::SECRET_CONFIGURED.get(),
                                        ))
                                    })
                                    .child(
                                        ui::ghost(
                                            SharedString::from(format!("bots-mcp-docs-{app_id}")),
                                            text::MCP_DOCS_BTN.get(),
                                        )
                                        .on_click(move |_, _, cx| cx.open_url(docs_url)),
                                    )
                                    .child(
                                        ui::switch(
                                            p,
                                            format!("bots-mcp-toggle-{app_id}"),
                                            is_enabled,
                                            busy,
                                            move |next, _, cx| {
                                                let mut tools = current_tools.clone();
                                                if let Some(slot) =
                                                    tools.iter_mut().find(|t| t.id == app_id)
                                                {
                                                    slot.enabled = next;
                                                } else if next {
                                                    tools.push(BotMcpToolConfig {
                                                        id: app_id.to_owned(),
                                                        name: app_name.to_owned(),
                                                        endpoint_url: Some(app_url.to_owned()),
                                                        command: Vec::new(),
                                                        auth_kind: app_auth,
                                                        header_names: app_header
                                                            .into_iter()
                                                            .map(ToOwned::to_owned)
                                                            .collect(),
                                                        secret_ref: None,
                                                        enabled: true,
                                                    });
                                                }
                                                let bid = bot_toggle.clone();
                                                entity.update(cx, |this, cx| {
                                                    this.save_mcp_tools(
                                                        bid,
                                                        tools,
                                                        None,
                                                        Vec::new(),
                                                        cx,
                                                    );
                                                });
                                            },
                                        )
                                        .aria_label(
                                            format!("{} {}", app.name, text::ENABLE_LABEL.get()),
                                        ),
                                    ),
                            ),
                    )
                    .when_some(
                        srv_status.as_ref().and_then(|s| s.last_error.clone()),
                        |d, err| d.child(ui::error_banner(p, err)),
                    )
                    .when(
                        is_enabled && app.auth_kind == BotMcpAuthKind::BrowserOAuth,
                        |d| {
                            let status_text = oauth_state.as_ref().map_or_else(
                                || {
                                    text::MCP_OAUTH_STATUS
                                        .format(&[("status", text::SECRET_NOT_SET.get())])
                                },
                                |st| text::MCP_OAUTH_STATUS.format(&[("status", &st.status)]),
                            );
                            let oauth_err = oauth_state.as_ref().and_then(|s| s.error.clone());
                            d.child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(ui::badge(p, ui::BadgeTone::Neutral, status_text))
                                    .child(
                                        ui::controls()
                                            .child(
                                                ui::secondary(
                                                    SharedString::from(format!(
                                                        "bots-mcp-oauth-start-{app_id}"
                                                    )),
                                                    text::MCP_OAUTH_START_BTN.get(),
                                                )
                                                .debug_selector(move || {
                                                    format!("bots-mcp-oauth-start-{app_id}")
                                                })
                                                .disabled(busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.start_mcp_oauth(
                                                        bot_oauth_start.clone(),
                                                        app_id.to_owned(),
                                                        cx,
                                                    );
                                                })),
                                            )
                                            .child(
                                                ui::ghost(
                                                    SharedString::from(format!(
                                                        "bots-mcp-oauth-check-{app_id}"
                                                    )),
                                                    text::MCP_OAUTH_CHECK_BTN.get(),
                                                )
                                                .disabled(busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.check_mcp_oauth_status(
                                                        bot_oauth_check.clone(),
                                                        app_id.to_owned(),
                                                        cx,
                                                    );
                                                })),
                                            )
                                            .child(
                                                ui::ghost(
                                                    SharedString::from(format!(
                                                        "bots-mcp-oauth-cancel-{app_id}"
                                                    )),
                                                    text::MCP_OAUTH_CANCEL_BTN.get(),
                                                )
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.cancel_mcp_oauth(
                                                        bot_oauth_cancel.clone(),
                                                        app_id.to_owned(),
                                                        cx,
                                                    );
                                                })),
                                            ),
                                    ),
                            )
                            .when_some(oauth_err, |dd, err| dd.child(ui::error_banner(p, err)))
                        },
                    )
                    .when(
                        is_enabled && app.auth_kind == BotMcpAuthKind::BearerHeader,
                        |d| {
                            d.child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .children(secret_input.as_ref().map(|i| {
                                        div().flex_1().min_w_0().child(Input::new(i).small())
                                    }))
                                    .child(
                                        ui::secondary(
                                            SharedString::from(format!(
                                                "bots-mcp-secret-save-{app_id}"
                                            )),
                                            text::SAVE_SECRET_BTN.get(),
                                        )
                                        .disabled(busy)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                let val = secret_in_clone
                                                    .as_ref()
                                                    .map(|i| i.read(cx).value().to_string())
                                                    .unwrap_or_default();
                                                let trimmed = val.trim().to_owned();
                                                if trimmed.is_empty() {
                                                    this.error = Some(SharedString::from(
                                                        text::ERR_EMPTY_FIELD.get(),
                                                    ));
                                                    cx.notify();
                                                    return;
                                                }
                                                let mut tools = tools_secret.clone();
                                                let sref =
                                                    mcp_secret_ref(bot_secret.as_str(), app_id);
                                                if let Some(slot) =
                                                    tools.iter_mut().find(|t| t.id == app_id)
                                                {
                                                    slot.secret_ref = Some(sref.clone());
                                                }
                                                this.save_mcp_tools(
                                                    bot_secret.clone(),
                                                    tools,
                                                    Some((sref, trimmed)),
                                                    vec![secret_in_clone.clone()],
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                    .when_some(clear_secret_ref.clone(), |dd, sref| {
                                        let tc = tools_clear.clone();
                                        let bc = bot_clear_sec.clone();
                                        dd.child(
                                            ui::ghost(
                                                SharedString::from(format!(
                                                    "bots-mcp-secret-clear-{app_id}"
                                                )),
                                                text::CLEAR_SECRET_BTN.get(),
                                            )
                                            .disabled(busy)
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.delete_mcp_secret(
                                                        bc.clone(),
                                                        app_id,
                                                        tc.clone(),
                                                        sref.clone(),
                                                        cx,
                                                    );
                                                }),
                                            ),
                                        )
                                    }),
                            )
                        },
                    ),
            );
        }

        // 3. Custom MCP Endpoints (HTTP URL vs Stdio Command + toggle/status)
        let custom_tools: Vec<BotMcpToolConfig> = bot
            .mcp_tools
            .iter()
            .filter(|t| !BOT_MCP_APPS.iter().any(|curated| curated.id == t.id))
            .cloned()
            .collect();
        let mut custom_card = ui::card(p)
            .id("bots-custom-mcp-card")
            .debug_selector(|| "bots-custom-mcp-card".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(text::CUSTOM_MCP_HEADING.get()),
            );

        if custom_tools.is_empty() {
            custom_card = custom_card.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::CUSTOM_MCP_EMPTY.get()),
            );
        } else {
            for ct in &custom_tools {
                let remove_id = ct.id.clone();
                let toggle_id = ct.id.clone();
                let bot_rem = bot_id.clone();
                let bot_tog = bot_id.clone();
                let tools_rem = bot.mcp_tools.clone();
                let tools_tog = bot.mcp_tools.clone();
                let entity_tog = cx.entity();
                let desc = ct
                    .endpoint_url
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| ct.command.join(" "));
                let srv = relay
                    .connected_tools
                    .iter()
                    .find(|s| s.id == ct.id)
                    .cloned();
                let btn_id = format!("bots-custom-mcp-rm-{}", ct.id);
                let btn_sel = btn_id.clone();
                custom_card =
                    custom_card.child(
                        div()
                            .w_full()
                            .p_2()
                            .rounded(ui::RADIUS_SM)
                            .bg(p.muted.opacity(0.35))
                            .border_1()
                            .border_color(p.border)
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(ui::label(p, ct.name.clone(), desc))
                            .child(
                                ui::controls()
                                    .when_some(srv, |d, st| {
                                        d.child(ui::badge(
                                            p,
                                            if st.connected {
                                                ui::BadgeTone::Success
                                            } else {
                                                ui::BadgeTone::Warning
                                            },
                                            text::MCP_TOOL_CONNECTED
                                                .format(&[("n", &st.tool_count.to_string())]),
                                        ))
                                    })
                                    .child(
                                        ui::switch(
                                            p,
                                            format!("bots-custom-mcp-toggle-{}", ct.id),
                                            ct.enabled,
                                            busy,
                                            move |next, _, cx| {
                                                let mut tools = tools_tog.clone();
                                                if let Some(slot) =
                                                    tools.iter_mut().find(|t| t.id == toggle_id)
                                                {
                                                    slot.enabled = next;
                                                }
                                                let bid = bot_tog.clone();
                                                entity_tog.update(cx, |this, cx| {
                                                    this.save_mcp_tools(
                                                        bid,
                                                        tools,
                                                        None,
                                                        Vec::new(),
                                                        cx,
                                                    );
                                                });
                                            },
                                        )
                                        .aria_label(
                                            format!("{} {}", ct.name, text::ENABLE_LABEL.get()),
                                        ),
                                    )
                                    .child(
                                        ui::ghost(
                                            SharedString::from(btn_id),
                                            text::CUSTOM_MCP_REMOVE_BTN.get(),
                                        )
                                        .debug_selector(move || btn_sel.clone())
                                        .disabled(busy)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                let mut next = tools_rem.clone();
                                                next.retain(|t| t.id != remove_id);
                                                this.save_mcp_tools(
                                                    bot_rem.clone(),
                                                    next,
                                                    None,
                                                    Vec::new(),
                                                    cx,
                                                );
                                            }),
                                        ),
                                    ),
                            ),
                    );
            }
        }

        let bot_add_custom = bot_id.clone();
        let tools_add_custom = bot.mcp_tools.clone();
        let kind_pills = ui::segmented_control(
            p,
            [
                (
                    false,
                    "bots-custom-mcp-kind-http",
                    text::CUSTOM_MCP_KIND_HTTP.get(),
                ),
                (
                    true,
                    "bots-custom-mcp-kind-stdio",
                    text::CUSTOM_MCP_KIND_STDIO.get(),
                ),
            ]
            .into_iter()
            .map(|(is_stdio, id, label)| {
                ui::segment(p, id, label, self.tools.custom_is_stdio == is_stdio).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.tools.custom_is_stdio = is_stdio;
                        cx.notify();
                    }),
                )
            }),
        );

        custom_card = custom_card.child(
            div()
                .w_full()
                .pt_2()
                .border_t_1()
                .border_color(p.border)
                .flex()
                .flex_col()
                .gap_1p5()
                .child(div().flex().items_center().child(kind_pills))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .gap_1p5()
                        .children(
                            self.tools.custom_name.as_ref().map(|i| {
                                div().flex_1().min_w(px(150.)).child(Input::new(i).small())
                            }),
                        )
                        .when(!self.tools.custom_is_stdio, |d| {
                            d.children(self.tools.custom_url.as_ref().map(|i| {
                                div().flex_1().min_w(px(200.)).child(Input::new(i).small())
                            }))
                        })
                        .when(self.tools.custom_is_stdio, |d| {
                            d.children(self.tools.custom_cmd.as_ref().map(|i| {
                                div().flex_1().min_w(px(220.)).child(Input::new(i).small())
                            }))
                        }),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1p5()
                        .children(
                            self.tools.custom_header.as_ref().map(|i| {
                                div().flex_1().min_w(px(150.)).child(Input::new(i).small())
                            }),
                        )
                        .children(
                            self.tools.custom_secret.as_ref().map(|i| {
                                div().flex_1().min_w(px(180.)).child(Input::new(i).small())
                            }),
                        )
                        .child(
                            ui::secondary("bots-custom-mcp-add", text::CUSTOM_MCP_ADD_BTN.get())
                                .debug_selector(|| "bots-custom-mcp-add".into())
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let name = this
                                        .tools
                                        .custom_name
                                        .as_ref()
                                        .map(|i| i.read(cx).value().trim().to_owned())
                                        .unwrap_or_default();
                                    let is_stdio = this.tools.custom_is_stdio;
                                    let url = this
                                        .tools
                                        .custom_url
                                        .as_ref()
                                        .map(|i| i.read(cx).value().trim().to_owned())
                                        .unwrap_or_default();
                                    let cmd_str = this
                                        .tools
                                        .custom_cmd
                                        .as_ref()
                                        .map(|i| i.read(cx).value().trim().to_owned())
                                        .unwrap_or_default();
                                    let target_ok = if is_stdio {
                                        !cmd_str.is_empty()
                                    } else {
                                        !url.is_empty()
                                    };
                                    if name.is_empty() || !target_ok {
                                        this.error =
                                            Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
                                        cx.notify();
                                        return;
                                    }
                                    let header = this
                                        .tools
                                        .custom_header
                                        .as_ref()
                                        .map(|i| i.read(cx).value().trim().to_owned())
                                        .filter(|s| !s.is_empty());
                                    let secret = this
                                        .tools
                                        .custom_secret
                                        .as_ref()
                                        .map(|i| i.read(cx).value().trim().to_owned())
                                        .filter(|s| !s.is_empty());

                                    let id = bot_slug_from_name(&name).as_str().to_owned();
                                    let sref = secret
                                        .as_ref()
                                        .map(|_| mcp_secret_ref(bot_add_custom.as_str(), &id));
                                    let mut next = tools_add_custom.clone();
                                    next.retain(|t| t.id != id);
                                    next.push(BotMcpToolConfig {
                                        id,
                                        name,
                                        endpoint_url: (!is_stdio).then_some(url),
                                        command: if is_stdio {
                                            cmd_str.split_whitespace().map(str::to_owned).collect()
                                        } else {
                                            Vec::new()
                                        },
                                        auth_kind: if sref.is_some() {
                                            BotMcpAuthKind::BearerHeader
                                        } else {
                                            BotMcpAuthKind::None
                                        },
                                        header_names: header.into_iter().collect(),
                                        secret_ref: sref.clone(),
                                        enabled: true,
                                    });
                                    let to_store = sref.zip(secret);
                                    let clears = vec![
                                        this.tools.custom_name.clone(),
                                        this.tools.custom_url.clone(),
                                        this.tools.custom_cmd.clone(),
                                        this.tools.custom_header.clone(),
                                        this.tools.custom_secret.clone(),
                                    ];
                                    this.save_mcp_tools(
                                        bot_add_custom.clone(),
                                        next,
                                        to_store,
                                        clears,
                                        cx,
                                    );
                                })),
                        ),
                ),
        );

        // 4. Sharing & Migration (.zip export/import)
        let bot_export = bot_id;
        let entity_mem = cx.entity();
        let sharing_card = ui::card(p)
            .id("bots-sharing-card")
            .debug_selector(|| "bots-sharing-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(ui::label(
                p,
                text::SHARING_HEADING.get(),
                Some(text::SHARING_DESC.get()),
            ))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                ui::switch(
                                    p,
                                    "bots-export-include-memory",
                                    self.tools.export_include_memory,
                                    self.busy,
                                    move |next, _, cx| {
                                        entity_mem.update(cx, |this, cx| {
                                            this.tools.export_include_memory = next;
                                            cx.notify();
                                        });
                                    },
                                )
                                .aria_label(text::EXPORT_INCLUDE_MEMORY.get()),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.foreground)
                                    .child(text::EXPORT_INCLUDE_MEMORY.get()),
                            ),
                    )
                    .child(
                        ui::controls()
                            .child(
                                ui::secondary("bots-sharing-export", text::EXPORT_BOT_BTN.get())
                                    .debug_selector(|| "bots-sharing-export".into())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.export_bot_zip(bot_export.clone(), cx);
                                    })),
                            )
                            .child(
                                ui::secondary(
                                    "bots-sharing-import-zip",
                                    text::IMPORT_ZIP_BTN.get(),
                                )
                                .debug_selector(|| "bots-sharing-import-zip".into())
                                .disabled(self.import_state.scanning)
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.import_bot_zip_from_dialog(cx);
                                    },
                                )),
                            )
                            .child(
                                ui::secondary(
                                    "bots-sharing-import-external",
                                    text::IMPORT_EXTERNAL_TITLE.get(),
                                )
                                .debug_selector(|| "bots-sharing-import-external".into())
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.open_import(cx);
                                    },
                                )),
                            ),
                    ),
            );

        ToolsCards {
            relay: relay_card.into_any_element(),
            curated: ui::card(p).child(curated_list).into_any_element(),
            custom: custom_card.into_any_element(),
            sharing: sharing_card.into_any_element(),
        }
    }
}
