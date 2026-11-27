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
                None => ctext::AGENT_FALLBACK.get().to_string(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        }
    }
}

pub(super) fn fallback_choices_for_agent(agent: &str) -> (Vec<ComposerModelItem>, Vec<String>) {
    let (models, efforts) = krill_bots_ui::agent_models::builtin(agent);
    let models = models
        .iter()
        .map(|m| ComposerModelItem {
            id: m.id.to_owned(),
            label: if m.id == "default" {
                ctext::MODEL_DEFAULT.get().to_owned()
            } else {
                m.label.to_owned()
            },
            efforts: m.efforts.iter().map(|e| (*e).to_owned()).collect(),
            default_effort: m.default_effort.map(str::to_owned),
        })
        .collect();
    (models, efforts.iter().map(|e| (*e).to_owned()).collect())
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

#[cfg(test)]
fn effort_bars(effort: &str) -> Option<usize> {
    match effort {
        "minimal" | "low" => Some(1),
        "medium" => Some(2),
        "high" => Some(3),
        "xhigh" | "very_high" | "very-high" => Some(4),
        "max" | "ultra" => Some(5),
        _ => None,
    }
}

pub(super) fn effort_text(effort: &str) -> String {
    match effort {
        "minimal" => ctext::EFFORT_MINIMAL.get().to_owned(),
        "low" => ctext::EFFORT_LOW.get().to_owned(),
        "medium" => ctext::EFFORT_MEDIUM.get().to_owned(),
        "high" => ctext::EFFORT_HIGH.get().to_owned(),
        "xhigh" | "very_high" | "very-high" => ctext::EFFORT_XHIGH.get().to_owned(),
        "max" => ctext::EFFORT_MAX.get().to_owned(),
        "ultra" => ctext::EFFORT_ULTRA.get().to_owned(),
        "none" => ctext::EFFORT_NONE.get().to_owned(),
        "off" => ctext::EFFORT_OFF.get().to_owned(),
        other => other.to_owned(),
    }
}

fn model_is_selected(active: Option<&str>, id: &str) -> bool {
    active.is_some_and(|active| active == id || active.starts_with(&format!("claude-{id}-")))
}

fn composer_popup(palette: &ui::Palette) -> gpui::Div {
    composer_popup_at(palette, false)
}

/// Above the composer, under the control that opened it (Orca's model picker sits at the right).
fn composer_popup_at(palette: &ui::Palette, right: bool) -> gpui::Div {
    div()
        .absolute()
        .when(right, gpui::Styled::right_0)
        .when(!right, gpui::Styled::left_0)
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

pub(super) fn effort_rank(raw: &str) -> usize {
    match raw.trim().to_ascii_lowercase().as_str() {
        "none" | "off" => 0,
        "minimal" => 1,
        "low" => 2,
        "medium" => 3,
        "high" => 4,
        "xhigh" | "x-high" | "extra-high" | "very_high" | "very-high" => 5,
        "max" => 6,
        "ultra" => 7,
        _ => usize::MAX,
    }
}

pub(super) fn sort_effort_choices(mut efforts: Vec<String>) -> Vec<String> {
    efforts.sort_by_key(|effort| effort_rank(effort));
    efforts
}

pub(super) fn resolved_composer_choices(
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
                .filter(|item| !item.efforts.is_empty() || m.efforts.is_empty())
                .map(|item| item.efforts.clone())
                .or_else(|| (!m.efforts.is_empty()).then(|| m.efforts.clone()))
        })
        .unwrap_or(fallback_efforts);
    (models, sort_effort_choices(efforts))
}

