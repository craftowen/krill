//! MK-085: isolate the provisional gpui diff API in this module and its parent.

use gpui::{AnyElement, Context, Entity, Subscription, Window, div, prelude::*};
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::diff::{DiffLineRange, DiffSide};
use gpui_component::input::{Textarea, TextareaState};
use std::fmt::Write as _;

use super::{Comparison, Panel};

use crate::i18n::catalog::diff_comments as text;

const MAX_COMMENTS: usize = 32;
const MAX_PROMPT: usize = 48 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendComments(pub String);

#[derive(Debug, Clone)]
struct Comment {
    range: DiffLineRange,
    comparison: Comparison,
    body: String,
}

impl Comment {
    fn location(&self) -> String {
        let side = |s| match s {
            DiffSide::Original => "old",
            DiffSide::Modified => "new",
        };
        format!(
            "{:?} [{}] {}:{}–{}:{}",
            self.range.path().as_ref(),
            match self.comparison {
                Comparison::Worktree => "worktree",
                Comparison::Index => "index",
            },
            side(self.range.side()),
            self.range.start(),
            side(self.range.end_side()),
            self.range.end(),
        )
    }
}

fn prompt(comments: &[Comment]) -> String {
    let mut result = text::PROMPT.get().to_owned();
    for (i, comment) in comments.iter().enumerate() {
        // Debug escaping keeps embedded newlines/control characters as literal text,
        // including paths containing terminal control bytes. One Enter submits one prompt.
        write!(
            result,
            "\n{}. {}\n{:?}",
            i + 1,
            comment.location(),
            comment.body
        )
        .ok();
    }
    result
}

/// One paste and one submit, including on agents without bracketed paste support.
pub fn input_bytes(prompt: &str, modes: krill_proto::term_modes::Modes) -> Vec<u8> {
    let mut bytes = if modes.contains(krill_proto::term_modes::Modes::BRACKETED_PASTE) {
        krill_term::paste::paste(prompt.as_bytes(), modes)
    } else {
        // Formatting newlines are spaces; comment newlines have already been escaped.
        prompt.replace('\n', " ").into_bytes()
    };
    bytes.push(b'\r');
    bytes
}

#[derive(Default)]
pub(super) struct Review {
    comments: Vec<Comment>,
    draft: Option<(DiffLineRange, Comparison, Entity<TextareaState>)>,
    observe: Option<Subscription>,
    status: Option<crate::i18n::Message>,
    language: std::cell::Cell<crate::i18n::Language>,
}

impl Review {
    pub(super) fn has_active_ui(&self) -> bool {
        self.draft.is_some() || !self.comments.is_empty() || self.status.is_some()
    }
}

impl Panel {
    pub(super) fn begin_comment(
        &mut self,
        range: DiffLineRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Preserve an unfinished draft when the user selects another line or file.
        if let Some((_, _, input)) = &self.review.draft {
            input.update(cx, |i, cx| i.focus(window, cx));
            return;
        }
        let input =
            cx.new(|cx| TextareaState::new(window, cx).placeholder(text::PLACEHOLDER.get()));
        self.review.observe = Some(cx.observe(&input, |_, _, cx| cx.notify()));
        input.update(cx, |i, cx| i.focus(window, cx));
        self.review.draft = Some((range, self.comparison, input));
        self.review.status = None;
        cx.notify();
    }

