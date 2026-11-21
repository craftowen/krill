// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Create Bot & 24-Template Gallery screen for Krill Bots modal.

use gpui::{
    App, Context, Entity, FontWeight, SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, IconName, Sizable as _};
use krill_proto::bots::{
    BUILTIN_BOT_TEMPLATES, BotRpcRequest, BotRpcResponse, BotTemplate, BotTemplateCategory,
    CreateBotParams, find_bot_template,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::rpc::{BOT_RUNTIMES, runtime_display_label, runtime_models};
use super::{Close, DetailTab, Screen, View, opt_input_composing};

#[derive(Default)]
pub(super) struct CreateState {
    pub(super) category: Option<BotTemplateCategory>,
    pub(super) selected_template_id: Option<&'static str>,
    pub(super) runtime_id: String,
    pub(super) search_query: String,
    pub(super) search_input: Option<Entity<InputState>>,
    pub(super) search_sub: Option<Subscription>,
    pub(super) name_input: Option<Entity<InputState>>,
    pub(super) description_input: Option<Entity<InputState>>,
    pub(super) emoji_input: Option<Entity<InputState>>,
    pub(super) model_input: Option<Entity<InputState>>,
    pub(super) workspace_input: Option<Entity<InputState>>,
    pub(super) model: ModelChoice,
    pub(super) show_templates: bool,
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
            || opt_input_composing(self.description_input.as_ref(), window, app)
            || opt_input_composing(self.emoji_input.as_ref(), window, app)
            || opt_input_composing(self.model_input.as_ref(), window, app)
            || opt_input_composing(self.workspace_input.as_ref(), window, app)
    }

    pub(super) fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<View>) {
        let updates = [
            (&self.search_input, text::SEARCH_PLACEHOLDER.get()),
            (&self.name_input, text::FIELD_NAME_PLACEHOLDER.get()),
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
            self.create.name_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_NAME_PLACEHOLDER.get())
            }));
        }
        if self.create.description_input.is_none() {
            self.create.description_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_DESCRIPTION_PLACEHOLDER.get())
            }));
        }
        if self.create.emoji_input.is_none() {
            self.create.emoji_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_EMOJI_PLACEHOLDER.get())
            }));
        }
        if self.create.model_input.is_none() {
            self.create.model_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::FIELD_MODEL_PLACEHOLDER.get())
            }));
        }
        if self.create.workspace_input.is_none() {
            let default_ws = self
                .default_cwd
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.create.workspace_input = Some(cx.new(|cx| {
                let mut state = InputState::new(window, cx)
                    .placeholder(text::FIELD_WORKSPACE_PLACEHOLDER.get());
                if !default_ws.is_empty() {
                    state.set_value(default_ws, window, cx);
                }
                state
            }));
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
        // The Bots modal always targets this Mac, so the folder can be checked here; krilld
        // would otherwise fall back to the bot's own folder without saying so.
        if workspace
            .as_deref()
            .is_some_and(|ws| !std::path::Path::new(ws).is_dir())
        {
            self.error = Some(SharedString::from(text::ERR_WORKSPACE_MISSING.get()));
            cx.notify();
            return;
        }

        let params = CreateBotParams {
            name,
            agent_id: self.create.effective_runtime().to_owned(),
            description,
            emoji,
            avatar: None,
            model,
            effort: None,
            permission_mode: None,
            workspace,
            template_id: template.map(|t| t.id.to_owned()),
            initial_soul: template.map(|t| t.default_soul.to_owned()),
        };

        let rpc = self.rpc.clone();
        self.create.submitting = true;
        self.error = None;
        self.notice = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { rpc(&BotRpcRequest::CreateBot(Box::new(params))) })
                .await;
            this.update(cx, |this, cx| {
                this.create.submitting = false;
                match res {
                    Ok(BotRpcResponse::Bot(created)) => {
                        let new_id = created.id.clone();
                        this.upsert_bot_config(*created);
                        // The next New bot starts empty; only the agent choice carries over.
                        let runtime_id = std::mem::take(&mut this.create.runtime_id);
                        this.create = CreateState {
                            runtime_id,
                            ..CreateState::default()
                        };
                        this.emit_badge(cx);
                        this.selected_bot_id = Some(new_id.clone());
                        this.screen = Screen::Detail;
                        this.detail_tab = DetailTab::Items;
                        this.load_selected_bot_detail(new_id, cx);
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected CreateBot response: {other:?}"
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

        let mut category_pills = div()
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
            category_pills = category_pills.child(
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

        let mut cards = div()
            .id("bots-template-grid")
            .debug_selector(|| "bots-template-grid".into())
            .w_full()
            .flex()
            .flex_col()
            .gap_2();
        for tpl in &filtered {
            let tpl = *tpl;
            let is_picked = self.create.selected_template_id == Some(tpl.id);
            let card_id = format!("bots-template-{}", tpl.id);
            let card_sel = card_id.clone();
            cards = cards.child(
                div()
                    .id(SharedString::from(card_id))
                    .debug_selector(move || card_sel.clone())
                    .w_full()
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
                    .gap_1()
                    .child(
                        div()
                            .w_full()
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
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.foreground)
                                    .child(tpl.name),
                            )
                            .when(is_picked, |d| {
                                d.child(ui::badge(
                                    p,
                                    ui::BadgeTone::Success,
                                    text::USING_TEMPLATE.get(),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(tpl.description),
                    )
                    .when(!tpl.connectors.is_empty(), |d| {
                        let mut chips = div().flex().flex_wrap().items_center().gap_1();
                        for src in tpl.connectors {
                            chips = chips.child(ui::badge(
                                p,
                                ui::BadgeTone::Neutral,
                                Self::source_label(*src),
                            ));
                        }
                        d.child(chips)
                    }),
            );
        }

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
            .child(category_pills)
            .child(cards)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One centered New bot form: identity, place, agent, footer"
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
            .justify_between()
            .gap_3()
            .child(div().when(!self.bots.is_empty(), |d| {
                d.child(
                    ui::ghost("bots-create-cancel", text::BACK.get())
                        .debug_selector(|| "bots-create-cancel".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.screen = Screen::Detail;
                            cx.notify();
                        })),
                )
            }))
            .child(ui::button_with_tooltip(
                Button::new("bots-close")
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .debug_selector(|| "bots-close".into())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Close))),
                "bots-close",
                text::CLOSE.get(),
            ));

        let template_banner = selected_tpl.map(|tpl| {
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
                        .gap_0p5()
                        .child(
                            div()
                                .text_size(ui::TEXT_XS)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.foreground)
                                .child(text::SELECTED_TEMPLATE.format(&[("name", tpl.name)])),
                        )
                        .child(
                            div()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(tpl.first_chat),
                        ),
                )
                .child(
                    ui::ghost("bots-clear-template", text::CLEAR_TEMPLATE.get())
                        .debug_selector(|| "bots-clear-template".into())
                        .on_click(cx.listener(|this, _, _, cx| this.clear_selected_template(cx))),
                )
        });

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
                        ui::primary(
                            "bots-create-submit",
                            if self.create.submitting {
                                text::CREATING.get()
                            } else {
                                text::CREATE_BOT_BTN.get()
                            },
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
            .children(template_banner)
            .child(identity)
            .child(workspace)
            .child(self.render_create_agent_row(p, cx))
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
}