impl Shell {
    pub(super) fn render_chat_composer(
        &mut self,
        pane: super::super::PaneId,
        key: ChatKey,
        input_state: &gpui::Entity<TextareaState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let submit_key = key.clone();
        let palette = ui::palette(cx);
        let muted_fg = palette.muted_foreground;
        let log = self.chat.logs.get(&key);
        let meta = log.map(|l| &l.meta);
        let agent_id = self.chat_agent_id(&key);
        let (model_choices, effort_choices) = resolved_composer_choices(agent_id, meta);
        let current_model = meta.and_then(|m| m.current_model_id.as_deref().or(m.model.as_deref()));
        let current_effort = meta.and_then(|m| m.effort.as_deref());
        let is_pending = meta.is_some_and(|m| m.pending);

        let supported = meta.is_some_and(|m| m.supported);
        let effort_supported = meta.is_some_and(|m| m.supported && m.effort_supported);
        let show_effort =
            !effort_choices.is_empty() && meta.is_none_or(|m| !m.supported || m.effort_supported);
        let model_label = if let Some(label) = meta.and_then(|m| m.model_label.as_deref()) {
            short_model_name(label)
        } else if let Some(m) = current_model {
            model_choices
                .iter()
                .find(|item| item.id == m)
                .map_or_else(|| short_model_name(m), |item| item.label.clone())
        } else if let Some(agent) = log
            .map(|l| l.agent.as_str())
            .filter(|a| !a.is_empty())
            .or_else(|| {
                self.chat_bot(&key)
                    .map(|b| krill_proto::agent::canonical_id(&b.agent_id))
                    .filter(|a| !a.is_empty())
            })
        {
            agent_display_name(agent)
        } else {
            ctext::AGENT_FALLBACK.get().to_string()
        };
        let effort_label = current_effort
            .filter(|e| !e.trim().is_empty())
            .map(effort_text);

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
            let attach_key = key.clone();
            composer_popup(palette)
                .id(SharedString::from(format!(
                    "chat-quick-menu-{}",
                    pane.get()
                )))
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    this.chat.quick_menu.remove(&close_key);
                    cx.notify();
                }))
                .child(
                    popup_row(
                        palette,
                        SharedString::from(format!("chat-quick-attach-{}", pane.get())),
                        true,
                    )
                    .debug_selector(|| "chat-quick-attach".into())
                    .gap_2()
                    .child(
                        gpui_component::Icon::new(IconName::File)
                            .size(px(ui::ICON_SM))
                            .text_color(palette.muted_foreground),
                    )
                    .child(ctext::ATTACH_FILE.get())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.chat.quick_menu.remove(&attach_key);
                        this.prompt_for_chat_attachments(&attach_key, cx);
                        cx.notify();
                    })),
                )
                .child(div().h_px().w_full().my_0p5().bg(palette.border))
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
                                    if let Some(ac) = this.chat.autocomplete.get_mut(&key) {
                                        ac.dismissed = Some((
                                            super::catalog::AutocompleteKind::Slash,
                                            0,
                                            cmd_str.trim_start_matches('/').to_owned(),
                                        ));
                                        ac.close();
                                    }
                                    let run_clear_now = cmd_str == "/clear"
                                        && input_state.read(cx).value().trim().is_empty();
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
                                    if run_clear_now {
                                        this.submit_chat_prompt(&key, window, cx);
                                    } else {
                                        cx.notify();
                                    }
                                },
                            ))
                        }),
                )
        });

        let kbd_selected_idx = self.chat.model_picker_selected.get(&key).copied();
        let model_picker = model_picker_open.then(|| {
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
            composer_popup_at(palette, true)
                .id(SharedString::from(format!(
                    "chat-model-picker-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-model-picker".into())
                // Orca `#47587`: clicking or dragging the picker's scrollbar must not close it.
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    this.chat.model_picker.remove(&close_key);
                    this.chat.model_picker_selected.remove(&close_key);
                    cx.notify();
                }))
                .child(popup_heading(palette, ctext::PICKER_MODEL.get()))
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "chat-model-list-{}",
                            pane.get()
                        )))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .children(model_choices.into_iter().enumerate().map(|(idx, item)| {
                            let id = item.id;
                            let is_selected = model_is_selected(active_model.as_deref(), &id);
                            let is_kbd = kbd_selected_idx == Some(idx);
                            let key = key.clone();
                            let option_id = id.clone();
                            let selector_id = id.clone();
                            let enabled = supported && !is_pending;
                            popup_row(
                                palette,
                                SharedString::from(format!(
                                    "chat-model-option-{id}-{}",
                                    pane.get()
                                )),
                                enabled,
                            )
                            .debug_selector(move || format!("chat-model-option-{selector_id}"))
                            .when(is_kbd, |row| row.bg(palette.muted))
                            .child(item.label)
                            .when(is_selected, |row| {
                                row.child(Icon::new(IconName::Check).size(px(ui::ICON_SM)))
                            })
                            .when(enabled, |row| {
                                row.on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_chat_model(&key, &option_id, cx);
                                }))
                            })
                        })),
                )
                .when(show_effort, |popup| {
                    popup
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
                                .debug_selector(move || {
                                    format!("chat-effort-option-{selector_effort}")
                                })
                                .label(effort_text(&effort))
                                .selected(is_selected)
                                .disabled(is_pending || !effort_supported)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener({
                                    let focus_input = input_state.clone();
                                    move |this, _, window, cx| {
                                        this.select_chat_effort(&key, &option_effort, cx);
                                        if !focus_input.read(cx).focus_handle(cx).is_focused(window)
                                        {
                                            focus_input
                                                .update(cx, |state, cx| state.focus(window, cx));
                                        }
                                    }
                                }))
                            }),
                        ))
                })
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

        let attachments = self
            .chat
            .draft_attachments
            .get(&key)
            .map_or(&[][..], Vec::as_slice);
        let has_attachments = !attachments.is_empty();
        let autocomplete_open = self
            .chat
            .autocomplete
            .get(&key)
            .is_some_and(super::catalog::AutocompleteState::should_render_popover);

        let autocomplete_popover = autocomplete_open.then(|| {
            let ac = &self.chat.autocomplete[&key];
            let items = ac.items.clone();
            let loading = ac.loading;
            let selected = ac.selected.min(items.len().saturating_sub(1));
            let kind = ac
                .trigger
                .as_ref()
                .map_or(super::catalog::AutocompleteKind::File, |t| t.kind);
            let heading = match kind {
                super::catalog::AutocompleteKind::File => ctext::AUTOCOMPLETE_FILES.get(),
                super::catalog::AutocompleteKind::Slash => ctext::AUTOCOMPLETE_COMMANDS.get(),
            };
            let close_key = key.clone();
            let mut list_box = div()
                .id(SharedString::from(format!(
                    "chat-autocomplete-list-{}",
                    pane.get()
                )))
                .max_h(px(216.))
                .overflow_y_scroll()
                .track_scroll(&ac.scroll)
                .flex()
                .flex_col()
                .gap_0p5();
            for (idx, item) in items.iter().enumerate() {
                let is_selected = idx == selected;
                let accept_key = key.clone();
                let icon_name = match item.source {
                    super::catalog::AutocompleteSource::File => gpui_kit_assets::IconName::File,
                    super::catalog::AutocompleteSource::Skill => {
                        gpui_kit_assets::IconName::Sparkles
                    }
                    super::catalog::AutocompleteSource::Builtin
                    | super::catalog::AutocompleteSource::Command => {
                        gpui_kit_assets::IconName::Terminal
                    }
                };
                let badge = match item.source {
                    super::catalog::AutocompleteSource::Skill => {
                        Some(ctext::AUTOCOMPLETE_SKILL_BADGE.get())
                    }
                    super::catalog::AutocompleteSource::Command => {
                        Some(ctext::AUTOCOMPLETE_CMD_BADGE.get())
                    }
                    super::catalog::AutocompleteSource::Builtin
                    | super::catalog::AutocompleteSource::File => None,
                };
                let mut row = popup_row(
                    palette,
                    SharedString::from(format!("chat-autocomplete-option-{idx}-{}", pane.get())),
                    true,
                )
                .debug_selector(move || format!("chat-autocomplete-option-{idx}"))
                .gap_2()
                .justify_between();
                if is_selected {
                    row = row.bg(palette.muted);
                }
                row = row
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .flex_1()
                            .child(Icon::new(icon_name).size(px(ui::ICON_SM)).text_color(
                                if is_selected {
                                    palette.foreground
                                } else {
                                    palette.muted_foreground
                                },
                            ))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(palette.foreground)
                                    .child(item.label.clone()),
                            )
                            .when(!item.detail.is_empty(), |parent| {
                                parent.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(palette.muted_foreground)
                                        .child(item.detail.clone()),
                                )
                            }),
                    )
                    .children(badge.map(|badge_text| {
                        div()
                            .flex_none()
                            .rounded(ui::RADIUS_SM)
                            .bg(palette.muted)
                            .px_1p5()
                            .py_0p5()
                            .text_size(ui::TEXT_XS)
                            .text_color(palette.muted_foreground)
                            .child(badge_text)
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.accept_chat_autocomplete(&accept_key, Some(idx), true, window, cx);
                    }));
                list_box = list_box.child(row);
            }
            div()
                .id(SharedString::from(format!(
                    "chat-autocomplete-popover-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-autocomplete-popover".into())
                .occlude()
                .absolute()
                .left_0()
                .bottom_full()
                .mb_1p5()
                .w_full()
                .rounded(ui::RADIUS_MD)
                .border_1()
                .border_color(palette.border)
                .bg(palette.popover)
                .shadow_lg()
                .p_1p5()
                .flex()
                .flex_col()
                .gap_1()
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    if let Some(ac) = this.chat.autocomplete.get_mut(&close_key) {
                        ac.dismiss_current();
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .px_2()
                        .py_0p5()
                        .text_size(ui::TEXT_XS)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(palette.muted_foreground)
                        .child(heading),
                )
                .child(if loading {
                    div()
                        .id(SharedString::from(format!(
                            "chat-autocomplete-loading-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-autocomplete-loading".into())
                        .px_2()
                        .py_1p5()
                        .text_size(ui::TEXT_XS)
                        .text_color(palette.muted_foreground)
                        .child(ctext::AUTOCOMPLETE_LOADING.get())
                        .into_any_element()
                } else if items.is_empty() {
                    div()
                        .id(SharedString::from(format!(
                            "chat-autocomplete-empty-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-autocomplete-empty".into())
                        .px_2()
                        .py_1p5()
                        .text_size(ui::TEXT_XS)
                        .text_color(palette.muted_foreground)
                        .child(ctext::AUTOCOMPLETE_EMPTY.get())
                        .into_any_element()
                } else {
                    list_box.into_any_element()
                })
        });

        // Orca NativeChatComposerField: numbered image thumbnails above the prompt, the
        // matching `Image #n` pills, then any other files as chips.
        let image_slots: Vec<(usize, std::path::PathBuf, std::path::PathBuf)> = attachments
            .iter()
            .enumerate()
            .filter(|(_, a)| a.is_image)
            .map(|(idx, a)| (idx, a.path.clone(), a.preview_path().to_path_buf()))
            .collect();
        let image_paths: Vec<std::path::PathBuf> = image_slots
            .iter()
            .map(|(_, path, _)| path.clone())
            .collect();
        let preview_paths: Vec<std::path::PathBuf> = image_slots
            .iter()
            .map(|(_, _, preview)| preview.clone())
            .collect();
        let image_numbers: Vec<usize> = (1..=image_paths.len()).collect();
        let file_attachments: Vec<(usize, DraftAttachment)> = attachments
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.is_image)
            .map(|(idx, a)| (idx, a.clone()))
            .collect();
        let image_thumbs = (!image_slots.is_empty()).then(|| {
            let mut row = div()
                .id(SharedString::from(format!(
                    "chat-attachment-images-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-attachment-images".into())
                .flex()
                .flex_wrap()
                .gap_2()
                .px_1()
                .pt_1p5()
                .mb_2();
            for (n, (idx, _, preview_path)) in image_slots.iter().enumerate() {
                let idx = *idx;
                let loaded = self.chat_thumbnail(preview_path, cx);
                let number = n + 1;
                let remove_key = key.clone();
                let open_paths = preview_paths.clone();
                let open_numbers = image_numbers.clone();
                let label = format!(
                    "{}: {}",
                    ctext::IMAGE_NUMBER.format(&[("number", &number.to_string())]),
                    ctext::IMAGE_VIEW.get()
                );
                let tile = super::images::image_tile(
                    SharedString::from(format!("chat-attachment-image-{idx}-{}", pane.get())),
                    &loaded,
                    super::images::COMPOSER_TILE,
                    Some(number),
                    palette,
                )
                .debug_selector(move || format!("chat-attachment-image-{n}"))
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_chat_image_viewer(
                        open_paths.clone(),
                        open_numbers.clone(),
                        n,
                        window,
                        cx,
                    );
                }))
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "chat-attachment-remove-{idx}-{}",
                            pane.get()
                        )))
                        .debug_selector(move || format!("chat-attachment-remove-{idx}"))
                        .absolute()
                        .top(px(-6.))
                        .right(px(-6.))
                        .size_4()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(palette.border)
                        .bg(palette.background)
                        .text_color(palette.muted_foreground)
                        .cursor_pointer()
                        .hover(|s| s.bg(palette.accent).text_color(palette.accent_foreground))
                        .tooltip(|window, cx| {
                            Tooltip::new(ctext::ATTACH_REMOVE.get()).build(window, cx)
                        })
                        .child(Icon::new(IconName::Close).size(px(12.)))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.remove_chat_attachment(&remove_key, idx, cx);
                        })),
                );
                row = row.child(tile);
            }
            row
        });
        let attachment_chips = (!file_attachments.is_empty()).then(|| {
            div()
                .id(SharedString::from(format!(
                    "chat-attachment-chips-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-attachment-chips".into())
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1p5()
                .px_1p5()
                .pt_0p5()
                .children(file_attachments.into_iter().map(|(idx, attachment)| {
                    let remove_key = key.clone();
                    let label = format!(
                        "{} ({})",
                        attachment.display_name,
                        super::catalog::format_byte_len(attachment.byte_len)
                    );
                    div()
                        .id(SharedString::from(format!(
                            "chat-attachment-chip-{idx}-{}",
                            pane.get()
                        )))
                        .debug_selector(move || format!("chat-attachment-chip-{idx}"))
                        .max_w(px(260.))
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(palette.border)
                        .bg(palette.muted)
                        .px_2()
                        .py_1()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(ui::TEXT_XS)
                        .text_color(palette.foreground)
                        .child(
                            Icon::new(gpui_kit_assets::IconName::File)
                                .size(px(ui::ICON_SM))
                                .text_color(palette.muted_foreground),
                        )
                        .child(div().min_w_0().truncate().child(label))
                        .child(
                            Button::new(SharedString::from(format!(
                                "chat-attachment-remove-{idx}-{}",
                                pane.get()
                            )))
                            .debug_selector(move || format!("chat-attachment-remove-{idx}"))
                            .icon(IconName::Close)
                            .tooltip(ctext::ATTACH_REMOVE.get())
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.remove_chat_attachment(&remove_key, idx, cx);
                                },
                            )),
                        )
                }))
        });

        let submission = if self.chat.prompt_tasks.contains_key(&key)
            || self.paste_queue.has_pending_for(pane)
        {
            super::ComposerSubmissionState::InFlight
        } else if self.chat.prompt_review.contains(&key) {
            super::ComposerSubmissionState::ReviewRequired
        } else {
            super::ComposerSubmissionState::Ready
        };
        let agent_state = self.session_agent_state(&key.0, key.1, cx);
        let interrupt_latched =
            self.is_chat_interrupt_latched(&key, cx) || self.is_chat_stop_cooldown(&key, cx);
        let draft_empty = {
            let state = input_state.read(cx);
            super::strip_image_token_spans(state.value().as_ref(), state.tokens())
                .trim()
                .is_empty()
                && !has_attachments
        };
        let primary_action = super::composer_primary_action(super::ComposerPrimaryInput {
            agent_state,
            submission,
            draft_empty,
            interrupt_latched,
        });
        let queued_prompts: Vec<_> = self
            .chat
            .logs
            .get(&key)
            .map(|log| log.queued_prompts.iter().cloned().collect())
            .unwrap_or_default();
        let is_working_now = agent_state == krill_proto::control::AgentState::Working;
        let focused_queued = self.chat.queued_focused.get(&key).copied();
        // Orca `#47346`: queued rows keep Steer always visible and reveal Edit & Delete on hover
        // or keyboard focus without layout shift.
        let queued_strip = (!queued_prompts.is_empty()).then(|| {
            let q_key = key.clone();
            div()
                .id(SharedString::from(format!(
                    "chat-queued-strip-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-queued-strip".into())
                .w_full()
                .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                .min_w_0()
                .mb_1p5()
                .rounded(ui::RADIUS_LG)
                .border_1()
                .border_color(palette.border)
                .bg(palette.chat_composer_surface())
                .overflow_hidden()
                .flex()
                .flex_col()
                .children(queued_prompts.into_iter().enumerate().map(|(idx, queued)| {
                    let cancel_key = q_key.clone();
                    let edit_key = q_key.clone();
                    let steer_key = q_key.clone();
                    let focus_key = q_key.clone();
                    let q_id = queued.id;
                    let editable = !queued.display_text.trim().is_empty();
                    let row_focused = focused_queued == Some(q_id);
                    let row_group = SharedString::from(format!("chat-queued-row-{q_id}"));
                    let preview: String = queued.display_text.trim().chars().take(96).collect();
                    let label = if preview.is_empty() {
                        let n = queued.attachments.len();
                        let msg = if n == 1 {
                            ctext::QUEUED_ATTACHMENT_ONE
                        } else {
                            ctext::QUEUED_ATTACHMENT_MANY
                        };
                        msg.format(&[("n", &n.to_string())])
                    } else {
                        preview
                    };
                    div()
                        .id(SharedString::from(format!("chat-queued-{q_id}")))
                        .debug_selector(|| "chat-queued-item".into())
                        .group(row_group.clone())
                        .tab_index(0)
                        .focus_visible(|d| d.border_color(palette.ring))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.focus_queued_chat_prompt(&focus_key, q_id, cx);
                            }),
                        )
                        .w_full()
                        .min_w_0()
                        .when(idx > 0, |d| d.border_t_1().border_color(palette.border))
                        .when(row_focused, |d| d.bg(palette.muted))
                        .px_2p5()
                        .py_1p5()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_1p5()
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    Icon::new(gpui_kit_assets::IconName::ListEnd)
                                        .size(px(ui::ICON_MD))
                                        .text_color(muted_fg),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_size(ui::TEXT_SM)
                                        .text_color(palette.foreground)
                                        .child(label),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_0p5()
                                // Edit & Delete reveal on hover/focus without shifting Steer (`#47346`).
                                .when(editable, |d| {
                                    d.child(
                                        div()
                                            .flex_none()
                                            .when(!row_focused, |d| {
                                                d.invisible().group_hover(
                                                    row_group.clone(),
                                                    gpui::Styled::visible,
                                                )
                                            })
                                            .child(
                                                Button::new(SharedString::from(format!(
                                                    "chat-queued-edit-{q_id}"
                                                )))
                                                .debug_selector(|| "chat-queued-edit".into())
                                                .icon(gpui_kit_assets::IconName::Pencil)
                                                .tooltip(ctext::EDIT_QUEUED.get())
                                                .accessibility_label(ctext::EDIT_QUEUED.get())
                                                .xsmall()
                                                .ghost()
                                                .on_mouse_down(
                                                    gpui::MouseButton::Left,
                                                    |_, _, cx| cx.stop_propagation(),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        cx.stop_propagation();
                                                        this.edit_queued_chat_prompt(
                                                            &edit_key, q_id, window, cx,
                                                        );
                                                    },
                                                )),
                                            ),
                                    )
                                })
                                .child(
                                    div()
                                        .flex_none()
                                        .when(!row_focused, |d| {
                                            d.invisible().group_hover(
                                                row_group.clone(),
                                                gpui::Styled::visible,
                                            )
                                        })
                                        .child(
                                            Button::new(SharedString::from(format!(
                                                "chat-queued-cancel-{q_id}"
                                            )))
                                            .debug_selector(|| "chat-queued-cancel".into())
                                            .icon(gpui_kit_assets::IconName::Trash)
                                            .tooltip(ctext::REMOVE_QUEUED.get())
                                            .accessibility_label(ctext::REMOVE_QUEUED.get())
                                            .xsmall()
                                            .ghost()
                                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation();
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.remove_queued_chat_prompt(
                                                    &cancel_key,
                                                    q_id,
                                                    cx,
                                                );
                                            })),
                                        ),
                                )
                                // Steer stays visible on every queued row (`#47346`).
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "chat-queued-steer-{q_id}"
                                    )))
                                    .debug_selector(|| "chat-queued-steer".into())
                                    .icon(if is_working_now {
                                        gpui_kit_assets::IconName::CornerDownRight
                                    } else {
                                        gpui_kit_assets::IconName::Send
                                    })
                                    .label(if is_working_now {
                                        ctext::STEER_HINT.get()
                                    } else {
                                        ctext::QUEUED_SEND_NOW.get()
                                    })
                                    .xsmall()
                                    .ghost()
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.steer_queued_chat_prompt(&steer_key, q_id, window, cx);
                                    })),
                                ),
                        )
                }))
        });

        // Orca's bot chat: icon-only permission picker in toolbar + host/folder/agy footer below.
        let bot = self.chat_bot(&key).map(|bot| {
            let bot_home_dir = Self::bot_folder_on(&key.0, bot);
            let path = bot
                .workspace
                .as_deref()
                .map(std::path::PathBuf::from)
                .or_else(|| bot_home_dir.clone());
            let folder = bot.workspace.as_deref().map_or_else(
                || bot.name.clone(),
                |w| {
                    let wp = std::path::Path::new(w);
                    let base = wp
                        .file_name()
                        .map_or_else(|| w.to_owned(), |n| n.to_string_lossy().into_owned());
                    let is_own_folder = bot_home_dir
                        .as_deref()
                        .is_some_and(|home| krill_proto::path::equivalent(home, wp))
                        || base == bot.slug
                        || base == bot.id.as_str();
                    if is_own_folder {
                        bot.name.clone()
                    } else {
                        base
                    }
                },
            );
            (bot.id.clone(), bot.permission, folder, path)
        });
        let permission_picker_open = self.chat.permission_picker.contains(&key);
        let permission_popover = (permission_picker_open)
            .then(|| {
                let (bot_id, current_perm, _, _) = bot.as_ref()?;
                let close_key = key.clone();
                let full_bot = bot_id.clone();
                let ask_bot = bot_id.clone();
                let full_key = key.clone();
                let ask_key = key.clone();
                let is_full = *current_perm == krill_proto::agent::Permission::Yolo;
                Some(
                    composer_popup(palette)
                        .id(SharedString::from(format!(
                            "chat-permission-popover-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-permission-popover".into())
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                            this.chat.permission_picker.remove(&close_key);
                            cx.notify();
                        }))
                        .child(
                            popup_row(
                                palette,
                                SharedString::from(format!("chat-perm-opt-full-{}", pane.get())),
                                true,
                            )
                            .debug_selector(|| "chat-perm-opt-full".into())
                            .py_1p5()
                            .h_auto()
                            .items_start()
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
                                            .gap_1p5()
                                            .text_color(palette.warning)
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .child(
                                                Icon::default()
                                                    .data(ui::icons::SHIELD_ALERT_SVG)
                                                    .size(px(ui::ICON_MD)),
                                            )
                                            .child(ctext::PERMISSION_FULL_ACCESS.get()),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(muted_fg)
                                            .child(ctext::PERMISSION_FULL_ACCESS_DESC.get()),
                                    ),
                            )
                            .when(is_full, |r| {
                                r.child(Icon::new(IconName::Check).size(px(ui::ICON_SM)))
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.chat.permission_picker.remove(&full_key);
                                    this.set_bot_permission(
                                        full_key.0.clone(),
                                        full_bot.clone(),
                                        krill_proto::agent::Permission::Yolo,
                                        cx,
                                    );
                                },
                            )),
                        )
                        .child(
                            popup_row(
                                palette,
                                SharedString::from(format!("chat-perm-opt-ask-{}", pane.get())),
                                true,
                            )
                            .debug_selector(|| "chat-perm-opt-ask".into())
                            .py_1p5()
                            .h_auto()
                            .items_start()
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
                                            .gap_1p5()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .child(
                                                Icon::default()
                                                    .data(ui::icons::HAND_SVG)
                                                    .size(px(ui::ICON_MD)),
                                            )
                                            .child(ctext::PERMISSION_ASK.get()),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(muted_fg)
                                            .child(ctext::PERMISSION_ASK_DESC.get()),
                                    ),
                            )
                            .when(!is_full, |r| {
                                r.child(Icon::new(IconName::Check).size(px(ui::ICON_SM)))
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.chat.permission_picker.remove(&ask_key);
                                    this.set_bot_permission(
                                        ask_key.0.clone(),
                                        ask_bot.clone(),
                                        krill_proto::agent::Permission::Manual,
                                        cx,
                                    );
                                },
                            )),
                        ),
                )
            })
            .flatten();
        // Orca `NativeChatPermissionModePicker` `appearance="icon"`: icon + chevron only, tooltip names mode.
        let permission_chip = bot.as_ref().map(|(_, permission, _, _)| {
            let full = *permission == krill_proto::agent::Permission::Yolo;
            let (icon, label, desc) = if full {
                (
                    ui::icons::SHIELD_ALERT_SVG,
                    ctext::PERMISSION_FULL_ACCESS.get(),
                    ctext::PERMISSION_FULL_ACCESS_DESC.get(),
                )
            } else {
                (
                    ui::icons::HAND_SVG,
                    ctext::PERMISSION_ASK.get(),
                    ctext::PERMISSION_ASK_DESC.get(),
                )
            };
            let tip = format!("{label} · {desc}");
            let perm_key = key.clone();
            let icon_color = if full { palette.warning } else { muted_fg };
            div()
                .id(SharedString::from(format!(
                    "chat-composer-permission-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-composer-permission".into())
                .aria_label(label)
                .cursor_pointer()
                .h(px(24.))
                .px_1p5()
                .rounded(ui::RADIUS_SM)
                .hover(|s| s.bg(palette.secondary))
                .flex()
                .items_center()
                .gap_0p5()
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.chat.quick_menu.remove(&perm_key);
                    this.chat.model_picker.remove(&perm_key);
                    if !this.chat.permission_picker.remove(&perm_key) {
                        this.chat.permission_picker.insert(perm_key.clone());
                    }
                    cx.notify();
                }))
                .child(
                    Icon::default()
                        .data(icon)
                        .size(px(ui::ICON_MD))
                        .text_color(icon_color),
                )
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(px(10.))
                        .text_color(muted_fg),
                )
        });

        let agy_chip = self.chat_is_agy(&key, cx).then(|| {
            let agy_label = match &self.chat.agy_version {
                Some(ver) => ctext::AGY_PROGRAM_YOUR_VER.format(&[("version", ver.as_str())]),
                None => ctext::AGY_PROGRAM_YOUR.get().to_owned(),
            };
            let agy_key = key.clone();
            Button::new(SharedString::from(format!(
                "chat-composer-agy-auth-{}",
                pane.get()
            )))
            .debug_selector(|| "chat-composer-agy-auth".into())
            .icon(
                Icon::new(gpui_kit_assets::IconName::SquareTerminal)
                    .size(px(ui::ICON_XS))
                    .text_color(muted_fg),
            )
            .label(agy_label)
            .dropdown_caret(true)
            .ghost()
            .xsmall()
            .text_color(muted_fg)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.chat.quick_menu.remove(&agy_key);
                this.chat.model_picker.remove(&agy_key);
                this.chat.permission_picker.remove(&agy_key);
                if !this.chat.agy_auth_picker.remove(&agy_key) {
                    this.chat.agy_auth_picker.insert(agy_key.clone());
                }
                cx.notify();
            }))
        });

        let agy_auth_popover = self.chat.agy_auth_picker.contains(&key).then(|| {
            let close_key = key.clone();
            let settings_key = key.clone();
            let program = match &self.chat.agy_version {
                Some(ver) => ctext::AGY_PROGRAM_YOUR_VER.format(&[("version", ver.as_str())]),
                None => ctext::AGY_PROGRAM_YOUR.get().to_owned(),
            };
            composer_popup_at(palette, true)
                .id(SharedString::from(format!(
                    "chat-agy-auth-popover-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-agy-auth-popover".into())
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    this.chat.agy_auth_picker.remove(&close_key);
                    cx.notify();
                }))
                .child(popup_heading(palette, ctext::AGY_AUTH_TITLE.get()))
                .child(
                    div()
                        .px_2()
                        .py_1p5()
                        .rounded(ui::RADIUS_SM)
                        .bg(palette.muted.opacity(0.45))
                        .flex()
                        .items_start()
                        .gap_2()
                        .child(
                            Icon::new(IconName::Check)
                                .size(px(ui::ICON_SM))
                                .text_color(palette.foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_size(ui::TEXT_SM)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(palette.foreground)
                                        .child(ctext::AGY_AUTH_ITEM.get()),
                                )
                                .child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(muted_fg)
                                        .child(ctext::AGY_AUTH_HINT.get()),
                                ),
                        ),
                )
                .child(div().h_px().w_full().my_0p5().bg(palette.border))
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(ui::TEXT_XS)
                        .text_color(muted_fg)
                        .child(ctext::AGY_AUTH_RUNS_ON.format(&[("program", program.as_str())])),
                )
                .child(
                    popup_row(
                        palette,
                        SharedString::from(format!("chat-agy-auth-settings-{}", pane.get())),
                        true,
                    )
                    .gap_2()
                    .justify_start()
                    .child(
                        Icon::new(gpui_kit_assets::IconName::Settings2)
                            .size(px(ui::ICON_MD))
                            .text_color(muted_fg),
                    )
                    .child(ctext::AGY_AUTH_SETTINGS.get())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.chat.agy_auth_picker.remove(&settings_key);
                        this.open_settings(
                            krill_app_core::settings::AdminTab::Accounts,
                            Some(window),
                            cx,
                        );
                    })),
                )
        });

        let resume_search_popover = self.chat.resume_search.get(&key).cloned().map(|query| {
            let q_lower = query.to_lowercase();
            let close_key = key.clone();
            let sidebar = self.sidebar.read(cx);
            let matches: Vec<(krill_app_core::hosts::HostId, SessionId, String, String)> = sidebar
                .connected_sessions()
                .filter_map(|(h, sid)| {
                    let s = sidebar.session_on(&h, sid)?;
                    let title = s
                        .status
                        .title
                        .as_deref()
                        .filter(|t| !t.is_empty())
                        .unwrap_or(&s.status.agent)
                        .to_owned();
                    let cwd = s
                        .cwd
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    let agent = s.status.agent.as_str();
                    let hit = q_lower.is_empty()
                        || title.to_lowercase().contains(&q_lower)
                        || cwd.to_lowercase().contains(&q_lower)
                        || agent.to_lowercase().contains(&q_lower);
                    hit.then_some((h, sid, title, cwd))
                })
                .take(8)
                .collect();
            composer_popup(palette)
                .id(SharedString::from(format!(
                    "chat-resume-search-popover-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-resume-search-popover".into())
                .w_full()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    this.chat.resume_search.remove(&close_key);
                    cx.notify();
                }))
                .child(popup_heading(palette, ctext::RESUME_SEARCH_TITLE.get()))
                .when(matches.is_empty(), |p| {
                    p.child(
                        div()
                            .px_2()
                            .py_1p5()
                            .text_size(ui::TEXT_XS)
                            .text_color(muted_fg)
                            .child(ctext::AUTOCOMPLETE_EMPTY.get()),
                    )
                })
                .children(matches.into_iter().map(|(h, sid, title, cwd)| {
                    let pick_key = key.clone();
                    popup_row(
                        palette,
                        SharedString::from(format!("chat-resume-hit-{}-{}", sid.0, pane.get())),
                        true,
                    )
                    .debug_selector(move || format!("chat-resume-hit-{}", sid.0))
                    .gap_2()
                    .child(div().min_w_0().truncate().child(title))
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(140.))
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .text_color(muted_fg)
                            .child(cwd),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.chat.resume_search.remove(&pick_key);
                        this.open_worker_session_from_dock(h.clone(), sid, cx);
                    }))
                }))
        });

        let workspace_footer = (bot.is_some() || agy_chip.is_some()).then(|| {
            let host_label = self.chat_host_label(&key.0, cx);
            let folder_el = bot.map(|(_, _, folder, path)| {
                let path_text = path.map(|p| p.display().to_string()).unwrap_or_default();
                // Orca `NativeChatComposerWorkspace.tsx`: plain bot folder shows name only (no "· Folder").
                let folder_label = folder;
                if path_text.is_empty() {
                    div()
                        .id(SharedString::from(format!(
                            "chat-composer-folder-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-composer-folder".into())
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_1p5()
                        .text_size(ui::TEXT_XS)
                        .text_color(muted_fg)
                        .child(
                            Icon::default()
                                .data(ui::icons::FOLDER_SVG)
                                .size(px(ui::ICON_XS)),
                        )
                        .child(div().truncate().child(folder_label))
                        .into_any_element()
                } else {
                    ui::button_with_tooltip(
                        Button::new(SharedString::from(format!(
                            "chat-composer-folder-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-composer-folder".into())
                        .icon(
                            Icon::default()
                                .data(ui::icons::FOLDER_SVG)
                                .size(px(ui::ICON_XS)),
                        )
                        .label(folder_label)
                        .ghost()
                        .xsmall()
                        .text_color(muted_fg)
                        .on_click({
                            let path_text = path_text.clone();
                            move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    path_text.clone(),
                                ));
                            }
                        }),
                        format!("chat-composer-folder-tip-{}", pane.get()),
                        path_text,
                    )
                    .into_any_element()
                }
            });
            div()
                .id(SharedString::from(format!(
                    "chat-composer-workspace-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-composer-workspace".into())
                .w_full()
                .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                .min_w_0()
                .mt_1()
                .h(px(22.))
                .px_3()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(ui::TEXT_XS)
                .text_color(muted_fg)
                .child(
                    div()
                        .flex_none()
                        .max_w(px(160.))
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Icon::default()
                                .data(ui::icons::SERVER_SVG)
                                .size(px(ui::ICON_XS)),
                        )
                        .child(div().truncate().child(host_label)),
                )
                .children(folder_el)
                .children(agy_chip.map(|chip| {
                    div()
                        .ml_auto()
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(chip)
                }))
        });

        // Orca `#42129`: leading `/btw ` or `/side ` shows a muted dismissible `Side chat` pill.
        let side_chat_pill = self
            .chat
            .side_chat_mode
            .get(&key)
            .cloned()
            .map(|prefix| super::side_chat::render_side_chat_pill(pane, &key, &prefix, cx));

        let worker_dock = self.render_chat_worker_dock(pane, &key, cx);
        let inner_card = div()
            .id("desktop-chat-composer")
            .debug_selector(|| "desktop-chat-composer".into())
            .capture_action(cx.listener({
                let bs_key = key.clone();
                let bs_input = input_state.clone();
                move |this, _: &gpui_component::input::Backspace, window, cx| {
                    if bs_input.read(cx).focus_handle(cx).is_focused(window)
                        && !super::Shell::chat_input_is_composing(&bs_input, window, cx)
                        && this.side_chat_backspace(&bs_key, window, cx)
                    {
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }
            }))
            .capture_action(cx.listener({
                let up_key = key.clone();
                let up_input = input_state.clone();
                move |this, _: &gpui_component::input::MoveUp, window, cx| {
                    if let Some(ac) = this.chat.autocomplete.get_mut(&up_key)
                        && ac.is_open()
                    {
                        ac.step_selection(-1);
                        cx.stop_propagation();
                        cx.notify();
                    } else if this.step_chat_model_picker(&up_key, -1) {
                        cx.stop_propagation();
                        cx.notify();
                    } else if up_input.read(cx).focus_handle(cx).is_focused(window)
                        && this.step_chat_prompt_history(&up_key, -1, window, cx)
                    {
                        window.prevent_default();
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
            }))
            .capture_action(cx.listener({
                let down_key = key.clone();
                let down_input = input_state.clone();
                move |this, _: &gpui_component::input::MoveDown, window, cx| {
                    if let Some(ac) = this.chat.autocomplete.get_mut(&down_key)
                        && ac.is_open()
                    {
                        ac.step_selection(1);
                        cx.stop_propagation();
                        cx.notify();
                    } else if this.step_chat_model_picker(&down_key, 1) {
                        cx.stop_propagation();
                        cx.notify();
                    } else if down_input.read(cx).focus_handle(cx).is_focused(window)
                        && this.step_chat_prompt_history(&down_key, 1, window, cx)
                    {
                        window.prevent_default();
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
            }))
            .capture_action(cx.listener({
                let tab_key = key.clone();
                move |this, _: &gpui_component::input::IndentInline, window, cx| {
                    if this.accept_chat_autocomplete(&tab_key, None, false, window, cx) {
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }
            }))
            .capture_action(cx.listener({
                let enter_key = key.clone();
                let enter_input = input_state.clone();
                move |this, action: &gpui_component::input::Enter, window, cx| {
                    if !action.secondary && !action.shift {
                        if super::Shell::chat_input_is_composing(&enter_input, window, cx) {
                            return;
                        }
                        if this.chat_autocomplete_should_submit_directly(
                            &enter_key,
                            &enter_input,
                            cx,
                        ) {
                            if let Some(ac) = this.chat.autocomplete.get_mut(&enter_key) {
                                ac.close();
                                ac.dismissed = None;
                            }
                            return;
                        }
                        if this.accept_chat_autocomplete(&enter_key, None, true, window, cx)
                            || this.accept_chat_model_picker(&enter_key, cx)
                        {
                            window.prevent_default();
                            cx.stop_propagation();
                        }
                    }
                }
            }))
            .capture_action(cx.listener({
                let paste_key = key.clone();
                let paste_input = input_state.clone();
                move |this, _: &gpui_component::input::Paste, window, cx| {
                    if paste_input.read(cx).focus_handle(cx).is_focused(window) {
                        this.intercept_chat_clipboard_paste(&paste_key, window, cx);
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }
            }))
            .capture_action(cx.listener({
                let escape_key = key.clone();
                let escape_input = input_state.clone();
                move |this, _: &gpui_component::input::Escape, window, cx| {
                    if super::Shell::chat_input_is_composing(&escape_input, window, cx) {
                        window.prevent_default();
                        return;
                    }
                    if this.dismiss_open_composer_popover(&escape_key) {
                        window.prevent_default();
                        cx.stop_propagation();
                        cx.notify();
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
                    if super::Shell::chat_input_is_composing(&escape_input, window, cx) {
                        return;
                    }
                    if this.dismiss_open_composer_popover(&escape_key) {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
            }))
            .on_action(cx.listener(
                move |this, action: &gpui_component::input::Enter, window, cx| {
                    if action.secondary && !action.shift {
                        this.steer_chat_prompt(&submit_key, window, cx);
                    } else if action.shift {
                        cx.propagate();
                    } else {
                        this.submit_chat_prompt(&submit_key, window, cx);
                    }
                },
            ))
            .relative()
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener({
                    let sync_key = key.clone();
                    move |_, _, window, cx| {
                        let sync_key = sync_key.clone();
                        let weak = cx.weak_entity();
                        window.defer(cx, move |window, cx| {
                            weak.update(cx, |this, cx| {
                                this.sync_chat_autocomplete(&sync_key, window, cx);
                            })
                            .ok();
                        });
                    }
                }),
            )
            .on_key_up(cx.listener({
                let sync_key = key.clone();
                move |_, e: &gpui::KeyUpEvent, window, cx| {
                    if matches!(
                        e.keystroke.key.as_str(),
                        "left" | "right" | "up" | "down" | "home" | "end"
                    ) {
                        let sync_key = sync_key.clone();
                        let weak = cx.weak_entity();
                        window.defer(cx, move |window, cx| {
                            weak.update(cx, |this, cx| {
                                this.sync_chat_autocomplete(&sync_key, window, cx);
                            })
                            .ok();
                        });
                    }
                }
            }))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener({
                    let focus_input = input_state.clone();
                    move |this, _, window, cx| {
                        if this.layout.focused() != Some(pane) {
                            this.change(false, cx, |l| l.focus(pane));
                            this.files.refocus = false;
                        }
                        if !focus_input.read(cx).focus_handle(cx).is_focused(window) {
                            focus_input.update(cx, |state, cx| state.focus(window, cx));
                        }
                        cx.stop_propagation();
                    }
                }),
            )
            .w_full()
            .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
            .min_w_0()
            .rounded(ui::RADIUS_XL)
            .border_1()
            .border_color(palette.chat_composer_border())
            .bg(palette.chat_composer_surface())
            .p_1p5()
            .flex()
            .flex_col()
            .gap_1()
            .children(quick_menu)
            .children(model_picker)
            .children(permission_popover)
            .children(agy_auth_popover)
            .children(resume_search_popover)
            .children(autocomplete_popover)
            .children(side_chat_pill)
            .children(image_thumbs)
            .children(attachment_chips)
            .child(
                Textarea::new(input_state)
                    .small()
                    .appearance(false)
                    // Orca `NativeChatImageReferencePill`: an image sits in the prompt as a chip
                    // numbered like its thumbnail, "이미지가 제거됨" once the thumbnail is gone.
                    .token({
                        let image_paths = image_paths.clone();
                        move |token, _, cx| {
                            let number = token
                                .token()
                                .id()
                                .strip_prefix(super::IMAGE_TOKEN_PREFIX)
                                .and_then(|path| {
                                    image_paths
                                        .iter()
                                        .position(|p| p.as_os_str() == std::ffi::OsStr::new(path))
                                })
                                .map(|ix| ix + 1);
                            super::image_token_pill(number, token.is_selected(), cx)
                        }
                    })
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
                                        if let Some(ac) = this.chat.autocomplete.get_mut(&key) {
                                            ac.dismiss_current();
                                        }
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
                                krill_app_core::i18n::catalog::settings::TAB_QUICK_COMMANDS.get(),
                            ))
                            .children(permission_chip),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "chat-composer-model-{}",
                                        pane.get()
                                    )))
                                    .debug_selector(|| "chat-composer-model".into())
                                    .h(px(24.))
                                    .px_1p5()
                                    .rounded(ui::RADIUS_SM)
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .when(!is_pending, |d| {
                                        d.cursor_pointer().hover(|s| s.bg(palette.secondary))
                                    })
                                    .when(is_pending, |d| d.opacity(0.5))
                                    .on_mouse_down(gpui::MouseButton::Left, {
                                        let down_key = key.clone();
                                        move |_, _, cx| {
                                            cx.default_global::<DismissedComposerPopovers>()
                                                .model_picker =
                                                model_picker_open.then(|| down_key.clone());
                                        }
                                    })
                                    .when(!is_pending, |d| {
                                        d.on_click(cx.listener({
                                            let key = key.clone();
                                            let input_state = input_state.clone();
                                            move |this, event, window, cx| {
                                                let dismissed = matches!(
                                                    event,
                                                    gpui::ClickEvent::Mouse(_)
                                                ) && cx
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
                                        }))
                                    })
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(palette.foreground)
                                            .child(model_label),
                                    )
                                    .children(effort_label.map(|effort| {
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(muted_fg)
                                            .child(effort)
                                    }))
                                    .child(
                                        Icon::new(IconName::ChevronDown)
                                            .size(px(11.))
                                            .text_color(muted_fg),
                                    ),
                            )
                            .children(context_ring)
                            .children(
                                (matches!(
                                    primary_action,
                                    super::ComposerPrimaryAction::Stop { .. }
                                ) && !draft_empty)
                                    .then(|| {
                                        let queue_key = key.clone();
                                        let steer_key = key.clone();
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                Button::new(SharedString::from(format!(
                                                    "chat-composer-queue-{}",
                                                    pane.get()
                                                )))
                                                .debug_selector(|| "chat-composer-queue".into())
                                                .label(ctext::QUEUE_HINT.get())
                                                .xsmall()
                                                .ghost()
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.submit_chat_prompt(
                                                            &queue_key, window, cx,
                                                        );
                                                    },
                                                )),
                                            )
                                            .child(
                                                Button::new(SharedString::from(format!(
                                                    "chat-composer-steer-{}",
                                                    pane.get()
                                                )))
                                                .debug_selector(|| "chat-composer-steer".into())
                                                .label(ctext::STEER_HINT.get())
                                                .xsmall()
                                                .ghost()
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.steer_chat_prompt(
                                                            &steer_key, window, cx,
                                                        );
                                                    },
                                                )),
                                            )
                                    }),
                            )
                            .child(self.render_chat_voice_control(
                                pane,
                                &key,
                                matches!(primary_action, super::ComposerPrimaryAction::Sending),
                                cx,
                            ))
                            .child({
                                use gpui_component::button::ButtonCustomVariant;
                                match primary_action {
                                    super::ComposerPrimaryAction::Stop { latched } => Button::new(
                                        SharedString::from(format!(
                                            "desktop-chat-stop-{}",
                                            pane.get()
                                        )),
                                    )
                                    .debug_selector(|| "desktop-chat-stop".into())
                                    .icon(
                                        gpui_component::Icon::default()
                                            .data(ui::icons::STOP_SQUARE_SVG)
                                            .size(px(ui::ICON_SM)),
                                    )
                                    .tooltip(if latched {
                                        ctext::STOPPING.get()
                                    } else {
                                        ctext::STOP.get()
                                    })
                                    .custom(
                                        ButtonCustomVariant::new(cx)
                                            .color(palette.foreground)
                                            .foreground(palette.background)
                                            .hover(palette.foreground.opacity(0.88))
                                            .active(palette.foreground.opacity(0.76)),
                                    )
                                    .w(px(28.))
                                    .h(px(28.))
                                    .p_0()
                                    .rounded(px(999.))
                                    .disabled(latched)
                                    .on_click(cx.listener(
                                        move |this, event: &gpui::ClickEvent, _, cx| {
                                            if event.click_count() > 1 {
                                                return;
                                            }
                                            // Orca `#47784`: clicking Stop focuses this chat pane.
                                            if this.layout.focused() != Some(pane) {
                                                this.change(false, cx, |l| l.focus(pane));
                                            }
                                            this.interrupt_chat_turn(&key, cx);
                                        },
                                    )),
                                    super::ComposerPrimaryAction::Sending => {
                                        Button::new(SharedString::from(format!(
                                            "desktop-chat-send-{}",
                                            pane.get()
                                        )))
                                        .debug_selector(|| "desktop-chat-send".into())
                                        .icon(Icon::new(IconName::ArrowUp).size(px(ui::ICON_MD)))
                                        .tooltip(ctext::SEND.get())
                                        .custom(
                                            ButtonCustomVariant::new(cx)
                                                .color(palette.foreground.opacity(0.4))
                                                .foreground(palette.background)
                                                .hover(palette.foreground.opacity(0.4))
                                                .active(palette.foreground.opacity(0.4)),
                                        )
                                        .w(px(28.))
                                        .h(px(28.))
                                        .p_0()
                                        .rounded(px(999.))
                                        .disabled(true)
                                    }
                                    super::ComposerPrimaryAction::Send { enabled } => {
                                        let bg = if enabled {
                                            palette.foreground
                                        } else {
                                            palette.foreground.opacity(0.4)
                                        };
                                        Button::new(SharedString::from(format!(
                                            "desktop-chat-send-{}",
                                            pane.get()
                                        )))
                                        .debug_selector(|| "desktop-chat-send".into())
                                        .icon(Icon::new(IconName::ArrowUp).size(px(ui::ICON_MD)))
                                        .tooltip(ctext::SEND.get())
                                        .custom(
                                            ButtonCustomVariant::new(cx)
                                                .color(bg)
                                                .foreground(palette.background)
                                                .hover(palette.foreground.opacity(0.88))
                                                .active(palette.foreground.opacity(0.76)),
                                        )
                                        .w(px(28.))
                                        .h(px(28.))
                                        .p_0()
                                        .rounded(px(999.))
                                        .disabled(!enabled)
                                        .on_click(cx.listener(
                                            move |this, event: &gpui::ClickEvent, window, cx| {
                                                if event.click_count() > 1 {
                                                    return;
                                                }
                                                // Orca `#47784`: clicking Send focuses this chat pane.
                                                if this.layout.focused() != Some(pane) {
                                                    this.change(false, cx, |l| l.focus(pane));
                                                }
                                                this.submit_chat_prompt(&key, window, cx);
                                            },
                                        ))
                                    }
                                }
                            }),
                    ),
            );

        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .px_3()
            .pt_2()
            .pb_3()
            .bg(palette.chat_canvas())
            .flex()
            .flex_col()
            .items_center()
            .children(worker_dock)
            .children(queued_strip)
            .child(inner_card)
            .children(workspace_footer)
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
            let dismiss_key = key.clone();
            notices.push(
                ui::error_banner(palette, ctext::PROMPT_FAILED.get())
                    .id("chat-prompt-error")
                    .debug_selector(|| "chat-prompt-error".into())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        Button::new("chat-prompt-error-dismiss")
                            .debug_selector(|| "chat-prompt-error-dismiss".into())
                            .icon(IconName::Close)
                            .tooltip(ctext::DISMISS_ERROR.get())
                            .accessibility_label(ctext::DISMISS_ERROR.get())
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.chat.prompt_failed.remove(&dismiss_key);
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        if self.chat.multiline_blocked.contains(key) {
            let dismiss_key = key.clone();
            notices.push(
                ui::error_banner(palette, ctext::MULTILINE_BLOCKED.get())
                    .id("chat-multiline-error")
                    .debug_selector(|| "chat-multiline-error".into())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        Button::new("chat-multiline-error-dismiss")
                            .debug_selector(|| "chat-multiline-error-dismiss".into())
                            .icon(IconName::Close)
                            .tooltip(ctext::DISMISS_ERROR.get())
                            .accessibility_label(ctext::DISMISS_ERROR.get())
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.chat.multiline_blocked.remove(&dismiss_key);
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        if self
            .chat
            .logs
            .get(key)
            .is_some_and(|log| log.meta.rate_limited)
        {
            notices.push(self.render_rate_limit_banner(key, cx));
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

impl Shell {
    /// Orca's `NativeChatUsageLimitRow` (`#47079`): the gauge, "{agent} 사용량 한도에 도달했습니다"
    /// with when it can continue, Switch account, Continue after switch even when reset time is
    /// unknown, auto-continue clock, and more-options dropdown.
    fn render_rate_limit_banner(&self, key: &ChatKey, cx: &mut Context<Self>) -> AnyElement {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let palette = ui::palette(cx);
        let now = self.chat_now_secs();
        let info = self.chat_limit_info(key, cx);
        let ready_after_switch = self.rate_limit_ready_after_switch(key, cx);
        let resumable = info.is_some_and(|i| {
            i.reset_at.saturating_sub(now) <= rate_limit::RESET_HORIZON_SECS
                && rate_limit::reset_clock(i.reset_at).is_some()
        });
        let scheduled = self.chat.auto_continue.get(key).map(|e| e.reset_at);
        let agent = self
            .chat
            .logs
            .get(key)
            .map(|log| log.agent.clone())
            .unwrap_or_default();
        let (title, status) = if ready_after_switch {
            (ctext::LIMIT_SWITCHED_READY.get().to_owned(), String::new())
        } else {
            rate_limit::banner_words(info, &agent, scheduled.is_some())
        };
        let muted = palette.muted_foreground;
        let mut tools = div().flex_none().flex().items_center().gap_0p5();
        if ready_after_switch {
            let continue_key = key.clone();
            tools = tools.child(
                Button::new("chat-rate-limit-continue")
                    .debug_selector(|| "chat-rate-limit-continue".into())
                    .label(ctext::LIMIT_CONTINUE_NOW.get())
                    .primary()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.continue_after_rate_limit(&continue_key, cx);
                    })),
            );
        }
        let alternatives = self
            .sidebar
            .read(cx)
            .session_account_alternatives(&key.0, key.1);
        let switch = Button::new("chat-switch-account")
            .debug_selector(|| "chat-switch-account".into())
            .icon(gpui_kit_assets::IconName::ArrowLeftRight)
            .small()
            .ghost()
            .text_color(muted);
        let switch = if alternatives.is_empty() {
            switch
                .tooltip(ctext::ADD_ACCOUNT.get())
                .accessibility_label(ctext::ADD_ACCOUNT.get())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_settings(
                        krill_app_core::settings::AdminTab::Accounts,
                        Some(window),
                        cx,
                    );
                }))
                .into_any_element()
        } else {
            let shell = cx.weak_entity();
            let key = key.clone();
            switch
                .tooltip(ctext::SWITCH_ACCOUNT.get())
                .accessibility_label(ctext::SWITCH_ACCOUNT.get())
                .dropdown_menu(move |mut menu, _, _| {
                    let now = krill_app_core::accounts::display::now();
                    for (account, label) in &alternatives {
                        let usage = krill_app_core::accounts::display::summary(&account.usage, now);
                        let target = krill_app_core::sidebar::SwitchSessionAccount {
                            host: key.0.clone(),
                            session: key.1,
                            provider: account.provider,
                            account_id: account.id.clone(),
                        };
                        let (shell, key) = (shell.clone(), key.clone());
                        menu = menu.item(
                            PopupMenuItem::new(format!("{label} · {usage}"))
                                .icon(ui::icons::provider_menu_icon(account.provider))
                                .on_click(move |_, _, cx| {
                                    shell
                                        .update(cx, |this, cx| {
                                            this.cancel_auto_continue(&key, cx);
                                            if let Some(log) = this.chat.logs.get_mut(&key) {
                                                log.rate_limit_account_switched = true;
                                            }
                                            this.sidebar.update(cx, |s, cx| {
                                                s.switch_session_account(target.clone(), cx);
                                            });
                                            cx.notify();
                                        })
                                        .ok();
                                }),
                        );
                    }
                    let shell = shell.clone();
                    menu.separator().item(
                        PopupMenuItem::new(ctext::MANAGE_ACCOUNTS.get()).on_click(
                            move |_, window, cx| {
                                shell
                                    .update(cx, |this, cx| {
                                        this.open_settings(
                                            krill_app_core::settings::AdminTab::Accounts,
                                            Some(window),
                                            cx,
                                        );
                                    })
                                    .ok();
                            },
                        ),
                    )
                })
                .into_any_element()
        };
        tools = tools.child(switch);
        // The clock: pressed while it will continue by itself (Orca's auto-continue toggle).
        if let Some(reset_at) = scheduled {
            let time = rate_limit::reset_clock(reset_at).unwrap_or_default();
            let label = ctext::LIMIT_CANCEL_AT.format(&[("time", &time)]);
            tools = tools.child(
                Button::new("chat-auto-continue-cancel")
                    .debug_selector(|| "chat-auto-continue-cancel".into())
                    .icon(gpui_kit_assets::IconName::Clock)
                    .tooltip(label.clone())
                    .accessibility_label(label)
                    .small()
                    .ghost()
                    .selected(true)
                    .on_click(cx.listener({
                        let key = key.clone();
                        move |this, _, _, cx| this.cancel_auto_continue(&key, cx)
                    })),
            );
        } else if resumable && !ready_after_switch {
            tools = tools.child(
                Button::new("chat-auto-continue-arm")
                    .debug_selector(|| "chat-auto-continue-arm".into())
                    .icon(gpui_kit_assets::IconName::Clock)
                    .tooltip(ctext::LIMIT_TURN_ON_AUTO.get())
                    .accessibility_label(ctext::LIMIT_TURN_ON_AUTO.get())
                    .small()
                    .ghost()
                    .text_color(muted)
                    .on_click(cx.listener({
                        let key = key.clone();
                        move |this, _, _, cx| {
                            this.arm_auto_continue(&key, cx);
                        }
                    })),
            );
        }
        let copy_text = format!("{title} {status}").trim().to_owned();
        let more_shell = cx.weak_entity();
        tools = tools.child(
            Button::new("chat-rate-limit-more")
                .debug_selector(|| "chat-rate-limit-more".into())
                .icon(gpui_kit_assets::IconName::Ellipsis)
                .small()
                .ghost()
                .text_color(muted)
                .dropdown_menu(move |menu, _, _| {
                    let copy_text = copy_text.clone();
                    let more_shell = more_shell.clone();
                    menu.item(PopupMenuItem::new(ctext::LIMIT_COPY_ERROR.get()).on_click(
                        move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                copy_text.clone(),
                            ));
                        },
                    ))
                    .item(
                        PopupMenuItem::new(ctext::LIMIT_AUTO_SETTINGS.get()).on_click(
                            move |_, window, cx| {
                                more_shell
                                    .update(cx, |this, cx| {
                                        this.open_settings(
                                            krill_app_core::settings::AdminTab::Accounts,
                                            Some(window),
                                            cx,
                                        );
                                    })
                                    .ok();
                            },
                        ),
                    )
                }),
        );
        div()
            .id("chat-rate-limit-banner")
            .debug_selector(|| "chat-rate-limit-banner".into())
            .w_full()
            .min_w_0()
            .flex_none()
            .rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(palette.border)
            .bg(palette.muted.opacity(0.2))
            .min_h(gpui::px(40.))
            .py_1p5()
            .pl_2p5()
            .pr_1p5()
            .flex()
            .items_start()
            .gap_x_2()
            .text_size(ui::TEXT_LG)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pt_1()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(div().flex_none().mt_px().child(if ready_after_switch {
                        gpui_component::Icon::new(gpui_kit_assets::IconName::Check)
                            .with_size(ui::ICON_LG)
                            .text_color(palette.success)
                    } else {
                        gpui_component::Icon::new(gpui_kit_assets::IconName::Gauge)
                            .with_size(ui::ICON_LG)
                            .text_color(palette.warning)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_wrap()
                            .items_baseline()
                            .gap_x_1p5()
                            .child(
                                div()
                                    .debug_selector(|| "chat-rate-limit-text".into())
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(palette.foreground)
                                    .child(title),
                            )
                            .when(!status.is_empty(), |d| {
                                d.child(
                                    div()
                                        .when(scheduled.is_some(), |d| {
                                            d.debug_selector(|| {
                                                "chat-auto-continue-scheduled".into()
                                            })
                                        })
                                        .text_size(ui::TEXT_SM)
                                        .text_color(muted)
                                        .child(status),
                                )
                            }),
                    ),
            )
            .child(tools)
            .into_any_element()
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
        assert_eq!(effort_bars("very_high"), Some(4));
        assert_eq!(effort_rank("very_high"), effort_rank("xhigh"));
        assert_eq!(effort_bars("unknown"), None);
    }

    #[test]
    fn effort_choices_sort_lowest_to_highest_including_live_meta() {
        let sorted = sort_effort_choices(vec![
            "max".into(),
            "high".into(),
            "low".into(),
            "xhigh".into(),
            "medium".into(),
            "custom".into(),
        ]);
        assert_eq!(
            sorted,
            vec!["low", "medium", "high", "xhigh", "max", "custom"]
        );

        let meta = ComposerMeta {
            model: Some("gpt-5.4".into()),
            current_model_id: Some("gpt-5.4".into()),
            models: vec![ComposerModelItem {
                id: "gpt-5.4".into(),
                label: "GPT-5.4".into(),
                efforts: vec!["xhigh".into(), "high".into(), "medium".into(), "low".into()],
                default_effort: Some("medium".into()),
            }],
            effort: Some("medium".into()),
            efforts: vec![],
            supported: true,
            ..Default::default()
        };
        let (_, resolved_efforts) = resolved_composer_choices("codex", Some(&meta));
        assert_eq!(resolved_efforts, vec!["low", "medium", "high", "xhigh"]);
    }
}
