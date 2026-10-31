// Composer pattern adapted from Orca NativeChatComposerField.tsx (MIT).
// Copyright (c) 2026 Lovecast Inc.
use super::*;
use gpui::prelude::FluentBuilder as _;
use gpui_component::{IconName, Selectable as _};

pub(super) fn agent_display_name(agent: &str) -> String {
    match agent.to_ascii_lowercase().as_str() {
        "claude" => "Claude".to_string(),
        "codex" => "Codex".to_string(),
        "gemini" => "Gemini".to_string(),
        "agy" | "antigravity" => "Antigravity".to_string(),
        "opencode" => "OpenCode".to_string(),
        "cursor" => "Cursor".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                None => "Agent".to_string(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        }
    }
}

pub(super) fn fallback_choices_for_agent(agent: &str) -> (Vec<ComposerModelItem>, Vec<String>) {
    match agent.to_ascii_lowercase().as_str() {
        "codex" => (
            vec![
                ComposerModelItem {
                    id: "gpt-5.4".to_owned(),
                    label: "GPT-5.4".to_owned(),
                    efforts: vec![
                        "low".to_owned(),
                        "medium".to_owned(),
                        "high".to_owned(),
                        "xhigh".to_owned(),
                    ],
                    default_effort: Some("high".to_owned()),
                },
                ComposerModelItem {
                    id: "gpt-5.3-codex".to_owned(),
                    label: "GPT-5.3 Codex".to_owned(),
                    efforts: vec![
                        "low".to_owned(),
                        "medium".to_owned(),
                        "high".to_owned(),
                        "xhigh".to_owned(),
                    ],
                    default_effort: Some("high".to_owned()),
                },
                ComposerModelItem {
                    id: "gpt-5.4-mini".to_owned(),
                    label: "GPT-5.4 Mini".to_owned(),
                    efforts: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
                    default_effort: Some("medium".to_owned()),
                },
            ],
            vec![
                "low".to_owned(),
                "medium".to_owned(),
                "high".to_owned(),
                "xhigh".to_owned(),
            ],
        ),
        "gemini" | "agy" | "antigravity" => (
            vec![
                ComposerModelItem {
                    id: "gemini-2.5-pro".to_owned(),
                    label: "Gemini 2.5 Pro".to_owned(),
                    efforts: vec![
                        "low".to_owned(),
                        "medium".to_owned(),
                        "high".to_owned(),
                        "max".to_owned(),
                    ],
                    default_effort: Some("high".to_owned()),
                },
                ComposerModelItem {
                    id: "gemini-2.5-flash".to_owned(),
                    label: "Gemini 2.5 Flash".to_owned(),
                    efforts: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
                    default_effort: Some("medium".to_owned()),
                },
            ],
            vec![
                "low".to_owned(),
                "medium".to_owned(),
                "high".to_owned(),
                "max".to_owned(),
            ],
        ),
        _ => (
            vec![
                ComposerModelItem {
                    id: "default".to_owned(),
                    label: "Default".to_owned(),
                    efforts: vec![
                        "low".to_owned(),
                        "medium".to_owned(),
                        "high".to_owned(),
                        "xhigh".to_owned(),
                        "max".to_owned(),
                    ],
                    default_effort: None,
                },
                ComposerModelItem {
                    id: "opus".to_owned(),
                    label: "Opus".to_owned(),
                    efforts: vec![
                        "low".to_owned(),
                        "medium".to_owned(),
                        "high".to_owned(),
                        "xhigh".to_owned(),
                        "max".to_owned(),
                    ],
                    default_effort: None,
                },
                ComposerModelItem {
                    id: "fable".to_owned(),
                    label: "Fable".to_owned(),
                    efforts: vec![
                        "low".to_owned(),
                        "medium".to_owned(),
                        "high".to_owned(),
                        "xhigh".to_owned(),
                        "max".to_owned(),
                    ],
                    default_effort: None,
                },
                ComposerModelItem {
                    id: "sonnet".to_owned(),
                    label: "Sonnet".to_owned(),
                    efforts: vec![
                        "low".to_owned(),
                        "medium".to_owned(),
                        "high".to_owned(),
                        "xhigh".to_owned(),
                        "max".to_owned(),
                    ],
                    default_effort: None,
                },
                ComposerModelItem {
                    id: "haiku".to_owned(),
                    label: "Haiku".to_owned(),
                    efforts: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
                    default_effort: None,
                },
            ],
            vec![
                "low".to_owned(),
                "medium".to_owned(),
                "high".to_owned(),
                "xhigh".to_owned(),
                "max".to_owned(),
            ],
        ),
    }
}

