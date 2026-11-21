// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Create Bot & 24-Template Gallery screen (`CreateBotV2`) for Krill Bots modal.

use gpui::{
    App, Context, Entity, Focusable as _, FontWeight, SharedString, Subscription, Window, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_proto::agent::Permission;
use krill_proto::bots::{
    BUILTIN_BOT_TEMPLATES, BotEventRuleConfig, BotRpcRequest, BotRpcResponse, BotSlug, BotTemplate,
    BotTemplateCategory, CreateBotParams, CreateBotV2Params, find_bot_template, validate_bot_slug,
};

use crate::i18n::catalog::bots as text;
use crate::layout::TabColor;
use crate::ui;

use super::rpc::{
    BOT_AVATAR_COLOR_PALETTE, BOT_EFFORT_OPTIONS, BOT_RUNTIMES, avatar_color_hex, call_checked,
    effort_label, runtime_display_label, runtime_models,
};
use super::{Close, DetailTab, Screen, View, clear_input_on_window, opt_input_composing};

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent form flags: fold states, focus request, submit in flight"
)]
pub(super) struct CreateState {
    pub(super) category: Option<BotTemplateCategory>,
    pub(super) selected_template_id: Option<&'static str>,
    pub(super) runtime_id: String,
    pub(super) effort: String,
    pub(super) permission_mode: Option<Permission>,
    pub(super) avatar_color: Option<TabColor>,
    pub(super) search_query: String,
    pub(super) search_input: Option<Entity<InputState>>,
    pub(super) search_sub: Option<Subscription>,
    pub(super) name_input: Option<Entity<InputState>>,
    pub(super) slug_input: Option<Entity<InputState>>,
    pub(super) description_input: Option<Entity<InputState>>,
    pub(super) emoji_input: Option<Entity<InputState>>,
    pub(super) model_input: Option<Entity<InputState>>,
    pub(super) workspace_input: Option<Entity<InputState>>,
    pub(super) submit_subs: Vec<Subscription>,
    pub(super) focus_name_pending: bool,
    pub(super) model: ModelChoice,
    pub(super) show_templates: bool,
    pub(super) show_advanced: bool,
    /// Opened from the sidebar "+": Escape closes the modal instead of going back to a bot.
    pub(super) opened_alone: bool,
    pub(super) submitting: bool,
}

/// Model picked in the New bot form. Presets come from [`runtime_models`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ModelChoice {
    /// The runtime's own default (`model: None`).
    #[default]
    Default,
    Preset(&'static str),
    /// Free text from `model_input`, for models the preset list doesn't know.
    Custom,
}

