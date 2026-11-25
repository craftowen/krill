// UI patterns adapted from Orca native-chat (MIT).
// Copyright (c) 2026 Lovecast Inc.
// NativeChatMessageRow.tsx, NativeChatReasoningRow.tsx, NativeChatComposerField.tsx,
// native-chat-autoscroll.ts; rendered with Krill design tokens and GPUI.

use super::body::normalize_code_language;
use super::work_run;
use super::*;
use gpui::StatefulInteractiveElement as _;
use gpui::Styled;
use gpui::prelude::FluentBuilder as _;
use gpui_component::menu::ContextMenuExt as _;
use gpui_kit_assets::IconName;

const CHAT_TOP_CONTROLS_GROUP: &str = "chat-top-controls";
const MAX_RENDERED_DIFF_LINES: usize = 200;

fn is_other_option_label(label: &str) -> bool {
    let lower = label.trim().to_ascii_lowercase();
    lower == "other"
        || lower.starts_with("other.")
        || lower.starts_with("other…")
        || lower.starts_with("other...")
        || lower.starts_with("other (")
        || lower.starts_with("other:")
        || lower.starts_with("type something")
}

fn render_diff_block(palette: &ui::Palette, text: &str) -> AnyElement {
    let mut rows = Vec::new();
    let total_lines = text.lines().count();
    for line in text.lines().take(MAX_RENDERED_DIFF_LINES) {
        let (bg, fg) = if line.starts_with('+') && !line.starts_with("+++") {
            (palette.success.opacity(0.14), palette.success)
        } else if line.starts_with('-') && !line.starts_with("---") {
            (palette.destructive.opacity(0.14), palette.destructive)
        } else if line.starts_with("@@") {
            (palette.accent.opacity(0.25), palette.ring)
        } else if line.starts_with("+++") || line.starts_with("---") || line.starts_with("***") {
            (ui::transparent(), palette.foreground)
        } else {
            (ui::transparent(), palette.muted_foreground)
        };
        rows.push(
            div()
                .w_full()
                .min_w_0()
                .px_1p5()
                .py_0p5()
                .bg(bg)
                .font_family(ui::FONT_MONO)
                .text_size(ui::TEXT_XS)
                .text_color(fg)
                .child(line.to_owned()),
        );
    }
    if total_lines > MAX_RENDERED_DIFF_LINES {
        rows.push(
            div()
                .w_full()
                .px_1p5()
                .py_0p5()
                .text_size(ui::TEXT_2XS)
                .text_color(palette.muted_foreground)
                .child(
                    ctext::DIFF_MORE_LINES
                        .format(&[("n", &(total_lines - MAX_RENDERED_DIFF_LINES).to_string())]),
                ),
        );
    }
    div()
        .w_full()
        .min_w_0()
        .rounded(ui::RADIUS_SM)
        .border_1()
        .border_color(palette.border)
        .bg(palette.muted.opacity(0.35))
        .py_1()
        .flex()
        .flex_col()
        .children(rows)
        .into_any_element()
}

struct SegmentStyle<'a> {
    pane: super::super::PaneId,
    session: SessionId,
    key: &'a ChatKey,
    abs_idx: usize,
    markdown: bool,
    stream_fade: bool,
    full_width: bool,
    text_size: gpui::Rems,
    text_color: gpui::Hsla,
    epoch: u64,
    palette: &'a ui::Palette,
}

impl Shell {
    fn append_chat_segments<T: gpui::ParentElement>(
        &self,
        mut container: T,
        text: &str,
        style: &SegmentStyle<'_>,
        cx: &mut Context<Self>,
    ) -> T {
        let pane = style.pane;
        let session = style.session;
        let abs_idx = style.abs_idx;
        let epoch = style.epoch;
        let border = style.palette.border;
        let fg = style.palette.foreground;
        let muted_fg = style.palette.muted_foreground;
        let mut next_code_idx = 0usize;
        for (seg_idx, segment) in parse_chat_segments(text).into_iter().enumerate() {
            container = match segment {
                ChatBodySegment::Text(text) => {
                    let holder = if style.full_width {
                        div().w_full()
                    } else {
                        div()
                    };
                    container.child(
                        holder
                            .min_w_0()
                            .text_size(style.text_size)
                            .text_color(style.text_color)
                            .child(if style.markdown {
                                gpui_component::text::TextView::markdown(
                                    SharedString::from(format!(
                                        "chat-markdown-{}-{epoch}-{abs_idx}-{seg_idx}",
                                        pane.get()
                                    )),
                                    text,
                                )
                                .scrollable(false)
                                .selectable(true)
                                .stream_fade(style.stream_fade)
                                .on_link_click(|url, _, _, cx| {
                                    if let Some(clean) = krill_bots_ui::bots::sanitize_http_url(url)
                                    {
                                        cx.open_url(&clean);
                                    }
                                })
                                .into_any_element()
                            } else {
                                gpui_component::SelectableText::new(
                                    SharedString::from(format!(
                                        "chat-text-{}-{epoch}-{abs_idx}-{seg_idx}",
                                        pane.get()
                                    )),
                                    text,
                                )
                                .into_any_element()
                            }),
                    )
                }
                ChatBodySegment::Code { lang, code } => {
                    let code_idx = next_code_idx;
                    next_code_idx += 1;
                    let display_lang = normalize_code_language(&lang);
                    let is_copied = self.chat.copied_code.as_ref().is_some_and(|(k, target)| {
                        k == style.key && target.abs_idx == abs_idx && target.block_idx == code_idx
                    });
                    let is_wrapped = self
                        .chat
                        .logs
                        .get(style.key)
                        .is_some_and(|log| log.is_code_wrapped(abs_idx, code_idx));

                    let wrap_button = Button::new(SharedString::from(format!(
                        "chat-wrap-code-{}-{abs_idx}-{code_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-wrap-code".into())
                    .icon(IconName::TextWrap)
                    .tooltip(if is_wrapped {
                        ctext::DISABLE_LINE_WRAP.get()
                    } else {
                        ctext::WRAP_LINES.get()
                    })
                    .accessibility_label(if is_wrapped {
                        ctext::DISABLE_LINE_WRAP.get()
                    } else {
                        ctext::WRAP_LINES.get()
                    })
                    .toggled(is_wrapped)
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        let key = (this.layout.host(pane).clone(), session);
                        if let Some(log) = this.chat.logs.get_mut(&key) {
                            log.toggle_code_wrap(abs_idx, code_idx);
                        }
                        cx.notify();
                    }));

                    let copy_key = style.key.clone();
                    let code_for_copy = code.clone();
                    let code_content = div()
                        .id(SharedString::from(format!(
                            "chat-code-{}-{abs_idx}-{code_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-code-content".into())
                        .w_full()
                        .min_w_0()
                        .when(is_wrapped, |d| d.overflow_hidden().whitespace_normal())
                        .when(!is_wrapped, |d| d.overflow_x_scroll().whitespace_nowrap())
                        .p_3()
                        .when(display_lang.is_none(), |d| d.pr(gpui::px(64.)))
                        .font_family(ui::FONT_MONO)
                        .text_size(ui::TEXT_SM)
                        .text_color(fg)
                        .child(super::code_view::SelectableHighlightedText::new(
                            SharedString::from(format!(
                                "chat-code-text-{}-{epoch}-{abs_idx}-{code_idx}",
                                pane.get()
                            )),
                            &lang,
                            code,
                            cx,
                        ));

                    let mut block = div()
                        .relative()
                        .w_full()
                        .min_w_0()
                        .rounded(ui::RADIUS_MD)
                        .border_1()
                        .border_color(border)
                        .bg(style.palette.muted)
                        .overflow_hidden()
                        .flex()
                        .flex_col();

                    if let Some(label) = display_lang {
                        let copy_btn = ui::button_with_reserved_label(
                            SharedString::from(format!(
                                "chat-copy-code-{}-{abs_idx}-{code_idx}",
                                pane.get()
                            )),
                            ctext::COPY_CODE.get(),
                            ctext::COPIED.get(),
                            is_copied,
                        )
                        .debug_selector(|| "chat-copy-code".into())
                        .icon(if is_copied {
                            IconName::Check
                        } else {
                            IconName::Copy
                        })
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.copy_chat_code_block(
                                copy_key.clone(),
                                abs_idx,
                                code_idx,
                                &code_for_copy,
                                cx,
                            );
                        }));

                        block = block
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "chat-code-header-{}-{abs_idx}-{code_idx}",
                                        pane.get()
                                    )))
                                    .debug_selector(|| "chat-code-header".into())
                                    .h(gpui::px(30.))
                                    .w_full()
                                    .min_w_0()
                                    .px_3()
                                    .border_b_1()
                                    .border_color(border)
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(ui::FONT_MONO)
                                            .text_size(ui::TEXT_XS)
                                            .text_color(muted_fg)
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .child(wrap_button)
                                            .child(copy_btn),
                                    ),
                            )
                            .child(code_content);
                    } else {
                        let copy_btn = Button::new(SharedString::from(format!(
                            "chat-copy-code-{}-{abs_idx}-{code_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-copy-code".into())
                        .icon(if is_copied {
                            IconName::Check
                        } else {
                            IconName::Copy
                        })
                        .tooltip(if is_copied {
                            ctext::COPIED.get()
                        } else {
                            ctext::COPY_CODE.get()
                        })
                        .accessibility_label(if is_copied {
                            ctext::COPIED.get()
                        } else {
                            ctext::COPY_CODE.get()
                        })
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.copy_chat_code_block(
                                copy_key.clone(),
                                abs_idx,
                                code_idx,
                                &code_for_copy,
                                cx,
                            );
                        }));

                        block = block.child(code_content).child(
                            div()
                                .absolute()
                                .top_2()
                                .right_2()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(wrap_button)
                                .child(copy_btn),
                        );
                    }

                    container.child(block)
                }
            };
        }
        container
    }
}

