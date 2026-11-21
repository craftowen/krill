// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Bot Settings tab: identity, PNG avatar upload, accent color, runtime/model/effort/permission,
//! workspace picker, notification toggles, and recurring cron schedules CRUD.

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{App, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use krill_proto::agent::Permission;
use krill_proto::bots::{
    BotConfig, BotId, BotNotifications, BotRpcRequest, BotRpcResponse, BotScheduleConfig,
    FieldPatch, UpdateBotExtrasParams, UpdateBotParams,
};

use crate::i18n::catalog::bots as text;
use crate::layout::TabColor;
use crate::ui;

use super::rpc::{
    BOT_AVATAR_COLOR_PALETTE, BOT_EFFORT_OPTIONS, BOT_RUNTIMES, avatar_color_hex, call_checked,
    decode_png_avatar, effort_label, expect_variant, parse_avatar_color,
};
use super::{View, clear_input_on_window, now_unix_ms, opt_input_composing};

#[derive(Default)]
pub(super) struct SettingsState {
    pub(super) synced_bot_id: Option<BotId>,
    pub(super) needs_sync_from_bot: bool,
    pub(super) name_input: Option<Entity<InputState>>,
    pub(super) desc_input: Option<Entity<InputState>>,
    pub(super) emoji_input: Option<Entity<InputState>>,
    pub(super) model_input: Option<Entity<InputState>>,
    pub(super) workspace_input: Option<Entity<InputState>>,
    pub(super) agent_id: String,
    pub(super) effort: String,
    pub(super) permission_mode: Permission,
    pub(super) avatar_color: Option<TabColor>,
    pub(super) notifications: BotNotifications,
    pub(super) schedules: Vec<BotScheduleConfig>,
    pub(super) sched_name_input: Option<Entity<InputState>>,
    pub(super) sched_cron_input: Option<Entity<InputState>>,
    pub(super) sched_prompt_input: Option<Entity<InputState>>,
    pub(super) avatar_cache: BTreeMap<BotId, Arc<gpui::Image>>,
    pub(super) saving: bool,
}

impl SettingsState {
    pub(super) fn sync_from_bot(&mut self, bot: &BotConfig) {
        self.synced_bot_id = Some(bot.id.clone());
        self.agent_id.clone_from(&bot.agent_id);
        self.effort = bot.effort.clone().unwrap_or_default();
        self.permission_mode = bot.permission_mode;
        self.avatar_color = parse_avatar_color(bot.avatar_color.as_deref());
        self.notifications = bot.notifications;
        self.schedules.clone_from(&bot.schedules);
        self.needs_sync_from_bot = true;
    }

    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        opt_input_composing(self.name_input.as_ref(), window, app)
            || opt_input_composing(self.desc_input.as_ref(), window, app)
            || opt_input_composing(self.emoji_input.as_ref(), window, app)
            || opt_input_composing(self.model_input.as_ref(), window, app)
            || opt_input_composing(self.workspace_input.as_ref(), window, app)
            || opt_input_composing(self.sched_name_input.as_ref(), window, app)
            || opt_input_composing(self.sched_cron_input.as_ref(), window, app)
            || opt_input_composing(self.sched_prompt_input.as_ref(), window, app)
    }

    pub(super) fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<View>) {
        let updates = [
            (&self.name_input, text::FIELD_NAME_PLACEHOLDER.get()),
            (&self.desc_input, text::FIELD_DESCRIPTION_PLACEHOLDER.get()),
            (&self.emoji_input, text::FIELD_EMOJI_PLACEHOLDER.get()),
            (&self.model_input, text::FIELD_MODEL_PLACEHOLDER.get()),
            (
                &self.workspace_input,
                text::FIELD_WORKSPACE_PLACEHOLDER.get(),
            ),
            (
                &self.sched_name_input,
                text::SCHEDULE_NAME_PLACEHOLDER.get(),
            ),
            (
                &self.sched_cron_input,
                text::SCHEDULE_CRON_PLACEHOLDER.get(),
            ),
            (
                &self.sched_prompt_input,
                text::SCHEDULE_PROMPT_PLACEHOLDER.get(),
            ),
        ];
        for (slot, placeholder) in updates {
            if let Some(input) = slot {
                input.update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            }
        }
    }
}