impl CreateState {
    pub(super) fn effective_runtime(&self) -> &str {
        if self.runtime_id.is_empty() {
            "claude"
        } else {
            &self.runtime_id
        }
    }

    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        opt_input_composing(self.search_input.as_ref(), window, app)
            || opt_input_composing(self.name_input.as_ref(), window, app)
            || opt_input_composing(self.slug_input.as_ref(), window, app)
            || opt_input_composing(self.description_input.as_ref(), window, app)
            || opt_input_composing(self.emoji_input.as_ref(), window, app)
            || opt_input_composing(self.model_input.as_ref(), window, app)
            || opt_input_composing(self.workspace_input.as_ref(), window, app)
    }

    pub(super) fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<View>) {
        let updates = [
            (&self.search_input, text::SEARCH_PLACEHOLDER.get()),
            (&self.name_input, text::FIELD_NAME_PLACEHOLDER.get()),
            (&self.slug_input, text::FIELD_SLUG_PLACEHOLDER.get()),
            (
                &self.description_input,
                text::FIELD_DESCRIPTION_PLACEHOLDER.get(),
            ),
            (&self.emoji_input, text::FIELD_EMOJI_PLACEHOLDER.get()),
            (&self.model_input, text::FIELD_MODEL_PLACEHOLDER.get()),
            (
                &self.workspace_input,
                text::FIELD_WORKSPACE_PLACEHOLDER.get(),
            ),
        ];
        for (slot, placeholder) in updates {
            if let Some(input) = slot {
                input.update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            }
        }
    }

    pub(super) fn clear_form_fields(
        &mut self,
        window_handle: Option<gpui::AnyWindowHandle>,
        default_cwd: Option<&std::path::Path>,
        cx: &mut App,
    ) {
        self.clear_form_fields_with(None, window_handle, default_cwd, cx);
    }

    pub(super) fn clear_form_fields_with(
        &mut self,
        mut window: Option<&mut Window>,
        window_handle: Option<gpui::AnyWindowHandle>,
        default_cwd: Option<&std::path::Path>,
        cx: &mut App,
    ) {
        self.selected_template_id = None;
        self.model = ModelChoice::Default;
        self.show_templates = false;
        self.show_advanced = false;
        self.effort.clear();
        self.permission_mode = None;
        self.avatar_color = None;
        self.focus_name_pending = true;
        for slot in [
            &mut self.name_input,
            &mut self.slug_input,
            &mut self.description_input,
            &mut self.emoji_input,
            &mut self.model_input,
        ] {
            if let Some(win) = window.as_deref_mut() {
                if let Some(input) = slot {
                    input.update(cx, |s, cx| s.set_value(String::new(), win, cx));
                }
            } else {
                clear_input_on_window(slot, window_handle, cx);
            }
        }
        let default_ws = default_cwd
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(ws_input) = &self.workspace_input {
            if let Some(win) = window {
                ws_input.update(cx, |s, cx| s.set_value(default_ws, win, cx));
            } else if let Some(handle) = window_handle {
                let ws_copy = default_ws;
                let ws_entity = ws_input.clone();
                drop(handle.update(cx, |_, window, cx| {
                    ws_entity.update(cx, |s, cx| s.set_value(ws_copy, window, cx));
                }));
            }
        } else {
            self.workspace_input = None;
        }
    }
}

/// Returns filtered templates from `BUILTIN_BOT_TEMPLATES` matching optional category and query.
#[must_use]
pub fn filter_templates(
    category: Option<BotTemplateCategory>,
    query: &str,
) -> Vec<&'static BotTemplate> {
    let q = query.trim().to_lowercase();
    BUILTIN_BOT_TEMPLATES
        .iter()
        .filter(|tpl| category.is_none_or(|c| tpl.category == c))
        .filter(|tpl| {
            if q.is_empty() {
                return true;
            }
            let hay = format!("{} {} {}", tpl.name, tpl.description, tpl.id).to_lowercase();
            q.split_whitespace().all(|term| hay.contains(term))
        })
        .collect()
}

/// Localized label for a [`BotTemplateCategory`].
#[must_use]
pub fn category_label(cat: BotTemplateCategory) -> &'static str {
    match cat {
        BotTemplateCategory::Coding => text::CAT_CODING.get(),
        BotTemplateCategory::Releases => text::CAT_RELEASES.get(),
        BotTemplateCategory::Support => text::CAT_SUPPORT.get(),
        BotTemplateCategory::Planning => text::CAT_PLANNING.get(),
        BotTemplateCategory::Ops => text::CAT_OPS.get(),
        BotTemplateCategory::Personal => text::CAT_PERSONAL.get(),
    }
}