fn resolved_composer_choices(
    agent: &str,
    meta: Option<&ComposerMeta>,
) -> (Vec<ComposerModelItem>, Vec<String>) {
    let (fallback_models, fallback_efforts) = fallback_choices_for_agent(agent);
    let models = meta
        .filter(|m| !m.models.is_empty())
        .map_or(fallback_models, |m| m.models.clone());
    let active_id = meta.and_then(|m| m.current_model_id.as_deref().or(m.model.as_deref()));
    let efforts = meta
        .and_then(|m| {
            active_id
                .and_then(|id| m.models.iter().find(|item| item.id == id))
                .filter(|item| !item.efforts.is_empty())
                .map(|item| item.efforts.clone())
                .or_else(|| (!m.efforts.is_empty()).then(|| m.efforts.clone()))
        })
        .unwrap_or(fallback_efforts);
    (models, efforts)
}

impl Shell {
    pub(super) fn render_chat_composer(
        &self,
        pane: super::super::PaneId,
        key: ChatKey,
        input_state: &gpui::Entity<TextareaState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let submit_key = key.clone();
        let palette = ui::palette(cx);
        let border = palette.border;
        let muted_fg = palette.muted_foreground;
        let log = self.chat.logs.get(&key);
        let meta = log.map(|l| &l.meta);
        let agent_id = log.map_or("claude", |l| {
            if l.agent.is_empty() {
                "claude"
            } else {
                l.agent.as_str()
            }
        });
        let (model_choices, effort_choices) = resolved_composer_choices(agent_id, meta);
        let current_model = meta.and_then(|m| m.current_model_id.as_deref().or(m.model.as_deref()));
        let current_effort = meta.and_then(|m| m.effort.as_deref());
        let is_pending = meta.is_some_and(|m| m.pending);

        let model_label = if let Some(label) = meta.and_then(|m| m.model_label.as_deref()) {
            label.to_owned()
        } else if let Some(m) = current_model {
            model_choices
                .iter()
                .find(|item| item.id == m)
                .map_or_else(|| m.to_owned(), |item| item.label.clone())
        } else if let Some(agent) = log.map(|l| &l.agent).filter(|a| !a.is_empty()) {
            agent_display_name(agent)
        } else {
            "Agent".to_string()
        };

        let effort_label =
            current_effort.map_or_else(|| ctext::PICKER_EFFORT.get().to_owned(), str::to_owned);

        let context_badge = meta.and_then(|m| m.context_percent).map(|pct| {
            let tone = if pct >= 85 {
                ui::BadgeTone::Destructive
            } else if pct >= 65 {
                ui::BadgeTone::Warning
            } else {
                ui::BadgeTone::Neutral
            };
            ui::badge(palette, tone, format!("{pct}%"))
                .id(SharedString::from(format!(
                    "chat-composer-context-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-composer-context".into())
        });

        let quick_menu_row = if self.chat.quick_menu.contains(&key) {
            let commands = ["/model", "/compact", "/clear", "/review"];
            Some(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1p5()
                    .children(commands.into_iter().map(|cmd| {
                        let cmd_str = cmd.to_owned();
                        let input_state = input_state.clone();
                        let key = key.clone();
                        let slug = cmd.trim_start_matches('/').to_owned();
                        Button::new(SharedString::from(format!(
                            "chat-quick-{slug}-{}",
                            pane.get()
                        )))
                        .debug_selector(move || format!("chat-quick-{slug}"))
                        .label(cmd)
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.chat.quick_menu.remove(&key);
                                if cmd_str == "/model" {
                                    this.toggle_chat_model_picker(&key, cx);
                                    return;
                                }
                                input_state.update(cx, |state, cx| {
                                    let val = state.value();
                                    let new_val = if val.trim().is_empty() {
                                        cmd_str.clone()
                                    } else {
                                        format!("{cmd_str} {val}")
                                    };
                                    state.set_value(new_val, window, cx);
                                    state.focus(window, cx);
                                });
                                cx.notify();
                            },
                        ))
                    })),
            )
        } else {
            None
        };