impl Shell {
    #[expect(
        clippy::too_many_arguments,
        reason = "display row rendering needs list coordinates and agent state"
    )]
    fn render_display_row(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        row: &work_run::DisplayRow,
        row_ix: usize,
        row_count: usize,
        joins_activity_below: bool,
        list_state: &gpui::ListState,
        agent_state: krill_proto::control::AgentState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_last_row = row_ix + 1 == row_count;
        let key = (self.layout.host(pane).clone(), session);
        let is_find_match = self.active_chat_find_row(&key) == Some(row_ix);
        let row_el = match row {
            work_run::DisplayRow::Item { abs_idx, item } => self.render_chat_item(
                pane,
                session,
                *abs_idx,
                item,
                row_ix,
                is_last_row,
                list_state,
                agent_state,
                cx,
            ),
            work_run::DisplayRow::QuestionBatch { items } => {
                self.render_question_batch(pane, session, items, cx)
            }
            work_run::DisplayRow::WorkRun(run) => {
                self.render_work_run(pane, session, run, is_last_row, agent_state, cx)
            }
            work_run::DisplayRow::ItemNote { note, .. } => match &note.action {
                work_run::NoteAction::Memory(receipt) => {
                    self.render_memory_note(pane, session, row_ix, note.at_ms, receipt, cx)
                }
                work_run::NoteAction::CreateOffer(offer) => {
                    self.render_create_offer(pane, session, offer, cx)
                }
                _ => Self::render_item_note(pane, row_ix, note, joins_activity_below, cx),
            },
            work_run::DisplayRow::BotCard { abs_idx, card } => {
                self.render_bot_chat_card(pane, session, *abs_idx, card, joins_activity_below, cx)
            }
            work_run::DisplayRow::TurnBar {
                user_abs_idx,
                worked_secs,
                folds,
                expanded,
            } => self.render_turn_bar(
                pane,
                session,
                *user_abs_idx,
                *worked_secs,
                *folds,
                *expanded,
                cx,
            ),
            work_run::DisplayRow::Notice { abs_idx, notice } => self.render_inbound_notice(
                pane,
                session,
                *abs_idx,
                row_ix,
                notice,
                joins_activity_below,
                cx,
            ),
            work_run::DisplayRow::ContextCleared { abs_idx } => Self::render_divider_row(
                SharedString::from(format!("chat-context-cleared-{}-{abs_idx}", pane.get())),
                "chat-context-cleared-divider",
                ctext::CONTEXT_CLEARED.get(),
                cx,
            ),
            work_run::DisplayRow::UnreadDivider { first_abs_idx } => Self::render_divider_row(
                SharedString::from(format!(
                    "chat-unread-divider-{}-{first_abs_idx}",
                    pane.get()
                )),
                "chat-unread-divider",
                ctext::UNREAD_DIVIDER.get(),
                cx,
            ),
        };
        if is_find_match {
            let palette = ui::palette(cx);
            div()
                .w_full()
                .min_w_0()
                .rounded(ui::RADIUS_MD)
                .border_l_2()
                .border_color(palette.ring)
                .bg(palette.accent.opacity(0.25))
                .child(row_el)
                .into_any_element()
        } else {
            row_el
        }
    }

    fn render_work_run(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        run: &work_run::WorkRun,
        is_last_row: bool,
        agent_state: krill_proto::control::AgentState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (self.layout.host(pane).clone(), session);
        let palette = ui::palette(cx);
        let fg = palette.foreground;
        let muted_fg = palette.muted_foreground;
        let run_span = run.span;
        let run_is_live = is_last_row && agent_state == krill_proto::control::AgentState::Working;
        let run_expanded = self
            .chat
            .logs
            .get(&key)
            .is_some_and(|log| log.is_run_expanded(run_span));
        let summary = run.header_label(run_is_live);
        let active_detail = run.active_detail();
        let header_text = if run_is_live && !active_detail.is_empty() {
            format!("{summary} · {active_detail}")
        } else {
            summary
        };
        let progress = run.progress_label(run_is_live);
        let failed = run.failed_count_label();
        let diff_summary = run.diff_summary_label();
        let run_elapsed_secs = self
            .chat
            .logs
            .get(&key)
            .and_then(|log| log.run_elapsed_secs(run_span.start(), std::time::Instant::now()));
        let elapsed = run_elapsed_secs.map(work_run::format_elapsed_secs);
        // Orca `NativeChatToolRun` / `NativeChatWorkingStatus`: when collapsed and settled, show
        // a quiet `"16s 작업 완료 >"` line instead of verbose icon + file/command chain + check.
        let quiet_collapsed = !run_is_live && !run_expanded && run.counts.errors == 0;
        let collapsed_label = if quiet_collapsed {
            if let Some(secs) = run_elapsed_secs {
                ctext::WORKED_FOR.format(&[("time", &work_run::format_turn_duration(secs))])
            } else {
                header_text.clone()
            }
        } else {
            header_text.clone()
        };

        let item_selector = format!("chat-item-{}-{}", pane.get(), run_span.start());
        let debug_selector = item_selector.clone();
        let mut container =
            div()
                .id(SharedString::from(item_selector))
                .debug_selector(move || debug_selector.clone())
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
                        .rounded(ui::RADIUS_SM)
                        .hover(|s| s.bg(palette.muted.opacity(0.6)))
                        .focus_visible(|s| s.bg(palette.muted.opacity(0.6)))
                        .w_full()
                        .min_w_0()
                        .min_h(gpui::px(26.))
                        .px_1p5()
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
                                log.toggle_run(run_span);
                            }
                            cx.notify();
                        }))
                        .when(!quiet_collapsed, |row| {
                            row.child(div().flex_none().flex().items_center().child(
                                if run_is_live {
                                    ui::status::status_glyph(
                                        krill_proto::control::AgentState::Working,
                                        false,
                                        ui::status::Size::Sm,
                                        cx,
                                    )
                                    .into_any_element()
                                } else {
                                    gpui_component::Icon::new(run.header_icon())
                                        .with_size(ui::ICON_MD)
                                        .text_color(if run.counts.errors > 0 {
                                            palette.destructive
                                        } else {
                                            muted_fg
                                        })
                                        .into_any_element()
                                },
                            ))
                        })
                        .child(
                            div()
                                .when(quiet_collapsed && diff_summary.is_none(), |d| {
                                    d.min_w_0().truncate()
                                })
                                .when(!quiet_collapsed || diff_summary.is_some(), |d| {
                                    d.flex_1().min_w_0().truncate()
                                })
                                .text_size(if quiet_collapsed {
                                    ui::TEXT_SM
                                } else {
                                    ui::TEXT_XS
                                })
                                .font_weight(if quiet_collapsed {
                                    gpui::FontWeight::NORMAL
                                } else {
                                    gpui::FontWeight::MEDIUM
                                })
                                .text_color(if run_is_live { fg } else { muted_fg })
                                .child(collapsed_label),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .children(failed.map(|label| {
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(palette.destructive)
                                        .child(label)
                                }))
                                .children(diff_summary.map(|label| {
                                    div()
                                        .id(SharedString::from(format!(
                                            "chat-work-run-diff-{}-{}",
                                            pane.get(),
                                            run_span.start()
                                        )))
                                        .debug_selector(|| "chat-work-run-diff-summary".into())
                                        .text_size(ui::TEXT_XS)
                                        .text_color(muted_fg)
                                        .child(label)
                                }))
                                .when(!quiet_collapsed, |d| {
                                    d.children(progress.map(|label| {
                                        div()
                                            .font_family(ui::FONT_MONO)
                                            .text_size(ui::TEXT_XS)
                                            .text_color(muted_fg)
                                            .child(label)
                                    }))
                                    .children(elapsed.map(|label| {
                                        div()
                                            .font_family(ui::FONT_MONO)
                                            .text_size(ui::TEXT_XS)
                                            .text_color(muted_fg)
                                            .child(label)
                                    }))
                                })
                                .child(
                                    gpui_component::Icon::new(if run_expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .with_size(ui::ICON_XS)
                                    .text_color(muted_fg),
                                ),
                        ),
                );
        if run_expanded {
            let mut body_col = div()
                .ml(gpui::px(7.))
                .pl(gpui::px(13.))
                .mt_0p5()
                .min_w_0()
                .border_l_1()
                .border_color(palette.border)
                .flex()
                .flex_col()
                .gap_0p5();
            for step in &run.steps {
                body_col =
                    body_col.child(self.render_work_step(pane, session, step, run_is_live, cx));
            }
            container = container.child(body_col);
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
                    .pb_3()
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
        run_is_live: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (self.layout.host(pane).clone(), session);
        let palette = ui::palette(cx);
        let fg = palette.foreground;
        let muted_fg = palette.muted_foreground;
        let step_abs_idx = step.primary_abs_idx();

        if let work_run::WorkStep::Note { item, .. } = step {
            return div()
                .id(SharedString::from(format!(
                    "chat-work-note-{}-{step_abs_idx}",
                    pane.get()
                )))
                .debug_selector(|| "chat-work-note".into())
                .w_full()
                .min_w_0()
                .min_h(gpui::px(24.))
                .px_1()
                .py_0p5()
                .flex()
                .items_start()
                .gap_1p5()
                .child(
                    div().flex_none().pt_0p5().child(
                        gpui_component::Icon::new(step.icon())
                            .with_size(ui::ICON_SM)
                            .text_color(muted_fg),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(ui::TEXT_XS)
                        .text_color(muted_fg)
                        .child(gpui_component::SelectableText::new(
                            SharedString::from(format!(
                                "chat-work-note-text-{}-{step_abs_idx}",
                                pane.get()
                            )),
                            item.text.trim().to_owned(),
                        )),
                )
                .into_any_element();
        }

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
        let headline = step.headline();
        let verb = step.verb(run_is_live);
        let step_is_live = run_is_live && step.is_pending_call();
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
            .hover(|s| s.bg(palette.muted.opacity(0.6)))
            .focus_visible(|s| s.bg(palette.muted.opacity(0.6)))
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
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(if step_is_live {
                        ui::status::status_glyph(
                            krill_proto::control::AgentState::Working,
                            false,
                            ui::status::Size::Sm,
                            cx,
                        )
                        .into_any_element()
                    } else {
                        gpui_component::Icon::new(step.icon())
                            .with_size(ui::ICON_SM)
                            .text_color(if step.is_error() {
                                palette.destructive
                            } else {
                                muted_fg
                            })
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(gpui::rems(7.5))
                    .truncate()
                    .text_size(ui::TEXT_XS)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(if step.is_error() {
                        palette.destructive
                    } else {
                        muted_fg
                    })
                    .child(verb),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui::TEXT_XS)
                    .text_color(if step.is_error() {
                        palette.destructive
                    } else {
                        fg
                    })
                    .child(headline),
            )
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
            );

        let header = div()
            .w_full()
            .min_w_0()
            .min_h(gpui::px(26.))
            .px_1()
            .py_0p5()
            .flex()
            .items_center()
            .gap_1p5()
            .child(toggle)
            .children((!file_path.is_empty()).then(|| {
                let open_host = self.layout.host(pane).clone();
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
                            this.open_file_on_at(open_host.clone(), path.clone(), 1, 1, cx);
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
                        this.mark_chat_item_copied(copy_key.clone(), step_abs_idx, cx);
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
                .max_h(gpui::px(360.))
                .overflow_y_scroll()
                .pl(gpui::px(22.))
                .pr_1()
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
                if work_run::is_unified_diff(section.text) {
                    section_el = section_el.child(render_diff_block(palette, section.text));
                } else {
                    section_el = self.append_chat_segments(
                        section_el,
                        section.text,
                        &SegmentStyle {
                            pane,
                            session,
                            key: &key,
                            abs_idx: section.abs_idx,
                            markdown: false,
                            stream_fade: false,
                            full_width: true,
                            text_size: ui::TEXT_SM,
                            text_color: muted_fg,
                            epoch,
                            palette,
                        },
                        cx,
                    );
                }
                body = body.child(section_el);
            }
            step_el = step_el.child(body);
        }
        step_el.into_any_element()
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "chat item rendering needs list scroll handle and agent state"
    )]
    fn render_chat_item(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        abs_idx: usize,
        item: &ChatItem,
        row_ix: usize,
        is_last_row: bool,
        list_state: &gpui::ListState,
        agent_state: krill_proto::control::AgentState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (self.layout.host(pane).clone(), session);
        let palette = ui::palette(cx);
        let border = palette.border;
        let fg = palette.foreground;
        let muted_fg = palette.muted_foreground;
        let epoch = self.chat.logs.get(&key).map_or(0, |log| log.epoch);
        let is_copied = self
            .chat
            .copied_item
            .as_ref()
            .is_some_and(|(copied_key, idx, at)| {
                *copied_key == key && *idx == abs_idx && at.elapsed() < Duration::from_secs(2)
            });

        // A user prompt's `file_path` lists its images; they draw as tiles, not a file link.
        let open_file_button = (!item.file_path.is_empty() && item.kind != ChatItemKind::User)
            .then(|| {
                let open_host = self.layout.host(pane).clone();
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
                .max_w(gpui::rems(5.5))
                .overflow_hidden()
                .tooltip(item.file_path.clone())
                .accessibility_label(item.file_path.clone())
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_file_on_at(open_host.clone(), path.clone(), 1, 1, cx);
                }))
            });

        let is_reasoning = matches!(item.kind, ChatItemKind::Reasoning);
        let message_row = matches!(item.kind, ChatItemKind::User | ChatItemKind::Assistant);
        let copy_button = (!item.text.trim().is_empty()).then(|| {
            let copy_text = krill_term::select::clean_text(&item.text);
            let copy_key = key.clone();
            let debug_name: &'static str = if is_reasoning {
                "chat-reasoning-copy"
            } else {
                "chat-copy-item"
            };
            Button::new(SharedString::from(format!(
                "chat-copy-{}-{abs_idx}",
                pane.get()
            )))
            .debug_selector(move || debug_name.into())
            .icon(if is_copied {
                IconName::Check
            } else {
                IconName::Copy
            })
            .tooltip(if is_copied {
                ctext::COPIED.get()
            } else {
                ctext::COPY_MESSAGE.get()
            })
            .accessibility_label(if is_copied {
                ctext::COPIED.get()
            } else {
                ctext::COPY_MESSAGE.get()
            })
            .ghost()
            // Orca's message rows: a 24px icon button; a thought keeps its labelled one.
            .map(|b| {
                if message_row {
                    b.small()
                        .text_color(if is_copied { palette.success } else { muted_fg })
                } else {
                    b.label(if is_copied {
                        ctext::COPIED.get()
                    } else {
                        ctext::COPY.get()
                    })
                    .xsmall()
                }
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                this.mark_chat_item_copied(copy_key.clone(), abs_idx, cx);
                cx.notify();
            }))
        });

        let quote_button = (!item.text.trim().is_empty()
            && matches!(item.kind, ChatItemKind::User | ChatItemKind::Assistant))
        .then(|| {
            let quote_text = krill_term::select::clean_text(&item.text);
            let quote_key = key.clone();
            Button::new(SharedString::from(format!(
                "chat-quote-{}-{abs_idx}",
                pane.get()
            )))
            .debug_selector(|| "chat-quote-item".into())
            .icon(IconName::Quote)
            .tooltip(ctext::QUOTE_MESSAGE.get())
            .accessibility_label(ctext::QUOTE_MESSAGE.get())
            .ghost()
            .small()
            .text_color(muted_fg)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.quote_text_into_chat_composer(&quote_key, &quote_text, window, cx);
            }))
        });

        let item_selector = format!("chat-item-{}-{abs_idx}", pane.get());
        let debug_selector = item_selector.clone();

        match item.kind {
            ChatItemKind::User => {
                // Automation result / wait inline rows and bot-created receipts.
                if let Some(inline) = super::bot_rows::parse_automation_inline_row(&item.text) {
                    return Self::render_automation_inline_row(
                        pane, abs_idx, &inline, &item.at, cx,
                    );
                }
                if let Some((bot_name, bot_id)) = super::bot_rows::parse_bot_created_row(&item.text)
                {
                    return Self::render_bot_created_row(pane, abs_idx, &bot_name, bot_id, cx);
                }
                // A turn one of the bot's schedules started reads as Orca's automation run row.
                if let Some(name) = self.chat_bot(&key).and_then(|bot| {
                    super::bot_rows::scheduled_run(&item.text, &bot.schedules).map(str::to_owned)
                }) {
                    return self.render_scheduled_run(pane, session, abs_idx, &name, item, cx);
                }
                let (sender_tag, item_text) = super::bot_rows::parse_prompt_sender_tag(&item.text);
                let image_paths = super::images::item_image_paths(item);
                let image_numbers = super::images::image_numbers(&item.text, image_paths.len());
                let is_remote = self.layout.host(pane).is_remote();
                let resolved_paths: Vec<Option<std::path::PathBuf>> = image_paths
                    .into_iter()
                    .map(|path| {
                        if is_remote {
                            self.chat
                                .images
                                .resolve_remote_preview(&path)
                                .map(std::path::Path::to_path_buf)
                        } else {
                            Some(path)
                        }
                    })
                    .collect();
                let images = self
                    .chat_message_images_for_item(
                        &format!("chat-item-{}-{abs_idx}", pane.get()),
                        Some((&key, abs_idx)),
                        &resolved_paths,
                        &image_numbers,
                        cx,
                    )
                    .map(gpui::Styled::mb_0p5);
                let sender_tag_el =
                    sender_tag.map(|tag| Self::render_prompt_sender_tag(&tag, palette));
                let has_text = !item_text.trim().is_empty();
                let foldable = is_user_prompt_foldable(item_text);
                let prompt_expanded = self
                    .chat
                    .logs
                    .get(&key)
                    .is_some_and(|log| log.is_user_message_expanded(abs_idx));
                let body = self.append_chat_segments(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .id(SharedString::from(format!(
                            "chat-body-{}-{abs_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-message-body".into()),
                    item_text,
                    &SegmentStyle {
                        pane,
                        session,
                        key: &key,
                        abs_idx,
                        markdown: false,
                        stream_fade: false,
                        full_width: false,
                        text_size: ui::TEXT_MD,
                        text_color: fg,
                        epoch,
                        palette,
                    },
                    cx,
                );
                let bubble = div()
                    .id(SharedString::from(item_selector))
                    .debug_selector(move || debug_selector.clone())
                    .min_w_0()
                    .max_w(gpui::relative(0.80))
                    .overflow_hidden()
                    .rounded(ui::RADIUS_XL)
                    .border_1()
                    .border_color(border)
                    .bg(palette.muted)
                    .px_3p5()
                    .py_2p5()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .children(images)
                    .when(has_text, |d| {
                        d.child(
                            div()
                                .min_w_0()
                                .when(foldable && !prompt_expanded, |d| {
                                    d.max_h(gpui::px(176.)).overflow_hidden()
                                })
                                .child(body),
                        )
                    })
                    .children(foldable.then(|| {
                        div().flex().justify_start().child(
                            Button::new(SharedString::from(format!(
                                "chat-user-fold-{}-{abs_idx}",
                                pane.get()
                            )))
                            .debug_selector(|| "chat-user-fold".into())
                            .label(if prompt_expanded {
                                ctext::SHOW_LESS.get()
                            } else {
                                ctext::SHOW_FULL_MESSAGE.get()
                            })
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(log) = this
                                        .chat
                                        .logs
                                        .get_mut(&(this.layout.host(pane).clone(), session))
                                    {
                                        log.toggle_user_message(abs_idx);
                                    }
                                    cx.notify();
                                },
                            )),
                        )
                    }));

                let time_badge = Self::chat_message_time(pane, abs_idx, &item.at, muted_fg);
                let reuse_button = (!item.text.trim().is_empty()).then(|| {
                    let reuse_text = krill_term::select::clean_text(&item.text);
                    let reuse_key = key.clone();
                    Button::new(SharedString::from(format!(
                        "chat-reuse-{}-{abs_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-reuse-item".into())
                    .icon(IconName::Pencil)
                    .tooltip(ctext::REUSE_PROMPT.get())
                    .accessibility_label(ctext::REUSE_PROMPT.get())
                    .ghost()
                    .small()
                    .text_color(muted_fg)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reuse_chat_prompt(&reuse_key, &reuse_text, window, cx);
                    }))
                });
                let has_meta = time_badge.is_some()
                    || open_file_button.is_some()
                    || copy_button.is_some()
                    || quote_button.is_some()
                    || reuse_button.is_some();
                let meta_group =
                    SharedString::from(format!("chat-user-row-{}-{abs_idx}", pane.get()));
                // Orca's `UserMessageMeta`: Copy, the time, then its own action, shown on hover.
                let user_col = div()
                    .group(meta_group.clone())
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap_0p5()
                    .children(sender_tag_el)
                    .child(bubble)
                    .children(has_meta.then(|| {
                        div()
                            .id(SharedString::from(format!(
                                "chat-user-meta-{}-{abs_idx}",
                                pane.get()
                            )))
                            .debug_selector(|| "chat-user-meta".into())
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_1()
                            .opacity(0.)
                            .group_hover(meta_group.clone(), |s| s.opacity(1.))
                            .children(copy_button)
                            .children(quote_button)
                            .children(time_badge)
                            .children(open_file_button)
                            .children(reuse_button)
                    }));

                return div()
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
                            .justify_end()
                            .child(user_col),
                    )
                    .into_any_element();
            }
            ChatItemKind::Assistant => {
                let body = self.append_chat_segments(
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
                        session,
                        key: &key,
                        abs_idx,
                        markdown: true,
                        stream_fade: is_last_row
                            && agent_state == krill_proto::control::AgentState::Working,
                        full_width: true,
                        text_size: ui::TEXT_MD,
                        text_color: fg,
                        epoch,
                        palette,
                    },
                    cx,
                );
                let time_badge = Self::chat_message_time(pane, abs_idx, &item.at, muted_fg);
                // Orca's `latestResponse`: the newest reply keeps its actions in view.
                let latest_response = self
                    .chat
                    .logs
                    .get(&key)
                    .and_then(|log| log.cached_feed.as_ref())
                    .and_then(|feed| {
                        feed.rows().iter().rev().find_map(|row| match row {
                            work_run::DisplayRow::Item { abs_idx, item }
                                if item.kind == ChatItemKind::Assistant =>
                            {
                                Some(*abs_idx)
                            }
                            _ => None,
                        })
                    })
                    == Some(abs_idx);
                let show_footer = (!is_last_row
                    || agent_state != krill_proto::control::AgentState::Working)
                    && (time_badge.is_some()
                        || !item.text.trim().is_empty()
                        || !item.file_path.is_empty());
                let scroll_list = list_state.clone();
                let scroll_top_btn = Button::new(SharedString::from(format!(
                    "chat-scroll-top-{}-{abs_idx}",
                    pane.get()
                )))
                .debug_selector(|| "chat-scroll-top".into())
                .icon(IconName::ArrowUp)
                .tooltip(ctext::SCROLL_MESSAGE_TO_TOP.get())
                .accessibility_label(ctext::SCROLL_MESSAGE_TO_TOP.get())
                .ghost()
                .small()
                .text_color(muted_fg)
                .on_click(cx.listener(move |_, _, _, cx| {
                    scroll_list.scroll_to(gpui::ListOffset {
                        item_ix: row_ix,
                        offset_in_item: gpui::px(0.),
                    });
                    cx.notify();
                }));
                // Orca forks "from this response" through its turn's end. Krill forks the whole
                // session, which is that only on the latest response.
                let fork_button = (latest_response && self.can_fork_pane(pane, cx)).then(|| {
                    Button::new(SharedString::from(format!(
                        "chat-fork-{}-{abs_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-fork-item".into())
                    .icon(IconName::GitFork)
                    .tooltip(ctext::FORK_FROM_RESPONSE_TIP.get())
                    .accessibility_label(ctext::FORK_FROM_RESPONSE.get())
                    .ghost()
                    .small()
                    .text_color(muted_fg)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.fork_pane_session(pane, cx);
                    }))
                });
                let retry_key = key.clone();
                let retry_button = Button::new(SharedString::from(format!(
                    "chat-retry-{}-{abs_idx}",
                    pane.get()
                )))
                .debug_selector(|| "chat-retry-item".into())
                .icon(IconName::RotateCcw)
                .tooltip(ctext::RETRY_TURN.get())
                .accessibility_label(ctext::RETRY_TURN.get())
                .ghost()
                .small()
                .text_color(muted_fg)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.retry_assistant_turn(&retry_key, abs_idx, window, cx);
                }));
                let meta_group =
                    SharedString::from(format!("chat-assistant-row-{}-{abs_idx}", pane.get()));

                // Orca's `NativeChatAgentControls`: Copy, Quote, Retry, Fork, scroll to top, time.
                // Shown on the latest response, on hover elsewhere.
                let assistant_el = div()
                    .id(SharedString::from(item_selector))
                    .debug_selector(move || debug_selector.clone())
                    .group(meta_group.clone())
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(body)
                    .children(show_footer.then(|| {
                        div()
                            .mt_1()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when(!latest_response, |d| {
                                d.opacity(0.)
                                    .group_hover(meta_group.clone(), |s| s.opacity(1.))
                            })
                            .children(copy_button)
                            .children(quote_button)
                            .child(retry_button)
                            .children(fork_button)
                            .children(open_file_button)
                            .child(scroll_top_btn)
                            .children(time_badge)
                    }));

                return div()
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
                            .child(assistant_el),
                    )
                    .into_any_element();
            }
            ChatItemKind::Reasoning => {
                let expanded = self
                    .chat
                    .logs
                    .get(&key)
                    .is_some_and(|log| log.is_step_expanded(abs_idx));
                let summary = item
                    .text
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map_or_else(|| ctext::REASONING.get().to_owned(), str::to_owned);

                let toggle = div()
                    .id(SharedString::from(format!(
                        "chat-disclosure-{}-{abs_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-disclosure".into())
                    .role(gpui::Role::Button)
                    .aria_label(if expanded {
                        ctext::HIDE_DETAILS.get()
                    } else {
                        ctext::SHOW_DETAILS.get()
                    })
                    .aria_expanded(expanded)
                    .tab_index(0)
                    .cursor_pointer()
                    .rounded(ui::RADIUS_XS)
                    .hover(|s| s.bg(palette.muted.opacity(0.6)))
                    .focus_visible(|s| s.bg(palette.muted.opacity(0.6)))
                    .w_full()
                    .min_w_0()
                    .px_1()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(log) = this
                            .chat
                            .logs
                            .get_mut(&(this.layout.host(pane).clone(), session))
                        {
                            log.toggle_step(abs_idx);
                        }
                        cx.notify();
                    }))
                    .child(
                        div().flex_none().flex().items_center().child(
                            gpui_component::Icon::new(IconName::Brain)
                                .with_size(ui::ICON_SM)
                                .text_color(muted_fg),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .text_color(muted_fg)
                            .child(summary),
                    )
                    .child(
                        div().flex_none().flex().items_center().child(
                            gpui_component::Icon::new(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .with_size(ui::ICON_XS)
                            .text_color(muted_fg),
                        ),
                    );

                let mut reasoning_el = div()
                    .id(SharedString::from(item_selector))
                    .debug_selector(move || debug_selector.clone())
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(toggle);

                if expanded {
                    let body = self.append_chat_segments(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .id(SharedString::from(format!(
                                "chat-body-{}-{abs_idx}",
                                pane.get()
                            )))
                            .debug_selector(|| "chat-message-body".into()),
                        &item.text,
                        &SegmentStyle {
                            pane,
                            session,
                            key: &key,
                            abs_idx,
                            markdown: false,
                            stream_fade: false,
                            full_width: true,
                            text_size: ui::TEXT_SM,
                            text_color: muted_fg,
                            epoch,
                            palette,
                        },
                        cx,
                    );
                    reasoning_el = reasoning_el
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "chat-reasoning-scroll-{}-{abs_idx}",
                                    pane.get()
                                )))
                                .w_full()
                                .min_w_0()
                                .pl(gpui::px(22.))
                                .max_h(gpui::px(320.))
                                .overflow_y_scroll()
                                .child(body),
                        )
                        .children(copy_button.map(|btn| {
                            div()
                                .pl(gpui::px(22.))
                                .pt_1()
                                .flex()
                                .items_center()
                                .child(btn)
                        }));
                }

                return div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                            .pb_3()
                            .flex()
                            .child(reasoning_el),
                    )
                    .into_any_element();
            }
            _ => {}
        }

        let is_request = matches!(item.kind, ChatItemKind::Question | ChatItemKind::Permission);
        let is_question = item.kind == ChatItemKind::Question;
        let role_label = match item.kind {
            ChatItemKind::Question => ctext::QUESTION.get(),
            ChatItemKind::Permission => ctext::PERMISSION.get(),
            ChatItemKind::ToolCall => ctext::TOOL_CALL.get(),
            ChatItemKind::ToolResult => ctext::TOOL_RESULT.get(),
            _ => ctext::ASSISTANT.get(),
        };
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
        // Orca `#47564`: a pending question reads as waiting (orange `agent_question`), never red blocked.
        let card_border = if is_question && current {
            palette.agent_question.opacity(0.65)
        } else if is_request && current {
            palette.primary.opacity(0.32)
        } else {
            border
        };

        let is_plan_approval = item.kind == ChatItemKind::Permission
            && item.tool_name.eq_ignore_ascii_case("ExitPlanMode");
        // Orca `#47117`: Bash/shell approval renders the command cleanly without duplicating description.
        let shell_approval = (item.kind == ChatItemKind::Permission)
            .then(|| {
                work_run::parse_shell_approval_detail(&item.text, &item.tool_name, &item.title)
            })
            .flatten();
        // Orca `#47164` & `#47222`: active permission card gets a focus handle so Enter approves the
        // default answer while ignoring held-key auto-repeat (`e.is_held`).
        let approval_focus =
            (item.kind == ChatItemKind::Permission && current && !disabled).then(|| {
                self.chat
                    .approval_focus
                    .entry((key.clone(), answer_key.clone()))
                    .or_insert_with(|| cx.focus_handle())
                    .clone()
            });
        let header_icon = if is_question {
            IconName::MessageCircleQuestionMark
        } else if item.kind == ChatItemKind::Permission {
            IconName::ShieldQuestionMark
        } else {
            IconName::Terminal
        };
        let header_icon_color = if is_question {
            palette.agent_question
        } else if item.kind == ChatItemKind::Permission && current {
            palette.warning
        } else {
            muted_fg
        };

        let mut card = div()
            .id(SharedString::from(item_selector))
            .debug_selector(move || debug_selector.clone())
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .rounded(ui::RADIUS_XL)
            .border_1()
            .border_color(card_border)
            .bg(if is_request {
                palette.chat_composer_surface()
            } else {
                palette.card
            })
            .flex()
            .flex_col()
            .when_some(approval_focus.clone(), |d, handle| {
                let enter_req = request.clone();
                let esc_req = request.clone();
                d.track_focus(&handle).on_key_down(cx.listener(
                    move |this, e: &gpui::KeyDownEvent, _, cx| {
                        // `#47222`: ignore held Enter or Space auto-repeat on prompt card buttons.
                        if e.is_held && matches!(e.keystroke.key.as_str(), "enter" | "space") {
                            cx.stop_propagation();
                            return;
                        }
                        if e.keystroke.modifiers.modified() {
                            return;
                        }
                        match e.keystroke.key.as_str() {
                            "enter" => {
                                this.approve_chat_permission(enter_req.clone(), true, cx);
                                cx.stop_propagation();
                            }
                            "escape" => {
                                this.approve_chat_permission(esc_req.clone(), false, cx);
                                cx.stop_propagation();
                            }
                            _ => {}
                        }
                    },
                ))
            })
            .child(
                // Orca `NativeChatQuestionCardHeader` / approval header band (no top step bar `#47959`).
                div()
                    .debug_selector(|| "chat-prompt-card-header".into())
                    .min_h(gpui::px(36.))
                    .px_3p5()
                    .py_1()
                    .bg(if is_question {
                        palette.agent_question.opacity(0.09)
                    } else if is_request {
                        palette.primary.opacity(0.07)
                    } else {
                        ui::transparent()
                    })
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
                            .child(
                                gpui_component::Icon::new(header_icon)
                                    .with_size(ui::ICON_MD)
                                    .text_color(header_icon_color),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui::TEXT_SM)
                                    .text_color(fg)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(if item.title.is_empty() {
                                        SharedString::from(role_label)
                                    } else {
                                        SharedString::from(format!("{role_label} · {}", item.title))
                                    }),
                            ),
                    )
                    .children(is_plan_approval.then(|| {
                        div()
                            .id(SharedString::from(format!(
                                "chat-plan-badge-{}-{abs_idx}",
                                pane.get()
                            )))
                            .debug_selector(|| "chat-plan-approval-badge".into())
                            .flex_none()
                            .px_1p5()
                            .py_0p5()
                            .rounded(ui::RADIUS_SM)
                            .bg(palette.primary.opacity(0.14))
                            .text_size(ui::TEXT_2XS)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(palette.primary)
                            .child(ctext::PLAN_APPROVAL_BADGE.get())
                    }))
                    .children(open_file_button)
                    .children(copy_button),
            );

        let mut scroll_body = div()
            .id(SharedString::from(format!(
                "chat-request-scroll-{}-{abs_idx}",
                pane.get()
            )))
            .w_full()
            .min_w_0()
            .max_h(gpui::px(300.))
            .overflow_y_scroll()
            .px_3p5()
            .pt_3()
            .pb_3()
            .flex()
            .flex_col()
            .gap_2p5();

        if let Some(shell_detail) = shell_approval {
            scroll_body = scroll_body
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "chat-shell-approval-cmd-{}-{abs_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-shell-approval-cmd".into())
                        .w_full()
                        .min_w_0()
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(border)
                        .bg(palette.muted.opacity(0.5))
                        .px_2p5()
                        .py_2()
                        .font_family(ui::FONT_MONO)
                        .text_size(ui::TEXT_SM)
                        .text_color(fg)
                        .whitespace_normal()
                        .child(shell_detail.command),
                )
                .children(shell_detail.other_arguments.map(|other| {
                    div()
                        .debug_selector(|| "chat-shell-approval-args".into())
                        .w_full()
                        .min_w_0()
                        .font_family(ui::FONT_MONO)
                        .text_size(ui::TEXT_XS)
                        .text_color(muted_fg)
                        .whitespace_normal()
                        .child(other)
                }));
        } else if !item.text.trim().is_empty() {
            let body = self.append_chat_segments(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .id(SharedString::from(format!(
                        "chat-body-{}-{abs_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-message-body".into()),
                &item.text,
                &SegmentStyle {
                    pane,
                    session,
                    key: &key,
                    abs_idx,
                    markdown: false,
                    stream_fade: false,
                    full_width: true,
                    text_size: if is_question {
                        ui::TEXT_HEADING
                    } else {
                        ui::TEXT_MD
                    },
                    text_color: fg,
                    epoch,
                    palette,
                },
                cx,
            );
            scroll_body = scroll_body
                .child(body.when(is_question, |d| d.font_weight(gpui::FontWeight::SEMIBOLD)));
        }

        if is_request {
            let status = if !current
                && !matches!(
                    submission,
                    Some(Submission::Confirmed | Submission::ReviewRequired)
                ) {
                ctext::REQUEST_UNVERIFIED.get()
            } else {
                submission.map_or(ctext::REQUEST_READY.get(), Submission::label)
            };
            scroll_body = scroll_body.child(
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
            scroll_body = scroll_body.child(opts_row);
            if let Some(other_idx) = item
                .options
                .iter()
                .position(|opt| is_other_option_label(&opt.label))
                && let Some(other_input) = self
                    .chat
                    .question_other_inputs
                    .get(&(key.clone(), answer_key.clone()))
                    .cloned()
            {
                let req_other = request.clone();
                let send_input = other_input.clone();
                scroll_body = scroll_body.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(gpui_component::input::Input::new(&other_input)),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "chat-q-other-submit-{}-{abs_idx}",
                                pane.get()
                            )))
                            .debug_selector(|| "chat-question-other-submit".into())
                            .label(ctext::OTHER_SUBMIT.get())
                            .small()
                            .primary()
                            .disabled(disabled)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let text = send_input.read(cx).value().to_string();
                                    this.answer_chat_question_other(
                                        req_other.clone(),
                                        other_idx,
                                        &text,
                                        cx,
                                    );
                                },
                            )),
                        ),
                );
            }
        } else if item.kind == ChatItemKind::Permission {
            let live_menu = current
                .then(|| {
                    self.live
                        .get(&pane)
                        .and_then(|live| {
                            krill_proto::screen::prompt::detect(&live.pane.screen().rows)
                        })
                        .or_else(|| {
                            self.chat
                                .logs
                                .get(&key)
                                .and_then(|log| log.meta.live_prompt.clone())
                        })
                })
                .flatten();
            if let Some(krill_proto::screen::prompt::PromptKind::NumberedMenu { options, .. }) =
                live_menu
                && !options.is_empty()
                && options.len() <= 6
            {
                let mut btns = div().w_full().min_w_0().flex().flex_wrap().gap_1p5();
                for (opt_idx, opt) in options.into_iter().enumerate() {
                    let choice_req = request.clone();
                    let label = format!("{}. {}", opt.number.unwrap_or(opt_idx + 1), opt.label);
                    let mut btn = Button::new(SharedString::from(format!(
                        "chat-perm-choice-{}-{abs_idx}-{opt_idx}",
                        pane.get()
                    )))
                    .debug_selector(move || format!("chat-permission-choice-{opt_idx}"))
                    .disabled(disabled)
                    .label(label)
                    .small();
                    btn = if opt_idx == 0 {
                        btn.primary()
                    } else {
                        btn.ghost()
                    };
                    btns = btns.child(btn.on_click(cx.listener(move |this, _, _, cx| {
                        this.approve_chat_permission_choice(choice_req.clone(), opt_idx, cx);
                    })));
                }
                scroll_body = scroll_body.child(btns);
            } else {
                let yes_request = request.clone();
                scroll_body = scroll_body.child(
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
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.approve_chat_permission(yes_request.clone(), true, cx);
                                },
                            )),
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
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.approve_chat_permission(request.clone(), false, cx);
                                },
                            )),
                        ),
                );
            }
        }

        card = card.child(scroll_body);

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
                    .child(card),
            )
            .into_any_element()
    }

    /// Orca's `NativeChatAgentMessageRow` (`dcde358a` / `#46739`, `028100ad` / `#47084`,
    /// `6edf9f05` / `#46659`): plain medium sender name (`+N` if multiple), first-line plain
    /// preview, always-visible chevron when expandable, and a quiet details box on click.
    #[expect(
        clippy::too_many_arguments,
        reason = "notice rendering needs pane/session/row coordinates and adjacency spacing"
    )]
    fn render_inbound_notice(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        abs_idx: usize,
        row_ix: usize,
        notice: &work_run::InboundNotice,
        joins_activity_below: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = ui::palette(cx);
        let muted = palette.muted_foreground;
        let key = (self.layout.host(pane).clone(), session);
        let icon = match notice.kind {
            work_run::NoticeKind::Update => ui::icons::ACTIVITY_SVG,
            work_run::NoticeKind::Message => ui::icons::MESSAGE_SQUARE_SVG,
        };
        let preview = work_run::agent_message_preview_line(&notice.detail);
        let has_body = !notice.detail.trim().is_empty();
        let expandable = has_body || notice.from.len() > 1;
        let open = expandable
            && self
                .chat
                .logs
                .get(&key)
                .is_some_and(|log| log.is_notice_expanded(abs_idx));
        let toggles = expandable
            && (preview.more
                || preview.line.is_empty()
                || preview.line.chars().count() > 48
                || open);

        let first_sender = notice
            .from
            .first()
            .map_or_else(|| ctext::ASSISTANT.get().to_owned(), Clone::clone);
        let extra_senders = notice.from.len().saturating_sub(1);
        let sender_summary = if extra_senders > 0 {
            format!("{first_sender} +{extra_senders}")
        } else {
            first_sender.clone()
        };
        let aria_line = match notice.kind {
            work_run::NoticeKind::Update => ctext::NOTICE_UPDATE
                .get()
                .replace("{name}", &sender_summary),
            work_run::NoticeKind::Message => ctext::NOTICE_MESSAGE
                .get()
                .replace("{name}", &sender_summary),
        };

        let sender_el = div()
            .debug_selector(|| "chat-notice-sender".into())
            .when(first_sender.chars().count() <= 10, gpui::Styled::flex_none)
            .when(first_sender.chars().count() > 10, |d| {
                d.flex_shrink(1.).min_w(gpui::px(96.))
            })
            .flex()
            .items_center()
            .gap_1()
            .overflow_hidden()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(palette.foreground)
                    .child(first_sender),
            )
            .when(extra_senders > 0, |d| {
                d.child(
                    div()
                        .flex_none()
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(muted)
                        .child(format!("+{extra_senders}")),
                )
            });

        let chevron: AnyElement = if toggles {
            div()
                .debug_selector(|| "chat-notice-chevron".into())
                .flex_none()
                .flex()
                .items_center()
                .child(
                    gpui_component::Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .with_size(ui::ICON_SM)
                    .text_color(muted),
                )
                .into_any_element()
        } else {
            div()
                .flex_none()
                .size(gpui::px(ui::ICON_SM))
                .into_any_element()
        };

        let has_preview = !open && !preview.line.is_empty();
        let preview_line = preview.line;

        let header = div()
            .id(SharedString::from(format!(
                "chat-notice-toggle-{}-{abs_idx}",
                pane.get()
            )))
            .debug_selector(|| "chat-notice-toggle".into())
            .w_full()
            .min_w_0()
            .min_h(gpui::px(24.))
            .overflow_hidden()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(ui::TEXT_SM)
            .text_color(muted)
            .when(toggles, |d| {
                d.role(gpui::Role::Button)
                    .aria_label(aria_line)
                    .aria_expanded(open)
                    .tab_index(0)
                    .cursor_pointer()
                    .rounded(ui::RADIUS_XS)
                    .hover(|s| s.text_color(palette.foreground))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(log) = this
                            .chat
                            .logs
                            .get_mut(&(this.layout.host(pane).clone(), session))
                        {
                            log.toggle_notice(abs_idx);
                        }
                        cx.notify();
                    }))
            })
            .child(
                div().flex_none().flex().items_center().child(
                    gpui_component::Icon::default()
                        .data(icon)
                        .with_size(ui::ICON_MD)
                        .text_color(muted),
                ),
            )
            .child(sender_el)
            .when(has_preview, |d| {
                d.child(
                    div()
                        .debug_selector(|| "chat-notice-preview".into())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(muted)
                        .child(preview_line),
                )
            })
            .when(!has_preview, |d| d.child(div().flex_1().min_w_0()))
            .child(chevron);

        let is_copied = self
            .chat
            .copied_item
            .as_ref()
            .is_some_and(|(copied_key, idx, at)| {
                *copied_key == key && *idx == abs_idx && at.elapsed() < Duration::from_secs(2)
            });
        let detail_for_copy = notice.detail.clone();
        let copy_key = key.clone();

        let details = open.then(|| {
            div()
                .debug_selector(|| "chat-notice-details".into())
                .ml_5()
                .mt_1()
                .rounded(ui::RADIUS_MD)
                .bg(palette.muted.opacity(0.6))
                .px_3()
                .py_2()
                .flex()
                .flex_col()
                .gap_1p5()
                .when(notice.from.len() > 1, |d| {
                    d.child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(muted)
                            .child(notice.from.join(", ")),
                    )
                })
                .when(has_body, |d| {
                    d.child(
                        div()
                            .min_w_0()
                            .text_size(ui::TEXT_SM)
                            .text_color(palette.foreground)
                            .child(gpui_component::SelectableText::new(
                                SharedString::from(format!(
                                    "chat-notice-body-{}-{abs_idx}",
                                    pane.get()
                                )),
                                notice.detail.clone(),
                            )),
                    )
                    .child(
                        div().flex().justify_end().child(
                            Button::new(SharedString::from(format!(
                                "chat-notice-copy-{}-{abs_idx}",
                                pane.get()
                            )))
                            .ghost()
                            .xsmall()
                            .label(if is_copied {
                                ctext::COPIED.get()
                            } else {
                                ctext::COPY.get()
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        detail_for_copy.clone(),
                                    ));
                                    this.mark_chat_item_copied(copy_key.clone(), abs_idx, cx);
                                    cx.notify();
                                },
                            )),
                        ),
                    )
                })
        });

        div()
            .id(SharedString::from(format!(
                "chat-notice-{}-{row_ix}",
                pane.get()
            )))
            .debug_selector(|| "chat-notice".into())
            // Orca `#46806` / `#46659`: fixed 6px gap between adjacent 1-line activity/message rows.
            .when(joins_activity_below, |d| d.pb(gpui::px(6.)))
            .when(!joins_activity_below, gpui::Styled::pb_4)
            .w_full()
            .min_w_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                    .flex()
                    .flex_col()
                    .child(header)
                    .children(details),
            )
            .into_any_element()
    }

    /// Orca's bot item note row (`6edf9f05`, `c9e22e23`, `028100ad`): a list mark, then the
    /// change with the item chip using full available row width before truncating while keeping
    /// at least `6rem` (`96px`) readable on narrow rows, and any comment/detail yielding first.
    fn render_item_note(
        pane: super::super::PaneId,
        row_ix: usize,
        note: &work_run::ItemNote,
        joins_activity_below: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use krill_proto::bots::BotItemStatus;
        use work_run::{NoteAction, NoteActor};
        let palette = ui::palette(cx);
        let status_name = |s: BotItemStatus| match s {
            BotItemStatus::Planned => krill_app_core::i18n::catalog::bots::STATUS_PLANNED.get(),
            BotItemStatus::Working => krill_app_core::i18n::catalog::bots::STATUS_WORKING.get(),
            BotItemStatus::Waiting => krill_app_core::i18n::catalog::bots::STATUS_WAITING.get(),
            BotItemStatus::NeedsYou => krill_app_core::i18n::catalog::bots::STATUS_NEEDS_YOU.get(),
            BotItemStatus::Done => krill_app_core::i18n::catalog::bots::STATUS_DONE.get(),
            BotItemStatus::Canceled => krill_app_core::i18n::catalog::bots::STATUS_CANCELED.get(),
        };
        let owner = note.actor == NoteActor::Owner;
        let line = match (&note.action, owner) {
            (NoteAction::Added, false) => ctext::NOTE_ADDED.get().to_owned(),
            (NoteAction::Added, true) => ctext::NOTE_YOU_ADDED.get().to_owned(),
            (NoteAction::Commented, false) => ctext::NOTE_COMMENTED.get().to_owned(),
            (NoteAction::Commented, true) => ctext::NOTE_YOU_COMMENTED.get().to_owned(),
            (NoteAction::Linked(link), _) => ctext::NOTE_LINKED.get().replace("{link}", link),
            (NoteAction::Updated, false) => ctext::NOTE_UPDATED.get().to_owned(),
            (NoteAction::Updated, true) => ctext::NOTE_YOU_CHANGED.get().to_owned(),
            // Drawn by `render_memory_note` and `render_create_offer`.
            (NoteAction::Memory(receipt), _) => receipt.summary(),
            (NoteAction::CreateOffer(offer), _) => offer.name.clone(),
            (NoteAction::Status(BotItemStatus::Working), false) => {
                ctext::NOTE_STARTED.get().to_owned()
            }
            (NoteAction::Status(BotItemStatus::Done), false) => ctext::NOTE_MARKED.get().to_owned(),
            (NoteAction::Status(BotItemStatus::Done), true) => {
                ctext::NOTE_YOU_MARKED.get().to_owned()
            }
            (NoteAction::Status(BotItemStatus::Canceled), false) => {
                ctext::NOTE_CANCELED.get().to_owned()
            }
            (NoteAction::Status(BotItemStatus::Canceled), true) => {
                ctext::NOTE_YOU_CANCELED.get().to_owned()
            }
            (NoteAction::Status(BotItemStatus::NeedsYou), false) => {
                ctext::NOTE_ASKED.get().to_owned()
            }
            (NoteAction::Status(status), false) => ctext::NOTE_MOVED
                .get()
                .replace("{status}", status_name(*status)),
            (NoteAction::Status(status), true) => ctext::NOTE_YOU_MOVED
                .get()
                .replace("{status}", status_name(*status)),
        };
        let (lead, tail) = line.split_once("{item}").unwrap_or((line.as_str(), ""));
        let (glyph, glyph_color): (&'static [u8], gpui::Hsla) = match note.status {
            BotItemStatus::Planned => (ui::icons::CIRCLE_SVG, palette.muted_foreground),
            BotItemStatus::Working => (ui::icons::CIRCLE_SVG, palette.warning),
            BotItemStatus::Waiting => (ui::status::BOT_ITEM_WAITING_SVG, palette.muted_foreground),
            BotItemStatus::NeedsYou => (ui::icons::TRIANGLE_ALERT_SVG, palette.warning),
            BotItemStatus::Done => (ui::icons::CIRCLE_CHECK_SVG, palette.success),
            BotItemStatus::Canceled => (ui::icons::CIRCLE_X_SVG, palette.muted_foreground),
        };
        let short_chip = note.title.chars().count() <= 10;
        let chip = div()
            .id(SharedString::from(format!(
                "chat-item-note-chip-{}-{row_ix}",
                pane.get()
            )))
            .debug_selector(|| "chat-item-note-chip".into())
            .when(short_chip, gpui::Styled::flex_none)
            .when(!short_chip, |d| d.flex_shrink(1.).min_w(gpui::px(96.)))
            .overflow_hidden()
            .px_1p5()
            .rounded(ui::RADIUS_SM)
            .bg(palette.foreground.opacity(0.07))
            .hover(|d| d.bg(palette.foreground.opacity(0.12)))
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_1()
            .text_size(ui::TEXT_XS)
            .text_color(palette.foreground)
            .child(
                div().flex_none().flex().items_center().child(
                    gpui_component::Icon::default()
                        .data(glyph)
                        .with_size(ui::ICON_XS)
                        .text_color(glyph_color),
                ),
            )
            .child(div().min_w_0().truncate().child(note.title.clone()))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                // Focus this pane first so its bot panel opens even in split view.
                this.change(false, cx, |l| l.focus(pane));
                // The chip opens the bot's items (Orca opens the item in its Items tab).
                if this.panel_collapsed {
                    this.toggle_panel(cx);
                }
                this.bot_ui.panel.tab = super::super::bot_panel::BotPanelTab::Home;
                this.bot_ui.panel.settings = None;
                cx.notify();
            }));
        let muted = palette.muted_foreground;
        div()
            .id(SharedString::from(format!(
                "chat-item-note-{}-{row_ix}",
                pane.get()
            )))
            .debug_selector(|| "chat-item-note".into())
            // Orca `#46806` / `#46659`: fixed 6px gap between adjacent 1-line activity rows.
            .when(joins_activity_below, |d| d.pb(gpui::px(6.)))
            .when(!joins_activity_below, gpui::Styled::pb_4)
            .w_full()
            .min_w_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                    .min_h(gpui::px(24.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(ui::TEXT_SM)
                    .text_color(muted)
                    .child(
                        div().flex_none().flex().items_center().child(
                            gpui_component::Icon::default()
                                .data(ui::icons::LIST_TODO_SVG)
                                .with_size(ui::ICON_MD)
                                .text_color(muted),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h(gpui::px(24.))
                            .overflow_hidden()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when(!lead.trim().is_empty(), |d| {
                                d.child(div().flex_none().child(lead.trim().to_owned()))
                            })
                            .child(chip)
                            .when(!tail.trim().is_empty(), |d| {
                                d.child(div().flex_none().child(tail.trim().to_owned()))
                            })
                            .when(!note.detail.is_empty(), |d| {
                                d.child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_color(palette.foreground)
                                        .child(format!(": {}", note.detail)),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    /// Orca's `BotServerOutageCard` (the remote terminal's recovery card) for a bot on a remote host
    /// that is down: retrying first, then Reconnect once retries stop. The chat stays as last read.
    fn render_bot_outage_card(
        &mut self,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let krill_app_core::hosts::HostId::Remote(host_id) = &key.0 else {
            return None;
        };
        self.chat_bot(key)?;
        let host_id = host_id.clone();
        let (label, status) = self
            .sidebar
            .read(cx)
            .remote_hosts()
            .iter()
            .find(|h| h.id == host_id)
            .map(|h| (h.label.clone(), h.status))?;
        if status == krill_app_core::hosts::HostStatus::Connected {
            self.chat.outage_since.remove(&host_id);
            return None;
        }
        let since = *self
            .chat
            .outage_since
            .entry(host_id.clone())
            .or_insert_with(std::time::Instant::now);
        let stopped = status == krill_app_core::hosts::HostStatus::Disconnected;
        let palette = ui::palette(cx);
        let host = [("host", label.as_str())];
        let (title, body) = if stopped {
            (
                ctext::SERVER_DOWN_TITLE.format(&host),
                ctext::SERVER_DOWN_BODY.format(&host),
            )
        } else {
            (
                ctext::SERVER_RECONNECTING_TITLE.format(&host),
                ctext::SERVER_RECONNECTING_BODY.get().to_owned(),
            )
        };
        let secs = since.elapsed().as_secs();
        let down_for = if secs < 60 {
            ctext::SERVER_DOWN_SECS.format(&[("n", &secs.to_string())])
        } else {
            ctext::SERVER_DOWN_MINS.format(&[("n", &(secs / 60).to_string())])
        };
        let reconnect_host = key.0.clone();
        Some(
            div()
                .flex_none()
                .w_full()
                .min_h(gpui::px(80.))
                .px_3()
                .pb_3()
                .flex()
                .items_end()
                .justify_center()
                .child(
                    div()
                        .id("chat-bot-server-outage")
                        .debug_selector(|| "chat-bot-server-outage".into())
                        .w_full()
                        .max_w(gpui::px(576.))
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded(ui::RADIUS_MD)
                        .border_1()
                        .border_color(palette.border)
                        .bg(palette.card.opacity(0.95))
                        .px_3()
                        .py_3()
                        .child(
                            div()
                                .size_8()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(ui::RADIUS_MD)
                                .border_1()
                                .border_color(palette.border)
                                .bg(palette.muted)
                                .text_color(palette.muted_foreground)
                                .child(
                                    gpui_component::Icon::new(if stopped {
                                        IconName::ServerOff
                                    } else {
                                        IconName::LoaderCircle
                                    })
                                    .with_size(ui::ICON_LG),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .flex()
                                        .items_baseline()
                                        .gap_2()
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(ui::TEXT_LG)
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(palette.card_foreground)
                                                .child(title),
                                        )
                                        .child(
                                            div()
                                                .ml_auto()
                                                .flex_none()
                                                .text_size(ui::TEXT_SM)
                                                .text_color(palette.muted_foreground)
                                                .child(down_for),
                                        ),
                                )
                                .child(
                                    div()
                                        .mt_0p5()
                                        .text_size(ui::TEXT_SM)
                                        .text_color(palette.muted_foreground)
                                        .child(body),
                                ),
                        )
                        .when(stopped, |card| {
                            card.child(
                                Button::new("chat-bot-server-reconnect")
                                    .debug_selector(|| "chat-bot-server-reconnect".into())
                                    .label(ctext::SERVER_RECONNECT.get())
                                    .primary()
                                    .small()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let host = reconnect_host.clone();
                                        this.sidebar.update(cx, |sidebar, cx| {
                                            sidebar.reconnect_host(host, cx);
                                        });
                                    })),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    /// Orca's `BotPausedBanner`: across the top of a paused bot's page until it is resumed.
    fn render_bot_paused_banner(
        &self,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let bot = self.chat_bot(key).filter(|bot| bot.paused)?;
        let bot_host = bot.host.clone();
        let bot_id = bot.id.clone();
        let palette = ui::palette(cx);
        Some(
            div()
                .id("chat-bot-paused-banner")
                .debug_selector(|| "chat-bot-paused-banner".into())
                .role(gpui::Role::Status)
                .flex_none()
                .w_full()
                .flex()
                .items_center()
                .gap_2p5()
                .border_b_1()
                .border_color(palette.warning.opacity(0.3))
                .bg(palette.warning.opacity(0.1))
                .py_1p5()
                .pr_2()
                .pl_3()
                .text_size(ui::TEXT_SM)
                .child(
                    gpui_component::Icon::new(IconName::Pause)
                        .with_size(ui::ICON_LG)
                        .text_color(palette.warning),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(palette.foreground)
                        .child(ctext::PAUSED_BANNER.get()),
                )
                .child(
                    Button::new("chat-bot-paused-resume")
                        .debug_selector(|| "chat-bot-paused-resume".into())
                        .label(ctext::PAUSED_RESUME.get())
                        .outline()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_bot_paused_on(&bot_host, bot_id.clone(), false, cx);
                        })),
                )
                .into_any_element(),
        )
    }

    /// Orca `NativeChatPaneHeader`: 봇 채팅 패인 우상단 테두리 없는 3개 뮤트 아이콘.
    #[inline(never)]
    fn render_bot_chat_top_controls(
        pane: super::super::PaneId,
        bot_id: krill_proto::bots::BotId,
        is_split: bool,
        muted_fg: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let new_chat_bot = bot_id.clone();
        let settings_bot = bot_id;
        div()
            .id(SharedString::from(format!(
                "chat-top-controls-{}",
                pane.get()
            )))
            .debug_selector(move || format!("chat-top-controls-{}", pane.get()))
            .group(CHAT_TOP_CONTROLS_GROUP)
            .absolute()
            .top_2()
            .right_3()
            .flex()
            .items_center()
            .gap_0p5()
            .text_color(muted_fg)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(ui::button_with_tooltip(
                Button::new(("chat-bot-new-chat", pane.get()))
                    .debug_selector(move || format!("chat-bot-new-chat-{}", pane.get()))
                    .icon(gpui_component::Icon::new(
                        gpui_kit_assets::IconName::MessageSquarePlus,
                    ))
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        let host = this.layout.host(pane).clone();
                        this.start_bot_session_on(&host, new_chat_bot.clone(), None, cx);
                    })),
                format!("chat-bot-new-chat-{}", pane.get()),
                krill_app_core::i18n::catalog::bots::BP_NEW_CHAT.get(),
            ))
            .child(ui::button_with_tooltip(
                Button::new(("chat-split-right", pane.get()))
                    .debug_selector(move || format!("chat-split-right-{}", pane.get()))
                    .icon(gpui_component::Icon::new(
                        gpui_kit_assets::IconName::SquareSplitHorizontal,
                    ))
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.split_pane(pane, krill_app_core::layout::Axis::Horizontal, cx);
                    })),
                format!("chat-split-right-{}", pane.get()),
                krill_app_core::i18n::catalog::shell::SPLIT_RIGHT.get(),
            ))
            .child(ui::button_with_tooltip(
                Button::new(("chat-bot-settings", pane.get()))
                    .debug_selector(move || format!("chat-bot-settings-{}", pane.get()))
                    .icon(IconName::Settings)
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        let host = this.layout.host(pane).clone();
                        if !this.open_bot_settings_in_panel_on(&host, &settings_bot, cx) {
                            this.open_settings(
                                krill_app_core::settings::AdminTab::Appearance,
                                Some(window),
                                cx,
                            );
                        }
                    })),
                format!("chat-bot-settings-{}", pane.get()),
                krill_app_core::i18n::catalog::general::SETTINGS_HINT.get(),
            ))
            .when(is_split, |d| {
                d.child(
                    div()
                        .flex_none()
                        .invisible()
                        .group_hover(CHAT_TOP_CONTROLS_GROUP, Styled::visible)
                        .child(ui::button_with_tooltip(
                            Button::new(("chat-pane-close", pane.get()))
                                .debug_selector(move || format!("chat-pane-close-{}", pane.get()))
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.maybe_close_pane(pane, cx);
                                })),
                            format!("chat-pane-close-{}", pane.get()),
                            krill_app_core::i18n::catalog::shell::CLOSE_PANE.get(),
                        )),
                )
            })
    }

    /// 일반 하이브리드 채팅 패인 우상단 컨트롤(분할 + 터미널 전환 알약).
    #[inline(never)]
    fn render_standard_chat_top_controls(
        pane: super::super::PaneId,
        is_split: bool,
        focused: bool,
        palette: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let border = palette.border;
        let split_right_btn = ui::button_with_tooltip(
            Button::new(("chat-split-right", pane.get()))
                .debug_selector(move || format!("chat-split-right-{}", pane.get()))
                .icon(gpui_component::Icon::default().data(ui::icons::SPLIT_HORIZONTAL_SVG))
                .rounded_full()
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.split_pane(pane, krill_app_core::layout::Axis::Horizontal, cx);
                })),
            format!("chat-split-right-{}", pane.get()),
            krill_app_core::i18n::catalog::shell::SPLIT_RIGHT.get(),
        );
        let split_down_btn = ui::button_with_tooltip(
            Button::new(("chat-split-down", pane.get()))
                .debug_selector(move || format!("chat-split-down-{}", pane.get()))
                .icon(gpui_component::Icon::default().data(ui::icons::SPLIT_VERTICAL_SVG))
                .rounded_full()
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.split_pane(pane, krill_app_core::layout::Axis::Vertical, cx);
                })),
            format!("chat-split-down-{}", pane.get()),
            krill_app_core::i18n::catalog::shell::SPLIT_DOWN.get(),
        );
        let hover_bg = palette.popover.opacity(0.92);
        let hover_controls = div()
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(ui::transparent())
            .bg(ui::transparent())
            .px_1()
            .py_0p5()
            .flex()
            .items_center()
            .gap_0p5()
            .group_hover(CHAT_TOP_CONTROLS_GROUP, move |d| {
                d.bg(hover_bg).border_color(border).shadow_lg()
            })
            .child(
                div()
                    .flex_none()
                    .invisible()
                    .group_hover(CHAT_TOP_CONTROLS_GROUP, Styled::visible)
                    .child(split_down_btn),
            )
            .child(
                div()
                    .flex_none()
                    .when(!focused, |d| {
                        d.invisible()
                            .group_hover(CHAT_TOP_CONTROLS_GROUP, Styled::visible)
                    })
                    .when(focused, |d| {
                        d.rounded_full()
                            .border_1()
                            .border_color(border)
                            .bg(hover_bg)
                            .shadow_lg()
                            .opacity(0.85)
                            .group_hover(CHAT_TOP_CONTROLS_GROUP, |s| {
                                s.opacity(1.0)
                                    .bg(ui::transparent())
                                    .border_color(ui::transparent())
                            })
                    })
                    .child(split_right_btn),
            )
            .when(is_split, |d| {
                d.child(
                    div()
                        .flex_none()
                        .invisible()
                        .group_hover(CHAT_TOP_CONTROLS_GROUP, Styled::visible)
                        .child(ui::button_with_tooltip(
                            Button::new(("chat-pane-close", pane.get()))
                                .debug_selector(move || format!("chat-pane-close-{}", pane.get()))
                                .icon(IconName::Close)
                                .rounded_full()
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.maybe_close_pane(pane, cx);
                                })),
                            format!("chat-pane-close-{}", pane.get()),
                            krill_app_core::i18n::catalog::shell::CLOSE_PANE.get(),
                        )),
                )
            });
        let primary_pill = div()
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(border)
            .bg(hover_bg)
            .shadow_lg()
            .px_1()
            .py_0p5()
            .flex()
            .items_center()
            .gap_0p5()
            .when(focused, |d| {
                d.opacity(0.85)
                    .group_hover(CHAT_TOP_CONTROLS_GROUP, |s| s.opacity(1.0))
            })
            .child(
                Button::new(SharedString::from(format!(
                    "desktop-chat-switch-term-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-switch-terminal".into())
                .icon(IconName::SquareTerminal)
                .label(ctext::TERM.get())
                .tooltip(krill_app_core::i18n::catalog::shell::TOGGLE_CHAT_VIEW.get())
                .rounded_full()
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.switch_pane_to_terminal(pane, window, cx);
                })),
            );
        div()
            .id(SharedString::from(format!(
                "chat-top-controls-{}",
                pane.get()
            )))
            .debug_selector(move || format!("chat-top-controls-{}", pane.get()))
            .group(CHAT_TOP_CONTROLS_GROUP)
            .absolute()
            .top_2()
            .right_3()
            .flex()
            .items_center()
            .gap_1()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(hover_controls)
            .child(primary_pill)
    }

    /// Orca's `BotHomeBanner`: the bot's face and name in a pill over its Home chat; it opens the
    /// bot's page in the right panel. Automatically hidden at pane widths `<= 264px` (`bot-home-banner.css`).
    fn render_bot_home_banner(
        &self,
        pane: super::super::PaneId,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self
            .chat
            .pane_widths
            .get(&pane)
            .is_some_and(|w| *w <= gpui::px(264.))
        {
            return None;
        }
        let bot = self
            .chat_bot(key)
            .filter(|bot| bot.home_session().is_some_and(|h| h.session_id == key.1))?;
        let palette = ui::palette(cx);
        let label = ctext::HOME_BANNER_LABEL.format(&[("name", bot.name.as_str())]);
        Some(
            div()
                .absolute()
                .top_2()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "chat-bot-home-banner-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-bot-home-banner".into())
                        .role(gpui::Role::Button)
                        .aria_label(label)
                        .tab_index(0)
                        .cursor_pointer()
                        .h_8()
                        .max_w(gpui::relative(0.6))
                        .pl_1()
                        .pr_3()
                        .rounded_full()
                        .border_1()
                        .border_color(palette.foreground.opacity(0.06))
                        .bg(palette.notice_surface())
                        .shadow_lg()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(ui::TEXT_LG)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(palette.foreground)
                        .child(self.bot_avatar(bot, 24., gpui::px(12.), palette))
                        .child(
                            div()
                                .min_w_0()
                                .max_w(gpui::px(200.))
                                .truncate()
                                .child(bot.name.clone()),
                        )
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Focus this pane first so its bot panel opens even in split view.
                            this.change(false, cx, |l| l.focus(pane));
                            if this.panel_collapsed {
                                this.toggle_panel(cx);
                            }
                            this.bot_ui.panel.tab = super::super::bot_panel::BotPanelTab::Home;
                            this.bot_ui.panel.settings = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// Orca's `NativeChatAutomationRunRow` for a turn a schedule started: "🕒 Scheduled · name",
    /// the time on hover, and pressed, its instructions with Copy instructions.
    fn render_scheduled_run(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        abs_idx: usize,
        name: &str,
        item: &ChatItem,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = ui::palette(cx);
        let muted = palette.muted_foreground;
        let key = (self.layout.host(pane).clone(), session);
        let open = self
            .chat
            .logs
            .get(&key)
            .is_some_and(|log| log.is_user_message_expanded(abs_idx));
        let group = SharedString::from(format!("chat-scheduled-{}-{abs_idx}", pane.get()));
        let time = Self::chat_message_time(pane, abs_idx, &item.at, muted);
        let header = div()
            .id(SharedString::from(format!(
                "chat-scheduled-run-{}-{abs_idx}",
                pane.get()
            )))
            .debug_selector(|| "chat-scheduled-run".into())
            .group(group.clone())
            .role(gpui::Role::Button)
            .aria_expanded(open)
            .tab_index(0)
            .cursor_pointer()
            .w_full()
            .min_w_0()
            .min_h(gpui::px(24.))
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(ui::TEXT_LG)
            .child(
                gpui_component::Icon::new(IconName::Clock)
                    .with_size(ui::ICON_LG)
                    .text_color(muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_none()
                            .text_color(muted)
                            .child(format!("{} · ", ctext::SCHEDULED_RUN.get())),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(palette.foreground)
                            .child(name.to_owned()),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(!open, |d| {
                        d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                    })
                    .child(
                        gpui_component::Icon::new(if open {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .with_size(ui::ICON_MD)
                        .text_color(muted),
                    )
                    .children(time),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(log) = this
                    .chat
                    .logs
                    .get_mut(&(this.layout.host(pane).clone(), session))
                {
                    log.toggle_user_message(abs_idx);
                }
                cx.notify();
            }));
        let prompt = item.text.trim().to_owned();
        let card = open.then(|| {
            let copy = prompt.clone();
            div()
                .ml_6()
                .mt_1()
                .rounded(ui::RADIUS_LG)
                .border_1()
                .border_color(palette.border)
                .bg(palette.card)
                .px_2()
                .py_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(ui::TEXT_SM)
                        .text_color(muted)
                        .child(ctext::RUN_INSTRUCTIONS.get()),
                )
                .child(
                    div()
                        .text_size(ui::TEXT_LG)
                        .text_color(palette.foreground)
                        .child(prompt),
                )
                .child(
                    div().mt_2().flex().child(
                        div()
                            .id(SharedString::from(format!(
                                "chat-scheduled-copy-{}-{abs_idx}",
                                pane.get()
                            )))
                            .debug_selector(|| "chat-scheduled-copy".into())
                            .cursor_pointer()
                            .text_size(ui::TEXT_SM)
                            .text_color(muted)
                            .underline()
                            .hover(|s| s.text_color(palette.foreground))
                            .child(ctext::COPY_INSTRUCTIONS.get())
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    copy.clone(),
                                ));
                            }),
                    ),
                )
        });
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
                    .flex_col()
                    .child(header)
                    .children(card),
            )
            .into_any_element()
    }

    /// Orca's `NativeChatMessageTimestamp`: `오후 5:07` (yesterday, this year and older carry their
    /// date), the full date and time on hover.
    fn chat_message_time(
        pane: super::super::PaneId,
        abs_idx: usize,
        at: &str,
        color: gpui::Hsla,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let epoch = krill_proto::time::parse_reset_epoch(at.trim())?;
        let epoch = if epoch > 10_000_000_000 {
            epoch / 1_000
        } else {
            epoch
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())?;
        let (short, full) = krill_app_core::chat_message_time(epoch, now)?;
        let tip_id = format!("chat-time-{}-{abs_idx}", pane.get());
        Some(
            div()
                .id(SharedString::from(tip_id.clone()))
                .debug_selector(|| "chat-message-time".into())
                .flex_none()
                .whitespace_nowrap()
                .text_size(ui::TEXT_SM)
                .text_color(color)
                .tooltip(move |window, cx| {
                    ui::tooltip_view(tip_id.clone(), full.clone(), window, cx)
                })
                .child(short),
        )
    }

    /// Orca's `NativeChatWorkingStatus` once a turn settles: the bot's face, "16s 작업 완료" and a
    /// chevron that opens the work folded behind the answer, over a rule.
    #[expect(clippy::too_many_arguments, reason = "turn bar row coordinates")]
    fn render_turn_bar(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        user_abs_idx: usize,
        worked_secs: u64,
        folds: bool,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = ui::palette(cx);
        let key = (self.layout.host(pane).clone(), session);
        let face = self
            .chat_bot(&key)
            .map(|bot| self.bot_avatar(bot, 24., ui::RADIUS_SM, palette));
        let label =
            ctext::WORKED_FOR.format(&[("time", &work_run::format_turn_duration(worked_secs))]);
        let muted = palette.muted_foreground;
        let line = div()
            .id(SharedString::from(format!(
                "chat-turn-bar-{}-{user_abs_idx}",
                pane.get()
            )))
            .debug_selector(move || format!("chat-turn-bar-{user_abs_idx}"))
            .w_full()
            .min_w_0()
            .min_h(gpui::px(32.))
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(palette.border)
            .text_size(ui::TEXT_LG)
            .text_color(muted)
            .children(face.map(|face| div().flex_none().mr_1().child(face)))
            .child(div().min_w_0().truncate().child(label))
            .when(folds, |line| {
                line.role(gpui::Role::Button)
                    .aria_label(ctext::TOGGLE_TURN_DETAILS.get())
                    .aria_expanded(expanded)
                    .tab_index(0)
                    .cursor_pointer()
                    .hover(|s| s.text_color(palette.foreground))
                    .child(
                        gpui_component::Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .with_size(ui::ICON_MD),
                    )
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let key = (this.layout.host(pane).clone(), session);
                        if this
                            .chat
                            .logs
                            .get_mut(&key)
                            .is_some_and(|log| log.toggle_turn(user_abs_idx))
                        {
                            this.recompute_chat_find(&key);
                        }
                        cx.notify();
                    }))
            });
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
                    .child(line),
            )
            .into_any_element()
    }

    /// Orca's `NativeChatBotCreateOfferCard`: "New bot", the name and its purpose, then Not now and
    /// Create bot. Created, it says so and opens the new bot.
    fn render_create_offer(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        offer: &krill_proto::bots::BotOffer,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = ui::palette(cx);
        let muted = palette.muted_foreground;
        let key = (self.layout.host(pane).clone(), session);
        let created = offer.created.as_ref().and_then(|id| {
            self.sidebar_bots
                .iter()
                .find(|b| b.host == key.0 && &b.id == id)
        });
        let face = match created {
            Some(bot) => self.bot_avatar(bot, 32., ui::RADIUS_LG, palette),
            None => krill_bots_ui::bots::avatar::render(
                &offer_avatar(offer),
                None,
                &offer.name,
                gpui::px(32.),
                ui::RADIUS_LG,
                palette,
            ),
        };
        let status = if offer.created.is_some() {
            Some((ctext::OFFER_CREATED.get(), palette.success))
        } else if offer.dismissed {
            Some((ctext::OFFER_DISMISSED.get(), muted))
        } else {
            None
        };
        let offer_id = offer.offer_id.clone();
        let footer = if let Some(bot_id) = offer.created.clone() {
            let open_host = key.0.clone();
            Some(
                div().flex().justify_end().child(
                    Button::new("chat-offer-open")
                        .debug_selector(|| "chat-offer-open".into())
                        .label(ctext::OFFER_OPEN.get())
                        .outline()
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_bot_home_on(&open_host, bot_id.clone(), None, cx);
                        })),
                ),
            )
        } else if offer.dismissed {
            None
        } else {
            let creating = self.chat.creating_offers.contains(&offer.offer_id);
            let (create_offer, dismiss_id, dismiss_key, create_key) =
                (offer.clone(), offer_id.clone(), key.clone(), key.clone());
            Some(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("chat-offer-not-now-{offer_id}")))
                            .debug_selector(|| "chat-offer-not-now".into())
                            .label(ctext::OFFER_NOT_NOW.get())
                            .outline()
                            .small()
                            .disabled(creating)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.resolve_bot_offer(&dismiss_key, &dismiss_id, None, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("chat-offer-create-{offer_id}")))
                            .debug_selector(|| "chat-offer-create".into())
                            .label(ctext::OFFER_CREATE.get())
                            .primary()
                            .small()
                            .disabled(creating)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.create_offered_bot(&create_key, &create_offer, cx);
                            })),
                    ),
            )
        };
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
                    .child(
                        div()
                            .id(SharedString::from(format!("chat-offer-{}", offer.offer_id)))
                            .debug_selector(|| "chat-create-offer".into())
                            .w_full()
                            .min_w_0()
                            .rounded(ui::RADIUS_XL)
                            .border_1()
                            .border_color(palette.border)
                            .bg(palette.card)
                            .text_size(ui::TEXT_LG)
                            .child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .p_3()
                                    .child(div().flex_none().mt_0p5().child(face))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_baseline()
                                                    .gap_x_2()
                                                    .text_size(ui::TEXT_SM)
                                                    .text_color(muted)
                                                    .child(ctext::OFFER_HEADING.get())
                                                    .children(status.map(|(text, color)| {
                                                        div()
                                                            .ml_auto()
                                                            .flex()
                                                            .items_center()
                                                            .gap_1p5()
                                                            .text_color(color)
                                                            .child(
                                                                div()
                                                                    .size_1p5()
                                                                    .rounded_full()
                                                                    .bg(color),
                                                            )
                                                            .child(text)
                                                    })),
                                            )
                                            .child(
                                                div()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .text_color(palette.foreground)
                                                    .child(offer.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_size(ui::TEXT_SM)
                                                    .text_color(muted)
                                                    .child(offer.purpose.clone()),
                                            ),
                                    ),
                            )
                            .children(footer.map(|footer| {
                                div()
                                    .border_t_1()
                                    .border_color(palette.border)
                                    .px_3()
                                    .py_2()
                                    .child(footer)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Orca's `NativeChatMemoryUpdatedRow`: "Memory updated · 1 added >" and, opened, each change
    /// with an Open memory link to the bot's Soul and memory settings.
    fn render_memory_note(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        row_ix: usize,
        at_ms: u64,
        receipt: &work_run::MemoryReceipt,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = ui::palette(cx);
        let muted = palette.muted_foreground;
        let open = self.chat.open_memory_notes.contains(&at_ms);
        let label = |text: &'static str| {
            div()
                .flex_none()
                .mr_2()
                .text_size(ui::TEXT_SM)
                .text_color(muted)
                .child(text)
        };
        let fact_row = |lead: &'static str, text: &str, struck: bool| {
            div()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_baseline()
                .child(label(lead))
                .child(
                    div()
                        .min_w_0()
                        .text_color(palette.foreground)
                        .when(struck, gpui::Styled::line_through)
                        .child(text.to_owned()),
                )
        };
        let trigger = div()
            .id(SharedString::from(format!(
                "chat-memory-note-{}-{row_ix}",
                pane.get()
            )))
            .debug_selector(|| "chat-memory-note".into())
            .role(gpui::Role::Button)
            .aria_expanded(open)
            .tab_index(0)
            .cursor_pointer()
            .max_w_full()
            .py_0p5()
            .flex()
            .items_start()
            .gap_1()
            .text_size(ui::TEXT_LG)
            .text_color(muted)
            .hover(|s| s.text_color(palette.foreground))
            .child(div().min_w_0().child(receipt.summary()))
            .child(
                div().flex_none().mt_1().child(
                    gpui_component::Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .with_size(ui::ICON_MD),
                ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.chat.open_memory_notes.remove(&at_ms) {
                    this.chat.open_memory_notes.insert(at_ms);
                }
                let key = (this.layout.host(pane).clone(), session);
                if let Some(log) = this.chat.logs.get_mut(&key) {
                    log.revision = log.revision.wrapping_add(1);
                }
                cx.notify();
            }));
        let receipt_body = open.then(|| {
            div()
                .mt_1()
                .py_1()
                .pl_3()
                .border_l_2()
                .border_color(palette.border)
                .flex()
                .flex_col()
                .gap_2()
                .text_size(ui::TEXT_LG)
                .children(
                    receipt
                        .added
                        .iter()
                        .map(|fact| fact_row(ctext::MEMORY_ROW_ADDED.get(), fact, false)),
                )
                .children(
                    receipt
                        .removed
                        .iter()
                        .map(|fact| fact_row(ctext::MEMORY_ROW_REMOVED.get(), fact, true)),
                )
                .when(receipt.other_changes, |d| {
                    d.child(
                        div()
                            .text_color(palette.foreground)
                            .child(ctext::MEMORY_INDEX_CHANGED.get()),
                    )
                })
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "chat-memory-open-{}-{row_ix}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-memory-open".into())
                        .cursor_pointer()
                        .text_size(ui::TEXT_SM)
                        .text_color(muted)
                        .underline()
                        .hover(|s| s.text_color(palette.foreground))
                        .child(ctext::MEMORY_OPEN.get())
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Focus this pane first so its bot panel opens even in split view.
                            this.change(false, cx, |l| l.focus(pane));
                            // The chat is on its bot's page, so its panel shows this bot.
                            if this.panel_collapsed {
                                this.toggle_panel(cx);
                            }
                            this.open_bot_settings_page(
                                krill_bots_ui::bots::DetailTab::SoulMemory,
                                cx,
                            );
                        })),
                )
        });
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
                    .pb_2()
                    .flex()
                    .flex_col()
                    .items_start()
                    .child(trigger)
                    .children(receipt_body),
            )
            .into_any_element()
    }

    fn render_pending_chat_card(
        &mut self,
        key: ChatKey,
        pane: PaneId,
        pending: &super::PendingChatPrompt,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = ui::palette(cx);
        let pending_id = pending.id;
        let is_review = matches!(pending.status, super::PendingStatus::ReviewRequired);
        let is_copied =
            self.chat
                .copied_pending
                .as_ref()
                .is_some_and(|(copied_key, copied_id, _)| {
                    *copied_key == key && *copied_id == pending_id
                });

        let pending_images: Vec<Option<std::path::PathBuf>> = pending
            .attachments
            .iter()
            .filter(|a| a.is_image)
            .map(|a| Some(a.preview_path().to_path_buf()))
            .collect();
        let pending_numbers: Vec<usize> = (1..=pending_images.len()).collect();
        let gallery = (!pending_images.is_empty())
            .then(|| self.collect_chat_gallery_images(&key, self.layout.host(pane).is_remote()));
        let image_strip = self
            .chat_message_images(
                &format!("chat-pending-{pending_id}-{}", pane.get()),
                &pending_images,
                &pending_numbers,
                gallery.as_ref(),
                cx,
            )
            .map(gpui::Styled::mb_0p5);
        let file_attachments: Vec<&super::DraftAttachment> =
            pending.attachments.iter().filter(|a| !a.is_image).collect();
        let attachment_strip = (!file_attachments.is_empty()).then(|| {
            div().flex().flex_wrap().items_center().gap_1().children(
                file_attachments.into_iter().map(|attachment| {
                    let badge = format!(
                        "{} ({})",
                        attachment.display_name,
                        super::catalog::format_byte_len(attachment.byte_len)
                    );
                    div()
                        .max_w(gpui::px(260.))
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(palette.border)
                        .bg(palette.card)
                        .px_2()
                        .py_0p5()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_size(ui::TEXT_XS)
                        .text_color(palette.muted_foreground)
                        .child(
                            gpui_component::Icon::new(IconName::File)
                                .size(gpui::px(ui::ICON_SM))
                                .text_color(palette.muted_foreground),
                        )
                        .child(div().min_w_0().truncate().child(badge))
                }),
            )
        });

        let trimmed_text = pending.display_text.trim();
        let text_block = (!trimmed_text.is_empty()).then(|| {
            div()
                .min_w_0()
                .text_size(ui::TEXT_MD)
                .text_color(palette.foreground)
                .whitespace_normal()
                .child(pending.display_text.clone())
        });

        let bubble = div()
            .id(SharedString::from(format!(
                "chat-pending-card-{pending_id}-{}",
                pane.get()
            )))
            .debug_selector(|| "chat-pending-card".into())
            .min_w_0()
            .max_w(gpui::relative(0.80))
            .overflow_hidden()
            .rounded(ui::RADIUS_XL)
            .border_1()
            .border_color(palette.border)
            .bg(palette.muted)
            .px_3p5()
            .py_2p5()
            .flex()
            .flex_col()
            .gap_1p5()
            .children(image_strip)
            .children(attachment_strip)
            .children(text_block);

        let retry_key = key.clone();
        let copy_key = key;
        let copy_text = pending.display_text.clone();
        let status_row = if is_review {
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_2()
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(palette.destructive)
                        .child(ctext::PENDING_REVIEW_REQUIRED.get()),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "chat-pending-retry-{pending_id}-{}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-pending-retry".into())
                    .label(ctext::PENDING_RETRY.get())
                    .xsmall()
                    .outline()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.retry_pending_chat_prompt(&retry_key, pending_id, window, cx);
                    })),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "chat-pending-copy-{pending_id}-{}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-pending-copy".into())
                    .label(if is_copied {
                        ctext::COPIED.get()
                    } else {
                        ctext::COPY.get()
                    })
                    .xsmall()
                    .outline()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                        this.mark_chat_pending_copied(copy_key.clone(), pending_id, cx);
                    })),
                )
        } else {
            let status_label = match pending.status {
                super::PendingStatus::Queued => ctext::PENDING_QUEUED.get(),
                super::PendingStatus::Sending | super::PendingStatus::ReviewRequired => {
                    ctext::PENDING_SENDING.get()
                }
            };
            div().w_full().flex().items_center().justify_end().child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(palette.muted_foreground)
                    .child(status_label),
            )
        };

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
                    .flex_col()
                    .items_end()
                    .gap_1()
                    .child(bubble)
                    .child(status_row),
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
        if self.layout.focused() == Some(pane) {
            self.apply_pending_draft(window, cx);
            if matches!(&self.chat.focus_pending, Some(ChatFocus::Composer(id)) if *id == key) {
                input_state.update(cx, |state, cx| state.focus(window, cx));
                self.chat.focus_pending = None;
            }
        }
        self.sync_bot_item_notes(&key, cx);
        let palette = ui::palette(cx);
        let bg = palette.chat_canvas();
        let border = palette.border;
        let muted_fg = palette.muted_foreground;

        let host = self.layout.host(pane).clone();
        let agent_state = self.session_agent_state(&host, session, cx);
        // A bot chat folds each settled turn behind its answer, as Orca's do.
        let fold = self
            .chat_bot(&key)
            .is_some()
            .then_some(agent_state == krill_proto::control::AgentState::Working);
        if self
            .chat
            .logs
            .get_mut(&key)
            .is_some_and(|log| log.set_turn_fold(fold))
        {
            self.recompute_chat_find(&key);
        }
        let unread = host.is_local() && self.sidebar.read(cx).is_unread(session);
        let poll_failed = self.chat.poll_failed.contains(&key);

        let (bound, revision, epoch, display_feed, active_elapsed_label) =
            match self.chat.logs.get_mut(&key) {
                Some(log) => {
                    let feed = log.display_feed();
                    let elapsed = if agent_state == krill_proto::control::AgentState::Working {
                        feed.rows().last().and_then(|row| match row {
                            work_run::DisplayRow::WorkRun(run) => log
                                .run_elapsed_secs(run.span.start(), std::time::Instant::now())
                                .map(work_run::format_elapsed_secs),
                            work_run::DisplayRow::Item { .. }
                            | work_run::DisplayRow::QuestionBatch { .. }
                            | work_run::DisplayRow::UnreadDivider { .. }
                            | work_run::DisplayRow::ContextCleared { .. }
                            | work_run::DisplayRow::BotCard { .. }
                            | work_run::DisplayRow::ItemNote { .. }
                            | work_run::DisplayRow::Notice { .. }
                            | work_run::DisplayRow::TurnBar { .. } => None,
                        })
                    } else {
                        None
                    };
                    (log.bound, log.revision, log.epoch, feed, elapsed)
                }
                None => (false, 0, 0, work_run::DisplayFeed::empty(), None),
            };

        let mut other_keys = Vec::new();
        for row in display_feed.rows() {
            if let work_run::DisplayRow::Item { abs_idx, item } = row
                && item.kind == ChatItemKind::Question
                && item
                    .options
                    .iter()
                    .any(|opt| is_other_option_label(&opt.label))
            {
                other_keys.push(format!("{}:{abs_idx}", item.tool_id));
            }
        }
        for answer_key in other_keys {
            self.ensure_question_other_input(&key, &answer_key, window, cx);
        }

        let narrow_pane = self
            .chat
            .pane_widths
            .get(&pane)
            .is_some_and(|w| *w <= gpui::px(264.));
        let home_pill = !narrow_pane
            && self
                .chat_bot(&key)
                .is_some_and(|bot| bot.home_session().is_some_and(|h| h.session_id == key.1));
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
            // Orca: the transcript opens below the Home pill (8px + 32px), 48px down.
            .when(home_pill, |d| d.pt(gpui::px(48.)))
            .flex()
            .flex_col()
            .gap_2()
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener({
                    let sel_key = key.clone();
                    move |_, e: &gpui::MouseUpEvent, window, cx| {
                        let sel_key = sel_key.clone();
                        let pos = e.position;
                        let weak = cx.weak_entity();
                        window.defer(cx, move |window, cx| {
                            weak.update(cx, |this, cx| {
                                this.sync_chat_selection_quote(&sel_key, Some(pos), window, cx);
                            })
                            .ok();
                        });
                    }
                }),
            );

        let pending_slice = self
            .chat
            .pending_prompts
            .get(&key)
            .map_or(&[][..], Vec::as_slice);
        let pending_cards: Arc<[super::PendingChatPrompt]> = Arc::from(pending_slice);
        let pending_len = pending_cards.len();
        let pending_rev = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for item in pending_slice {
                item.id.hash(&mut hasher);
                item.status.hash(&mut hasher);
                let copied = self
                    .chat
                    .copied_pending
                    .as_ref()
                    .is_some_and(|(ck, cid, _)| *ck == key && *cid == item.id);
                copied.hash(&mut hasher);
            }
            hasher.finish()
        };

        let persona = (!poll_failed)
            .then(|| self.chat_bot(&key))
            .flatten()
            .map(|bot| {
                let avatar = self.bot_avatar(bot, 56., ui::RADIUS_XL, palette);
                let name = [("name", bot.name.as_str())];
                // Orca `BotHomeChat` / `BotFirstScreen`: avatar + bot name only (no greeting suffix)
                // + SOUL.md first paragraph in muted_foreground.
                let subtitle = self
                    .chat
                    .bot_souls
                    .get(&(host.clone(), bot.id.clone()))
                    .cloned()
                    .unwrap_or_else(|| ctext::BOT_EMPTY_SUBTITLE.format(&name));
                let agent_label = super::composer::agent_display_name(
                    krill_proto::agent::canonical_id(&bot.agent_id),
                );
                let first_setup =
                    !bound && agent_state == krill_proto::control::AgentState::Working;
                (avatar, bot.name.clone(), subtitle, agent_label, first_setup)
            });
        if display_feed.is_empty() && pending_len == 0 {
            let badge_bg = if poll_failed {
                palette.destructive.opacity(0.1)
            } else {
                palette.accent
            };
            let badge_icon = if poll_failed {
                IconName::TriangleAlert
            } else {
                IconName::MessageSquare
            };
            let badge_fg = if poll_failed {
                palette.destructive
            } else {
                muted_fg
            };
            feed = feed.child(
                div()
                    .id("chat-empty")
                    .debug_selector(|| "chat-empty".into())
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if this.layout.focused() != Some(pane) {
                                this.change(false, cx, |l| l.focus(pane));
                                this.files.refocus = false;
                            }
                            if let Some(chat_key) = this.chat_key(pane) {
                                let input = this.ensure_chat_input(chat_key, window, cx);
                                input.update(cx, |state, cx| state.focus(window, cx));
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .flex_1()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .items_center()
                    .gap_3()
                    .p_6()
                    .map(|empty| match persona {
                        // Orca `BotHomeChat` / `BotFirstScreen` / `BotHowToAsk`: 56px avatar,
                        // bot name only, SOUL.md first paragraph, and optional first-run steps.
                        Some((avatar, title, subtitle, agent_label, first_setup)) => empty
                            .child(avatar)
                            .child(
                                div()
                                    .debug_selector(|| "chat-bot-empty-name".into())
                                    .text_align(gpui::TextAlign::Center)
                                    .text_size(ui::TEXT_XL)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(palette.foreground)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .max_w(gpui::px(384.))
                                    .text_align(gpui::TextAlign::Center)
                                    .text_size(ui::TEXT_SM)
                                    .text_color(muted_fg)
                                    .child(if first_setup {
                                        ctext::BOT_FIRST_SUBTITLE
                                            .format(&[("agent", agent_label.as_str())])
                                    } else {
                                        subtitle
                                    }),
                            )
                            .when(first_setup, |d| {
                                d.child(Self::render_bot_first_screen_steps(
                                    &agent_label,
                                    palette,
                                    cx,
                                ))
                            })
                            .when(!first_setup, |d| {
                                d.child(
                                    div()
                                        .debug_selector(|| "chat-bot-how-to-ask".into())
                                        .mt_1()
                                        .max_w(gpui::px(384.))
                                        .rounded(ui::RADIUS_MD)
                                        .border_1()
                                        .border_color(border)
                                        .bg(palette.muted.opacity(0.35))
                                        .px_3()
                                        .py_2()
                                        .text_align(gpui::TextAlign::Center)
                                        .text_size(ui::TEXT_XS)
                                        .text_color(muted_fg)
                                        .child(ctext::BOT_HOW_TO_ASK_HINT.get()),
                                )
                            }),
                        None => empty
                            .child(
                                div()
                                    .size(gpui::px(48.))
                                    .rounded_full()
                                    .bg(badge_bg)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        gpui_component::Icon::new(badge_icon)
                                            .with_size(ui::ICON_XL)
                                            .text_color(badge_fg),
                                    ),
                            )
                            .child(
                                div()
                                    .text_align(gpui::TextAlign::Center)
                                    .text_size(ui::TEXT_LG)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(palette.foreground)
                                    .child(ctext::EMPTY_TITLE.get()),
                            )
                            .child(
                                div()
                                    .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                                    .text_align(gpui::TextAlign::Center)
                                    .text_size(ui::TEXT_SM)
                                    .text_color(muted_fg)
                                    .child(if poll_failed {
                                        ctext::POLL_FAILED.get()
                                    } else if bound {
                                        ctext::EMPTY_BOUND.get()
                                    } else {
                                        ctext::EMPTY_UNBOUND.get()
                                    }),
                            ),
                    }),
            );
        } else {
            let at_history_cap = self.chat.logs.get(&key).is_some_and(|log| {
                log.items.len() >= super::MAX_CHAT_KEPT && log.earlier.is_some()
            });
            if at_history_cap {
                feed = feed.child(
                    div()
                        .id(SharedString::from(format!(
                            "chat-history-cap-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-history-cap".into())
                        .w_full()
                        .flex_none()
                        .py_1()
                        .px_3()
                        .flex()
                        .justify_center()
                        .child(
                            div().text_size(ui::TEXT_XS).text_color(muted_fg).child(
                                ctext::EARLIER_HISTORY_CAP
                                    .get()
                                    .replace("{n}", &super::MAX_CHAT_KEPT.to_string()),
                            ),
                        ),
                );
            }
            let owner = cx.weak_entity();
            let list = self.chat.lists.entry((pane, session)).or_insert_with(|| {
                ChatList::new(
                    host,
                    &display_feed,
                    pending_len,
                    pending_rev,
                    revision,
                    epoch,
                    owner,
                )
            });
            let list_state = list.sync(&display_feed, pending_len, pending_rev, revision, epoch);
            self.trigger_load_earlier_if_near_top(pane, &key, cx);
            let owner = cx.weak_entity();
            let rows = Arc::clone(&display_feed);
            let pending_rows = Arc::clone(&pending_cards);
            let row_count = rows.len();
            let list_state_for_rows = list_state.clone();
            let pending_key = key.clone();
            feed = feed.child(
                gpui::list(list_state.clone(), move |row_ix, _, cx| {
                    if row_ix >= rows.len() {
                        let Some(pending_item) = pending_rows.get(row_ix - rows.len()) else {
                            return div().into_any_element();
                        };
                        let key_for_row = pending_key.clone();
                        return owner
                            .update(cx, |this, cx| {
                                this.render_pending_chat_card(key_for_row, pane, pending_item, cx)
                            })
                            .unwrap_or_else(|_| div().into_any_element());
                    }
                    let Some(row) = rows.get(row_ix) else {
                        return div().into_any_element();
                    };
                    let joins_activity_below = rows
                        .get(row_ix + 1)
                        .is_some_and(|next| next.activity_edges().0);
                    owner
                        .update(cx, |this, cx| {
                            this.render_display_row(
                                pane,
                                session,
                                row,
                                row_ix,
                                row_count,
                                joins_activity_below,
                                &list_state_for_rows,
                                agent_state,
                                cx,
                            )
                        })
                        .unwrap_or_else(|_| div().into_any_element())
                })
                .size_full(),
            );
            // Vertical turn minimap rail when there are >= 4 user turns.
            let mut user_turns = Vec::new();
            for row_ix in 0..display_feed.len() {
                if let Some(work_run::DisplayRow::Item { item, .. }) = display_feed.get(row_ix)
                    && item.kind == ChatItemKind::User
                {
                    let turn_num = user_turns.len() + 1;
                    let preview: String = item
                        .text
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(48)
                        .collect();
                    user_turns.push((turn_num, row_ix, preview));
                }
            }
            if user_turns.len() >= 4 {
                // Orca `NativeChatMessageRail` (`#47553`): light the turn whose row is at or
                // immediately above the viewport top, even when scroll stops between two rows.
                let scroll_top_row = list_state.logical_scroll_top().item_ix;
                let active_turn_num = user_turns
                    .iter()
                    .rfind(|(_, row_ix, _)| *row_ix <= scroll_top_row)
                    .or_else(|| user_turns.first())
                    .map(|(n, _, _)| *n);
                let sampled: Vec<(usize, usize, String)> = if user_turns.len() <= 16 {
                    user_turns
                } else {
                    const MAX_PIPS: usize = 16;
                    (0..MAX_PIPS)
                        .filter_map(|i| {
                            let idx = (i * (user_turns.len() - 1)) / (MAX_PIPS - 1);
                            user_turns.get(idx).cloned()
                        })
                        .collect()
                };
                let roomy = sampled.len() <= 10;
                feed = feed.child(
                    div()
                        .absolute()
                        .right_2p5()
                        .top_16()
                        .bottom_4()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "chat-turn-minimap-{}",
                                    pane.get()
                                )))
                                .debug_selector(|| "chat-turn-minimap".into())
                                .w_6()
                                .flex()
                                .flex_col()
                                .items_center()
                                .opacity(0.45)
                                .hover(|s| s.opacity(1.0))
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .children(sampled.into_iter().map(
                                    |(turn_num, row_ix, preview)| {
                                        let jump_list = list_state.clone();
                                        let turn_label = ctext::MINIMAP_TURN
                                            .get()
                                            .replace("{n}", &turn_num.to_string());
                                        let tip = if preview.is_empty() {
                                            turn_label
                                        } else {
                                            format!("{turn_label}: {preview}")
                                        };
                                        let id =
                                            format!("chat-minimap-turn-{}-{turn_num}", pane.get());
                                        let lit = active_turn_num == Some(turn_num);
                                        // Contiguous vertical padding (no gap) so the pointer never
                                        // falls between two ticks (`NativeChatMessageRail.tsx`).
                                        div()
                                            .id(SharedString::from(id.clone()))
                                            .debug_selector(move || {
                                                format!("chat-minimap-turn-{turn_num}")
                                            })
                                            .w_full()
                                            .when(roomy, gpui::Styled::py_1)
                                            .when(!roomy, gpui::Styled::py_0p5)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .tooltip(move |window, cx| {
                                                ui::tooltip_view(
                                                    id.clone(),
                                                    tip.clone(),
                                                    window,
                                                    cx,
                                                )
                                            })
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                jump_list.scroll_to(gpui::ListOffset {
                                                    item_ix: row_ix,
                                                    offset_in_item: gpui::px(0.),
                                                });
                                                cx.notify();
                                            }))
                                            .child(
                                                div()
                                                    .h(gpui::px(3.))
                                                    .rounded_full()
                                                    .when(lit, |b| b.w_5().bg(palette.foreground))
                                                    .when(!lit, |b| {
                                                        b.w_3().bg(palette.foreground.opacity(0.5))
                                                    }),
                                            )
                                    },
                                )),
                        ),
                );
            }

            // Floating affordance occupies no feed space, so it cannot move the composer.
            if !list_state.is_following_tail() {
                feed = feed.child(
                    div()
                        .absolute()
                        .bottom_3()
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .rounded_full()
                                .border_1()
                                .border_color(border)
                                .bg(palette.popover)
                                .shadow_lg()
                                .child({
                                    let jump_key = key.clone();
                                    Button::new("chat-jump-latest")
                                        .debug_selector(|| "chat-jump-latest".into())
                                        .label(ctext::JUMP_LATEST.get())
                                        .icon(IconName::ArrowDown)
                                        .rounded_full()
                                        .small()
                                        .ghost()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.mark_chat_read(&jump_key);
                                            list_state.set_follow_mode(gpui::FollowMode::Tail);
                                            cx.notify();
                                        }))
                                }),
                        ),
                );
            }
        }

        // Orca `#47164`: when an active permission card appears and the composer isn't already
        // focused with user typing, focus the permission card once so Enter picks its default answer.
        if self.layout.focused() == Some(pane)
            && !gpui::Focusable::focus_handle(input_state.read(cx), cx).is_focused(window)
            && input_state.read(cx).value().trim().is_empty()
            && !self.chat.finds.contains_key(&key)
        {
            for row in display_feed.rows().iter().rev() {
                if let work_run::DisplayRow::Item { abs_idx, item } = row
                    && item.kind == ChatItemKind::Permission
                {
                    let req = self.chat_request_identity(pane, session, *abs_idx, item);
                    if self.chat_request_is_current(&req, cx) {
                        let focus_key = (key.clone(), req.key.clone());
                        if self.chat.approval_auto_focused.insert(focus_key.clone()) {
                            let handle = self
                                .chat
                                .approval_focus
                                .entry(focus_key)
                                .or_insert_with(|| cx.focus_handle())
                                .clone();
                            window.focus(&handle, cx);
                        }
                    }
                    break;
                }
            }
        }

        // Orca's `BotServerOutageCard` stands where the composer was while its server is down.
        self.sync_chat_image_tokens(&key, &input_state, window, cx);
        self.sync_side_chat_pill(&key, window, cx);
        self.ensure_agy_version(cx);
        let composer = match self.render_bot_outage_card(&key, cx) {
            Some(card) => card,
            None => self.render_chat_composer(pane, key.clone(), &input_state, cx),
        };
        let paused_banner = self.render_bot_paused_banner(&key, cx);
        let worker_auto_close_banner = self.render_worker_auto_close_banner(pane, &key, cx);
        let resume_toast = self.render_resume_toast(&key, cx);
        let home_banner = self.render_bot_home_banner(pane, &key, cx);

        // Orca #47157: floating `Add to chat` pill when text is selected in chat replies.
        let selection_quote_btn = if self.layout.focused() == Some(pane)
            && !self.chat_pane_has_focus(pane, window, cx)
            && !self.chat_find_is_open(cx)
        {
            let live_selected = gpui_component::TextSelection::selected_text(window, cx);
            let active_text = if live_selected.trim().is_empty() {
                self.chat.selection_quote_dismissed = None;
                self.chat
                    .selection_quote
                    .as_ref()
                    .filter(|sq| sq.key == key && !sq.text.trim().is_empty())
                    .map(|sq| sq.text.clone())
            } else if self.chat.selection_quote_dismissed.as_deref() == Some(live_selected.as_str())
            {
                None
            } else {
                Some(live_selected)
            };
            active_text.map(|selected| {
                super::selection_quote::render_add_to_chat_floating_button(
                    pane,
                    key.clone(),
                    selected,
                    cx,
                )
            })
        } else {
            None
        };
        if let Some(btn) = selection_quote_btn {
            feed = feed.child(btn);
        }

        let live_line = (agent_state == krill_proto::control::AgentState::Working)
            .then(|| self.pane_live_status_line(pane))
            .flatten();
        let turn_status_line =
            (agent_state == krill_proto::control::AgentState::Working).then(|| {
                let status_text = match &live_line {
                    Some(line) => format!("{} · {line}", ctext::STATE_WORKING.get()),
                    None => ctext::STATE_WORKING.get().to_owned(),
                };
                div()
                    .id(SharedString::from(format!(
                        "chat-turn-status-{}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-turn-status".into())
                    .flex_none()
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .pt_1()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ui::status::status_glyph(
                                agent_state,
                                unread,
                                ui::status::Size::Sm,
                                cx,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(muted_fg)
                                    .child(status_text),
                            )
                            .children(active_elapsed_label.map(|elapsed| {
                                div()
                                    .flex_none()
                                    .font_family(ui::FONT_MONO)
                                    .text_size(ui::TEXT_XS)
                                    .text_color(muted_fg)
                                    .child(elapsed)
                            })),
                    )
            });

        let is_split = self.layout.visible().len() > 1;
        let focused = self.layout.focused() == Some(pane);
        let chat_bot_id = self.chat_bot(&key).map(|b| b.id.clone());
        let top_controls = match chat_bot_id {
            Some(bot_id) => {
                Self::render_bot_chat_top_controls(pane, bot_id, is_split, muted_fg, cx)
            }
            None => Self::render_standard_chat_top_controls(pane, is_split, focused, palette, cx),
        };

        let find_bar = self.chat.finds.get(&key).map(|find| {
            let find_key = key.clone();
            let count_text = if find.query.trim().is_empty() {
                String::new()
            } else if find.matches.is_empty() {
                ctext::FIND_NO_MATCH.get().to_owned()
            } else {
                ctext::FIND_COUNT
                    .get()
                    .replace("{cur}", &(find.active + 1).to_string())
                    .replace("{total}", &find.matches.len().to_string())
            };
            let has_matches = !find.matches.is_empty();
            let prev_key = key.clone();
            let next_key = key.clone();
            let close_key = key.clone();
            div()
                .id(SharedString::from(format!("chat-find-bar-{}", pane.get())))
                .debug_selector(|| "chat-find-bar".into())
                .key_context(krill_app_core::search::CONTEXT)
                .occlude()
                .absolute()
                .top_12()
                .right_3()
                .w(gpui::px(320.))
                .max_w(gpui::relative(0.9))
                .p_1()
                .rounded(ui::RADIUS_MD)
                .border_1()
                .border_color(border)
                .bg(palette.popover)
                .shadow_lg()
                .flex()
                .items_center()
                .gap_1()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .capture_action(cx.listener({
                    let key = find_key.clone();
                    let find_input = find.input.clone();
                    move |this, action: &gpui_component::input::Enter, window, cx| {
                        if action.secondary
                            || krill_app_core::jump::entity_input_is_composing(
                                &find_input,
                                window,
                                cx,
                            )
                        {
                            return;
                        }
                        let dir = if action.shift || window.modifiers().shift {
                            krill_proto::search::Direction::Previous
                        } else {
                            krill_proto::search::Direction::Next
                        };
                        cx.stop_propagation();
                        this.step_chat_find_for_key(&key, dir, cx);
                    }
                }))
                .capture_action(cx.listener({
                    let key = find_key.clone();
                    let find_input = find.input.clone();
                    move |this, _: &gpui_component::input::Escape, window, cx| {
                        if krill_app_core::jump::entity_input_is_composing(&find_input, window, cx)
                        {
                            return;
                        }
                        cx.stop_propagation();
                        this.close_chat_find(&key, window, cx);
                    }
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(gpui_component::input::Input::new(&find.input)),
                )
                .child(
                    div()
                        .flex_none()
                        .px_1()
                        .font_family(ui::FONT_MONO)
                        .text_size(ui::TEXT_XS)
                        .text_color(muted_fg)
                        .child(count_text),
                )
                .child(
                    Button::new(SharedString::from(format!("chat-find-prev-{}", pane.get())))
                        .debug_selector(|| "chat-find-prev".into())
                        .icon(IconName::ChevronUp)
                        .ghost()
                        .xsmall()
                        .disabled(!has_matches)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.step_chat_find_for_key(
                                &prev_key,
                                krill_proto::search::Direction::Previous,
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("chat-find-next-{}", pane.get())))
                        .debug_selector(|| "chat-find-next".into())
                        .icon(IconName::ChevronDown)
                        .ghost()
                        .xsmall()
                        .disabled(!has_matches)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.step_chat_find_for_key(
                                &next_key,
                                krill_proto::search::Direction::Next,
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "chat-find-close-{}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-find-close".into())
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_chat_find(&close_key, window, cx);
                    })),
                )
        });

        let in_card = self.workspace_stage() == super::super::pane_frame::WorkspaceStage::CardStage;
        let overlay_weak = cx.weak_entity();
        let width_tracker = {
            let weak = cx.weak_entity();
            gpui::canvas(
                move |bounds, _, cx| {
                    let w = bounds.size.width;
                    weak.update(cx, |this, cx| {
                        let prev = this.chat.pane_widths.get(&pane).copied();
                        let crossed =
                            prev.is_none_or(|p| (p <= gpui::px(264.)) != (w <= gpui::px(264.)));
                        if prev != Some(w) {
                            this.chat.pane_widths.insert(pane, w);
                            if crossed {
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                },
                |_, (), _, _| {},
            )
            .absolute()
            .inset_0()
        };

        let inner = div()
            .id(SharedString::from(format!(
                "desktop-chat-overlay-surface-{}",
                pane.get()
            )))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    if this.layout.focused() != Some(pane) {
                        this.change(false, cx, |l| l.focus(pane));
                        this.files.refocus = false;
                    }
                    if !this.chat_pane_has_focus(pane, window, cx) && !this.focus.is_focused(window)
                    {
                        window.focus(&this.focus, cx);
                    }
                }),
            )
            .on_drop(cx.listener({
                let drop_key = key.clone();
                move |this, paths: &gpui::ExternalPaths, window, cx| {
                    this.attach_dropped_paths_to_chat(&drop_key, paths, window, cx);
                }
            }))
            .on_key_down(cx.listener({
                let esc_key = key.clone();
                move |this, e: &gpui::KeyDownEvent, window, cx| {
                    if e.keystroke.key == "escape"
                        && !e.keystroke.modifiers.modified()
                        && this.dismiss_chat_selection_quote(&esc_key, window, cx)
                    {
                        cx.stop_propagation();
                    }
                }
            }))
            .context_menu(move |menu, _, cx| {
                super::super::terminal_menu::build_chat_overlay(menu, &overlay_weak, pane, cx)
            })
            .child(width_tracker)
            // Orca: paused banner and temporary worker auto-close banner sit above the chat.
            .children(paused_banner)
            .children(worker_auto_close_banner)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(feed)
                    .children(self.render_chat_prompt_notices(&key, cx))
                    .children(resume_toast)
                    .children(turn_status_line)
                    .child(composer)
                    .children(home_banner)
                    .child(top_controls)
                    .children(find_bar),
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
                .bg(bg)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        if this.layout.focused() != Some(pane) {
                            this.change(false, cx, |l| l.focus(pane));
                            this.files.refocus = false;
                        }
                        if !this.focus.is_focused(window) {
                            window.focus(&this.focus, cx);
                        }
                    }),
                )
                .child(inner)
                .into_any_element(),
        )
    }

    /// Orca `NativeChatDividerRow.tsx`: thin 1px `border` rules on both sides + quiet `12px`
    /// `muted_foreground` plain text centered with no pill background.
    fn render_divider_row(
        id: SharedString,
        selector: &'static str,
        label: &'static str,
        cx: &gpui::App,
    ) -> AnyElement {
        let palette = ui::palette(cx);
        div()
            .id(id)
            .debug_selector(move || selector.into())
            .aria_label(label)
            .w_full()
            .min_w_0()
            .flex()
            .justify_center()
            .py_2()
            .child(
                div()
                    .w_full()
                    .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().h_px().bg(palette.border))
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui::TEXT_SM)
                            .text_color(palette.muted_foreground)
                            .child(label),
                    )
                    .child(div().flex_1().h_px().bg(palette.border)),
            )
            .into_any_element()
    }

    /// Orca `BotFirstScreen.tsx` setup steps card shown while a new bot starts its first session.
    fn render_bot_first_screen_steps(
        agent_label: &str,
        palette: &ui::Palette,
        cx: &gpui::App,
    ) -> gpui::Stateful<gpui::Div> {
        let muted = palette.muted_foreground;
        div()
            .id("chat-bot-first-screen")
            .debug_selector(|| "chat-bot-first-screen".into())
            .mt_3()
            .w(gpui::px(384.))
            .max_w_full()
            .rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(palette.border)
            .bg(palette.card)
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1p5()
            .text_size(ui::TEXT_LG)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        gpui_component::Icon::new(IconName::Check)
                            .with_size(ui::ICON_MD)
                            .text_color(palette.success),
                    )
                    .child(
                        div()
                            .text_color(palette.foreground)
                            .child(ctext::BOT_FIRST_STEP_CREATE.get()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(ui::status::status_glyph(
                        krill_proto::control::AgentState::Working,
                        false,
                        ui::status::Size::Sm,
                        cx,
                    ))
                    .child(
                        div()
                            .text_color(palette.foreground)
                            .child(ctext::BOT_FIRST_STEP_START.format(&[("agent", agent_label)])),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        gpui_component::Icon::new(IconName::Circle)
                            .with_size(ui::ICON_MD)
                            .text_color(muted),
                    )
                    .child(
                        div()
                            .text_color(muted)
                            .child(ctext::BOT_FIRST_STEP_REPLY.get()),
                    ),
            )
    }

    /// Orca `#47859`: resume toast whose primary Resume button is filled with `theme_accent`.
    fn render_resume_toast(&self, key: &ChatKey, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (toast_key, detail) = self.chat.resume_toast.as_ref()?;
        if toast_key != key {
            return None;
        }
        let palette = ui::palette(cx);
        let accent = palette.theme_accent();
        let text = if detail.is_empty() {
            ctext::RESUME_TOAST_TITLE.get().to_owned()
        } else {
            detail.clone()
        };
        let resume_key = key.clone();
        Some(
            div()
                .w_full()
                .min_w_0()
                .px_3()
                .pt_1()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("chat-resume-toast")
                        .debug_selector(|| "chat-resume-toast".into())
                        .w_full()
                        .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                        .rounded(ui::RADIUS_MD)
                        .border_1()
                        .border_color(palette.border)
                        .bg(palette.popover)
                        .px_3()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(ui::TEXT_SM)
                                .text_color(palette.foreground)
                                .child(text),
                        )
                        .child(
                            div()
                                .id("chat-resume-toast-btn")
                                .debug_selector(|| "chat-resume-toast-btn".into())
                                .role(gpui::Role::Button)
                                .cursor_pointer()
                                .rounded(ui::RADIUS_SM)
                                .bg(accent)
                                .hover(|s| s.opacity(0.9))
                                .px_2p5()
                                .py_1()
                                .text_size(ui::TEXT_XS)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(palette.primary_foreground)
                                .child(ctext::RESUME_TOAST_BTN.get())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.chat.resume_toast = None;
                                    this.continue_after_rate_limit(&resume_key, cx);
                                })),
                        )
                        .child(
                            Button::new("chat-resume-toast-dismiss")
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.chat.resume_toast = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Orca `NativeChatTypedByTag` / `NativeChatConnectorAskerTag` above a user bubble.
    fn render_prompt_sender_tag(
        tag: &super::bot_rows::PromptSenderTag,
        palette: &ui::Palette,
    ) -> gpui::Div {
        let (icon, label) = match tag {
            super::bot_rows::PromptSenderTag::TypedBy(name) => (
                IconName::UserRound,
                ctext::TYPED_BY_LABEL.format(&[("name", name.as_str())]),
            ),
            super::bot_rows::PromptSenderTag::ConnectorAsker { source, name } => (
                IconName::Plug,
                ctext::CONNECTOR_ASKER_LABEL
                    .format(&[("source", source.as_str()), ("name", name.as_str())]),
            ),
        };
        div()
            .debug_selector(|| "chat-prompt-sender-tag".into())
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .text_size(ui::TEXT_2XS)
            .text_color(palette.muted_foreground)
            .child(
                gpui_component::Icon::new(icon)
                    .with_size(ui::ICON_XS)
                    .text_color(palette.muted_foreground),
            )
            .child(label)
    }

    /// Orca `NativeChatAutomationResultRow` & `NativeChatWaitRow`.
    fn render_automation_inline_row(
        pane: super::super::PaneId,
        abs_idx: usize,
        row: &super::bot_rows::AutomationInlineRow,
        at: &str,
        cx: &gpui::App,
    ) -> AnyElement {
        let palette = ui::palette(cx);
        let muted = palette.muted_foreground;
        let time = Self::chat_message_time(pane, abs_idx, at, muted);
        let (icon, lead, detail) = match row {
            super::bot_rows::AutomationInlineRow::Result { name, summary } => (
                IconName::CircleCheck,
                format!("{} · {name}", ctext::AUTOMATION_RESULT_LABEL.get()),
                summary.clone(),
            ),
            super::bot_rows::AutomationInlineRow::Wait { label } => (
                IconName::Hourglass,
                ctext::AUTOMATION_WAIT_LABEL.get().to_owned(),
                label.clone(),
            ),
        };
        div()
            .id(SharedString::from(format!(
                "chat-automation-inline-{}-{abs_idx}",
                pane.get()
            )))
            .debug_selector(|| "chat-automation-inline".into())
            .w_full()
            .min_w_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                    .pb(gpui::px(6.))
                    .min_h(gpui::px(24.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(ui::TEXT_SM)
                    .text_color(muted)
                    .child(
                        gpui_component::Icon::new(icon)
                            .with_size(ui::ICON_MD)
                            .text_color(muted),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(palette.foreground)
                            .child(lead),
                    )
                    .when(!detail.is_empty(), |d| {
                        d.child(div().flex_1().min_w_0().truncate().child(detail))
                    })
                    .children(time),
            )
            .into_any_element()
    }

    /// Orca `NativeChatBotCreatedRow`: quiet event row naming a newly created bot.
    fn render_bot_created_row(
        pane: super::super::PaneId,
        abs_idx: usize,
        bot_name: &str,
        bot_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = ui::palette(cx);
        let muted = palette.muted_foreground;
        div()
            .id(SharedString::from(format!(
                "chat-bot-created-{}-{abs_idx}",
                pane.get()
            )))
            .debug_selector(|| "chat-bot-created".into())
            .w_full()
            .min_w_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                    .pb(gpui::px(6.))
                    .min_h(gpui::px(24.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(ui::TEXT_SM)
                    .text_color(muted)
                    .child(
                        gpui_component::Icon::new(IconName::Bot)
                            .with_size(ui::ICON_MD)
                            .text_color(muted),
                    )
                    .child(div().flex_none().child(ctext::BOT_CREATED_LABEL.get()))
                    .child(
                        div()
                            .px_1p5()
                            .rounded(ui::RADIUS_SM)
                            .bg(palette.foreground.opacity(0.07))
                            .text_size(ui::TEXT_XS)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(palette.foreground)
                            .child(bot_name.to_owned()),
                    )
                    .children(bot_id.map(|id| {
                        Button::new(SharedString::from(format!(
                            "chat-bot-created-open-{}-{abs_idx}",
                            pane.get()
                        )))
                        .label(ctext::OFFER_OPEN.get())
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Ok(bot_id) = krill_proto::bots::BotId::new(&id) {
                                let host = this.layout.host(pane).clone();
                                this.open_bot_home_on(&host, bot_id, None, cx);
                            }
                        }))
                    })),
            )
            .into_any_element()
    }

    /// Orca `NativeChatQuestionCard` for multi-question batches (`#46963`, `#47959`, `#42789`,
    /// `#47564`, `#42515`): shows 1 question at a time with ONLY header title + `· N of M` + `‹ ›`
    /// arrows (no top step bar), orange waiting tone, and `Skip` submitting already-answered steps.
    fn render_question_batch(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        items: &[(usize, Arc<ChatItem>)],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some((first_abs_idx, _)) = items.first() else {
            return div().into_any_element();
        };
        let first_abs_idx = *first_abs_idx;
        let total = items.len();
        let key = (self.layout.host(pane).clone(), session);
        let batch_key = (key.clone(), first_abs_idx);
        let step_idx = self
            .chat
            .question_batch_step
            .get(&batch_key)
            .copied()
            .unwrap_or(0)
            .min(total.saturating_sub(1));
        let (_, item) = &items[step_idx];
        let palette = ui::palette(cx);
        let fg = palette.foreground;
        let muted = palette.muted_foreground;
        let current = items.iter().any(|(idx, it)| {
            let req = self.chat_request_identity(pane, session, *idx, it);
            self.chat_request_is_current(&req, cx)
        });
        let batch_busy = items.iter().any(|(idx, it)| {
            let req = self.chat_request_identity(pane, session, *idx, it);
            self.chat.logs.get(&key).is_some_and(|log| {
                log.submissions
                    .get(&req.key)
                    .is_some_and(super::Submission::blocks_input)
            })
        });
        let interactive = current && !batch_busy;
        let saved_answers = self
            .chat
            .question_batch_answers
            .get(&batch_key)
            .cloned()
            .unwrap_or_default();
        let current_picks = saved_answers.get(&step_idx).cloned().unwrap_or_default();
        let current_answered = !current_picks.is_empty();
        let answered_count = saved_answers.values().filter(|v| !v.is_empty()).count();
        let any_answered = answered_count > 0;
        let is_last = step_idx + 1 >= total;
        let header_title = if item.title.trim().is_empty() {
            ctext::QUESTION.get().to_owned()
        } else {
            item.title.clone()
        };
        let count_text = format!(
            "· {}",
            ctext::QUESTION_STEP_OF.format(&[
                ("cur", &(step_idx + 1).to_string()),
                ("total", &total.to_string()),
            ])
        );

        let prev_key = batch_key.clone();
        let next_key = batch_key.clone();
        // Header band: NO segmented top step bar (`#47959`) — only icon, title, `· N of M`, check, `‹ ›`.
        let header = div()
            .debug_selector(|| "chat-question-batch-header".into())
            .min_h(gpui::px(36.))
            .py_1()
            .pr_1p5()
            .pl_3p5()
            .bg(palette.agent_question.opacity(0.09))
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(ui::TEXT_SM)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(fg)
            .child(
                gpui_component::Icon::new(IconName::MessageCircleQuestionMark)
                    .with_size(ui::ICON_MD)
                    .text_color(palette.agent_question),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap_1()
                    .child(div().min_w_0().truncate().child(header_title))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(muted)
                            .child(count_text),
                    ),
            )
            .when(current_answered, |d| {
                d.child(
                    gpui_component::Icon::new(IconName::Check)
                        .with_size(ui::ICON_XS)
                        .text_color(palette.success),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new(SharedString::from(format!(
                    "chat-qbatch-prev-{}-{first_abs_idx}",
                    pane.get()
                )))
                .debug_selector(|| "chat-question-step-prev".into())
                .icon(IconName::ChevronLeft)
                .tooltip(ctext::QUESTION_PREV.get())
                .ghost()
                .xsmall()
                .disabled(step_idx == 0)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.chat
                        .question_batch_step
                        .insert(prev_key.clone(), step_idx.saturating_sub(1));
                    cx.notify();
                })),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "chat-qbatch-next-{}-{first_abs_idx}",
                    pane.get()
                )))
                .debug_selector(|| "chat-question-step-next".into())
                .icon(IconName::ChevronRight)
                .tooltip(ctext::QUESTION_NEXT.get())
                .ghost()
                .xsmall()
                .disabled(is_last)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.chat
                        .question_batch_step
                        .insert(next_key.clone(), (step_idx + 1).min(total - 1));
                    cx.notify();
                })),
            );

        let mut opts_col = div().w_full().min_w_0().flex().flex_col().gap_1p5();
        for (opt_idx, opt) in item.options.iter().enumerate() {
            let selected = current_picks.contains(&opt_idx);
            let pick_key = batch_key.clone();
            let multi = item.multi_select;
            let opt_border = if selected {
                palette.agent_question
            } else {
                palette.border
            };
            let opt_bg = if selected {
                palette.agent_question.opacity(0.10)
            } else {
                palette.muted.opacity(0.35)
            };
            opts_col = opts_col.child(
                div()
                    .id(SharedString::from(format!(
                        "chat-qbatch-opt-{}-{first_abs_idx}-{step_idx}-{opt_idx}",
                        pane.get()
                    )))
                    .debug_selector(move || format!("chat-question-batch-option-{opt_idx}"))
                    .w_full()
                    .min_w_0()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(opt_border)
                    .bg(opt_bg)
                    .px_3()
                    .py_2()
                    .flex()
                    .items_start()
                    .gap_2()
                    .when(interactive, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.bg(palette.muted.opacity(0.65)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let batch_map = this
                                    .chat
                                    .question_batch_answers
                                    .entry(pick_key.clone())
                                    .or_default();
                                if multi {
                                    let entry = batch_map.entry(step_idx).or_default();
                                    if let Some(pos) = entry.iter().position(|&v| v == opt_idx) {
                                        entry.remove(pos);
                                    } else {
                                        entry.push(opt_idx);
                                    }
                                    if entry.is_empty() {
                                        batch_map.remove(&step_idx);
                                    }
                                    if batch_map.is_empty() {
                                        this.chat.question_batch_answers.remove(&pick_key);
                                    }
                                } else {
                                    batch_map.insert(step_idx, vec![opt_idx]);
                                    if step_idx + 1 < total {
                                        this.chat
                                            .question_batch_step
                                            .insert(pick_key.clone(), step_idx + 1);
                                    }
                                }
                                cx.notify();
                            }))
                    })
                    .child(
                        div()
                            .w(ui::SPACE_4)
                            .flex_none()
                            .text_size(ui::TEXT_SM)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(if selected {
                                palette.agent_question
                            } else {
                                muted
                            })
                            .child(if multi {
                                if selected {
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
                            .gap_0p5()
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(fg)
                                    .child(opt.label.clone()),
                            )
                            .when(!opt.description.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(muted)
                                        .child(opt.description.clone()),
                                )
                            }),
                    ),
            );
        }

        let skip_key = batch_key.clone();
        let submit_key = batch_key.clone();
        let submit_items = items.to_vec();
        let skip_items = items.to_vec();
        // `#42789`: Skipping on the last step sends any already-answered steps instead of dropping them.
        let other_answered_count = saved_answers
            .iter()
            .filter(|(s, v)| **s != step_idx && !v.is_empty())
            .count();
        let skip_label = if is_last && any_answered && !current_answered {
            ctext::SUBMIT.format(&[("n", &answered_count.to_string())])
        } else {
            ctext::QUESTION_SKIP.get().to_owned()
        };
        let footer = div()
            .border_t_1()
            .border_color(palette.border)
            .px_3p5()
            .py_2()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(
                Button::new(SharedString::from(format!(
                    "chat-qbatch-skip-{}-{first_abs_idx}",
                    pane.get()
                )))
                .debug_selector(|| "chat-question-batch-skip".into())
                .label(skip_label)
                .outline()
                .small()
                .disabled(!interactive || (is_last && other_answered_count == 0))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !is_last {
                        this.chat
                            .question_batch_step
                            .insert(skip_key.clone(), step_idx + 1);
                        cx.notify();
                    } else if let Some(answers) =
                        this.chat.question_batch_answers.get(&skip_key).cloned()
                    {
                        // Submit all previously answered steps in ascending order (`#42789`).
                        let batch: Vec<_> = answers
                            .into_iter()
                            .filter(|(s, picks)| *s != step_idx && !picks.is_empty())
                            .filter_map(|(ans_step, picks)| {
                                let (ans_abs, ans_item) = skip_items.get(ans_step)?;
                                let req =
                                    this.chat_request_identity(pane, session, *ans_abs, ans_item);
                                Some((req, picks, ans_item.multi_select))
                            })
                            .collect();
                        if !batch.is_empty() {
                            this.answer_chat_question_batch(batch, cx);
                        }
                    }
                })),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "chat-qbatch-confirm-{}-{first_abs_idx}",
                    pane.get()
                )))
                .debug_selector(|| "chat-question-batch-confirm".into())
                .label(if is_last {
                    ctext::OTHER_SUBMIT.get()
                } else {
                    ctext::QUESTION_NEXT_BTN.get()
                })
                .primary()
                .small()
                .disabled(!interactive || (!current_answered && !any_answered))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !is_last {
                        this.chat
                            .question_batch_step
                            .insert(submit_key.clone(), step_idx + 1);
                        cx.notify();
                    } else if let Some(answers) =
                        this.chat.question_batch_answers.get(&submit_key).cloned()
                    {
                        let batch: Vec<_> = answers
                            .into_iter()
                            .filter(|(_, picks)| !picks.is_empty())
                            .filter_map(|(ans_step, picks)| {
                                let (ans_abs, ans_item) = submit_items.get(ans_step)?;
                                let req =
                                    this.chat_request_identity(pane, session, *ans_abs, ans_item);
                                Some((req, picks, ans_item.multi_select))
                            })
                            .collect();
                        if !batch.is_empty() {
                            this.answer_chat_question_batch(batch, cx);
                        }
                    }
                })),
            );

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
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "chat-question-batch-{}-{first_abs_idx}",
                                pane.get()
                            )))
                            .debug_selector(|| "chat-question-batch".into())
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .rounded(ui::RADIUS_XL)
                            .border_1()
                            .border_color(if current {
                                palette.agent_question.opacity(0.65)
                            } else {
                                palette.border
                            })
                            .bg(palette.chat_composer_surface())
                            .flex()
                            .flex_col()
                            .child(header)
                            .child(
                                // Unified `pt_3()` top padding matching single-question cards (`#47959`).
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .px_3p5()
                                    .pt_3()
                                    .pb_3()
                                    .flex()
                                    .flex_col()
                                    .gap_2p5()
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_HEADING)
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(fg)
                                            .child(item.text.clone()),
                                    )
                                    .child(opts_col),
                            )
                            .child(footer),
                    ),
            )
            .into_any_element()
    }

    /// Orca `NativeChatCardFrame.tsx`: shared rounded-xl card frame used by all bot offer & setup cards.
    #[expect(
        clippy::too_many_arguments,
        reason = "card frame slots mirror NativeChatCardFrame.tsx"
    )]
    fn render_chat_card_frame(
        id: SharedString,
        selector: &'static str,
        avatar: Option<AnyElement>,
        heading: &str,
        status: Option<(&'static str, gpui::Hsla)>,
        title: &str,
        body: Option<AnyElement>,
        footer: Option<AnyElement>,
        palette: &ui::Palette,
    ) -> AnyElement {
        let muted = palette.muted_foreground;
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
                    .child(
                        div()
                            .id(id)
                            .debug_selector(move || selector.into())
                            .w_full()
                            .min_w_0()
                            .rounded(ui::RADIUS_XL)
                            .border_1()
                            .border_color(palette.border)
                            .bg(palette.muted)
                            .text_size(ui::TEXT_LG)
                            .child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .p_3()
                                    .children(avatar.map(|a| div().flex_none().mt_0p5().child(a)))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_wrap()
                                                    .items_baseline()
                                                    .gap_x_2()
                                                    .text_size(ui::TEXT_SM)
                                                    .text_color(muted)
                                                    .child(heading.to_owned())
                                                    .children(status.map(|(text, color)| {
                                                        div()
                                                            .ml_auto()
                                                            .flex()
                                                            .items_center()
                                                            .gap_1p5()
                                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                                            .text_color(color)
                                                            .child(
                                                                div()
                                                                    .size_1p5()
                                                                    .rounded_full()
                                                                    .bg(color),
                                                            )
                                                            .child(text)
                                                    })),
                                            )
                                            .when(!title.is_empty(), |d| {
                                                d.child(
                                                    div()
                                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                                        .text_color(palette.foreground)
                                                        .child(title.to_owned()),
                                                )
                                            })
                                            .children(body),
                                    ),
                            )
                            .children(footer.map(|footer| {
                                div()
                                    .border_t_1()
                                    .border_color(palette.border)
                                    .px_3()
                                    .py_2()
                                    .child(footer)
                            })),
                    ),
            )
            .into_any_element()
    }

    /// Renders Orca bot offer/setup cards (`ConnectorOfferCard`, `GoogleOfferCard`, `ToolAddedCard`,
    /// `WebhookSetupCard`, `ImportPlanCard`/`ImportDoneCard`, `SignInOffer`).
    fn render_bot_chat_card(
        &mut self,
        pane: super::super::PaneId,
        session: SessionId,
        abs_idx: usize,
        card: &work_run::BotChatCard,
        joins_activity_below: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = ui::palette(cx);
        let muted = palette.muted_foreground;
        let key = (self.layout.host(pane).clone(), session);
        let card_key = (key.clone(), abs_idx);
        let pane_id = pane.get();
        let resolution = self.chat.resolved_bot_cards.get(&card_key).copied();
        let card_avatar = |icon: IconName, color: gpui::Hsla| {
            div()
                .size_8()
                .rounded_full()
                .border_1()
                .border_color(palette.border)
                .bg(palette.card)
                .flex()
                .items_center()
                .justify_center()
                .child(
                    gpui_component::Icon::new(icon)
                        .with_size(ui::ICON_LG)
                        .text_color(color),
                )
                .into_any_element()
        };

        match card {
            work_run::BotChatCard::ToolAdded {
                card_id,
                tool_name,
                subtitle,
            } => {
                let removed = resolution == Some(super::BotCardResolution::Removed);
                let removable_bot =
                    (!removed && !tool_name.trim().is_empty() && self.bot_rpc_on(&key.0).is_some())
                        .then(|| self.chat_bot(&key).map(|b| b.id.clone()))
                        .flatten();
                div()
                    .id(SharedString::from(format!(
                        "chat-tool-added-{pane_id}-{abs_idx}-{card_id}"
                    )))
                    .debug_selector(|| "chat-tool-added-card".into())
                    .when(joins_activity_below, |d| d.pb(gpui::px(6.)))
                    .when(!joins_activity_below, gpui::Styled::pb_4)
                    .w_full()
                    .min_w_0()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                            .min_h(gpui::px(24.))
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_size(ui::TEXT_SM)
                            .text_color(muted)
                            .child(
                                gpui_component::Icon::new(IconName::Wrench)
                                    .with_size(ui::ICON_MD)
                                    .text_color(muted),
                            )
                            .child(div().flex_none().child(if removed {
                                ctext::CARD_TOOL_REMOVED.get()
                            } else {
                                ctext::CARD_TOOL_ADDED.get()
                            }))
                            .child(
                                div()
                                    .px_1p5()
                                    .rounded(ui::RADIUS_SM)
                                    .bg(palette.foreground.opacity(0.07))
                                    .text_size(ui::TEXT_XS)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(palette.foreground)
                                    .child(tool_name.clone()),
                            )
                            .when(!subtitle.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .child(format!("· {subtitle}")),
                                )
                            })
                            .when_some(removable_bot, |d, bot_id| {
                                let rm_key = key.clone();
                                let rm_tool = tool_name.clone();
                                d.child(
                                    Button::new(SharedString::from(format!(
                                        "chat-tool-remove-{pane_id}-{abs_idx}"
                                    )))
                                    .debug_selector(|| "chat-tool-remove-btn".into())
                                    .label(ctext::CARD_TOOL_REMOVE.get())
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_bot_tool_from_card(
                                            rm_key.clone(),
                                            abs_idx,
                                            bot_id.clone(),
                                            &rm_tool,
                                            cx,
                                        );
                                    })),
                                )
                            }),
                    )
                    .into_any_element()
            }
            work_run::BotChatCard::ConnectorOffer {
                card_id,
                service,
                title,
                description,
            } => {
                let status = (resolution == Some(super::BotCardResolution::Dismissed))
                    .then_some((ctext::OFFER_DISMISSED.get(), muted));
                let footer = resolution.is_none().then(|| {
                    let dismiss_key = card_key.clone();
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new(SharedString::from(format!(
                                "chat-conn-dismiss-{pane_id}-{abs_idx}-{card_id}"
                            )))
                            .debug_selector(|| "chat-connector-offer-dismiss".into())
                            .label(ctext::OFFER_NOT_NOW.get())
                            .outline()
                            .small()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.chat.resolved_bot_cards.insert(
                                        dismiss_key.clone(),
                                        super::BotCardResolution::Dismissed,
                                    );
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "chat-conn-accept-{pane_id}-{abs_idx}-{card_id}"
                            )))
                            .debug_selector(|| "chat-connector-offer-connect".into())
                            .label(
                                ctext::CARD_CONNECTOR_CONNECT
                                    .format(&[("service", service.as_str())]),
                            )
                            .primary()
                            .small()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if this.panel_collapsed {
                                        this.toggle_panel(cx);
                                    }
                                    this.bot_ui.panel.tab =
                                        super::super::bot_panel::BotPanelTab::Connectors;
                                    this.bot_ui.panel.settings = None;
                                    cx.notify();
                                },
                            )),
                        )
                        .into_any_element()
                });
                let body = (!description.is_empty()).then(|| {
                    div()
                        .mt_0p5()
                        .text_size(ui::TEXT_SM)
                        .text_color(muted)
                        .child(description.clone())
                        .into_any_element()
                });
                let shown_title = if title.is_empty() { service } else { title };
                Self::render_chat_card_frame(
                    SharedString::from(format!(
                        "chat-connector-offer-{pane_id}-{abs_idx}-{card_id}"
                    )),
                    "chat-connector-offer-card",
                    Some(card_avatar(IconName::Plug, palette.foreground)),
                    ctext::CARD_CONNECTOR_HEADING.get(),
                    status,
                    shown_title,
                    body,
                    footer,
                    palette,
                )
            }
            work_run::BotChatCard::GoogleOffer {
                card_id,
                title,
                description,
            } => {
                let status = (resolution == Some(super::BotCardResolution::Dismissed))
                    .then_some((ctext::OFFER_DISMISSED.get(), muted));
                let footer = resolution.is_none().then(|| {
                    let dismiss_key = card_key.clone();
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new(SharedString::from(format!(
                                "chat-google-dismiss-{pane_id}-{abs_idx}-{card_id}"
                            )))
                            .debug_selector(|| "chat-google-offer-dismiss".into())
                            .label(ctext::OFFER_NOT_NOW.get())
                            .outline()
                            .small()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.chat.resolved_bot_cards.insert(
                                        dismiss_key.clone(),
                                        super::BotCardResolution::Dismissed,
                                    );
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "chat-google-connect-{pane_id}-{abs_idx}-{card_id}"
                            )))
                            .debug_selector(|| "chat-google-offer-connect".into())
                            .label(ctext::CARD_GOOGLE_CONNECT.get())
                            .primary()
                            .small()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if this.panel_collapsed {
                                        this.toggle_panel(cx);
                                    }
                                    this.bot_ui.panel.tab =
                                        super::super::bot_panel::BotPanelTab::Connectors;
                                    this.bot_ui.panel.settings = None;
                                    cx.notify();
                                },
                            )),
                        )
                        .into_any_element()
                });
                let body = (!description.is_empty()).then(|| {
                    div()
                        .mt_0p5()
                        .text_size(ui::TEXT_SM)
                        .text_color(muted)
                        .child(description.clone())
                        .into_any_element()
                });
                Self::render_chat_card_frame(
                    SharedString::from(format!("chat-google-offer-{pane_id}-{abs_idx}-{card_id}")),
                    "chat-google-offer-card",
                    Some(card_avatar(IconName::Mail, palette.foreground)),
                    ctext::CARD_GOOGLE_HEADING.get(),
                    status,
                    title,
                    body,
                    footer,
                    palette,
                )
            }
            work_run::BotChatCard::WebhookSetup {
                card_id,
                title,
                url,
                description,
            } => {
                let copied = resolution == Some(super::BotCardResolution::Copied);
                let copy_key = card_key.clone();
                let copy_url = url.clone();
                let body = div()
                    .mt_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(!description.is_empty(), |d| {
                        d.child(
                            div()
                                .text_size(ui::TEXT_SM)
                                .text_color(muted)
                                .child(description.clone()),
                        )
                    })
                    .when(!url.is_empty(), |d| {
                        d.child(
                            div()
                                .rounded(ui::RADIUS_SM)
                                .border_1()
                                .border_color(palette.border)
                                .bg(palette.card)
                                .px_2()
                                .py_1()
                                .font_family(ui::FONT_MONO)
                                .text_size(ui::TEXT_XS)
                                .text_color(palette.foreground)
                                .truncate()
                                .child(url.clone()),
                        )
                    })
                    .into_any_element();
                let footer = (!url.is_empty()).then(|| {
                    div()
                        .flex()
                        .justify_end()
                        .child(
                            Button::new(SharedString::from(format!(
                                "chat-webhook-copy-{pane_id}-{abs_idx}-{card_id}"
                            )))
                            .icon(if copied {
                                IconName::Check
                            } else {
                                IconName::Copy
                            })
                            .label(if copied {
                                ctext::COPIED.get()
                            } else {
                                ctext::CARD_WEBHOOK_COPY_URL.get()
                            })
                            .outline()
                            .small()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        copy_url.clone(),
                                    ));
                                    this.chat
                                        .resolved_bot_cards
                                        .insert(copy_key.clone(), super::BotCardResolution::Copied);
                                    cx.notify();
                                },
                            )),
                        )
                        .into_any_element()
                });
                Self::render_chat_card_frame(
                    SharedString::from(format!("chat-webhook-setup-{pane_id}-{abs_idx}-{card_id}")),
                    "chat-webhook-setup-card",
                    Some(card_avatar(IconName::Webhook, palette.foreground)),
                    ctext::CARD_WEBHOOK_HEADING.get(),
                    Some((ctext::CARD_WEBHOOK_READY.get(), palette.success)),
                    title,
                    Some(body),
                    footer,
                    palette,
                )
            }
            work_run::BotChatCard::ImportPlan {
                card_id,
                source,
                bot_name,
                summary,
            } => {
                let status = (resolution == Some(super::BotCardResolution::Dismissed))
                    .then_some((ctext::OFFER_DISMISSED.get(), muted));
                let footer = resolution.is_none().then(|| {
                    let dismiss_key = card_key.clone();
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new(SharedString::from(format!(
                                "chat-import-cancel-{pane_id}-{abs_idx}-{card_id}"
                            )))
                            .debug_selector(|| "chat-import-plan-dismiss".into())
                            .label(ctext::OFFER_NOT_NOW.get())
                            .outline()
                            .small()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.chat.resolved_bot_cards.insert(
                                        dismiss_key.clone(),
                                        super::BotCardResolution::Dismissed,
                                    );
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "chat-import-confirm-{pane_id}-{abs_idx}-{card_id}"
                            )))
                            .debug_selector(|| "chat-import-plan-confirm".into())
                            .label(ctext::CARD_IMPORT_CONFIRM.get())
                            .primary()
                            .small()
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_bots_screen(
                                        super::super::bot_section::BotsScreen::Import,
                                        Some(window),
                                        cx,
                                    );
                                },
                            )),
                        )
                        .into_any_element()
                });
                let body = (!summary.is_empty()).then(|| {
                    div()
                        .mt_0p5()
                        .text_size(ui::TEXT_SM)
                        .text_color(muted)
                        .child(summary.clone())
                        .into_any_element()
                });
                let heading = if source.is_empty() {
                    ctext::CARD_IMPORT_PLAN_HEADING.get().to_owned()
                } else {
                    format!("{} · {source}", ctext::CARD_IMPORT_PLAN_HEADING.get())
                };
                Self::render_chat_card_frame(
                    SharedString::from(format!("chat-import-plan-{pane_id}-{abs_idx}-{card_id}")),
                    "chat-import-plan-card",
                    Some(card_avatar(IconName::Download, palette.foreground)),
                    &heading,
                    status,
                    bot_name,
                    body,
                    footer,
                    palette,
                )
            }
            work_run::BotChatCard::ImportDone {
                card_id,
                bot_name,
                summary,
            } => {
                let body = (!summary.is_empty()).then(|| {
                    div()
                        .mt_0p5()
                        .text_size(ui::TEXT_SM)
                        .text_color(muted)
                        .child(summary.clone())
                        .into_any_element()
                });
                Self::render_chat_card_frame(
                    SharedString::from(format!("chat-import-done-{pane_id}-{abs_idx}-{card_id}")),
                    "chat-import-done-card",
                    Some(card_avatar(IconName::BadgeCheck, palette.success)),
                    ctext::CARD_IMPORT_DONE_HEADING.get(),
                    Some((ctext::OFFER_CREATED.get(), palette.success)),
                    bot_name,
                    body,
                    None,
                    palette,
                )
            }
            work_run::BotChatCard::SignInOffer {
                card_id,
                provider,
                description,
            } => {
                let footer = div()
                    .flex()
                    .justify_end()
                    .child(
                        Button::new(SharedString::from(format!(
                            "chat-signin-offer-{pane_id}-{abs_idx}-{card_id}"
                        )))
                        .icon(IconName::LogIn)
                        .label(ctext::CARD_SIGNIN_BTN.get())
                        .primary()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_settings(
                                krill_app_core::settings::AdminTab::Accounts,
                                Some(window),
                                cx,
                            );
                        })),
                    )
                    .into_any_element();
                let body = (!description.is_empty()).then(|| {
                    div()
                        .mt_0p5()
                        .text_size(ui::TEXT_SM)
                        .text_color(muted)
                        .child(description.clone())
                        .into_any_element()
                });
                Self::render_chat_card_frame(
                    SharedString::from(format!("chat-signin-card-{pane_id}-{abs_idx}-{card_id}")),
                    "chat-sign-in-offer-card",
                    Some(card_avatar(IconName::KeyRound, palette.warning)),
                    ctext::CARD_SIGNIN_HEADING.get(),
                    None,
                    provider,
                    body,
                    Some(footer),
                    palette,
                )
            }
        }
    }
}

/// A proposed bot's picture: one of the mascots, the same for the same proposal (Orca draws a
/// random mascot for it), which the bot keeps if it is created.
fn offer_avatar(offer: &krill_proto::bots::BotOffer) -> krill_bots_ui::bots::avatar::Avatar {
    use krill_bots_ui::bots::avatar::{Avatar, MASCOT_IDS};
    let seed = offer.offer_id.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(usize::from(b))
    });
    Avatar::from_fields(
        MASCOT_IDS.get(seed % MASCOT_IDS.len()).copied(),
        None,
        None,
        &offer.name,
    )
}

/// The mascot `offer_avatar` draws, as the avatar field a created bot stores.
pub(super) fn offer_mascot(offer: &krill_proto::bots::BotOffer) -> Option<String> {
    match offer_avatar(offer) {
        krill_bots_ui::bots::avatar::Avatar::Mascot { id, .. } => Some(id.to_owned()),
        _ => None,
    }
}
