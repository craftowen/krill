// UI patterns adapted from Orca native-chat (MIT).
// Copyright (c) 2026 Lovecast Inc.
// NativeChatMessageRow.tsx, NativeChatReasoningRow.tsx, NativeChatComposerField.tsx,
// native-chat-autoscroll.ts; rendered with Krill design tokens and GPUI.

use super::work_run;
use super::*;
use gpui::StatefulInteractiveElement as _;
use gpui::prelude::FluentBuilder as _;
use gpui_component::IconName;
use std::hash::{Hash as _, Hasher as _};

struct SegmentStyle<'a> {
    pane: super::super::PaneId,
    abs_idx: usize,
    markdown: bool,
    text_size: gpui::Rems,
    text_color: gpui::Hsla,
    epoch: u64,
    palette: &'a ui::Palette,
}

fn append_chat_segments<T: gpui::ParentElement>(
    mut container: T,
    text: &str,
    style: &SegmentStyle<'_>,
) -> T {
    let pane = style.pane;
    let abs_idx = style.abs_idx;
    let epoch = style.epoch;
    let border = style.palette.border;
    let fg = style.palette.foreground;
    let muted_fg = style.palette.muted_foreground;
    for (code_idx, segment) in parse_chat_segments(text).into_iter().enumerate() {
        container = match segment {
            ChatBodySegment::Text(text) => container.child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_size(style.text_size)
                    .text_color(style.text_color)
                    .child(if style.markdown {
                        gpui_component::text::TextView::markdown(
                            {
                                let mut hash = std::hash::DefaultHasher::new();
                                text.hash(&mut hash);
                                SharedString::from(format!(
                                    "chat-markdown-{}-{epoch}-{abs_idx}-{code_idx}-{:x}",
                                    pane.get(),
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
                container.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(border)
                        .bg(style.palette.muted)
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
                                    .on_click(
                                        move |_, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                                code_for_copy.clone(),
                                            ));
                                        },
                                    ),
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
    container
}

impl Shell {
    fn render_display_row(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        row: &work_run::DisplayRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            work_run::DisplayRow::Item { abs_idx, item } => {
                self.render_chat_item(pane, session, *abs_idx, item, cx)
            }
            work_run::DisplayRow::WorkRun(run) => self.render_work_run(pane, session, run, cx),
        }
    }

    fn render_work_run(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        run: &work_run::WorkRun,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (self.layout.host(pane).clone(), session);
        let palette = ui::palette(cx);
        let fg = palette.foreground;
        let muted_fg = palette.muted_foreground;
        let run_span = run.span;
        let run_expanded = self
            .chat
            .logs
            .get(&key)
            .is_some_and(|log| log.is_run_expanded(run_span));
        let summary = run.summary_label();
        let header_text = if run.latest_detail.is_empty() {
            summary
        } else {
            format!("{summary} · {}", run.latest_detail)
        };
        let item_selector = format!("chat-item-{}-{}", pane.get(), run_span.start());
        let debug_selector = item_selector.clone();
        let mut container = div()
            .id(SharedString::from(item_selector))
            .debug_selector(move || debug_selector.clone())
            .rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(palette.border)
            .bg(palette.card)
            .overflow_hidden()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id(SharedString::from(format!(
                        "chat-work-run-{}-{}",
                        pane.get(),
                        run_span.start()
                    )))
                    .debug_selector(|| "chat-work-run-toggle".into())
                    .role(gpui::Role::Button)
                    .aria_label(if run_expanded {
                        ctext::RUN_HIDE.get()
                    } else {
                        ctext::RUN_SHOW.get()
                    })
                    .aria_expanded(run_expanded)
                    .tab_index(0)
                    .cursor_pointer()
                    .hover(|s| s.bg(palette.accent))
                    .focus_visible(|s| s.bg(palette.accent))
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(log) = this
                            .chat
                            .logs
                            .get_mut(&(this.layout.host(pane).clone(), session))
                        {
                            log.toggle_run(run_span);
                        }
                        cx.notify();
                    }))
                    .child(
                        div().flex_none().flex().items_center().child(
                            gpui_component::Icon::new(if run_expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .with_size(ui::ICON_XS)
                            .text_color(muted_fg),
                        ),
                    )
                    .children((run.counts.errors > 0).then(|| {
                        ui::badge(palette, ui::BadgeTone::Destructive, ctext::RUN_ERROR.get())
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(fg)
                            .child(header_text),
                    ),
            );
        if run_expanded {
            for step in &run.steps {
                container = container
                    .child(div().h_px().w_full().bg(palette.border))
                    .child(self.render_work_step(pane, session, step, cx));
            }
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
                    .child(container),
            )
            .into_any_element()
    }

    fn render_work_step(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        step: &work_run::WorkStep,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (self.layout.host(pane).clone(), session);
        let palette = ui::palette(cx);
        let fg = palette.foreground;
        let muted_fg = palette.muted_foreground;
        let step_abs_idx = step.primary_abs_idx();
        let step_expanded = self
            .chat
            .logs
            .get(&key)
            .is_some_and(|log| log.is_step_expanded(step_abs_idx));
        let epoch = self.chat.logs.get(&key).map_or(0, |log| log.epoch);
        let is_copied = self
            .chat
            .copied_item
            .as_ref()
            .is_some_and(|(copied_key, idx, at)| {
                *copied_key == key && *idx == step_abs_idx && at.elapsed() < Duration::from_secs(2)
            });
        let (badge_tone, badge_text) = step.badge();
        let headline = step.headline();
        let file_path = step.file_path();
        let copy_text = step.copy_text();
        let sections = step.body_sections();

        let toggle = div()
            .id(SharedString::from(format!(
                "chat-disclosure-{}-{step_abs_idx}",
                pane.get()
            )))
            .debug_selector(|| "chat-disclosure".into())
            .role(gpui::Role::Button)
            .aria_label(if step_expanded {
                ctext::HIDE_DETAILS.get()
            } else {
                ctext::SHOW_DETAILS.get()
            })
            .aria_expanded(step_expanded)
            .tab_index(0)
            .cursor_pointer()
            .hover(|s| s.bg(palette.accent))
            .focus_visible(|s| s.bg(palette.accent))
            .flex_1()
            .min_w_0()
            .rounded(ui::RADIUS_XS)
            .px_1()
            .py_0p5()
            .flex()
            .items_center()
            .gap_1p5()
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(log) = this
                    .chat
                    .logs
                    .get_mut(&(this.layout.host(pane).clone(), session))
                {
                    log.toggle_step(step_abs_idx);
                }
                cx.notify();
            }))
            .child(
                div().flex_none().flex().items_center().child(
                    gpui_component::Icon::new(if step_expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .with_size(ui::ICON_XS)
                    .text_color(muted_fg),
                ),
            )
            .child(ui::badge(palette, badge_tone, badge_text))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui::TEXT_XS)
                    .text_color(fg)
                    .child(headline),
            );

        let header = div()
            .w_full()
            .min_w_0()
            .px_3()
            .py_1p5()
            .flex()
            .items_center()
            .gap_2()
            .child(toggle)
            .children((!file_path.is_empty()).then(|| {
                let file_path_owned = file_path.to_owned();
                let raw_path = std::path::PathBuf::from(&file_path_owned);
                let path = if raw_path.is_relative() {
                    self.pane_cwd(pane, cx)
                        .map_or_else(|| raw_path.clone(), |cwd| cwd.join(&raw_path))
                } else {
                    raw_path
                };
                let file_label = std::path::Path::new(&file_path_owned)
                    .file_name()
                    .map_or_else(
                        || file_path_owned.clone(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                div()
                    .flex_none()
                    .max_w(gpui::rems(5.5))
                    .overflow_hidden()
                    .child(
                        Button::new(SharedString::from(format!(
                            "chat-open-file-{}-{step_abs_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-open-file".into())
                        .label(file_label)
                        .max_w_full()
                        .overflow_hidden()
                        .tooltip(file_path_owned.clone())
                        .accessibility_label(file_path_owned)
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_file_at(path.clone(), 1, 1, cx);
                        })),
                    )
            }))
            .children((!copy_text.is_empty()).then(|| {
                let copy_key = key.clone();
                div().flex_none().child(
                    Button::new(SharedString::from(format!(
                        "chat-copy-{}-{step_abs_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-copy-item".into())
                    .icon(if is_copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .tooltip(if is_copied {
                        ctext::COPIED.get()
                    } else {
                        ctext::COPY.get()
                    })
                    .accessibility_label(if is_copied {
                        ctext::COPIED.get()
                    } else {
                        ctext::COPY.get()
                    })
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                        this.chat.copied_item =
                            Some((copy_key.clone(), step_abs_idx, std::time::Instant::now()));
                        cx.notify();
                    })),
                )
            }));

        let mut step_el = div().w_full().min_w_0().flex().flex_col().child(header);
        if step_expanded && !sections.is_empty() {
            let mut body = div()
                .id(SharedString::from(format!(
                    "chat-body-{}-{step_abs_idx}",
                    pane.get()
                )))
                .debug_selector(|| "chat-message-body".into())
                .w_full()
                .min_w_0()
                .px_3()
                .pb_2()
                .pt_1()
                .flex()
                .flex_col()
                .gap_2();
            for section in sections {
                let mut section_el = div().w_full().min_w_0().flex().flex_col().gap_1();
                if let Some(label) = section.label {
                    section_el = section_el.child(
                        div()
                            .text_size(ui::TEXT_2XS)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(muted_fg)
                            .child(label),
                    );
                }
                section_el = append_chat_segments(
                    section_el,
                    section.text,
                    &SegmentStyle {
                        pane,
                        abs_idx: section.abs_idx,
                        markdown: false,
                        text_size: ui::TEXT_SM,
                        text_color: muted_fg,
                        epoch,
                        palette,
                    },
                );
                body = body.child(section_el);
            }
            step_el = step_el.child(body);
        }
        step_el.into_any_element()
    }
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
        let is_assistant = item.kind == ChatItemKind::Assistant;
        let is_request = matches!(item.kind, ChatItemKind::Question | ChatItemKind::Permission);
        let disclosure = item.kind == ChatItemKind::Reasoning;
        let log_agent = self
            .chat
            .logs
            .get(&key)
            .map(|log| log.agent.clone())
            .unwrap_or_default();
        let expanded = self
            .chat
            .logs
            .get(&key)
            .is_some_and(|log| log.is_step_expanded(abs_idx));
        let role_label: String = match item.kind {
            ChatItemKind::User => ctext::YOU.get().to_owned(),
            ChatItemKind::Assistant => {
                if log_agent.is_empty() {
                    ctext::ASSISTANT.get().to_owned()
                } else {
                    composer::agent_display_name(&log_agent)
                }
            }
            ChatItemKind::Reasoning => ctext::REASONING.get().to_owned(),
            ChatItemKind::ToolCall => ctext::TOOL_CALL.get().to_owned(),
            ChatItemKind::ToolResult => ctext::TOOL_RESULT.get().to_owned(),
            ChatItemKind::Question => ctext::QUESTION.get().to_owned(),
            ChatItemKind::Permission => ctext::PERMISSION.get().to_owned(),
        };
        let is_copied = self
            .chat
            .copied_item
            .as_ref()
            .is_some_and(|(copied_key, idx, at)| {
                *copied_key == key && *idx == abs_idx && at.elapsed() < Duration::from_secs(2)
            });
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
                card.rounded(if is_user {
                    ui::RADIUS_LG
                } else {
                    ui::RADIUS_MD
                })
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
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .children(is_assistant.then(|| {
                                let icon_agent = if log_agent.is_empty() {
                                    "agent"
                                } else {
                                    log_agent.as_str()
                                };
                                ui::icons::agent_brand_icon(icon_agent, ui::ICON_XS, fg)
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(if is_assistant { fg } else { muted_fg })
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(if item.title.is_empty() {
                                        SharedString::from(role_label)
                                    } else {
                                        SharedString::from(format!("{role_label} • {}", item.title))
                                    }),
                            ),
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
                        .debug_selector(|| "chat-open-file".into())
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
                        let copy_key = key.clone();
                        Button::new(SharedString::from(format!(
                            "chat-copy-{}-{abs_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-copy-item".into())
                        .icon(if is_copied {
                            IconName::Check
                        } else {
                            IconName::Copy
                        })
                        .label(if is_copied {
                            ctext::COPIED.get()
                        } else {
                            ctext::COPY.get()
                        })
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                copy_text.clone(),
                            ));
                            this.chat.copied_item =
                                Some((copy_key.clone(), abs_idx, std::time::Instant::now()));
                            cx.notify();
                        }))
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
                                log.toggle_step(abs_idx);
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
            let epoch = self.chat.logs.get(&key).map_or(0, |log| log.epoch);
            let body = append_chat_segments(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .id(SharedString::from(format!(
                        "chat-body-{}-{abs_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-message-body".into()),
                &item.text,
                &SegmentStyle {
                    pane,
                    abs_idx,
                    markdown: item.kind == ChatItemKind::Assistant,
                    text_size: ui::TEXT_MD,
                    text_color: if disclosure { muted_fg } else { fg },
                    epoch,
                    palette,
                },
            );
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
        has_sticky_bar: bool,
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

        let (agent_label, bound, revision, epoch, display_feed) = match self.chat.logs.get_mut(&key)
        {
            Some(log) => (
                if log.agent.is_empty() {
                    "agent".to_owned()
                } else {
                    log.agent.clone()
                },
                log.bound,
                log.revision,
                log.epoch,
                log.display_feed(),
            ),
            None => (
                "agent".to_owned(),
                false,
                0,
                0,
                work_run::DisplayFeed::empty(),
            ),
        };

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

        if display_feed.is_empty() {
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
            let host = self.layout.host(pane).clone();
            let owner = cx.weak_entity();
            let list = self
                .chat
                .lists
                .entry((pane, session))
                .or_insert_with(|| ChatList::new(host, &display_feed, revision, epoch, owner));
            let list_state = list.sync(&display_feed, revision, epoch);
            let owner = cx.weak_entity();
            let rows = Arc::clone(&display_feed);
            feed = feed.child(
                gpui::list(list_state.clone(), move |row_ix, _, cx| {
                    let Some(row) = rows.get(row_ix) else {
                        return div().into_any_element();
                    };
                    owner
                        .update(cx, |this, cx| {
                            this.render_display_row(pane, session, row, cx)
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
        let host = self.layout.host(pane);
        let agent_state = self
            .sidebar
            .read(cx)
            .session_on(host, session)
            .map_or(krill_proto::control::AgentState::Unknown, |s| {
                s.status.state
            });
        let unread = host.is_local() && self.sidebar.read(cx).is_unread(session);
        let state_badge = match agent_state {
            krill_proto::control::AgentState::Working => Some(ui::badge(
                palette,
                ui::BadgeTone::Warning,
                ctext::STATE_WORKING.get(),
            )),
            krill_proto::control::AgentState::Waiting => Some(ui::badge(
                palette,
                ui::BadgeTone::Accent,
                ctext::STATE_WAITING.get(),
            )),
            krill_proto::control::AgentState::Blocked => Some(ui::badge(
                palette,
                ui::BadgeTone::Destructive,
                ctext::STATE_BLOCKED.get(),
            )),
            _ => None,
        };

        let in_card = self.workspace_stage() == super::super::pane_frame::WorkspaceStage::CardStage;
        let strip = div()
            .flex_none()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .px_3()
            .py_1p5()
            .when(in_card, |d| d.rounded_b(ui::PANE_CARD_INNER_RADIUS))
            .border_t_1()
            .border_color(border)
            .bg(panel_bg)
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(ui::status::status_glyph(
                agent_state,
                unread,
                ui::status::Size::Sm,
                cx,
            ))
            .child(ui::icons::agent_brand_icon(
                &agent_label,
                ui::ICON_XS,
                palette.foreground,
            ))
            .children(state_badge)
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
                .when(in_card && !has_sticky_bar, |d| {
                    d.rounded(ui::PANE_CARD_INNER_RADIUS)
                })
                .when(in_card && has_sticky_bar, |d| {
                    d.rounded_b(ui::PANE_CARD_INNER_RADIUS)
                })
                .occlude()
                .flex()
                .flex_col()
                .bg(bg)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        if this.layout.focused() != Some(pane) {
                            this.change(false, cx, |l| l.focus(pane));
                            this.files.refocus = false;
                        }
                        if !this.chat_pane_has_focus(pane, window, cx) {
                            if let Some(chat_key) = this.chat_key(pane) {
                                let input = this.ensure_chat_input(chat_key, window, cx);
                                input.update(cx, |state, cx| state.focus(window, cx));
                            } else if !this.focus.is_focused(window) {
                                window.focus(&this.focus, cx);
                            }
                        }
                    }),
                )
                .child(feed)
                .children(self.render_chat_prompt_notices(&key, cx))
                .child(composer)
                .child(strip)
                .into_any_element(),
        )
    }
}