impl View {
    fn ensure_settings_inputs(
        &mut self,
        bot: &BotConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        if self.settings.name_input.is_none() {
            self.settings.name_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_NAME_PLACEHOLDER.get())
            }));
        }
        if self.settings.desc_input.is_none() {
            self.settings.desc_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_DESCRIPTION_PLACEHOLDER.get())
            }));
        }
        if self.settings.emoji_input.is_none() {
            self.settings.emoji_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_EMOJI_PLACEHOLDER.get())
            }));
        }
        if self.settings.model_input.is_none() {
            self.settings.model_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_MODEL_PLACEHOLDER.get())
            }));
        }
        if self.settings.workspace_input.is_none() {
            self.settings.workspace_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_WORKSPACE_PLACEHOLDER.get())
            }));
        }
        if self.settings.sched_name_input.is_none() {
            self.settings.sched_name_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::SCHEDULE_NAME_PLACEHOLDER.get())
            }));
        }
        if self.settings.sched_cron_input.is_none() {
            self.settings.sched_cron_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::SCHEDULE_CRON_PLACEHOLDER.get())
            }));
        }
        if self.settings.sched_prompt_input.is_none() {
            self.settings.sched_prompt_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::SCHEDULE_PROMPT_PLACEHOLDER.get())
            }));
        }
        if self.settings.synced_bot_id.as_ref() != Some(&bot.id) {
            self.settings.sync_from_bot(bot);
        }
        if self.settings.needs_sync_from_bot {
            self.settings.needs_sync_from_bot = false;
            if let Some(i) = &self.settings.name_input {
                let v = bot.name.clone();
                i.update(cx, |s, cx| s.set_value(v, window, cx));
            }
            if let Some(i) = &self.settings.desc_input {
                let v = bot.description.clone();
                i.update(cx, |s, cx| s.set_value(v, window, cx));
            }
            if let Some(i) = &self.settings.emoji_input {
                let v = bot.emoji.clone().unwrap_or_default();
                i.update(cx, |s, cx| s.set_value(v, window, cx));
            }
            if let Some(i) = &self.settings.model_input {
                let v = bot.model.clone().unwrap_or_default();
                i.update(cx, |s, cx| s.set_value(v, window, cx));
            }
            if let Some(i) = &self.settings.workspace_input {
                let v = bot.workspace.clone().unwrap_or_default();
                i.update(cx, |s, cx| s.set_value(v, window, cx));
            }
        }
    }

    fn pick_settings_workspace(window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(text::FIELD_WORKSPACE.get().into()),
        });
        let win_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await
                && let Some(path) = paths.into_iter().next()
            {
                let path_str = path.to_string_lossy().into_owned();
                win_handle
                    .update(cx, |_, window, cx| {
                        this.update(cx, |this, cx| {
                            if let Some(input) = &this.settings.workspace_input {
                                input.update(cx, |s, cx| s.set_value(path_str, window, cx));
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .ok();
            }
        })
        .detach();
    }

    fn pick_and_upload_avatar(
        &mut self,
        bot_id: BotId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(text::SETTINGS_AVATAR_LABEL.get().into()),
        });
        let rpc = self.rpc.clone();
        self.error = None;
        self.avatar_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let bot_id_bg = bot_id.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                    let image = decode_png_avatar(&bytes)?;
                    let updated = expect_variant(
                        &rpc,
                        &BotRpcRequest::UploadBotAvatar {
                            bot_id: bot_id_bg,
                            png_bytes: bytes,
                        },
                        |r| match r {
                            BotRpcResponse::Bot(b) => Some(*b),
                            _ => None,
                        },
                        "Bot",
                    )?;
                    Ok::<_, String>((updated, image))
                })
                .await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok((updated, image)) => {
                        this.settings.avatar_cache.insert(updated.id.clone(), image);
                        this.upsert_bot_config(updated);
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        let _ = window;
    }

    pub fn clear_bot_avatar(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.settings.saving {
            return;
        }
        let rpc = self.rpc.clone();
        self.settings.saving = true;
        self.error = None;
        self.avatar_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                            bot_id: bot_id_bg,
                            name: None,
                            description: None,
                            emoji: FieldPatch::Unchanged,
                            avatar: FieldPatch::Clear,
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
                            mcp_tools: None,
                        })),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.settings.saving = false;
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.settings.avatar_cache.remove(&bot_id);
                        this.upsert_bot_config(*updated);
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                    }
                    Ok(_) => {
                        this.settings.avatar_cache.remove(&bot_id);
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn save_bot_settings(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.settings.saving {
            return;
        }
        let name = self
            .settings
            .name_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        if name.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_NAME.get()));
            cx.notify();
            return;
        }
        let description = self
            .settings
            .desc_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        let emoji = match self
            .settings
            .emoji_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
        {
            Some(v) if !v.is_empty() => FieldPatch::Set(v),
            Some(_) => FieldPatch::Clear,
            None => FieldPatch::Unchanged,
        };
        let model = match self
            .settings
            .model_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
        {
            Some(v) if !v.is_empty() => FieldPatch::Set(v),
            Some(_) => FieldPatch::Clear,
            None => FieldPatch::Unchanged,
        };
        let workspace = match self
            .settings
            .workspace_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
        {
            Some(v) if !v.is_empty() => FieldPatch::Set(v),
            Some(_) => FieldPatch::Clear,
            None => FieldPatch::Unchanged,
        };
        let effort = if self.settings.effort.trim().is_empty() {
            FieldPatch::Clear
        } else {
            FieldPatch::Set(self.settings.effort.clone())
        };
        let avatar_color_patch = match self.settings.avatar_color {
            Some(color) => FieldPatch::Set(avatar_color_hex(color).to_owned()),
            None => FieldPatch::Clear,
        };
        let params = UpdateBotParams {
            bot_id: bot_id.clone(),
            name: Some(name),
            description: Some(description),
            emoji,
            avatar: FieldPatch::Unchanged,
            agent_id: (!self.settings.agent_id.is_empty()).then(|| self.settings.agent_id.clone()),
            model,
            effort,
            permission_mode: Some(self.settings.permission_mode),
            workspace,
            paused: None,
            first_run_completed: None,
            notifications: Some(self.settings.notifications),
            schedules: Some(self.settings.schedules.clone()),
            look_around: None,
            memory_tidy: None,
            mcp_tools: None,
        };
        let extras = UpdateBotExtrasParams {
            bot_id,
            avatar_color: avatar_color_patch,
            skills: None,
            events: None,
        };

        let rpc = self.rpc.clone();
        self.settings.saving = true;
        self.error = None;
        self.notice = None;
        self.mutation_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let first = call_checked(&rpc, &BotRpcRequest::UpdateBot(Box::new(params)))?;
                    let second =
                        call_checked(&rpc, &BotRpcRequest::UpdateBotExtras(Box::new(extras)))?;
                    Ok::<_, String>(match second {
                        BotRpcResponse::Bot(b) => Some(*b),
                        _ => match first {
                            BotRpcResponse::Bot(b) => Some(*b),
                            _ => None,
                        },
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.settings.saving = false;
                match res {
                    Ok(Some(updated)) => {
                        this.settings.sync_from_bot(&updated);
                        this.upsert_bot_config(updated);
                        this.emit_sessions_changed(cx);
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                    }
                    Ok(None) => {
                        this.notice = Some(SharedString::from(text::SAVED.get()));
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

    pub fn add_schedule(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.settings.saving {
            return;
        }
        let name = self
            .settings
            .sched_name_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        let cron = self
            .settings
            .sched_cron_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        let prompt = self
            .settings
            .sched_prompt_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        if name.is_empty() || cron.is_empty() || prompt.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_SCHEDULE.get()));
            cx.notify();
            return;
        }
        let mut next = self.settings.schedules.clone();
        next.push(BotScheduleConfig {
            id: format!("sched-{}", now_unix_ms()),
            name,
            cron,
            prompt,
            enabled: true,
            timezone: None,
        });
        self.persist_schedules(bot_id, next, true, cx);
    }

    pub fn toggle_schedule_enabled(
        &mut self,
        bot_id: BotId,
        schedule_id: &str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let mut next = self.settings.schedules.clone();
        if let Some(slot) = next.iter_mut().find(|s| s.id == schedule_id) {
            slot.enabled = enabled;
        }
        self.persist_schedules(bot_id, next, false, cx);
    }

    pub fn remove_schedule(&mut self, bot_id: BotId, schedule_id: &str, cx: &mut Context<Self>) {
        let mut next = self.settings.schedules.clone();
        next.retain(|s| s.id != schedule_id);
        self.persist_schedules(bot_id, next, false, cx);
    }

    fn persist_schedules(
        &mut self,
        bot_id: BotId,
        schedules: Vec<BotScheduleConfig>,
        clear_inputs: bool,
        cx: &mut Context<Self>,
    ) {
        if self.settings.saving {
            return;
        }
        let params = UpdateBotParams {
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
            schedules: Some(schedules),
            look_around: None,
            memory_tidy: None,
            mcp_tools: None,
        };
        let rpc = self.rpc.clone();
        self.settings.saving = true;
        self.error = None;
        self.mutation_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(
                    async move { call_checked(&rpc, &BotRpcRequest::UpdateBot(Box::new(params))) },
                )
                .await;
            this.update(cx, |this, cx| {
                this.settings.saving = false;
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.settings.schedules.clone_from(&updated.schedules);
                        this.upsert_bot_config(*updated);
                        if clear_inputs {
                            let handle = this.window_handle;
                            clear_input_on_window(&mut this.settings.sched_name_input, handle, cx);
                            clear_input_on_window(&mut this.settings.sched_cron_input, handle, cx);
                            clear_input_on_window(
                                &mut this.settings.sched_prompt_input,
                                handle,
                                cx,
                            );
                        }
                        this.notice = Some(SharedString::from(text::SAVED.get()));
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

    pub(super) fn render_settings_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_settings_inputs(bot, window, cx);
        let bot_id_save = bot.id.clone();
        let bot_id_avatar = bot.id.clone();
        let bot_id_clear_avatar = bot.id.clone();
        let bot_id_sched_add = bot.id.clone();

        let mut runtime_pills = div().flex().flex_wrap().items_center().gap_1p5();
        for rt in BOT_RUNTIMES {
            let is_active = self.settings.agent_id == rt.id;
            runtime_pills = runtime_pills.child(
                ui::segment(
                    p,
                    format!("bots-settings-rt-{}", rt.id),
                    rt.label(),
                    is_active,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    rt.id.clone_into(&mut this.settings.agent_id);
                    cx.notify();
                })),
            );
        }

        let effort_pills = ui::segmented_control(
            p,
            BOT_EFFORT_OPTIONS.into_iter().map(|eff| {
                let id = format!(
                    "bots-settings-effort-{}",
                    if eff.is_empty() { "default" } else { eff }
                );
                ui::segment(
                    p,
                    SharedString::from(id),
                    effort_label(eff),
                    self.settings.effort == eff,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    eff.clone_into(&mut this.settings.effort);
                    cx.notify();
                }))
            }),
        );

        let perm_pills = ui::segmented_control(
            p,
            [
                (Permission::Manual, "manual", text::PERMISSION_MANUAL.get()),
                (Permission::Yolo, "yolo", text::PERMISSION_YOLO.get()),
            ]
            .into_iter()
            .map(|(perm, suffix, label)| {
                ui::segment(
                    p,
                    format!("bots-settings-perm-{suffix}"),
                    label,
                    self.settings.permission_mode == perm,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.settings.permission_mode = perm;
                    cx.notify();
                }))
            }),
        );

        let mut color_swatches = div().flex().flex_wrap().items_center().gap_1p5().child(
            ui::segment(
                p,
                "bots-settings-color-none",
                text::SETTINGS_COLOR_NONE.get(),
                self.settings.avatar_color.is_none(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings.avatar_color = None;
                cx.notify();
            })),
        );
        for (idx, (color, _)) in BOT_AVATAR_COLOR_PALETTE.iter().enumerate() {
            let color = *color;
            let active = self.settings.avatar_color == Some(color);
            color_swatches = color_swatches.child(
                div()
                    .id(("bots-settings-color", idx))
                    .size(px(20.))
                    .rounded_full()
                    .bg(ui::tab_tag_color(color))
                    .border_2()
                    .border_color(if active {
                        p.foreground
                    } else {
                        ui::transparent()
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings.avatar_color = Some(color);
                        cx.notify();
                    })),
            );
        }

        let avatar_preview = self.settings.avatar_cache.get(&bot.id).cloned();
        let identity_card = ui::card(p)
            .debug_selector(|| "bots-settings-identity-card".into())
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(text::SETTINGS_IDENTITY_HEADING.get()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when_some(avatar_preview, |d, img| {
                        d.child(gpui::img(img).size(px(40.)).rounded_full().flex_none())
                    })
                    .child(ui::field_label(text::SETTINGS_AVATAR_LABEL.get(), None, p))
                    .child(
                        ui::secondary(
                            "bots-settings-upload-avatar",
                            text::SETTINGS_UPLOAD_AVATAR_BTN.get(),
                        )
                        .debug_selector(|| "bots-settings-upload-avatar".into())
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.pick_and_upload_avatar(bot_id_avatar.clone(), window, cx);
                            },
                        )),
                    )
                    .when(bot.avatar.is_some(), |d| {
                        d.child(
                            ui::ghost(
                                "bots-settings-clear-avatar",
                                text::SETTINGS_CLEAR_AVATAR_BTN.get(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.clear_bot_avatar(bot_id_clear_avatar.clone(), cx);
                                },
                            )),
                        )
                    }),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(200.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::FIELD_NAME.get(), None, p))
                            .children(
                                self.settings
                                    .name_input
                                    .as_ref()
                                    .map(|i| Input::new(i).small()),
                            ),
                    )
                    .child(
                        div()
                            .w(px(110.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::FIELD_EMOJI.get(), None, p))
                            .children(
                                self.settings
                                    .emoji_input
                                    .as_ref()
                                    .map(|i| Input::new(i).small()),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(ui::field_label(text::FIELD_DESCRIPTION.get(), None, p))
                    .children(
                        self.settings
                            .desc_input
                            .as_ref()
                            .map(|i| Input::new(i).small()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(ui::field_label(
                        text::SETTINGS_AVATAR_COLOR_LABEL.get(),
                        None,
                        p,
                    ))
                    .child(color_swatches),
            );

        let runtime_card = ui::card(p)
            .debug_selector(|| "bots-settings-runtime-card".into())
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(text::SETTINGS_RUNTIME_HEADING.get()),
            )
            .child(runtime_pills)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::FIELD_EFFORT.get(), None, p))
                            .child(effort_pills),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::FIELD_PERMISSION.get(), None, p))
                            .child(perm_pills),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(200.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::FIELD_MODEL.get(), None, p))
                            .children(
                                self.settings
                                    .model_input
                                    .as_ref()
                                    .map(|i| Input::new(i).small()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(240.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::FIELD_WORKSPACE.get(), None, p))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .children(self.settings.workspace_input.as_ref().map(|i| {
                                        div().flex_1().min_w_0().child(Input::new(i).small())
                                    }))
                                    .child(
                                        ui::secondary(
                                            "bots-settings-pick-workspace",
                                            text::CHOOSE_FOLDER.get(),
                                        )
                                        .on_click(
                                            cx.listener(|_, _, window, cx| {
                                                Self::pick_settings_workspace(window, cx);
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(text::SETTINGS_NOTIFICATIONS_HEADING.get()),
                    )
                    .child({
                        let ent = cx.entity();
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(ui::label(p, text::NOTIFY_NEEDS_PERMISSION.get(), None))
                            .child(
                                ui::switch(
                                    p,
                                    "bots-notify-permission",
                                    self.settings.notifications.needs_permission,
                                    false,
                                    move |checked, _, cx| {
                                        ent.update(cx, |this, cx| {
                                            this.settings.notifications.needs_permission = checked;
                                            cx.notify();
                                        });
                                    },
                                )
                                .aria_label(text::NOTIFY_NEEDS_PERMISSION.get()),
                            )
                    })
                    .child({
                        let ent = cx.entity();
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(ui::label(p, text::NOTIFY_TASK_COMPLETE.get(), None))
                            .child(
                                ui::switch(
                                    p,
                                    "bots-notify-complete",
                                    self.settings.notifications.task_complete,
                                    false,
                                    move |checked, _, cx| {
                                        ent.update(cx, |this, cx| {
                                            this.settings.notifications.task_complete = checked;
                                            cx.notify();
                                        });
                                    },
                                )
                                .aria_label(text::NOTIFY_TASK_COMPLETE.get()),
                            )
                    })
                    .child({
                        let ent = cx.entity();
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(ui::label(p, text::NOTIFY_COULD_NOT_FINISH.get(), None))
                            .child(
                                ui::switch(
                                    p,
                                    "bots-notify-fail",
                                    self.settings.notifications.could_not_finish,
                                    false,
                                    move |checked, _, cx| {
                                        ent.update(cx, |this, cx| {
                                            this.settings.notifications.could_not_finish = checked;
                                            cx.notify();
                                        });
                                    },
                                )
                                .aria_label(text::NOTIFY_COULD_NOT_FINISH.get()),
                            )
                    }),
            )
            .child(
                div().flex().justify_end().child(
                    ui::button_with_reserved_label(
                        "bots-settings-save",
                        text::SETTINGS_SAVE_BTN.get(),
                        text::SAVED.get(),
                        false,
                    )
                    .debug_selector(|| "bots-settings-save".into())
                    .disabled(self.settings.saving)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.save_bot_settings(bot_id_save.clone(), cx);
                    })),
                ),
            );

        let sched_heading = text::SETTINGS_SCHEDULES_HEADING
            .format(&[("n", &self.settings.schedules.len().to_string())]);
        let mut sched_list = div().flex().flex_col().gap_2();
        if self.settings.schedules.is_empty() {
            sched_list = sched_list.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::SCHEDULE_EMPTY.get()),
            );
        } else {
            for (idx, sched) in self.settings.schedules.iter().enumerate() {
                let sid_toggle = sched.id.clone();
                let sid_del = sched.id.clone();
                let bid_toggle = bot.id.clone();
                let bid_del = bot.id.clone();
                let aria_lbl = format!("{} ({})", sched.name, text::ENABLE_LABEL.get());
                sched_list = sched_list.child(
                    div()
                        .w_full()
                        .px_3()
                        .py_2()
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(p.border)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_size(ui::TEXT_XS)
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(p.foreground)
                                                .child(sched.name.clone()),
                                        )
                                        .child(ui::badge(
                                            p,
                                            ui::BadgeTone::Muted,
                                            sched.cron.clone(),
                                        )),
                                )
                                .child(
                                    div()
                                        .text_size(ui::TEXT_2XS)
                                        .text_color(p.muted_foreground)
                                        .child(sched.prompt.clone()),
                                ),
                        )
                        .child(
                            ui::controls()
                                .child({
                                    let ent = cx.entity();
                                    ui::switch(
                                        p,
                                        SharedString::from(format!("bots-sched-toggle-{idx}")),
                                        sched.enabled,
                                        self.settings.saving,
                                        move |checked, _, cx| {
                                            let bid = bid_toggle.clone();
                                            let sid = sid_toggle.clone();
                                            ent.update(cx, |this, cx| {
                                                this.toggle_schedule_enabled(
                                                    bid, &sid, checked, cx,
                                                );
                                            });
                                        },
                                    )
                                    .aria_label(aria_lbl)
                                })
                                .child(
                                    ui::ghost(
                                        format!("bots-sched-del-{idx}"),
                                        text::CUSTOM_MCP_REMOVE_BTN.get(),
                                    )
                                    .disabled(self.settings.saving)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.remove_schedule(bid_del.clone(), &sid_del, cx);
                                        },
                                    )),
                                ),
                        ),
                );
            }
        }

        let schedules_card = ui::card(p)
            .debug_selector(|| "bots-settings-schedules-card".into())
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(sched_heading),
            )
            .child(sched_list)
            .child(
                div()
                    .pt_2()
                    .border_t_1()
                    .border_color(p.border)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children(self.settings.sched_name_input.as_ref().map(|i| {
                                div().flex_1().min_w(px(160.)).child(Input::new(i).small())
                            }))
                            .children(
                                self.settings
                                    .sched_cron_input
                                    .as_ref()
                                    .map(|i| div().w(px(160.)).child(Input::new(i).small())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children(
                                self.settings
                                    .sched_prompt_input
                                    .as_ref()
                                    .map(|i| div().flex_1().min_w_0().child(Input::new(i).small())),
                            )
                            .child(
                                ui::secondary("bots-sched-add", text::SCHEDULE_ADD_BTN.get())
                                    .debug_selector(|| "bots-sched-add".into())
                                    .disabled(self.settings.saving)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.add_schedule(bot_id_sched_add.clone(), cx);
                                    })),
                            ),
                    ),
            );

        div()
            .id("bots-tab-settings-body")
            .debug_selector(|| "bots-tab-settings-body".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_4()
            .flex()
            .flex_col()
            .gap_4()
            .child(identity_card)
            .child(runtime_card)
            .child(schedules_card)
    }
}
