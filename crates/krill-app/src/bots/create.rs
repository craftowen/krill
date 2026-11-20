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

use super::rpc::BOT_RUNTIMES;
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
    pub(super) submitting: bool,
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
        runtime_id.clone_into(&mut self.create.runtime_id);
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
        let model = self
            .create
            .model_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty());
        let workspace = self
            .create
            .workspace_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .filter(|s| !s.is_empty());

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
                        this.create.selected_template_id = None;
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

    #[expect(
        clippy::too_many_lines,
        reason = "Template gallery + custom bot creation form"
    )]
    pub(super) fn render_create_view(
        &mut self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_create_inputs(window, cx);
        let filtered = filter_templates(self.create.category, &self.create.search_query);
        let selected_tpl = self.create.selected_template_id.and_then(find_bot_template);
        let active_runtime = self.create.effective_runtime().to_owned();

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
                            .child(text::CREATE_HEADING.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::CREATE_DESC.get()),
                    ),
            )
            .child(
                ui::controls()
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

        // Category pills (All + 6 categories).
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

        // Template cards grid (2 columns wrapping cleanly).
        let mut cards = div()
            .id("bots-template-grid")
            .debug_selector(|| "bots-template-grid".into())
            .w_full()
            .flex()
            .flex_wrap()
            .gap_2p5();
        for tpl in &filtered {
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

        // Runtime selector pills (Krill's 8 enabled runtimes).
        let mut runtime_pills = div()
            .id("bots-runtime-picker")
            .debug_selector(|| "bots-runtime-picker".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5();
        for rt in BOT_RUNTIMES {
            let is_active = active_runtime == rt.id;
            let rt_id = format!("bots-runtime-{}", rt.id);
            let rt_sel = rt_id.clone();
            runtime_pills = runtime_pills.child(
                div()
                    .id(SharedString::from(rt_id))
                    .debug_selector(move || rt_sel.clone())
                    .px_2p5()
                    .py_1()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(if is_active { p.ring } else { p.border })
                    .bg(if is_active { p.accent } else { p.card })
                    .cursor_pointer()
                    .hover(|s| s.bg(p.accent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_create_runtime(rt.id, cx);
                    }))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(ui::icons::agent_brand_icon(
                        rt.brand_id,
                        ui::ICON_XS,
                        p.foreground,
                    ))
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .font_weight(if is_active {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(p.foreground)
                            .child(rt.label()),
                    ),
            );
        }

        // Configuration form card.
        let form_card = ui::card(p)
            .flex()
            .flex_col()
            .gap_3()
            .when_some(selected_tpl, |d, tpl| {
                let banner_text = text::SELECTED_TEMPLATE.format(&[("name", tpl.name)]);
                d.child(
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
                                        .child(banner_text),
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
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.clear_selected_template(cx)),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(220.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::FIELD_NAME.get(), None, p))
                            .children(
                                self.create
                                    .name_input
                                    .as_ref()
                                    .map(|i| Input::new(i).small()),
                            ),
                    )
                    .child(
                        div()
                            .w(px(110.))
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
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
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
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(ui::field_label(text::FIELD_RUNTIME.get(), None, p))
                    .child(runtime_pills),
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
                            .child(ui::field_label(
                                text::FIELD_MODEL.get(),
                                Some(text::OPTIONAL_TAG.get()),
                                p,
                            ))
                            .children(
                                self.create
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
                                    .children(self.create.workspace_input.as_ref().map(|i| {
                                        div().flex_1().min_w_0().child(Input::new(i).small())
                                    }))
                                    .child(
                                        ui::secondary(
                                            "bots-create-pick-folder",
                                            text::CHOOSE_FOLDER.get(),
                                        )
                                        .debug_selector(|| "bots-create-pick-folder".into())
                                        .on_click(
                                            cx.listener(|_, _, window, cx| {
                                                Self::pick_create_workspace_folder(window, cx);
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
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

        let templates_heading =
            text::TEMPLATES_HEADING.format(&[("n", &filtered.len().to_string())]);

        div()
            .id("bots-create-view")
            .debug_selector(|| "bots-create-view".into())
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
            .child(
                div()
                    .id("bots-create-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .py_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(form_card)
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
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.foreground)
                                    .child(templates_heading),
                            )
                            .children(self.create.search_input.as_ref().map(|i| {
                                div().w(px(220.)).max_w_full().child(Input::new(i).small())
                            })),
                    )
                    .child(category_pills)
                    .child(cards),
            )
    }
}
