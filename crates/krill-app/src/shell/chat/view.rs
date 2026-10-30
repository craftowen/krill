// UI patterns adapted from Orca native-chat (MIT).
// Copyright (c) 2026 Lovecast Inc.
// NativeChatMessageRow.tsx, NativeChatReasoningRow.tsx, NativeChatComposerField.tsx,
// native-chat-autoscroll.ts; rendered with Krill design tokens and GPUI.

use super::*;
use gpui::StatefulInteractiveElement as _;
use gpui::prelude::FluentBuilder as _;
use gpui_component::IconName;
use std::hash::{Hash as _, Hasher as _};

impl Shell {
    fn render_chat_item(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        abs_idx: usize,
        item: &ChatItem,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (self.layout.host(pane).clone(), session);
        let palette = ui::palette(cx);
        let border = palette.border;
        let fg = palette.foreground;
        let muted_fg = palette.muted_foreground;
        let is_user = item.kind == ChatItemKind::User;
        let is_request = matches!(item.kind, ChatItemKind::Question | ChatItemKind::Permission);
        let disclosure = matches!(
            item.kind,
            ChatItemKind::Reasoning | ChatItemKind::ToolCall | ChatItemKind::ToolResult
        );
        let expanded = self
            .chat
            .logs
            .get(&key)
            .is_some_and(|log| log.expanded.contains(&abs_idx));
        let role_label = match item.kind {
            ChatItemKind::User => ctext::YOU.get(),
            ChatItemKind::Assistant => ctext::ASSISTANT.get(),
            ChatItemKind::Reasoning => ctext::REASONING.get(),
            ChatItemKind::ToolCall => ctext::TOOL_CALL.get(),
            ChatItemKind::ToolResult => ctext::TOOL_RESULT.get(),
            ChatItemKind::Question => ctext::QUESTION.get(),
            ChatItemKind::Permission => ctext::PERMISSION.get(),
        };
        let card_border = if is_request {
            palette.agent_question
        } else {
            border
        };
        let item_selector = format!("chat-item-{}-{abs_idx}", pane.get());
        let debug_selector = item_selector.clone();
        let mut card = div()
            .id(SharedString::from(item_selector))
            .debug_selector(move || debug_selector.clone())
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .p_3()
            .when(is_user || is_request, |card| {
                card.rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(card_border)
                    .bg(if is_user { palette.muted } else { palette.card })
            })
            .flex()
            .flex_col()
            .gap_1p5()
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
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(if item.title.is_empty() {
                                SharedString::from(role_label)
                            } else {
                                SharedString::from(format!("{role_label} • {}", item.title))
                            }),
                    )
                    .children((!item.file_path.is_empty()).then(|| {
                        let raw_path = std::path::PathBuf::from(&item.file_path);
                        let path = if raw_path.is_relative() {
                            self.pane_cwd(pane, cx)
                                .map_or_else(|| raw_path.clone(), |cwd| cwd.join(&raw_path))
                        } else {
                            raw_path
                        };
                        Button::new(SharedString::from(format!(
                            "chat-open-file-{}-{abs_idx}",
                            pane.get()
                        )))
                        .label(
                            std::path::Path::new(&item.file_path)
                                .file_name()
                                .map_or_else(
                                    || item.file_path.clone(),
                                    |name| name.to_string_lossy().into_owned(),
                                ),
                        )
                        .max_w(gpui::relative(0.35))
                        .overflow_hidden()
                        .tooltip(item.file_path.clone())
                        .accessibility_label(item.file_path.clone())
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_file_at(path.clone(), 1, 1, cx);
                        }))
                    }))
                    .children((!item.text.trim().is_empty()).then(|| {
                        let copy_text = krill_term::select::clean_text(&item.text);
                        Button::new(SharedString::from(format!(
                            "chat-copy-{}-{abs_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-copy-item".into())
                        .label(ctext::COPY.get())
                        .ghost()
                        .xsmall()
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                copy_text.clone(),
                            ));
                        })
                    })),
            );

        if disclosure {
            let summary = item
                .text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .trim()
                .to_owned();
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!(
                            "chat-disclosure-{}-{abs_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-disclosure".into())
                        .icon(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .label(ctext::DETAILS.get())
                        .accessibility_label(if expanded {
                            ctext::HIDE_DETAILS.get()
                        } else {
                            ctext::SHOW_DETAILS.get()
                        })
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(log) = this
                                .chat
                                .logs
                                .get_mut(&(this.layout.host(pane).clone(), session))
                            {
                                if !log.expanded.remove(&abs_idx) {
                                    log.expanded.insert(abs_idx);
                                }
                                log.revision = log.revision.wrapping_add(1);
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_SM)
                            .text_color(muted_fg)
                            .child(summary),
                    ),
            );
        }
        if !disclosure || expanded {
            let mut body = div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_3()
                .id(SharedString::from(format!(
                    "chat-body-{}-{abs_idx}",
                    pane.get()
                )))
                .debug_selector(|| "chat-message-body".into());
            for (code_idx, segment) in parse_chat_segments(&item.text).into_iter().enumerate() {
                body = match segment {
                    ChatBodySegment::Text(text) => body.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_size(ui::TEXT_MD)
                            .text_color(if disclosure { muted_fg } else { fg })
                            .child(if item.kind == ChatItemKind::Assistant {
                                gpui_component::text::TextView::markdown(
                                    {
                                        let mut hash = std::hash::DefaultHasher::new();
                                        text.hash(&mut hash);
                                        SharedString::from(format!(
                                            "chat-markdown-{}-{}-{abs_idx}-{code_idx}-{:x}",
                                            pane.get(),
                                            self.chat.logs.get(&key).map_or(0, |log| log.epoch),
                                            hash.finish()
                                        ))
                                    },
                                    text,
                                )
                                .scrollable(false)
                                .into_any_element()
                            } else {
                                div().child(text).into_any_element()
                            }),
                    ),
                    ChatBodySegment::Code { lang, code } => {
                        let code_for_copy = code.clone();
                        body.child(
                            div()
                                .w_full()
                                .min_w_0()
                                .rounded(ui::RADIUS_SM)
                                .border_1()
                                .border_color(border)
                                .bg(palette.muted)
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .px_2()
                                        .py_1()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(ui::TEXT_XS)
                                                .text_color(muted_fg)
                                                .child(if lang.is_empty() {
                                                    ctext::CODE.get().to_owned()
                                                } else {
                                                    lang
                                                }),
                                        )
                                        .child(
                                            Button::new(SharedString::from(format!(
                                                "chat-copy-code-{}-{abs_idx}-{code_idx}",
                                                pane.get()
                                            )))
                                            .debug_selector(|| "chat-copy-code".into())
                                            .label(ctext::COPY_CODE.get())
                                            .ghost()
                                            .xsmall()
                                            .on_click(move |_, _, cx| {
                                                cx.write_to_clipboard(
                                                    gpui::ClipboardItem::new_string(
                                                        code_for_copy.clone(),
                                                    ),
                                                );
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "chat-code-{}-{abs_idx}-{code_idx}",
                                            pane.get()
                                        )))
                                        .debug_selector(|| "chat-code-content".into())
                                        .w_full()
                                        .min_w_0()
                                        .overflow_x_scroll()
                                        .p_2()
                                        .font_family(ui::FONT_MONO)
                                        .text_size(ui::TEXT_SM)
                                        .whitespace_nowrap()
                                        .text_color(fg)
                                        .child(code),
                                ),
                        )
                    }
                };
            }
            card = card.child(body);
        }

        let request = self.chat_request_identity(pane, session, abs_idx, item);
        let answer_key = request.key.clone();
        let submission = self
            .chat
            .logs
            .get(&key)
            .and_then(|log| log.submissions.get(&answer_key));
        let current = self.chat_request_is_current(&request, cx);
        let disabled = !current
            || submission.is_some_and(Submission::blocks_input)
            || self.chat.prompt_tasks.contains_key(&key);
        if matches!(item.kind, ChatItemKind::Question | ChatItemKind::Permission) {
            let status = if !current
                && !matches!(
                    submission,
                    Some(Submission::Confirmed | Submission::ReviewRequired)
                ) {
                ctext::REQUEST_UNVERIFIED.get()
            } else {
                submission.map_or(ctext::REQUEST_READY.get(), Submission::label)
            };
            card = card.child(
                div()
                    .id(SharedString::from(format!(
                        "chat-request-status-{}-{abs_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-request-status".into())
                    .text_size(ui::TEXT_XS)
                    .text_color(muted_fg)
                    .child(status),
            );
        }
        if item.kind == ChatItemKind::Question && !item.options.is_empty() {
            let multi_select = item.multi_select;
            let selected_set = self
                .chat
                .logs
                .get(&key)
                .and_then(|l| l.multi_selected.get(&answer_key))
                .cloned()
                .unwrap_or_default();
            let mut opts_row = div().w_full().min_w_0().flex().flex_col().gap_1p5();
            for (opt_idx, opt) in item.options.iter().enumerate() {
                let request_clone = request.clone();
                let is_checked = selected_set.contains(&opt_idx);
                let prefix = if multi_select {
                    if is_checked { "[✓] " } else { "[ ] " }
                } else if is_checked {
                    "✓ "
                } else {
                    ""
                };
                let label = if opt.description.is_empty() {
                    format!("{prefix}{}", opt.label)
                } else {
                    format!("{prefix}{} — {}", opt.label, opt.description)
                };
                opts_row = opts_row.child(
                    Button::new(SharedString::from(format!(
                        "chat-q-{}-{abs_idx}-{opt_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-question-option".into())
                    .w_full()
                    .max_w_full()
                    .overflow_hidden()
                    .tooltip(label.clone())
                    .accessibility_label(label)
                    .small()
                    .ghost()
                    .h_auto()
                    .py_2()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .items_start()
                            .gap_2()
                            .whitespace_normal()
                            .text_align(gpui::TextAlign::Left)
                            .child(
                                div()
                                    .w(ui::SPACE_4)
                                    .flex_none()
                                    .text_size(ui::TEXT_SM)
                                    .child(if multi_select {
                                        if is_checked {
                                            "✓".to_owned()
                                        } else {
                                            "□".to_owned()
                                        }
                                    } else {
                                        (opt_idx + 1).to_string()
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .items_start()
                                    .gap_1()
                                    .child(
                                        div()
                                            .w_full()
                                            .text_size(ui::TEXT_SM)
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .child(opt.label.clone()),
                                    )
                                    .children((!opt.description.is_empty()).then(|| {
                                        div()
                                            .w_full()
                                            .id(SharedString::from(format!(
                                                "chat-option-description-{}-{abs_idx}-{opt_idx}",
                                                pane.get()
                                            )))
                                            .debug_selector(|| "chat-option-description".into())
                                            .text_size(ui::TEXT_XS)
                                            .text_color(muted_fg)
                                            .child(opt.description.clone())
                                    })),
                            ),
                    )
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if multi_select {
                            this.toggle_chat_multi_option(&request_clone, opt_idx, cx);
                        } else {
                            this.answer_chat_question(
                                request_clone.clone(),
                                vec![opt_idx],
                                false,
                                cx,
                            );
                        }
                    })),
                );
            }
            if multi_select {
                let request_submit = request.clone();
                let picks: Vec<usize> = selected_set.into_iter().collect();
                opts_row = opts_row.child(
                    Button::new(SharedString::from(format!(
                        "chat-q-submit-{}-{abs_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-question-submit".into())
                    .label(ctext::SUBMIT.format(&[("n", &picks.len().to_string())]))
                    .small()
                    .primary()
                    .disabled(disabled || picks.is_empty())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.answer_chat_question(request_submit.clone(), picks.clone(), true, cx);
                    })),
                );
            }
            card = card.child(opts_row);
        } else if item.kind == ChatItemKind::Permission {
            let yes_request = request.clone();
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!(
                            "chat-perm-yes-{}-{abs_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-permission-approve".into())
                        .disabled(disabled)
                        .label(ctext::APPROVE.get())
                        .small()
                        .primary()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.approve_chat_permission(yes_request.clone(), true, cx);
                        })),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "chat-perm-no-{}-{abs_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-permission-deny".into())
                        .disabled(disabled)
                        .label(ctext::DENY.get())
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.approve_chat_permission(request.clone(), false, cx);
                        })),
                    ),
            );
        }

        div()
            .w_full()
            .min_w_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                    .pb_4()
                    .flex()
                    .when(is_user, gpui::Styled::justify_end)
                    .child(
                        div()
                            .min_w_0()
                            .w_full()
                            .when(is_user, |bubble| bubble.max_w(gpui::relative(0.85)))
                            .child(card),
                    ),
            )
            .into_any_element()
    }

    pub(in crate::shell) fn render_pane_chat_overlay(
        &mut self,
        pane: super::super::PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.prune_chat_lists();
        if self.layout.file(pane).is_some() {
            return None;
        }
        let session = self.layout.session(pane)?;
        let key = (self.layout.host(pane).clone(), session);
        if self.layout.focused() == Some(pane)
            && matches!(&self.chat.focus_pending, Some(ChatFocus::Terminal(id)) if *id == key)
        {
            window.focus(&self.focus, cx);
            self.chat.focus_pending = None;
        }
        if !self.is_pane_chat_view(pane, cx) {
            return None;
        }
        self.ensure_chat_poll_timer(cx);
        if !self.chat.logs.contains_key(&key)
            && !self.chat.poll_failed.contains(&key)
            && self.target_for_host(self.layout.host(pane)).is_some()
        {
            self.poll_session_chat(pane, session, cx);
        }
        let input_state = self.ensure_chat_input(key.clone(), window, cx);
        if self.layout.focused() == Some(pane)
            && matches!(&self.chat.focus_pending, Some(ChatFocus::Composer(id)) if *id == key)
        {
            input_state.update(cx, |state, cx| state.focus(window, cx));
            self.chat.focus_pending = None;
        }
        let palette = ui::palette(cx);
        let bg = palette.background;
        let panel_bg = palette.muted;
        let border = palette.border;
        let muted_fg = palette.muted_foreground;

        let log = self.chat.logs.get(&key);
        let agent_label = log
            .and_then(|l| (!l.agent.is_empty()).then_some(l.agent.as_str()))
            .unwrap_or("agent")
            .to_owned();
        let bound = log.is_some_and(|l| l.bound);
        let base_index = log.map_or(0, |l| l.base_index);
        let revision = log.map_or(0, |l| l.revision);
        let epoch = log.map_or(0, |l| l.epoch);
        let items = log.map_or_else(Vec::new, |l| l.items.clone());

        let mut feed = div()
            .id(SharedString::from(format!(
                "desktop-chat-feed-{}",
                pane.get()
            )))
            .debug_selector(|| "desktop-chat-feed".into())
            .relative()
            .w_full()
            .min_w_0()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .p_3()
            .flex()
            .flex_col()
            .gap_2();

        if items.is_empty() {
            feed = feed.child(
                div()
                    .id("chat-empty")
                    .debug_selector(|| "chat-empty".into())
                    .flex_1()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .items_center()
                    .gap_3()
                    .p_4()
                    .child(ui::icons::agent_brand_icon(
                        &agent_label,
                        ui::ICON_XL,
                        palette.foreground,
                    ))
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(palette.foreground)
                            .child(ctext::EMPTY_TITLE.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(muted_fg)
                            .child(if bound {
                                ctext::EMPTY_BOUND.get()
                            } else {
                                ctext::EMPTY_UNBOUND.get()
                            }),
                    ),
            );
        } else {
            let count = items.len();
            let host = self.layout.host(pane).clone();
            let owner = cx.weak_entity();
            let list = self
                .chat
                .lists
                .entry((pane, session))
                .or_insert_with(|| ChatList {
                    host,
                    base_index,
                    count,
                    revision,
                    epoch,
                    state: {
                        let state =
                            gpui::ListState::new(count, gpui::ListAlignment::Bottom, ui::SPACE_4);
                        state.set_follow_mode(gpui::FollowMode::Tail);
                        state.set_scroll_handler(move |_, _, cx| {
                            owner.update(cx, |_, cx| cx.notify()).ok();
                        });
                        state
                    },
                });
            let list_state = list.sync(base_index, count, revision, epoch);
            let owner = cx.weak_entity();
            feed = feed.child(
                gpui::list(list_state.clone(), move |index, _, cx| {
                    let Some(item) = items.get(index) else {
                        return div().into_any_element();
                    };
                    owner
                        .update(cx, |this, cx| {
                            this.render_chat_item(pane, session, base_index + index, item, cx)
                        })
                        .unwrap_or_else(|_| div().into_any_element())
                })
                .size_full(),
            );
            // Floating affordance occupies no feed space, so it cannot move the composer.
            if !list_state.is_following_tail() {
                feed = feed.child(
                    div().absolute().bottom_3().right_3().child(
                        Button::new("chat-jump-latest")
                            .debug_selector(|| "chat-jump-latest".into())
                            .label(ctext::JUMP_LATEST.get())
                            .icon(IconName::ArrowDown)
                            .small()
                            .outline()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                list_state.set_follow_mode(gpui::FollowMode::Tail);
                                cx.notify();
                            })),
                    ),
                );
            }
        }

        let composer = self.render_chat_composer(pane, key.clone(), &input_state, cx);

        let live_line = self.pane_live_status_line(pane);
        let strip_label = match live_line {
            Some(line) => format!("{agent_label} · {line}"),
            None => agent_label.clone(),
        };

        let strip = div()
            .flex_none()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .px_3()
            .py_1p5()
            .border_t_1()
            .border_color(border)
            .bg(panel_bg)
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(ui::icons::agent_brand_icon(
                &agent_label,
                ui::ICON_XS,
                palette.foreground,
            ))
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(muted_fg)
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(strip_label),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .flex_none()
                    .child(
                        Button::new(SharedString::from(format!(
                            "desktop-chat-refresh-{}",
                            pane.get()
                        )))
                        .label(ctext::REFRESH.get())
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.poll_session_chat(pane, session, cx);
                        })),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "desktop-chat-switch-term-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-switch-terminal".into())
                        .label(ctext::TERM.get())
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.set_pane_chat_view(pane, false, cx);
                                this.chat.focus_pending = None;
                                window.focus(&this.focus, cx);
                                cx.notify();
                            },
                        )),
                    ),
            );

        Some(
            div()
                .id(SharedString::from(format!(
                    "desktop-chat-overlay-{}",
                    pane.get()
                )))
                .debug_selector(|| "desktop-chat-overlay".into())
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .flex()
                .flex_col()
                .bg(bg)
                .child(feed)
                .children(self.render_chat_prompt_notices(&key, cx))
                .child(composer)
                .child(strip)
                .into_any_element(),
        )
    }
}
