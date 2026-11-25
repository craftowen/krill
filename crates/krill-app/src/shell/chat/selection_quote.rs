// Adapted from Orca NativeChatSelectionQuote.tsx & SelectionAddToChatPopover.tsx (MIT).
// Copyright (c) 2026 Lovecast Inc.
//! Reusable selection-quote formatting, draft appending, and floating `Add to chat` popover.
//!
//! Item activity views (e.g. `bots/home_item.rs`) can reuse [`format_selection_quote`] and
//! [`append_selection_quote`] directly or via [`SelectionQuoteTarget`] with a 1-line hook:
//! ```ignore
//! selection_quote::quote_selection_for_composer(&mut comment_target, selected_text);
//! ```

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, Pixels, Point, SharedString, Styled as _, div, px,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_kit_assets::IconName;
use krill_app::i18n::catalog::chat as ctext;
use krill_app::ui;

use super::{ChatKey, PaneId, Shell};

/// Any composer or comment box that can receive a quoted selection (Orca `NativeChatQuoteTarget`).
pub(crate) trait SelectionQuoteTarget {
    fn accepts_quote(&self) -> bool {
        true
    }
    fn current_draft(&self) -> String;
    fn set_draft_and_focus(&mut self, next: String);
}

/// Active floating `Add to chat` state for a chat pane's text selection.
#[derive(Clone, Debug)]
pub(super) struct ActiveSelectionQuote {
    pub key: ChatKey,
    pub text: String,
    pub _anchor: Option<Point<Pixels>>,
}

/// Formats selected text as a Markdown blockquote (`> line` per line + trailing blank line).
pub(crate) fn format_selection_quote(text: &str) -> Option<String> {
    let stripped = krill_app::pull_request::checks::strip_ansi_codes(text);
    let cleaned = krill_term::select::clean_text(&stripped);
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut quoted = String::new();
    for line in trimmed.lines() {
        if line.is_empty() {
            quoted.push_str(">\n");
        } else {
            quoted.push_str("> ");
            quoted.push_str(line);
            quoted.push('\n');
        }
    }
    quoted.push('\n');
    Some(quoted)
}

/// Appends a formatted selection quote after `existing` without overwriting existing typed text.
pub(crate) fn append_selection_quote(existing: &str, selected: &str) -> Option<String> {
    let quoted = format_selection_quote(selected)?;
    let mut next = String::new();
    if !existing.is_empty() {
        next.push_str(existing);
        if !existing.ends_with("\n\n") {
            if existing.ends_with('\n') {
                next.push('\n');
            } else {
                next.push_str("\n\n");
            }
        }
    }
    next.push_str(&quoted);
    Some(next)
}

/// Reusable helper for chat composers and item-activity comment boxes (Orca #47157).
/// Returns `true` when the formatted quote was appended to `target`.
pub(crate) fn quote_selection_for_composer<T: SelectionQuoteTarget>(
    target: &mut T,
    selected: &str,
) -> bool {
    if !target.accepts_quote() {
        return false;
    }
    let existing = target.current_draft();
    let Some(next) = append_selection_quote(&existing, selected) else {
        return false;
    };
    target.set_draft_and_focus(next);
    true
}

/// Floating `Add to chat` pill rendered over a chat feed when text is selected.
pub(super) fn render_add_to_chat_floating_button(
    pane: PaneId,
    key: ChatKey,
    selected_text: String,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let palette = ui::palette(cx);
    div()
        .id(SharedString::from(format!(
            "chat-selection-quote-bar-{}",
            pane.get()
        )))
        .debug_selector(|| "chat-selection-quote-bar".into())
        .absolute()
        .bottom(px(52.))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .rounded_full()
                .border_1()
                .border_color(palette.border)
                .bg(palette.popover)
                .shadow_lg()
                .p_0p5()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Button::new(SharedString::from(format!(
                        "chat-selection-add-to-chat-{}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-selection-add-to-chat".into())
                    .icon(IconName::Quote)
                    .label(ctext::ADD_TO_CHAT.get())
                    .rounded_full()
                    .small()
                    .ghost()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.quote_text_into_chat_composer(&key, &selected_text, window, cx);
                        gpui_component::TextSelection::clear(window, cx);
                        this.chat.selection_quote = None;
                        this.chat.selection_quote_dismissed = None;
                        cx.notify();
                    })),
                ),
        )
        .into_any_element()
}
