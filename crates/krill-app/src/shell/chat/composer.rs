// Composer pattern adapted from Orca NativeChatComposerField.tsx (MIT).
// Copyright (c) 2026 Lovecast Inc.
use super::*;

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
        let bg = palette.background;
        let border = palette.border;
        let muted_fg = palette.muted_foreground;
        div()
            .id("desktop-chat-composer")
            .debug_selector(|| "desktop-chat-composer".into())
            .on_action(cx.listener(
                move |this, action: &gpui_component::input::Enter, window, cx| {
                    if action.secondary || action.shift {
                        cx.propagate();
                    } else {
                        // Consume the propagated submit action before the platform can
                        // insert a fallback newline into this multiline field.
                        this.submit_chat_prompt(&submit_key, window, cx);
                    }
                },
            ))
            .w_full()
            .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
            .mx_auto()
            .min_w_0()
            .flex_none()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(border)
            .bg(bg)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                Textarea::new(input_state)
                    .small()
                    .accessibility_id("chat-composer-input")
                    .aria_label(ctext::PROMPT_PLACEHOLDER.get()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .text_color(muted_fg)
                            .child(ctext::COMPOSER_HINT.get()),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "desktop-chat-send-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "desktop-chat-send".into())
                        .label(ctext::SEND.get())
                        .small()
                        .primary()
                        .disabled(
                            input_state.read(cx).value().trim().is_empty()
                                || self.chat.prompt_tasks.contains_key(&key)
                                || self.chat.prompt_review.contains(&key),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.submit_chat_prompt(&key, window, cx);
                            },
                        )),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_chat_prompt_notices(
        &self,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut notices = Vec::new();
        if self.chat.poll_failed.contains(key) {
            notices.push(
                ui::error_banner(ui::palette(cx), ctext::POLL_FAILED.get())
                    .id("chat-poll-error")
                    .debug_selector(|| "chat-poll-error".into())
                    .into_any_element(),
            );
        }
        if self.chat.prompt_failed.contains(key) {
            notices.push(
                ui::error_banner(ui::palette(cx), ctext::PROMPT_FAILED.get())
                    .id("chat-prompt-error")
                    .debug_selector(|| "chat-prompt-error".into())
                    .into_any_element(),
            );
        }
        if self.chat.multiline_blocked.contains(key) {
            notices.push(
                ui::error_banner(ui::palette(cx), ctext::MULTILINE_BLOCKED.get())
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
                        ui::error_banner(ui::palette(cx), ctext::PROMPT_REVIEW.get())
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