impl View {
    fn subscribe_create_enter(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sub = cx.subscribe_in(input, window, |this, inp, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. })
                && !crate::jump::entity_input_is_composing(inp, window, cx)
            {
                this.submit_create_bot(cx);
            }
        });
        self.create.submit_subs.push(sub);
    }

    fn ensure_create_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        if self.create.search_input.is_none() {
            let input = cx
                .new(|cx| InputState::new(window, cx).placeholder(text::SEARCH_PLACEHOLDER.get()));
            self.create.search_sub =
                Some(cx.subscribe(&input, |this, input, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.create.search_query = input.read(cx).value().to_string();
                        cx.notify();
                    }
                }));
            self.create.search_input = Some(input);
        }
        if self.create.name_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_NAME_PLACEHOLDER.get())
            });
            self.subscribe_create_enter(&input, window, cx);
            self.create.name_input = Some(input);
        }
        if self.create.slug_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_SLUG_PLACEHOLDER.get())
            });
            self.subscribe_create_enter(&input, window, cx);
            self.create.slug_input = Some(input);
        }
        if self.create.description_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_DESCRIPTION_PLACEHOLDER.get())
            });
            self.subscribe_create_enter(&input, window, cx);
            self.create.description_input = Some(input);
        }
        if self.create.emoji_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_EMOJI_PLACEHOLDER.get())
            });
            self.subscribe_create_enter(&input, window, cx);
            self.create.emoji_input = Some(input);
        }
        if self.create.model_input.is_none() {
            let input = cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_MODEL_PLACEHOLDER.get())
            });
            self.subscribe_create_enter(&input, window, cx);
            self.create.model_input = Some(input);
        }
        if self.create.workspace_input.is_none() {
            let default_ws = self
                .default_cwd
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let input = cx.new(|cx| {
                let mut state = InputState::new(window, cx)
                    .placeholder(text::FIELD_WORKSPACE_PLACEHOLDER.get());
                if !default_ws.is_empty() {
                    state.set_value(default_ws, window, cx);
                }
                state
            });
            self.subscribe_create_enter(&input, window, cx);
            self.create.workspace_input = Some(input);
        }
        if self.create.focus_name_pending
            && let Some(name_input) = &self.create.name_input
        {
            self.create.focus_name_pending = false;
            let focus = name_input.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
    }

    pub fn select_template(
        &mut self,
        template: &'static BotTemplate,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.create.selected_template_id = Some(template.id);
        if let Some(window) = window {
            if let Some(input) = &self.create.name_input {
                input.update(cx, |s, cx| {
                    s.set_value(template.name.to_owned(), window, cx);
                });
            }
            if let Some(input) = &self.create.slug_input {
                input.update(cx, |s, cx| {
                    s.set_value(template.id.to_owned(), window, cx);
                });
            }
            if let Some(input) = &self.create.description_input {
                input.update(cx, |s, cx| {
                    s.set_value(template.description.to_owned(), window, cx);
                });
            }
        }
        cx.notify();
    }

    pub fn clear_selected_template(&mut self, cx: &mut Context<Self>) {
        self.create.selected_template_id = None;
        self.create.show_templates = false;
        cx.notify();
    }

    pub fn start_blank_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let default_cwd = self.default_cwd.clone();
        self.create
            .clear_form_fields_with(Some(window), None, default_cwd.as_deref(), cx);
        if let Some(name_input) = &self.create.name_input {
            let focus = name_input.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    pub fn set_template_category(
        &mut self,
        category: Option<BotTemplateCategory>,
        cx: &mut Context<Self>,
    ) {
        self.create.category = category;
        cx.notify();
    }

    pub fn set_create_runtime(&mut self, runtime_id: &'static str, cx: &mut Context<Self>) {
        if self.create.effective_runtime() != runtime_id {
            // A preset id belongs to one runtime's CLI, so a new runtime starts from its default.
            self.create.model = ModelChoice::Default;
        }
        runtime_id.clone_into(&mut self.create.runtime_id);
        cx.notify();
    }

    pub(super) fn set_create_model(&mut self, model: ModelChoice, cx: &mut Context<Self>) {
        self.create.model = model;
        cx.notify();
    }

    pub fn toggle_create_templates(&mut self, cx: &mut Context<Self>) {
        self.create.show_templates = !self.create.show_templates;
        cx.notify();
    }

    fn pick_create_workspace_folder(window: &mut Window, cx: &mut Context<Self>) {
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
                            if let Some(input) = &this.create.workspace_input {
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

    pub fn submit_create_bot(&mut self, cx: &mut Context<Self>) {
        if self.create.submitting {
            return;
        }
        let template = self.create.selected_template_id.and_then(find_bot_template);
        let raw_name = self
            .create
            .name_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        let name = if !raw_name.is_empty() {
            raw_name
        } else if let Some(tpl) = template {
            tpl.name.to_owned()
        } else {
            self.error = Some(SharedString::from(text::ERR_EMPTY_NAME.get()));
            cx.notify();
            return;
        };

        let raw_slug = self
            .create
            .slug_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        let slug = if raw_slug.is_empty() {
            None
        } else {
            if validate_bot_slug(&raw_slug).is_err() {
                self.error = Some(SharedString::from(text::ERR_INVALID_SLUG.get()));
                cx.notify();
                return;
            }
            Some(BotSlug::from_name(&raw_slug))
        };

        let description = self
            .create
            .description_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty())
            .or_else(|| template.map(|t| t.description.to_owned()))
            .unwrap_or_default();
        let emoji = self
            .create
            .emoji_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty());
        let model = match self.create.model {
            ModelChoice::Default => None,
            ModelChoice::Preset(id) => Some(id.to_owned()),
            ModelChoice::Custom => self
                .create
                .model_input
                .as_ref()
                .map(|i| i.read(cx).value().trim().to_owned())
                .filter(|s| !s.is_empty()),
        };
        let workspace = self
            .create
            .workspace_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty());
        if self.create.model == ModelChoice::Custom && model.is_none() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_MODEL.get()));
            cx.notify();
            return;
        }
        // A folder on this Mac can be checked here; krilld would otherwise fall back to the
        // bot's own folder without saying so. Remote hosts check their own paths.
        if self.host.is_local()
            && workspace
                .as_deref()
                .is_some_and(|ws| !std::path::Path::new(ws).is_dir())
        {
            self.error = Some(SharedString::from(text::ERR_WORKSPACE_MISSING.get()));
            cx.notify();
            return;
        }
        let effort = (!self.create.effort.trim().is_empty()).then(|| self.create.effort.clone());

        let base = CreateBotParams {
            name,
            agent_id: self.create.effective_runtime().to_owned(),
            description,
            emoji,
            avatar: None,
            model,
            effort,
            permission_mode: self.create.permission_mode,
            workspace,
            template_id: template.map(|t| t.id.to_owned()),
            initial_soul: template.map(|t| t.default_soul.to_owned()),
        };
        let params = CreateBotV2Params {
            base,
            slug,
            avatar_color: self
                .create
                .avatar_color
                .map(avatar_color_hex)
                .map(str::to_owned),
            skills: template
                .map(|t| t.skills.iter().map(|s| (*s).to_owned()).collect())
                .unwrap_or_default(),
            events: template
                .map(|t| {
                    t.events
                        .iter()
                        .map(|ev| BotEventRuleConfig {
                            id: ev.id.to_owned(),
                            source: ev.source.to_owned(),
                            event: ev.event.to_owned(),
                            prompt: ev.prompt.to_owned(),
                            enabled: false,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };

        let rpc = self.rpc.clone();
        self.create.submitting = true;
        self.error = None;
        self.notice = None;
        self.mutation_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let req = BotRpcRequest::CreateBotV2(Box::new(params));
                    call_checked(&rpc, &req)
                })
                .await;
            this.update(cx, |this, cx| {
                this.create.submitting = false;
                match res {
                    Ok(BotRpcResponse::Bot(created)) => {
                        let new_id = created.id.clone();
                        this.upsert_bot_config(*created);
                        let handle = this.window_handle;
                        let default_cwd = this.default_cwd.clone();
                        this.create
                            .clear_form_fields(handle, default_cwd.as_deref(), cx);
                        this.emit_badge(cx);
                        this.selected_bot_id = Some(new_id.clone());
                        this.screen = Screen::Detail;
                        this.detail_tab = DetailTab::Items;
                        this.load_selected_bot_detail(new_id, cx);
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected CreateBotV2 response: {other:?}"
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

    #[expect(
        clippy::too_many_lines,
        reason = "One centered New bot form: identity, place, agent, advanced, footer"
    )]
    pub(super) fn render_create_view(
        &mut self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_create_inputs(window, cx);
        let selected_tpl = self.create.selected_template_id.and_then(find_bot_template);
        let templates_shown = self.create.show_templates || selected_tpl.is_some();

        let header = div()
            .flex_none()
            .px_5()
            .py_3()
            .flex()
            .items_center()
            .justify_end()
            .child(
                ui::controls()
                    .child(
                        ui::secondary("bots-create-start-blank", text::START_BLANK.get())
                            .debug_selector(|| "bots-create-start-blank".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_blank_template(window, cx);
                            })),
                    )
                    .when(!self.bots.is_empty(), |d| {
                        d.child(
                            ui::ghost("bots-create-cancel", text::BACK.get())
                                .debug_selector(|| "bots-create-cancel".into())
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

        let identity = div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_end()
                    .gap_2()
                    .child(
                        div()
                            .w(px(64.))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(
                                text::FIELD_EMOJI.get(),
                                Some(text::OPTIONAL_TAG.get()),
                                p,
                            ))
                            .children(
                                self.create
                                    .emoji_input
                                    .as_ref()
                                    .map(|i| Input::new(i).small()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::FIELD_NAME.get(), None, p))
                            .children(self.create.name_input.as_ref().map(Input::new)),
                    ),
            )
            .child(ui::field_label(
                text::FIELD_DESCRIPTION.get(),
                Some(text::OPTIONAL_TAG.get()),
                p,
            ))
            .children(
                self.create
                    .description_input
                    .as_ref()
                    .map(|i| Input::new(i).small()),
            );

        let workspace = div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(ui::field_label(
                text::FIELD_WORKSPACE.get(),
                Some(text::OPTIONAL_TAG.get()),
                p,
            ))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .children(
                        self.create
                            .workspace_input
                            .as_ref()
                            .map(|i| div().flex_1().min_w_0().child(Input::new(i).small())),
                    )
                    .child(
                        ui::secondary("bots-create-pick-folder", text::CHOOSE_FOLDER.get())
                            .icon(IconName::FolderOpen)
                            .debug_selector(|| "bots-create-pick-folder".into())
                            .on_click(cx.listener(|_, _, window, cx| {
                                Self::pick_create_workspace_folder(window, cx);
                            })),
                    ),
            );

        let footer = div()
            .w_full()
            .pt_3()
            .border_t_1()
            .border_color(p.border)
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                ui::ghost(
                    "bots-create-toggle-templates",
                    if templates_shown {
                        text::HIDE_TEMPLATES.get()
                    } else {
                        text::SHOW_TEMPLATES.get()
                    },
                )
                .icon(IconName::LayoutDashboard)
                .debug_selector(|| "bots-create-toggle-templates".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.create.selected_template_id.is_some() {
                        this.clear_selected_template(cx);
                    } else {
                        this.toggle_create_templates(cx);
                    }
                })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    // Same chord the key handler below accepts (`secondary` = Cmd on macOS).
                    .child(ui::keycap(
                        p,
                        if cfg!(target_os = "macos") {
                            "⌘↵"
                        } else {
                            "Ctrl+Enter"
                        },
                    ))
                    .child(
                        ui::button_with_reserved_label(
                            "bots-create-submit",
                            text::CREATE_BOT_BTN.get(),
                            text::CREATING.get(),
                            self.create.submitting,
                        )
                        .debug_selector(|| "bots-create-submit".into())
                        .disabled(self.create.submitting)
                        .on_click(cx.listener(|this, _, _, cx| this.submit_create_bot(cx))),
                    ),
            );

        let form = div()
            .w_full()
            .max_w(px(520.))
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(text::CREATE_HEADING.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .text_center()
                            .child(text::CREATE_DESC.get()),
                    ),
            )
            .children(
                self.error
                    .as_ref()
                    .map(|err| ui::error_banner(p, err.clone())),
            )
            .children(self.notice.as_ref().map(|msg| {
                div()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.success.opacity(0.3))
                    .bg(p.success.opacity(0.1))
                    .text_size(ui::TEXT_XS)
                    .text_color(p.success)
                    .child(msg.clone())
            }))
            .children(selected_tpl.map(|tpl| Self::render_template_banner(tpl, p, cx)))
            .child(identity)
            .child(workspace)
            .child(self.render_create_agent_row(p, cx))
            .child(self.render_create_advanced(p, cx))
            .child(footer)
            .when(templates_shown, |d| {
                d.child(self.render_create_templates(p, cx))
            });

        div()
            .id("bots-create-view")
            .debug_selector(|| "bots-create-view".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let keys = &event.keystroke;
                if keys.key == "enter"
                    && keys.modifiers.secondary()
                    && !this.input_is_composing(window, cx)
                {
                    this.submit_create_bot(cx);
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .child(
                div()
                    .id("bots-create-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .pb_6()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(form),
            )
    }

    fn render_template_category_pills(
        &self,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut pills = div()
            .id("bots-template-categories")
            .debug_selector(|| "bots-template-categories".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .child(
                ui::segment(
                    p,
                    "bots-cat-all",
                    text::CAT_ALL.get(),
                    self.create.category.is_none(),
                )
                .debug_selector(|| "bots-cat-all".into())
                .on_click(cx.listener(|this, _, _, cx| this.set_template_category(None, cx))),
            );
        for &cat in BotTemplateCategory::ALL {
            let cat_id = format!("bots-cat-{}", cat.as_str());
            let cat_sel = cat_id.clone();
            pills = pills.child(
                ui::segment(
                    p,
                    SharedString::from(cat_id),
                    category_label(cat),
                    self.create.category == Some(cat),
                )
                .debug_selector(move || cat_sel.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_template_category(Some(cat), cx);
                })),
            );
        }
        pills
    }

    fn render_template_cards(
        &self,
        filtered: &[&'static BotTemplate],
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut cards = div()
            .id("bots-template-grid")
            .debug_selector(|| "bots-template-grid".into())
            .w_full()
            .flex()
            .flex_wrap()
            .gap_2p5();
        for tpl in filtered {
            let tpl = *tpl;
            let is_picked = self.create.selected_template_id == Some(tpl.id);
            let card_id = format!("bots-template-{}", tpl.id);
            let card_sel = card_id.clone();
            cards = cards.child(
                div()
                    .id(SharedString::from(card_id))
                    .debug_selector(move || card_sel.clone())
                    .w(px(330.))
                    .max_w_full()
                    .p_3()
                    .rounded(ui::RADIUS_MD)
                    .bg(if is_picked { p.accent } else { p.card })
                    .border_1()
                    .border_color(if is_picked { p.ring } else { p.border })
                    .cursor_pointer()
                    .hover(|s| s.bg(p.accent))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_template(tpl, Some(window), cx);
                    }))
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(ui::badge(
                                        p,
                                        ui::BadgeTone::Muted,
                                        category_label(tpl.category),
                                    ))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(ui::TEXT_SM)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(tpl.name),
                                    ),
                            )
                            .child(ui::badge(
                                p,
                                if is_picked {
                                    ui::BadgeTone::Success
                                } else {
                                    ui::BadgeTone::Neutral
                                },
                                if is_picked {
                                    text::USING_TEMPLATE.get()
                                } else {
                                    text::USE_TEMPLATE.get()
                                },
                            )),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(tpl.description),
                    )
                    .child({
                        let mut meta = div().flex().flex_wrap().items_center().gap_1();
                        for src in tpl.connectors {
                            meta = meta.child(ui::badge(
                                p,
                                ui::BadgeTone::Neutral,
                                Self::source_label(*src),
                            ));
                        }
                        if !tpl.schedules.is_empty() {
                            meta = meta.child(ui::badge(
                                p,
                                ui::BadgeTone::Muted,
                                text::TEMPLATE_SCHEDULES_COUNT
                                    .format(&[("n", &tpl.schedules.len().to_string())]),
                            ));
                        }
                        if !tpl.events.is_empty() {
                            meta = meta.child(ui::badge(
                                p,
                                ui::BadgeTone::Muted,
                                text::TEMPLATE_EVENTS_COUNT
                                    .format(&[("n", &tpl.events.len().to_string())]),
                            ));
                        }
                        if !tpl.skills.is_empty() {
                            meta = meta.child(ui::badge(
                                p,
                                ui::BadgeTone::Muted,
                                text::TEMPLATE_SKILLS_COUNT
                                    .format(&[("n", &tpl.skills.len().to_string())]),
                            ));
                        }
                        if let Some(setup) = tpl.needs_setup {
                            meta = meta.child(ui::badge(
                                p,
                                ui::BadgeTone::Warning,
                                text::TEMPLATE_NEEDS_SETUP.format(&[("text", setup)]),
                            ));
                        }
                        meta
                    }),
            );
        }
        cards
    }

    fn model_choice_label(&self) -> String {
        match self.create.model {
            ModelChoice::Default => text::MODEL_DEFAULT.get().to_owned(),
            ModelChoice::Preset(id) => runtime_models(self.create.effective_runtime())
                .iter()
                .find(|(preset, _)| *preset == id)
                .map_or(id, |(_, label)| *label)
                .to_owned(),
            ModelChoice::Custom => text::MODEL_CUSTOM.get().to_owned(),
        }
    }

    /// Agent and model pickers, side by side like Orca's New bot dialog.
    fn render_create_agent_row(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};

        let runtime = self.create.effective_runtime().to_owned();
        let this = cx.weak_entity();
        let runtime_button = Button::new("bots-runtime-picker")
            .outline()
            .small()
            .w_full()
            .dropdown_caret(true)
            // Icon and name share one child so the caret layout keeps them left-aligned.
            .accessibility_label(runtime_display_label(&runtime))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(ui::icons::agent_brand_icon(
                        &runtime,
                        ui::ICON_SM,
                        p.foreground,
                    ))
                    .child(div().truncate().child(runtime_display_label(&runtime))),
            )
            .debug_selector(|| "bots-runtime-picker".into())
            .dropdown_menu({
                let runtime = runtime.clone();
                move |mut menu, _, _| {
                    for rt in BOT_RUNTIMES {
                        let this = this.clone();
                        menu = menu.item(
                            PopupMenuItem::new(rt.label())
                                .icon(ui::icons::agent_menu_icon(rt.id))
                                .checked(runtime == rt.id)
                                .on_click(move |_, _, cx| {
                                    this.update(cx, |v, cx| v.set_create_runtime(rt.id, cx))
                                        .ok();
                                }),
                        );
                    }
                    menu
                }
            });

        let current = self.create.model;
        let presets = runtime_models(&runtime);
        let this = cx.weak_entity();
        let model_button = Button::new("bots-model-picker")
            .outline()
            .small()
            .w_full()
            .dropdown_caret(true)
            .label(self.model_choice_label())
            .debug_selector(|| "bots-model-picker".into())
            .dropdown_menu(move |mut menu, _, _| {
                let pick = |choice: ModelChoice| {
                    let this = this.clone();
                    move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                        this.update(cx, |v, cx| v.set_create_model(choice, cx)).ok();
                    }
                };
                menu = menu.item(
                    PopupMenuItem::new(text::MODEL_DEFAULT.get())
                        .checked(current == ModelChoice::Default)
                        .on_click(pick(ModelChoice::Default)),
                );
                for (id, label) in presets {
                    menu = menu.item(
                        PopupMenuItem::new(*label)
                            .checked(current == ModelChoice::Preset(id))
                            .on_click(pick(ModelChoice::Preset(id))),
                    );
                }
                menu.separator().item(
                    PopupMenuItem::new(text::MODEL_CUSTOM.get())
                        .checked(current == ModelChoice::Custom)
                        .on_click(pick(ModelChoice::Custom)),
                )
            });

        let field = |label: &'static str, control: gpui::AnyElement| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(ui::field_label(label, None, p))
                .child(control)
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .flex()
                    .gap_3()
                    .child(field(
                        text::FIELD_RUNTIME.get(),
                        runtime_button.into_any_element(),
                    ))
                    .child(field(
                        text::FIELD_MODEL.get(),
                        model_button.into_any_element(),
                    )),
            )
            .when(self.create.model == ModelChoice::Custom, |d| {
                d.children(
                    self.create
                        .model_input
                        .as_ref()
                        .map(|i| Input::new(i).small()),
                )
            })
    }

    /// Template gallery, folded under "Start from a template" until asked for.
    fn render_create_templates(
        &self,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let filtered = filter_templates(self.create.category, &self.create.search_query);
        div()
            .id("bots-create-templates")
            .debug_selector(|| "bots-create-templates".into())
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(
                                text::TEMPLATES_HEADING
                                    .format(&[("n", &filtered.len().to_string())]),
                            ),
                    )
                    .children(
                        self.create
                            .search_input
                            .as_ref()
                            .map(|i| div().w(px(200.)).max_w_full().child(Input::new(i).small())),
                    ),
            )
            .child(self.render_template_category_pills(p, cx))
            .child(self.render_template_cards(&filtered, p, cx))
    }

    /// Address, effort, permission and avatar color: folded so the form stays short.
    fn render_create_advanced(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let open = self.create.show_advanced;
        let toggle = ui::ghost("bots-create-toggle-advanced", text::ADVANCED_OPTIONS.get())
            .icon(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .debug_selector(|| "bots-create-toggle-advanced".into())
            .on_click(cx.listener(|this, _, _, cx| {
                this.create.show_advanced = !this.create.show_advanced;
                cx.notify();
            }));
        if !open {
            return div().w_full().child(toggle);
        }

        let effort_pills = ui::segmented_control(
            p,
            BOT_EFFORT_OPTIONS.into_iter().map(|eff| {
                let id = format!(
                    "bots-create-effort-{}",
                    if eff.is_empty() { "default" } else { eff }
                );
                ui::segment(
                    p,
                    SharedString::from(id),
                    effort_label(eff),
                    self.create.effort == eff,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    eff.clone_into(&mut this.create.effort);
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
                let is_on = self.create.permission_mode.is_some_and(|cur| cur == perm);
                ui::segment(p, format!("bots-create-perm-{suffix}"), label, is_on).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.create.permission_mode = if this.create.permission_mode == Some(perm) {
                            None
                        } else {
                            Some(perm)
                        };
                        cx.notify();
                    }),
                )
            }),
        );

        let mut color_swatches = div().flex().flex_wrap().items_center().gap_1p5().child(
            ui::segment(
                p,
                "bots-create-color-none",
                text::SETTINGS_COLOR_NONE.get(),
                self.create.avatar_color.is_none(),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.create.avatar_color = None;
                cx.notify();
            })),
        );
        for (idx, (color, _)) in BOT_AVATAR_COLOR_PALETTE.iter().enumerate() {
            let color = *color;
            let active = self.create.avatar_color == Some(color);
            color_swatches = color_swatches.child(
                div()
                    .id(("bots-create-color", idx))
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
                        this.create.avatar_color = Some(color);
                        cx.notify();
                    })),
            );
        }

        let labeled = |label: &'static str, control: gpui::AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(ui::field_label(label, Some(text::OPTIONAL_TAG.get()), p))
                .child(control)
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(toggle)
            .child(
                labeled(
                    text::FIELD_SLUG.get(),
                    self.create.slug_input.as_ref().map_or_else(
                        || div().into_any_element(),
                        |i| Input::new(i).small().into_any_element(),
                    ),
                )
                .w_full(),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(labeled(
                        text::FIELD_EFFORT.get(),
                        effort_pills.into_any_element(),
                    ))
                    .child(labeled(
                        text::FIELD_PERMISSION.get(),
                        perm_pills.into_any_element(),
                    )),
            )
            .child(labeled(
                text::SETTINGS_AVATAR_COLOR_LABEL.get(),
                color_swatches.into_any_element(),
            ))
    }

    fn render_template_banner(
        tpl: &'static BotTemplate,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let banner_text = text::SELECTED_TEMPLATE.format(&[("name", tpl.name)]);
        let ex_text = if !tpl.example[0].is_empty() || !tpl.example[1].is_empty() {
            Some(format!("{} — {}", tpl.example[0], tpl.example[1]))
        } else {
            None
        };
        div()
            .w_full()
            .px_3()
            .py_2()
            .rounded(ui::RADIUS_SM)
            .bg(p.accent.opacity(0.6))
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
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(banner_text),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(format!(
                                "{}: {}",
                                text::TEMPLATE_FIRST_CHAT.get(),
                                tpl.first_chat
                            )),
                    )
                    .when_some(ex_text, |dd, ex| {
                        dd.child(
                            div()
                                .text_size(ui::TEXT_2XS)
                                .text_color(p.muted_foreground)
                                .child(format!("{}: {ex}", text::TEMPLATE_EXAMPLE.get())),
                        )
                    }),
            )
            .child(
                ui::ghost("bots-clear-template", text::CLEAR_TEMPLATE.get())
                    .debug_selector(|| "bots-clear-template".into())
                    .on_click(cx.listener(|this, _, _, cx| this.clear_selected_template(cx))),
            )
    }
}