    fn save_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((range, comparison, input)) = &self.review.draft else {
            return;
        };
        let body = input.read(cx).value().trim().to_owned();
        if body.is_empty() {
            return;
        }
        let comment = Comment {
            range: range.clone(),
            comparison: *comparison,
            body,
        };
        let mut comments = self.review.comments.clone();
        comments.push(comment);
        if comments.len() > MAX_COMMENTS || prompt(&comments).len() > MAX_PROMPT {
            self.review.status = Some(text::LIMIT);
        } else {
            self.review.comments = comments;
            self.review.status = None;
            self.cancel_comment(window, cx);
        }
        cx.notify();
    }

    fn cancel_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, _, input)) = self.review.draft.take() {
            input.update(cx, |i, cx| i.focus(window, cx));
            window.blur(cx);
        }
        self.review.observe = None;
        cx.notify();
    }

    pub fn comments_sent(&mut self, accepted: bool, cx: &mut Context<Self>) {
        if self.editing.recovery_pending {
            self.editing.recovery_pending = false;
            if accepted {
                self.editing.failure = None;
                self.editing.note = crate::i18n::catalog::diff_edit::RECOVERY_SENT.get().into();
            } else {
                self.editing.note = crate::i18n::catalog::diff_edit::RECOVERY_FAILED
                    .get()
                    .into();
            }
            cx.notify();
            return;
        }
        if self.checks.fix_pending {
            self.checks.fix_pending = false;
            if accepted {
                self.checks.selected_checks.clear();
                self.checks.selected_comments.clear();
                self.checks.fix_status = Some(crate::i18n::catalog::pull_request::FIX_SENT);
            } else {
                self.checks.fix_status = Some(crate::i18n::catalog::pull_request::FIX_FAILED);
            }
            cx.notify();
            return;
        }
        if accepted {
            self.review.comments.clear();
        }
        self.review.status = if accepted {
            Some(text::QUEUED)
        } else {
            Some(text::FAILED)
        };
        cx.notify();
    }

    pub fn no_agent(&mut self, cx: &mut Context<Self>) {
        if self.editing.recovery_pending {
            self.editing.recovery_pending = false;
            self.editing.note = crate::i18n::catalog::diff_edit::RECOVERY_NO_AGENT
                .get()
                .into();
            cx.notify();
            return;
        }
        if self.checks.fix_pending {
            self.checks.fix_pending = false;
            self.checks.fix_status = Some(crate::i18n::catalog::pull_request::FIX_NO_AGENT);
            cx.notify();
            return;
        }
        self.review.status = Some(text::NO_TARGET);
        cx.notify();
    }

    pub(super) fn render_comments(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.review.language.get() != crate::i18n::language() {
            self.review.language.set(crate::i18n::language());
            if let Some((_, _, input)) = &self.review.draft {
                input.update(cx, |input, cx| {
                    input.set_placeholder(text::PLACEHOLDER.get(), window, cx);
                });
            }
        }
        let p = crate::ui::palette(cx);
        let no_agent = self.review.status == Some(text::NO_TARGET);
        let mut view = div()
            .flex_none()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .border_t_1()
            .border_color(p.sidebar_border)
            .text_size(crate::ui::TEXT_XS)
            .child(text::HINT.get());
        if let Some((range, comparison, input)) = &self.review.draft {
            let location = Comment {
                range: range.clone(),
                comparison: *comparison,
                body: String::new(),
            }
            .location();
            view = view
                .child(location)
                .child(Textarea::new(input).h(crate::ui::SPACE_4 * 4.))
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            Button::new("comment-save")
                                .debug_selector(|| "comment-save".into())
                                .label(text::ADD.get())
                                .primary()
                                .disabled(input.read(cx).value().trim().is_empty())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.save_comment(window, cx);
                                })),
                        )
                        .child(
                            Button::new("comment-cancel")
                                .label(text::CANCEL.get())
                                .ghost()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.cancel_comment(window, cx);
                                })),
                        ),
                );
        }
        let comments = self
            .review
            .comments
            .iter()
            .enumerate()
            .map(|(i, c)| {
                div()
                    .flex()
                    .gap_1()
                    .items_center()
                    .child(div().flex_1().min_w_0().truncate().child(format!(
                        "{}: {}",
                        c.location(),
                        c.body
                    )))
                    .child(
                        Button::new(("comment-remove", i))
                            .label(text::REMOVE.get())
                            .ghost()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if i < this.review.comments.len() {
                                    this.review.comments.remove(i);
                                }
                                cx.notify();
                            })),
                    )
            })
            .collect::<Vec<_>>();
        view.child(
            div()
                .id("review-comments")
                .max_h(crate::ui::SPACE_4 * 6.)
                .overflow_y_scroll()
                .children(comments),
        )
        .child(
            Button::new("comments-send")
                .debug_selector(|| "comments-send".into())
                .label(format!(
                    "{} ({})",
                    text::SEND.get(),
                    self.review.comments.len()
                ))
                .primary()
                .disabled(self.review.comments.is_empty() || self.review.draft.is_some())
                .on_click(cx.listener(|this, _, _, cx| {
                    if !this.review.comments.is_empty() && this.review.draft.is_none() {
                        cx.emit(SendComments(prompt(&this.review.comments)));
                    }
                })),
        )
        .when_some(self.review.status, |d, status| {
            d.child(
                div()
                    .debug_selector(move || {
                        if no_agent {
                            "comments-no-agent".into()
                        } else {
                            "comments-status".into()
                        }
                    })
                    .text_color(p.muted_foreground)
                    .child(status.get()),
            )
        })
        .into_any_element()
    }
}

#[cfg(test)]
mod tests;
