// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Tab 4 — Tools & Sharing (13 curated MCP apps, custom MCP endpoints, `BotToolRelay` status,
//! sanitized .zip export/import) + dedicated Import screen (Orca/OpenClaw/Hermes/OpenBot/.zip).

use std::collections::BTreeMap;

use gpui::{App, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_proto::bots::{
    BOT_MCP_APPS, BotConfig, BotId, BotMcpAuthKind, BotMcpToolConfig, BotPackageExportOptions,
    BotRpcRequest, BotRpcResponse, BotToolRelayStatus, ExternalBotCandidate, ExternalBotSourceApp,
    FieldPatch, MAX_RPC_ZIP_BYTES, UpdateBotParams, bot_slug_from_name,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::rpc::mcp_secret_ref;
use super::{Close, DetailTab, Screen, View, opt_input_composing};

#[derive(Default)]
pub(super) struct ToolsState {
    pub(super) relay_status: Option<BotToolRelayStatus>,
    pub(super) app_secret_inputs: BTreeMap<String, Entity<InputState>>,
    pub(super) custom_name: Option<Entity<InputState>>,
    pub(super) custom_url: Option<Entity<InputState>>,
    pub(super) custom_header: Option<Entity<InputState>>,
    pub(super) custom_secret: Option<Entity<InputState>>,
    pub(super) export_include_memory: bool,
}

impl ToolsState {
    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        opt_input_composing(self.custom_name.as_ref(), window, app)
            || opt_input_composing(self.custom_url.as_ref(), window, app)
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
    pub(super) scanning: bool,
}

fn auth_kind_label(kind: BotMcpAuthKind) -> &'static str {
    match kind {
        BotMcpAuthKind::BrowserOAuth => text::MCP_AUTH_OAUTH.get(),
        BotMcpAuthKind::BearerHeader => text::MCP_AUTH_BEARER.get(),
        BotMcpAuthKind::None => text::MCP_AUTH_NONE.get(),
    }
}

fn external_source_label(app: ExternalBotSourceApp) -> &'static str {
    match app {
        ExternalBotSourceApp::Orca => text::IMPORT_SOURCE_ORCA.get(),
        ExternalBotSourceApp::OpenClaw => text::IMPORT_SOURCE_OPENCLAW.get(),
        ExternalBotSourceApp::Hermes => text::IMPORT_SOURCE_HERMES.get(),
        ExternalBotSourceApp::OpenBot => text::IMPORT_SOURCE_OPENBOT.get(),
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
            self.tools.custom_url = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::CUSTOM_MCP_URL_PLACEHOLDER.get())
            }));
            self.tools.custom_header = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::CUSTOM_MCP_HEADER_PLACEHOLDER.get())
            }));
            self.tools.custom_secret = Some(cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(text::MCP_SECRET_PLACEHOLDER.get())
                    .masked(true)
            }));
        }
    }

    fn ensure_app_secret_input(
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
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { rpc(&BotRpcRequest::ToolRelayStatus) })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(BotRpcResponse::ToolRelayStatus(st)) = res {
                    this.tools.relay_status = Some(st);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn save_mcp_tools(
        &mut self,
        bot_id: BotId,
        tools: Vec<BotMcpToolConfig>,
        secret_to_store: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    if let Some((secret_ref, value)) = secret_to_store {
                        match rpc(&BotRpcRequest::SetSecret { secret_ref, value })? {
                            BotRpcResponse::Ok => {}
                            BotRpcResponse::Error(msg) => return Err(msg),
                            other => {
                                return Err(format!("unexpected SetSecret response: {other:?}"));
                            }
                        }
                    }
                    rpc(&BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
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
                    })))
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.upsert_bot_config(*updated);
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                        this.refresh_tool_relay_status(cx);
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

    pub fn export_bot_zip(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        let include_memory = self.tools.export_include_memory;
        let default_name = self
            .bots
            .iter()
            .find(|b| b.id == bot_id)
            .map_or_else(|| "bot.zip".to_owned(), |b| format!("{}.zip", b.slug));
        let start_dir = self
            .default_cwd
            .clone()
            .unwrap_or_else(crate::accounts::default_home);
        let save_target = cx.prompt_for_new_path(&start_dir, Some(&default_name));
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(chosen_path))) = save_target.await else {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    cx.notify();
                })
                .ok();
                return;
            };
            let path_bg = chosen_path.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    match rpc(&BotRpcRequest::ExportBotZip {
                        bot_id,
                        options: BotPackageExportOptions {
                            include_memory,
                            include_avatar: true,
                        },
                    })? {
                        BotRpcResponse::ExportedZip { zip_bytes, .. } => {
                            std::fs::write(&path_bg, zip_bytes).map_err(|e| e.to_string())?;
                            Ok(path_bg.to_string_lossy().into_owned())
                        }
                        BotRpcResponse::Error(msg) => Err(msg),
                        other => Err(format!("unexpected ExportBotZip response: {other:?}")),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(saved_path) => {
                        let msg = text::EXPORT_SUCCESS.format(&[("path", &saved_path)]);
                        this.notice = Some(SharedString::from(msg));
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
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(text::IMPORT_ZIP_BTN.get().into()),
        });
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let path = match chosen.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            };
            let Some(path) = path else {
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
                    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                    if bytes.len() > MAX_RPC_ZIP_BYTES {
                        let max_kib = (MAX_RPC_ZIP_BYTES / 1024).to_string();
                        return Err(text::IMPORT_ZIP_TOO_LARGE.format(&[("max", &max_kib)]));
                    }
                    rpc(&BotRpcRequest::ImportBotZip {
                        zip_bytes: bytes,
                        name_override: None,
                        agent_override: None,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::Bot(imported)) => {
                        let id = imported.id.clone();
                        this.upsert_bot_config(*imported);
                        this.selected_bot_id = Some(id.clone());
                        this.screen = Screen::Detail;
                        this.detail_tab = DetailTab::Items;
                        this.notice = Some(SharedString::from(
                            text::IMPORT_SUCCESS.format(&[("n", "1")]),
                        ));
                        this.load_selected_bot_detail(id, cx);
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

    pub fn run_external_import(&mut self, dry_run: bool, cx: &mut Context<Self>) {
        let source = self.import_state.source_filter;
        let rpc = self.rpc.clone();
        self.import_state.scanning = true;
        self.error = None;
        self.notice = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    rpc(&BotRpcRequest::ImportExternalBots {
                        source,
                        path: None,
                        dry_run,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.import_state.scanning = false;
                match res {
                    Ok(BotRpcResponse::ImportedExternalBots {
                        candidates,
                        imported,
                    }) => {
                        this.import_state.candidates = candidates;
                        if dry_run {
                            let msg = text::IMPORT_SCAN_FOUND
                                .format(&[("n", &this.import_state.candidates.len().to_string())]);
                            this.notice = Some(SharedString::from(msg));
                        } else {
                            let count = imported.len();
                            let first_id = imported.first().map(|b| b.id.clone());
                            for b in imported {
                                this.upsert_bot_config(b);
                            }
                            let msg = text::IMPORT_SUCCESS.format(&[("n", &count.to_string())]);
                            this.notice = Some(SharedString::from(msg));
                            if let Some(id) = first_id {
                                this.selected_bot_id = Some(id.clone());
                                this.screen = Screen::Detail;
                                this.load_selected_bot_detail(id, cx);
                            }
                        }
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

    #[expect(
        clippy::too_many_lines,
        reason = "BotToolRelay card, 13 curated MCP apps, custom endpoints, and sharing"
    )]
    pub(super) fn render_tools_sharing_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_tools_inputs(window, cx);
        let bot_id = bot.id.clone();

        // 1. Live BotToolRelay loopback status card
        let relay = self.tools.relay_status.clone().unwrap_or_default();
        let relay_badge = if relay.running {
            ui::badge(p, ui::BadgeTone::Success, text::TOOL_RELAY_RUNNING.get())
        } else {
            ui::badge(p, ui::BadgeTone::Muted, text::TOOL_RELAY_STOPPED.get())
        };
        let relay_count =
            text::TOOL_RELAY_TOOLS_COUNT.format(&[("n", &relay.connected_tools.len().to_string())]);
        let mut relay_card = ui::card(p)
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
                            .child(relay_badge)
                            .child(ui::badge(p, ui::BadgeTone::Neutral, relay_count))
                            .child(
                                ui::secondary(
                                    "bots-relay-refresh",
                                    text::TOOL_RELAY_REFRESH_BTN.get(),
                                )
                                .debug_selector(|| "bots-relay-refresh".into())
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.refresh_tool_relay_status(cx);
                                    },
                                )),
                            ),
                    ),
            );
        if let Some(addr) = relay.endpoint_url.or(relay.listen_addr) {
            relay_card = relay_card.child(
                div()
                    .font_family(ui::FONT_MONO)
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(addr),
            );
        }
        if let Some(err) = relay.last_error {
            relay_card = relay_card.child(ui::error_banner(p, err));
        }

        // 2. All 13 Curated MCP Apps
        let enabled_curated = BOT_MCP_APPS
            .iter()
            .filter(|entry| bot.mcp_tools.iter().any(|t| t.id == entry.id && t.enabled))
            .count();
        let curated_heading = text::CURATED_MCP_HEADING.format(&[
            ("enabled", &enabled_curated.to_string()),
            ("total", &BOT_MCP_APPS.len().to_string()),
        ]);

        let mut curated_cards = div()
            .id("bots-curated-mcp-list")
            .debug_selector(|| "bots-curated-mcp-list".into())
            .w_full()
            .flex()
            .flex_col()
            .gap_2();

        for app in BOT_MCP_APPS {
            let existing = bot.mcp_tools.iter().find(|t| t.id == app.id).cloned();
            let is_enabled = existing.as_ref().is_some_and(|t| t.enabled);
            let has_secret = existing.as_ref().is_some_and(|t| t.secret_ref.is_some());
            let secret_input = self.ensure_app_secret_input(app.id, window, cx);
            let current_tools = bot.mcp_tools.clone();
            let bot_toggle = bot_id.clone();
            let entity = cx.entity();
            let app_id = app.id;
            let app_name = app.name;
            let app_url = app.endpoint_url;
            let app_auth = app.auth_kind;
            let app_header = app.header_name;
            let docs_url = app.docs_url;

            let bot_secret = bot_id.clone();
            let tools_secret = bot.mcp_tools.clone();
            let secret_in_clone = secret_input.clone();

            curated_cards = curated_cards.child(
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
                                    .child(ui::switch(
                                        p,
                                        format!("bots-mcp-toggle-{app_id}"),
                                        is_enabled,
                                        self.busy,
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
                                                this.save_mcp_tools(bid, tools, None, cx);
                                            });
                                        },
                                    )),
                            ),
                    )
                    .when(is_enabled && app.auth_kind != BotMcpAuthKind::None, |d| {
                        d.child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .children(
                                    secret_input.as_ref().map(|i| {
                                        div().flex_1().min_w_0().child(Input::new(i).small())
                                    }),
                                )
                                .child(
                                    ui::secondary(
                                        SharedString::from(format!(
                                            "bots-mcp-secret-save-{app_id}"
                                        )),
                                        text::SAVE_SECRET_BTN.get(),
                                    )
                                    .disabled(self.busy)
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            let val = secret_in_clone
                                                .as_ref()
                                                .map(|i| {
                                                    let v = i.read(cx).value().to_string();
                                                    i.update(cx, |s, cx| {
                                                        s.set_value(String::new(), window, cx);
                                                    });
                                                    v
                                                })
                                                .unwrap_or_default();
                                            let trimmed = val.trim().to_owned();
                                            if trimmed.is_empty() {
                                                return;
                                            }
                                            let mut tools = tools_secret.clone();
                                            let sref = mcp_secret_ref(bot_secret.as_str(), app_id);
                                            if let Some(slot) =
                                                tools.iter_mut().find(|t| t.id == app_id)
                                            {
                                                slot.secret_ref = Some(sref.clone());
                                            }
                                            this.save_mcp_tools(
                                                bot_secret.clone(),
                                                tools,
                                                Some((sref, trimmed)),
                                                cx,
                                            );
                                        },
                                    )),
                                ),
                        )
                    }),
            );
        }

        // 3. Custom MCP Endpoints
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
                let bot_rem = bot_id.clone();
                let tools_rem = bot.mcp_tools.clone();
                let endpoint = ct.endpoint_url.clone().unwrap_or_default();
                let btn_id = format!("bots-custom-mcp-rm-{}", ct.id);
                let btn_sel = btn_id.clone();
                custom_card = custom_card.child(
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
                        .child(ui::label(p, ct.name.clone(), endpoint))
                        .child(
                            ui::ghost(
                                SharedString::from(btn_id),
                                text::CUSTOM_MCP_REMOVE_BTN.get(),
                            )
                            .debug_selector(move || btn_sel.clone())
                            .disabled(self.busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let mut next = tools_rem.clone();
                                    next.retain(|t| t.id != remove_id);
                                    this.save_mcp_tools(bot_rem.clone(), next, None, cx);
                                },
                            )),
                        ),
                );
            }
        }

        let bot_add_custom = bot_id.clone();
        let tools_add_custom = bot.mcp_tools.clone();
        custom_card =
            custom_card.child(
                div()
                    .w_full()
                    .pt_2()
                    .border_t_1()
                    .border_color(p.border)
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_wrap()
                            .gap_1p5()
                            .children(self.tools.custom_name.as_ref().map(|i| {
                                div().flex_1().min_w(px(150.)).child(Input::new(i).small())
                            }))
                            .children(self.tools.custom_url.as_ref().map(|i| {
                                div().flex_1().min_w(px(200.)).child(Input::new(i).small())
                            })),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1p5()
                            .children(self.tools.custom_header.as_ref().map(|i| {
                                div().flex_1().min_w(px(150.)).child(Input::new(i).small())
                            }))
                            .children(self.tools.custom_secret.as_ref().map(|i| {
                                div().flex_1().min_w(px(180.)).child(Input::new(i).small())
                            }))
                            .child(
                                ui::secondary(
                                    "bots-custom-mcp-add",
                                    text::CUSTOM_MCP_ADD_BTN.get(),
                                )
                                .debug_selector(|| "bots-custom-mcp-add".into())
                                .disabled(self.busy)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        let name = this
                                            .tools
                                            .custom_name
                                            .as_ref()
                                            .map(|i| i.read(cx).value().trim().to_owned())
                                            .unwrap_or_default();
                                        let url = this
                                            .tools
                                            .custom_url
                                            .as_ref()
                                            .map(|i| i.read(cx).value().trim().to_owned())
                                            .unwrap_or_default();
                                        if name.is_empty() || url.is_empty() {
                                            return;
                                        }
                                        let header = this
                                            .tools
                                            .custom_header
                                            .as_ref()
                                            .map(|i| i.read(cx).value().trim().to_owned())
                                            .filter(|s| !s.is_empty());
                                        let secret_val = this
                                            .tools
                                            .custom_secret
                                            .as_ref()
                                            .map(|i| i.read(cx).value().trim().to_owned())
                                            .filter(|s| !s.is_empty());
                                        for i in [
                                            &this.tools.custom_name,
                                            &this.tools.custom_url,
                                            &this.tools.custom_header,
                                            &this.tools.custom_secret,
                                        ]
                                        .into_iter()
                                        .flatten()
                                        {
                                            i.update(cx, |s, cx| {
                                                s.set_value(String::new(), window, cx);
                                            });
                                        }
                                        let id = bot_slug_from_name(&name).as_str().to_owned();
                                        let secret_ref = secret_val
                                            .as_ref()
                                            .map(|_| mcp_secret_ref(bot_add_custom.as_str(), &id));
                                        let auth_kind = if secret_ref.is_some() || header.is_some()
                                        {
                                            BotMcpAuthKind::BearerHeader
                                        } else {
                                            BotMcpAuthKind::None
                                        };
                                        let mut next = tools_add_custom.clone();
                                        next.retain(|t| t.id != id);
                                        next.push(BotMcpToolConfig {
                                            id,
                                            name,
                                            endpoint_url: Some(url),
                                            command: Vec::new(),
                                            auth_kind,
                                            header_names: header.into_iter().collect(),
                                            secret_ref: secret_ref.clone(),
                                            enabled: true,
                                        });
                                        let secret_pair = secret_ref.zip(secret_val);
                                        this.save_mcp_tools(
                                            bot_add_custom.clone(),
                                            next,
                                            secret_pair,
                                            cx,
                                        );
                                    },
                                )),
                            ),
                    ),
            );

        // 4. Sharing & Migration card
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
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::EXPORT_INCLUDE_MEMORY.get()),
                    )
                    .child(ui::switch(
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
                    )),
            )
            .child(
                ui::controls()
                    .child(
                        ui::primary("bots-sharing-export", text::EXPORT_BOT_BTN.get())
                            .debug_selector(|| "bots-sharing-export".into())
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.export_bot_zip(bot_export.clone(), cx);
                            })),
                    )
                    .child(
                        ui::secondary("bots-sharing-import-zip", text::IMPORT_ZIP_BTN.get())
                            .debug_selector(|| "bots-sharing-import-zip".into())
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.import_bot_zip_from_dialog(cx);
                            })),
                    )
                    .child(
                        ui::secondary(
                            "bots-sharing-import-external",
                            text::IMPORT_EXTERNAL_TITLE.get(),
                        )
                        .debug_selector(|| "bots-sharing-import-external".into())
                        .on_click(cx.listener(|this, _, _, cx| this.open_import(cx))),
                    ),
            );

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
            .child(relay_card)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(curated_heading),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::CURATED_MCP_DESC.get()),
                    ),
            )
            .child(curated_cards)
            .child(custom_card)
            .child(sharing_card)
    }

    pub(super) fn render_import_view(
        &mut self,
        p: &ui::Palette,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let header = div()
            .flex_none()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(p.border)
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(text::IMPORT_TITLE.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::IMPORT_DESC.get()),
                    ),
            )
            .child(
                ui::controls()
                    .when(!self.bots.is_empty(), |d| {
                        d.child(
                            ui::ghost("bots-import-back", text::BACK.get())
                                .debug_selector(|| "bots-import-back".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.screen = Screen::Detail;
                                    cx.notify();
                                })),
                        )
                    })
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
            );

        let sources = [
            (None, "bots-import-src-all", text::IMPORT_SOURCE_ALL.get()),
            (
                Some(ExternalBotSourceApp::Orca),
                "bots-import-src-orca",
                text::IMPORT_SOURCE_ORCA.get(),
            ),
            (
                Some(ExternalBotSourceApp::OpenClaw),
                "bots-import-src-openclaw",
                text::IMPORT_SOURCE_OPENCLAW.get(),
            ),
            (
                Some(ExternalBotSourceApp::Hermes),
                "bots-import-src-hermes",
                text::IMPORT_SOURCE_HERMES.get(),
            ),
            (
                Some(ExternalBotSourceApp::OpenBot),
                "bots-import-src-openbot",
                text::IMPORT_SOURCE_OPENBOT.get(),
            ),
        ];
        let source_bar = ui::segmented_control(
            p,
            sources.into_iter().map(|(src, id, lbl)| {
                ui::segment(p, id, lbl, self.import_state.source_filter == src)
                    .debug_selector(move || id.into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.import_state.source_filter = src;
                        cx.notify();
                    }))
            }),
        );

        let mut candidates_list = div()
            .id("bots-import-candidates")
            .debug_selector(|| "bots-import-candidates".into())
            .w_full()
            .flex()
            .flex_col()
            .gap_2();
        if self.import_state.candidates.is_empty() {
            candidates_list = candidates_list.child(
                div()
                    .py_4()
                    .text_center()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::IMPORT_EMPTY.get()),
            );
        } else {
            for (idx, cand) in self.import_state.candidates.iter().enumerate() {
                candidates_list = candidates_list.child(
                    div()
                        .id(SharedString::from(format!("bots-import-cand-{idx}")))
                        .w_full()
                        .p_2p5()
                        .rounded(ui::RADIUS_SM)
                        .bg(p.muted.opacity(0.35))
                        .border_1()
                        .border_color(p.border)
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(ui::badge(
                                    p,
                                    ui::BadgeTone::Accent,
                                    external_source_label(cand.app),
                                ))
                                .child(
                                    div()
                                        .text_size(ui::TEXT_SM)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.foreground)
                                        .child(cand.name.clone()),
                                )
                                .child(ui::badge(p, ui::BadgeTone::Muted, cand.agent.clone())),
                        )
                        .child(
                            div()
                                .font_family(ui::FONT_MONO)
                                .text_size(ui::TEXT_2XS)
                                .text_color(p.muted_foreground)
                                .truncate()
                                .child(cand.path.clone()),
                        )
                        .when_some(cand.soul_preview.as_ref(), |d, preview| {
                            d.child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(preview.clone()),
                            )
                        }),
                );
            }
        }

        div()
            .id("bots-import-view")
            .debug_selector(|| "bots-import-view".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .children(
                self.error
                    .as_ref()
                    .map(|err| div().px_5().pt_3().child(ui::error_banner(p, err.clone()))),
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
            .child(
                div()
                    .id("bots-import-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .py_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        ui::row(p)
                            .child(ui::label(
                                p,
                                text::IMPORT_ZIP_BTN.get(),
                                Some(text::SHARING_DESC.get()),
                            ))
                            .child(
                                ui::controls().child(
                                    ui::primary("bots-import-zip-btn", text::IMPORT_ZIP_BTN.get())
                                        .debug_selector(|| "bots-import-zip-btn".into())
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.import_bot_zip_from_dialog(cx);
                                        })),
                                ),
                            ),
                    )
                    .child(
                        ui::card(p)
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.foreground)
                                    .child(text::IMPORT_EXTERNAL_TITLE.get()),
                            )
                            .child(source_bar)
                            .child(
                                ui::controls()
                                    .child(
                                        ui::secondary(
                                            "bots-import-scan",
                                            text::SCAN_EXTERNAL_BTN.get(),
                                        )
                                        .debug_selector(|| "bots-import-scan".into())
                                        .disabled(self.import_state.scanning)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.run_external_import(true, cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        ui::primary(
                                            "bots-import-apply",
                                            text::IMPORT_ALL_BTN.get(),
                                        )
                                        .debug_selector(|| "bots-import-apply".into())
                                        .disabled(self.import_state.scanning)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.run_external_import(false, cx);
                                            }),
                                        ),
                                    ),
                            )
                            .child(candidates_list),
                    ),
            )
    }
}
