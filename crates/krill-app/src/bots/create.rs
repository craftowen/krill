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
    effort_label,
};
use super::{Close, DetailTab, Screen, View, clear_input_on_window, opt_input_composing};

#[derive(Default)]
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

    pub(super) fn render_create_view(
        &mut self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_create_inputs(window, cx);
        let filtered = filter_templates(self.create.category, &self.create.search_query);
        let selected_tpl = self.create.selected_template_id.and_then(find_bot_template);

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

        let category_pills = self.render_template_category_pills(p, cx);
        let cards = self.render_template_cards(&filtered, p, cx);
        let form_card = self.render_create_form_card(selected_tpl, p, cx);
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

    fn render_create_form_card(
        &self,
        selected_tpl: Option<&'static BotTemplate>,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let active_runtime = self.create.effective_runtime().to_owned();
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

        ui::card(p)
            .flex()
            .flex_col()
            .gap_3()
            .when_some(selected_tpl, |d, tpl| {
                let banner_text = text::SELECTED_TEMPLATE.format(&[("name", tpl.name)]);
                let ex_text = if !tpl.example[0].is_empty() || !tpl.example[1].is_empty() {
                    Some(format!("{} — {}", tpl.example[0], tpl.example[1]))
                } else {
                    None
                };
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
                                            .child(format!(
                                                "{}: {ex}",
                                                text::TEMPLATE_EXAMPLE.get()
                                            )),
                                    )
                                }),
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
                            .min_w(px(200.))
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
                            .flex_1()
                            .min_w(px(180.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(
                                text::FIELD_SLUG.get(),
                                Some(text::OPTIONAL_TAG.get()),
                                p,
                            ))
                            .children(
                                self.create
                                    .slug_input
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
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(
                                text::FIELD_EFFORT.get(),
                                Some(text::OPTIONAL_TAG.get()),
                                p,
                            ))
                            .child(effort_pills),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(
                                text::FIELD_PERMISSION.get(),
                                Some(text::OPTIONAL_TAG.get()),
                                p,
                            ))
                            .child(perm_pills),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(
                                text::SETTINGS_AVATAR_COLOR_LABEL.get(),
                                Some(text::OPTIONAL_TAG.get()),
                                p,
                            ))
                            .child(color_swatches),
                    ),
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
            )
    }
}