        let model_picker_panel = if self.chat.model_picker.contains(&key) {
            let active_model = current_model.map(str::to_owned);
            let active_effort = current_effort.map(str::to_owned);
            let reason_text = meta.and_then(|m| m.reason.clone());
            Some(
                div()
                    .id(SharedString::from(format!(
                        "chat-model-picker-{}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-model-picker".into())
                    .w_full()
                    .min_w_0()
                    .p_2()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(border)
                    .bg(palette.muted)
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(muted_fg)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(ctext::PICKER_MODEL.get()),
                            )
                            .children(model_choices.into_iter().map(|item| {
                                let id = item.id;
                                let label = item.label;
                                let is_selected = active_model.as_deref() == Some(id.as_str());
                                let key = key.clone();
                                let option_id = id.clone();
                                let selector_id = id.clone();
                                Button::new(SharedString::from(format!(
                                    "chat-model-option-{id}-{}",
                                    pane.get()
                                )))
                                .debug_selector(move || format!("chat-model-option-{selector_id}"))
                                .when(is_selected, |btn| btn.icon(IconName::Check).selected(true))
                                .disabled(is_pending)
                                .label(label)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.select_chat_model(&key, &option_id, cx);
                                    },
                                ))
                            })),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(muted_fg)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(ctext::PICKER_EFFORT.get()),
                            )
                            .children(effort_choices.into_iter().map(|effort| {
                                let is_selected = active_effort.as_deref() == Some(effort.as_str());
                                let key = key.clone();
                                let option_effort = effort.clone();
                                let selector_effort = effort.clone();
                                Button::new(SharedString::from(format!(
                                    "chat-effort-option-{effort}-{}",
                                    pane.get()
                                )))
                                .debug_selector(move || {
                                    format!("chat-effort-option-{selector_effort}")
                                })
                                .when(is_selected, |btn| btn.icon(IconName::Check).selected(true))
                                .disabled(is_pending)
                                .label(effort)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.select_chat_effort(&key, &option_effort, cx);
                                    },
                                ))
                            })),
                    )
                    .children(reason_text.map(|reason| {
                        div()
                            .id(SharedString::from(format!(
                                "chat-model-picker-reason-{}",
                                pane.get()
                            )))
                            .debug_selector(|| "chat-model-picker-reason".into())
                            .text_size(ui::TEXT_XS)
                            .text_color(muted_fg)
                            .child(reason)
                    })),
            )
        } else {
            None
        };

        let can_send = !input_state.read(cx).value().trim().is_empty()
            && !self.chat.prompt_tasks.contains_key(&key)
            && !self.chat.prompt_review.contains(&key);

        let inner_card = div()
            .id("desktop-chat-composer")
            .debug_selector(|| "desktop-chat-composer".into())
            .on_action(cx.listener(
                move |this, action: &gpui_component::input::Enter, window, cx| {
                    if action.secondary || action.shift {
                        cx.propagate();
                    } else {
                        this.submit_chat_prompt(&submit_key, window, cx);
                    }
                },
            ))
            .w_full()
            .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
            .min_w_0()
            .rounded(ui::RADIUS_LG)
            .border_1()
            .border_color(border)
            .bg(palette.card)
            .p_2p5()
            .flex()
            .flex_col()
            .gap_2()
            .children(quick_menu_row)
            .children(model_picker_panel)
            .child(
                Textarea::new(input_state)
                    .small()
                    .w_full()
                    .min_w_0()
                    .accessibility_id("chat-composer-input")
                    .aria_label(ctext::PROMPT_PLACEHOLDER.get()),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1p5()
                            .child(
                                Button::new(SharedString::from(format!(
                                    "chat-composer-plus-{}",
                                    pane.get()
                                )))
                                .debug_selector(|| "chat-composer-plus".into())
                                .icon(IconName::Plus)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener({
                                    let key = key.clone();
                                    move |this, _, _, cx| {
                                        this.chat.model_picker.remove(&key);
                                        if this.chat.quick_menu.contains(&key) {
                                            this.chat.quick_menu.remove(&key);
                                        } else {
                                            this.chat.quick_menu.insert(key.clone());
                                        }
                                        cx.notify();
                                    }
                                })),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "chat-composer-model-{}",
                                    pane.get()
                                )))
                                .debug_selector(|| "chat-composer-model".into())
                                .icon(IconName::ChevronDown)
                                .label(model_label)
                                .disabled(is_pending)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener({
                                    let key = key.clone();
                                    move |this, _, _, cx| {
                                        this.toggle_chat_model_picker(&key, cx);
                                    }
                                })),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "chat-composer-effort-{}",
                                    pane.get()
                                )))
                                .debug_selector(|| "chat-composer-effort".into())
                                .icon(IconName::ChevronDown)
                                .label(effort_label)
                                .disabled(is_pending)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener({
                                    let key = key.clone();
                                    move |this, _, _, cx| {
                                        this.toggle_chat_model_picker(&key, cx);
                                    }
                                })),
                            )
                            .children(context_badge)
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(muted_fg)
                                    .child(ctext::COMPOSER_HINT.get()),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "desktop-chat-send-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "desktop-chat-send".into())
                        .icon(IconName::ArrowUp)
                        .label(ctext::SEND.get())
                        .small()
                        .primary()
                        .disabled(!can_send)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.submit_chat_prompt(&key, window, cx);
                            },
                        )),
                    ),
            );

        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .px_3()
            .py_2()
            .bg(palette.background)
            .flex()
            .justify_center()
            .child(inner_card)
            .into_any_element()
    }

    pub(super) fn render_chat_prompt_notices(
        &self,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let palette = ui::palette(cx);
        let mut notices = Vec::new();
        if self.chat.poll_failed.contains(key) {
            notices.push(
                ui::error_banner(palette, ctext::POLL_FAILED.get())
                    .id("chat-poll-error")
                    .debug_selector(|| "chat-poll-error".into())
                    .into_any_element(),
            );
        }
        if self.chat.prompt_failed.contains(key) {
            notices.push(
                ui::error_banner(palette, ctext::PROMPT_FAILED.get())
                    .id("chat-prompt-error")
                    .debug_selector(|| "chat-prompt-error".into())
                    .into_any_element(),
            );
        }
        if self.chat.multiline_blocked.contains(key) {
            notices.push(
                ui::error_banner(palette, ctext::MULTILINE_BLOCKED.get())
                    .id("chat-multiline-error")
                    .debug_selector(|| "chat-multiline-error".into())
                    .into_any_element(),
            );
        }
        if self.chat.prompt_review.contains(key) {
            notices.push(
                div()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        ui::error_banner(palette, ctext::PROMPT_REVIEW.get())
                            .id("chat-prompt-review")
                            .debug_selector(|| "chat-prompt-review".into()),
                    )
                    .child(
                        Button::new("chat-prompt-review-done")
                            .debug_selector(|| "chat-prompt-review-done".into())
                            .label(ctext::REVIEW_DONE.get())
                            .small()
                            .ghost()
                            .on_click(cx.listener({
                                let key = key.clone();
                                move |this, _, _, cx| {
                                    this.chat.prompt_review.remove(&key);
                                    cx.notify();
                                }
                            })),
                    )
                    .into_any_element(),
            );
        }
        notices
    }
}
