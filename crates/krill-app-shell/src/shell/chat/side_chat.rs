// Adapted from Orca NativeChatSideChatPill.tsx & native-chat-side-chat-node.ts (MIT).
// Copyright (c) 2026 Lovecast Inc.
//! Leading `/btw ` and `/side ` side-chat pill inside the chat composer.

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::Icon;
use gpui_kit_assets::IconName;
use krill_app_core::i18n::catalog::chat as ctext;
use krill_app_core::ui;

use super::{ChatKey, PaneId, Shell};

/// The exact command prefix that activated the side-chat pill, preserved on submit & Backspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SideChatPrefix {
    /// Exact typed command without trailing space (e.g. `"/btw"` or `"/side"`).
    pub command: String,
}

impl SideChatPrefix {
    #[cfg(test)]
    pub(super) fn btw() -> Self {
        Self {
            command: "/btw".to_owned(),
        }
    }

    /// Reconstructs the wire draft (`"/btw <question>"` or `"/side <question>"`).
    pub(super) fn compose(&self, question: &str) -> String {
        format!("{} {question}", self.command)
    }
}

/// Result of parsing a leading `/btw ` or `/side ` command at the start of a composer draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParsedSideChatDraft<'a> {
    pub prefix: SideChatPrefix,
    pub rest: &'a str,
}

/// True when the agent/chat supports side questions (Orca #42129 excludes terminal-only shells
/// and queued-message edits).
pub(super) fn supports_side_chat(agent: &str, editing_queued: bool) -> bool {
    if editing_queued {
        return false;
    }
    !matches!(
        agent.trim().to_ascii_lowercase().as_str(),
        "terminal" | "shell" | "bash" | "zsh" | "sh" | "fish"
    )
}

/// Parses a literal ASCII space after a leading `/btw` or `/side` (case-insensitive) at offset 0.
pub(super) fn parse_leading_side_chat_command(draft: &str) -> Option<ParsedSideChatDraft<'_>> {
    for len in [5usize, 6usize] {
        let Some(head) = draft.get(..len) else {
            continue;
        };
        if (len == 5 && head.eq_ignore_ascii_case("/btw "))
            || (len == 6 && head.eq_ignore_ascii_case("/side "))
        {
            let command = head.trim_end().to_owned();
            let rest = draft.get(len..).unwrap_or_default();
            return Some(ParsedSideChatDraft {
                prefix: SideChatPrefix { command },
                rest,
            });
        }
    }
    None
}

/// Renders the muted `Side chat` tag pill inside the composer (Orca `NativeChatSideChatPill`).
pub(super) fn render_side_chat_pill(
    pane: PaneId,
    key: &ChatKey,
    prefix: &SideChatPrefix,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let palette = ui::palette(cx);
    let cmd_attr = prefix.command.clone();
    let dismiss_key = key.clone();
    div()
        .id(SharedString::from(format!(
            "chat-side-chat-pill-row-{}",
            pane.get()
        )))
        .w_full()
        .min_w_0()
        .px_2()
        .pt_1()
        .flex()
        .items_center()
        .child(
            div()
                .id(SharedString::from(format!(
                    "chat-side-chat-pill-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-side-chat-pill".into())
                .role(gpui::Role::Button)
                .aria_label(format!(
                    "{} ({cmd_attr}) · {}",
                    ctext::SIDE_CHAT_PILL.get(),
                    ctext::SIDE_CHAT_DISMISS.get()
                ))
                .cursor_pointer()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .rounded(ui::RADIUS_SM)
                .border_1()
                .border_color(palette.border)
                .bg(palette.muted)
                .px_1p5()
                .py_0p5()
                .text_size(ui::TEXT_XS)
                .font_weight(FontWeight::MEDIUM)
                .text_color(palette.muted_foreground)
                .hover(|d| d.bg(palette.accent).text_color(palette.accent_foreground))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.dismiss_side_chat_pill(&dismiss_key, false, window, cx);
                }))
                .child(
                    Icon::new(IconName::GitFork)
                        .size(px(ui::ICON_XS))
                        .text_color(palette.muted_foreground),
                )
                .child(ctext::SIDE_CHAT_PILL.get())
                .child(
                    div()
                        .debug_selector(|| "chat-side-chat-pill-dismiss".into())
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(
                            Icon::new(IconName::Close)
                                .size(px(10.))
                                .text_color(palette.muted_foreground),
                        ),
                ),
        )
        .into_any_element()
}
