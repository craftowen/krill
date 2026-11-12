// Composer pattern adapted from Orca NativeChatComposerField.tsx (MIT).
// Copyright (c) 2026 Lovecast Inc.
use super::*;
use gpui::prelude::FluentBuilder as _;
use gpui::{Focusable as _, StatefulInteractiveElement as _, px};
use gpui_component::{Icon, IconName, Selectable as _, progress::ProgressCircle, tooltip::Tooltip};

#[derive(Default)]
struct DismissedComposerPopovers {
    quick_menu: Option<ChatKey>,
    model_picker: Option<ChatKey>,
}

impl gpui::Global for DismissedComposerPopovers {}

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
                    label: "Opus 5.5".to_owned(),
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
                    label: "Fable 5.1".to_owned(),
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
                    label: "Sonnet 5.5".to_owned(),
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
                    label: "Haiku 5.5".to_owned(),
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

/// `claude-opus-5-5` reads as `opus 5.5`, the way the phone shows it. Other labels stay as reported.
pub(super) fn short_model_name(text: &str) -> String {
    let lower = text.to_lowercase();
    let name = lower.split(['(', '[']).next().unwrap_or_default().trim();
    let Some(rest) = name.strip_prefix("claude-") else {
        return name
            .strip_prefix("claude ")
            .map_or_else(|| text.to_owned(), str::to_owned);
    };
    let mut parts: Vec<_> = rest.split('-').collect();
    if parts
        .last()
        .is_some_and(|part| part.len() == 8 && part.bytes().all(|b| b.is_ascii_digit()))
    {
        parts.pop();
    }
    let digits = parts
        .iter()
        .rev()
        .take_while(|part| part.bytes().all(|b| b.is_ascii_digit()))
        .count()
        .min(2);
    match parts.split_at(parts.len() - digits) {
        (family, []) => family.join("-"),
        (family, version) => format!("{} {}", family.join("-"), version.join(".")),
    }
}

fn effort_bars(effort: &str) -> Option<usize> {
    match effort {
        "minimal" | "low" => Some(1),
        "medium" => Some(2),
        "high" => Some(3),
        "xhigh" => Some(4),
        "max" | "ultra" => Some(5),
        _ => None,
    }
}

fn effort_text(effort: &str) -> String {
    match effort {
        "low" => ctext::EFFORT_LOW.get().to_owned(),
        "medium" => ctext::EFFORT_MEDIUM.get().to_owned(),
        "high" => ctext::EFFORT_HIGH.get().to_owned(),
        "xhigh" => ctext::EFFORT_XHIGH.get().to_owned(),
        "max" => ctext::EFFORT_MAX.get().to_owned(),
        other => other.to_owned(),
    }
}

fn model_is_selected(active: Option<&str>, id: &str) -> bool {
    active.is_some_and(|active| active == id || active.starts_with(&format!("claude-{id}-")))
}

fn effort_bars_icon(palette: &ui::Palette, filled: usize) -> gpui::Div {
    div()
        .flex()
        .items_end()
        .gap(px(1.))
        .h(px(11.))
        .children((0..5u8).map(|bar| {
            div()
                .w(px(2.))
                .h(px(3. + f32::from(bar) * 2.))
                .rounded(px(1.))
                .bg(if usize::from(bar) < filled {
                    palette.foreground
                } else {
                    palette.border
                })
        }))
}

fn composer_popup(palette: &ui::Palette) -> gpui::Div {
    div()
        .absolute()
        .left_0()
        .bottom_full()
        .mb_1p5()
        .w(px(280.))
        .max_w_full()
        .rounded(ui::RADIUS_LG)
        .border_1()
        .border_color(palette.border)
        .bg(palette.popover)
        .p_1p5()
        .flex()
        .flex_col()
        .gap_0p5()
}

fn popup_row(palette: &ui::Palette, id: SharedString, enabled: bool) -> gpui::Stateful<gpui::Div> {
    let hover = palette.muted;
    div()
        .id(id)
        .role(gpui::Role::ListBoxOption)
        .w_full()
        .min_h(px(28.))
        .px_2()
        .rounded(ui::RADIUS_SM)
        .flex()
        .items_center()
        .justify_between()
        .text_size(ui::TEXT_SM)
        .text_color(if enabled {
            palette.foreground
        } else {
            palette.muted_foreground
        })
        .when(enabled, |row| {
            row.cursor_pointer().hover(move |style| style.bg(hover))
        })
}

fn popup_heading(palette: &ui::Palette, text: &'static str) -> gpui::Div {
    div()
        .px_2()
        .pt_1()
        .pb_0p5()
        .text_size(ui::TEXT_XS)
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(palette.muted_foreground)
        .child(text)
}

fn resolved_composer_choices(
    agent: &str,
    meta: Option<&ComposerMeta>,
) -> (Vec<ComposerModelItem>, Vec<String>) {
    let (fallback_models, fallback_efforts) = fallback_choices_for_agent(agent);
    // A busy Claude session only reports its configured model, so keep every native alias listed.
    let claude = agent.eq_ignore_ascii_case("claude");
    let models = meta
        .filter(|m| !m.models.is_empty() && (m.supported || !claude))
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

        let supported = meta.is_some_and(|m| m.supported);
        let model_label = if let Some(label) = meta.and_then(|m| m.model_label.as_deref()) {
            short_model_name(label)
        } else if let Some(m) = current_model {
            model_choices
                .iter()
                .find(|item| item.id == m)
                .map_or_else(|| short_model_name(m), |item| item.label.clone())
        } else if let Some(agent) = log.map(|l| &l.agent).filter(|a| !a.is_empty()) {
            agent_display_name(agent)
        } else {
            "Agent".to_string()
        };
        let filled_bars = current_effort.and_then(effort_bars);

        let context_ring = meta.and_then(|m| m.context_percent).map(|pct| {
            let color = if pct >= 85 {
                palette.destructive
            } else if pct >= 65 {
                palette.warning
            } else {
                muted_fg
            };
            let description: SharedString = ctext::CONTEXT_USED
                .get()
                .replace("{percent}", &pct.to_string())
                .into();
            div()
                .id(SharedString::from(format!(
                    "chat-composer-context-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-composer-context".into())
                .tooltip({
                    let description = description.clone();
                    move |window, cx| Tooltip::new(description.clone()).build(window, cx)
                })
                .child(
                    ProgressCircle::new(SharedString::from(format!(
                        "chat-composer-context-ring-{}",
                        pane.get()
                    )))
                    .value(f32::from(pct))
                    .color(color)
                    .size_4()
                    .accessibility_label(description),
                )
        });

        let quick_menu_open = self.chat.quick_menu.contains(&key);
        let model_picker_open = self.chat.model_picker.contains(&key);

        let quick_menu = quick_menu_open.then(|| {
            let close_key = key.clone();
            composer_popup(palette)
                .id(SharedString::from(format!(
                    "chat-quick-menu-{}",
                    pane.get()
                )))
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    this.chat.quick_menu.remove(&close_key);
                    cx.notify();
                }))
                .children(
                    ["/model", "/compact", "/clear", "/review"]
                        .into_iter()
                        .map(|cmd| {
                            let cmd_str = cmd.to_owned();
                            let input_state = input_state.clone();
                            let key = key.clone();
                            let slug = cmd.trim_start_matches('/').to_owned();
                            popup_row(
                                palette,
                                SharedString::from(format!("chat-quick-{slug}-{}", pane.get())),
                                true,
                            )
                            .debug_selector(move || format!("chat-quick-{slug}"))
                            .child(cmd)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.chat.quick_menu.remove(&key);
                                    if cmd_str == "/model" {
                                        this.toggle_chat_model_picker(&key, cx);
                                        if this.chat.model_picker.contains(&key)
                                            && !input_state
                                                .read(cx)
                                                .focus_handle(cx)
                                                .is_focused(window)
                                        {
                                            input_state
                                                .update(cx, |state, cx| state.focus(window, cx));
                                        }
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
                        }),
                )
        });

        let model_picker =
            model_picker_open.then(|| {
                let active_model = current_model.map(str::to_owned);
                let active_effort = current_effort.map(str::to_owned);
                let reason = meta.and_then(|m| m.reason.as_deref());
                let waiting = !supported || reason.is_some_and(|text| text.starts_with("Wait for"));
                let notice = if waiting {
                    Some(SharedString::from(ctext::PICKER_WAIT.get()))
                } else {
                    reason.map(|text| SharedString::from(text.to_owned()))
                };
                let close_key = key.clone();
                composer_popup(palette)
                    .id(SharedString::from(format!(
                        "chat-model-picker-{}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-model-picker".into())
                    .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                        this.chat.model_picker.remove(&close_key);
                        cx.notify();
                    }))
                    .child(popup_heading(palette, ctext::PICKER_MODEL.get()))
                    .children(model_choices.into_iter().map(|item| {
                        let id = item.id;
                        let is_selected = model_is_selected(active_model.as_deref(), &id);
                        let key = key.clone();
                        let option_id = id.clone();
                        let selector_id = id.clone();
                        let enabled = supported && !is_pending;
                        popup_row(
                            palette,
                            SharedString::from(format!("chat-model-option-{id}-{}", pane.get())),
                            enabled,
                        )
                        .debug_selector(move || format!("chat-model-option-{selector_id}"))
                        .child(item.label)
                        .when(is_selected, |row| {
                            row.child(Icon::new(IconName::Check).size(px(ui::ICON_SM)))
                        })
                        .when(enabled, |row| {
                            row.on_click(cx.listener(move |this, _, _, cx| {
                                this.select_chat_model(&key, &option_id, cx);
                            }))
                        })
                    }))
                    .child(popup_heading(palette, ctext::PICKER_EFFORT.get()))
                    .child(div().flex().flex_wrap().gap_0p5().children(
                        effort_choices.into_iter().map(|effort| {
                            let is_selected = active_effort.as_deref() == Some(effort.as_str());
                            let key = key.clone();
                            let option_effort = effort.clone();
                            let selector_effort = effort.clone();
                            Button::new(SharedString::from(format!(
                                "chat-effort-option-{effort}-{}",
                                pane.get()
                            )))
                            .debug_selector(move || format!("chat-effort-option-{selector_effort}"))
                            .label(effort_text(&effort))
                            .selected(is_selected)
                            .disabled(is_pending || !supported)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.select_chat_effort(&key, &option_effort, cx);
                                },
                            ))
                        }),
                    ))
                    .children(notice.map(|notice| {
                        div()
                            .id(SharedString::from(format!(
                                "chat-model-picker-reason-{}",
                                pane.get()
                            )))
                            .debug_selector(|| "chat-model-picker-reason".into())
                            .px_2()
                            .pt_1()
                            .text_size(ui::TEXT_XS)
                            .text_color(muted_fg)
                            .child(notice)
                    }))
            });

        let can_send = !input_state.read(cx).value().trim().is_empty()
            && !self.chat.prompt_tasks.contains_key(&key)
            && !self.chat.prompt_review.contains(&key);

        let inner_card = div()
            .id("desktop-chat-composer")
            .debug_selector(|| "desktop-chat-composer".into())
            .capture_action(cx.listener({
                let escape_input = input_state.clone();
                move |_, _: &gpui_component::input::Escape, window, cx| {
                    let composing = escape_input.update(cx, |state, cx| {
                        state.focus_handle(cx).is_focused(window)
                            && gpui::EntityInputHandler::marked_text_range(state, window, cx)
                                .is_some()
                    });
                    if composing {
                        window.prevent_default();
                    }
                }
            }))
            .capture_key_down(cx.listener({
                let escape_key = key.clone();
                let escape_input = input_state.clone();
                move |this, e: &gpui::KeyDownEvent, window, cx| {
                    if e.keystroke.key != "escape"
                        || e.keystroke.modifiers.modified()
                        || window.default_prevented()
                    {
                        return;
                    }
                    if !this.chat.quick_menu.contains(&escape_key)
                        && !this.chat.model_picker.contains(&escape_key)
                    {
                        return;
                    }
                    let composing = escape_input.update(cx, |state, cx| {
                        state.focus_handle(cx).is_focused(window)
                            && gpui::EntityInputHandler::marked_text_range(state, window, cx)
                                .is_some()
                    });
                    if composing {
                        return;
                    }
                    this.chat.quick_menu.remove(&escape_key);
                    this.chat.model_picker.remove(&escape_key);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(
                move |this, action: &gpui_component::input::Enter, window, cx| {
                    if action.secondary || action.shift {
                        cx.propagate();
                    } else {
                        this.submit_chat_prompt(&submit_key, window, cx);
                    }
                },
            ))
            .relative()
            .w_full()
            .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
            .min_w_0()
            .rounded(ui::RADIUS_LG)
            .border_1()
            .border_color(border)
            .bg(palette.card)
            .px_2()
            .pt_1p5()
            .pb_1p5()
            .flex()
            .flex_col()
            .gap_1()
            .children(quick_menu)
            .children(model_picker)
            .child(
                Textarea::new(input_state)
                    .small()
                    .appearance(false)
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
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(ui::button_with_tooltip_when(
                                !quick_menu_open,
                                Button::new(SharedString::from(format!(
                                    "chat-composer-plus-{}",
                                    pane.get()
                                )))
                                .debug_selector(|| "chat-composer-plus".into())
                                .icon(IconName::Plus)
                                .ghost()
                                .xsmall()
                                .on_mouse_down(gpui::MouseButton::Left, {
                                    let down_key = key.clone();
                                    move |_, _, cx| {
                                        cx.default_global::<DismissedComposerPopovers>()
                                            .quick_menu = quick_menu_open.then(|| down_key.clone());
                                    }
                                })
                                .on_click(cx.listener({
                                    let key = key.clone();
                                    let input_state = input_state.clone();
                                    move |this, event, window, cx| {
                                        let dismissed = matches!(event, gpui::ClickEvent::Mouse(_))
                                            && cx
                                                .default_global::<DismissedComposerPopovers>()
                                                .quick_menu
                                                .take()
                                                .as_ref()
                                                == Some(&key);
                                        if dismissed {
                                            return;
                                        }
                                        this.chat.model_picker.remove(&key);
                                        if this.chat.quick_menu.contains(&key) {
                                            this.chat.quick_menu.remove(&key);
                                        } else {
                                            this.chat.quick_menu.insert(key.clone());
                                            if matches!(event, gpui::ClickEvent::Mouse(_))
                                                && !input_state
                                                    .read(cx)
                                                    .focus_handle(cx)
                                                    .is_focused(window)
                                            {
                                                input_state.update(cx, |state, cx| {
                                                    state.focus(window, cx);
                                                });
                                            }
                                        }
                                        cx.notify();
                                    }
                                })),
                                "chat-composer-plus",
                                krill_app::i18n::catalog::settings::TAB_QUICK_COMMANDS.get(),
                            ))
                            .child(
                                Button::new(SharedString::from(format!(
                                    "chat-composer-model-{}",
                                    pane.get()
                                )))
                                .debug_selector(|| "chat-composer-model".into())
                                .label(model_label)
                                .children(
                                    filled_bars.map(|filled| effort_bars_icon(palette, filled)),
                                )
                                .dropdown_caret(true)
                                .disabled(is_pending)
                                .ghost()
                                .xsmall()
                                .on_mouse_down(gpui::MouseButton::Left, {
                                    let down_key = key.clone();
                                    move |_, _, cx| {
                                        cx.default_global::<DismissedComposerPopovers>()
                                            .model_picker =
                                            model_picker_open.then(|| down_key.clone());
                                    }
                                })
                                .on_click(cx.listener({
                                    let key = key.clone();
                                    let input_state = input_state.clone();
                                    move |this, event, window, cx| {
                                        let dismissed = matches!(event, gpui::ClickEvent::Mouse(_))
                                            && cx
                                                .default_global::<DismissedComposerPopovers>()
                                                .model_picker
                                                .take()
                                                .as_ref()
                                                == Some(&key);
                                        if dismissed {
                                            return;
                                        }
                                        this.toggle_chat_model_picker(&key, cx);
                                        if this.chat.model_picker.contains(&key)
                                            && matches!(event, gpui::ClickEvent::Mouse(_))
                                            && !input_state
                                                .read(cx)
                                                .focus_handle(cx)
                                                .is_focused(window)
                                        {
                                            input_state.update(cx, |state, cx| {
                                                state.focus(window, cx);
                                            });
                                        }
                                    }
                                })),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children(context_ring)
                            .child(
                                Button::new(SharedString::from(format!(
                                    "desktop-chat-send-{}",
                                    pane.get()
                                )))
                                .debug_selector(|| "desktop-chat-send".into())
                                .icon(IconName::ArrowUp)
                                .tooltip(ctext::SEND.get())
                                .small()
                                .primary()
                                .rounded(px(999.))
                                .disabled(!can_send)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.submit_chat_prompt(&key, window, cx);
                                    },
                                )),
                            ),
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

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn model_ids_read_like_the_phone() {
        for (raw, short) in [
            ("claude-opus-5-5", "opus 5.5"),
            ("claude-fable-5-1", "fable 5.1"),
            ("claude-sonnet-4-5-20251101", "sonnet 4.5"),
            ("claude-opus-5-5[1m]", "opus 5.5"),
            ("Claude Opus 5.5", "opus 5.5"),
            ("GPT-6.1-Sol", "GPT-6.1-Sol"),
            ("Opus", "Opus"),
        ] {
            assert_eq!(short_model_name(raw), short, "{raw}");
        }
    }

    #[test]
    fn aliases_match_the_running_model_and_unknown_effort_shows_no_bars() {
        assert!(model_is_selected(Some("claude-opus-5-5"), "opus"));
        assert!(model_is_selected(Some("opus"), "opus"));
        assert!(!model_is_selected(Some("claude-opus-5-5"), "sonnet"));
        assert_eq!(effort_bars("high"), Some(3));
        assert_eq!(effort_bars("unknown"), None);
    }
}
