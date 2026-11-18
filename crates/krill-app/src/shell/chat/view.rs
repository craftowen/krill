// UI patterns adapted from Orca native-chat (MIT).
// Copyright (c) 2026 Lovecast Inc.
// NativeChatMessageRow.tsx, NativeChatReasoningRow.tsx, NativeChatComposerField.tsx,
// native-chat-autoscroll.ts; rendered with Krill design tokens and GPUI.

use super::body::normalize_code_language;
use super::work_run;
use super::*;
use gpui::StatefulInteractiveElement as _;
use gpui::prelude::FluentBuilder as _;
use gpui_component::menu::ContextMenuExt as _;
use gpui_kit_assets::IconName;
use std::hash::{Hash as _, Hasher as _};

struct SegmentStyle<'a> {
    pane: super::super::PaneId,
    session: SessionId,
    key: &'a ChatKey,
    abs_idx: usize,
    markdown: bool,
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
                                    {
                                        let mut hash = std::hash::DefaultHasher::new();
                                        text.hash(&mut hash);
                                        SharedString::from(format!(
                                            "chat-markdown-{}-{epoch}-{abs_idx}-{seg_idx}-{:x}",
                                            pane.get(),
                                            hash.finish()
                                        ))
                                    },
                                    text,
                                )
                                .scrollable(false)
                                .into_any_element()
                            } else {
                                div()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .child(text)
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
                        .child(code);

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
        total_rows: usize,
        list_state: &gpui::ListState,
        agent_state: krill_proto::control::AgentState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_last_row = row_ix + 1 == total_rows;
        match row {
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
            work_run::DisplayRow::WorkRun(run) => {
                self.render_work_run(pane, session, run, is_last_row, agent_state, cx)
            }
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
        let elapsed = self
            .chat
            .logs
            .get(&key)
            .and_then(|log| log.run_elapsed_secs(run_span.start(), std::time::Instant::now()))
            .map(work_run::format_elapsed_secs);

        let item_selector = format!("chat-item-{}-{}", pane.get(), run_span.start());
        let debug_selector = item_selector.clone();
        let mut container = div()
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
                    .min_h(gpui::px(28.))
                    .px_1p5()
                    .py_1()
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
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .child(if run_is_live {
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
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(if run_is_live { fg } else { muted_fg })
                            .child(header_text),
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
                            .children(progress.map(|label| {
                                div()
                                    .font_family(ui::FONT_MONO)
                                    .text_size(ui::TEXT_XS)
                                    .text_color(muted_fg)
                                    .child(label)
                            }))
                            .children((!run_is_live).then(|| {
                                gpui_component::Icon::new(IconName::Check)
                                    .with_size(ui::ICON_XS)
                                    .text_color(if run.counts.errors > 0 {
                                        palette.destructive
                                    } else {
                                        muted_fg
                                    })
                            }))
                            .children(elapsed.map(|label| {
                                div()
                                    .font_family(ui::FONT_MONO)
                                    .text_size(ui::TEXT_XS)
                                    .text_color(muted_fg)
                                    .child(label)
                            }))
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
                        .whitespace_normal()
                        .text_size(ui::TEXT_XS)
                        .text_color(muted_fg)
                        .child(item.text.trim().to_owned()),
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
                section_el = self.append_chat_segments(
                    section_el,
                    section.text,
                    &SegmentStyle {
                        pane,
                        session,
                        key: &key,
                        abs_idx: section.abs_idx,
                        markdown: false,
                        full_width: true,
                        text_size: ui::TEXT_SM,
                        text_color: muted_fg,
                        epoch,
                        palette,
                    },
                    cx,
                );
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

        let open_file_button = (!item.file_path.is_empty()).then(|| {
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
                this.open_file_at(path.clone(), 1, 1, cx);
            }))
        });

        let copy_button = (!item.text.trim().is_empty()).then(|| {
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
            .tooltip(if is_copied {
                ctext::COPIED.get()
            } else {
                ctext::COPY_MESSAGE.get()
            })
            .ghost()
            .xsmall()
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy_text.clone()));
                this.mark_chat_item_copied(copy_key.clone(), abs_idx, cx);
                cx.notify();
            }))
        });

        let item_selector = format!("chat-item-{}-{abs_idx}", pane.get());
        let debug_selector = item_selector.clone();

        match item.kind {
            ChatItemKind::User => {
                let foldable = is_user_prompt_foldable(&item.text);
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
                    &item.text,
                    &SegmentStyle {
                        pane,
                        session,
                        key: &key,
                        abs_idx,
                        markdown: false,
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
                    .child(
                        div()
                            .min_w_0()
                            .when(foldable && !prompt_expanded, |d| {
                                d.max_h(gpui::px(176.)).overflow_hidden()
                            })
                            .child(body),
                    )
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

                let has_meta = open_file_button.is_some() || copy_button.is_some();
                let user_col = div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap_1()
                    .child(bubble)
                    .children(has_meta.then(|| {
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_1()
                            .children(open_file_button)
                            .children(copy_button)
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
                        full_width: true,
                        text_size: ui::TEXT_MD,
                        text_color: fg,
                        epoch,
                        palette,
                    },
                    cx,
                );
                let show_footer = (!is_last_row
                    || agent_state != krill_proto::control::AgentState::Working)
                    && (!item.text.trim().is_empty() || !item.file_path.is_empty());
                let scroll_list = list_state.clone();
                let scroll_top_btn = Button::new(SharedString::from(format!(
                    "chat-scroll-top-{}-{abs_idx}",
                    pane.get()
                )))
                .debug_selector(|| "chat-scroll-top".into())
                .icon(IconName::ArrowUp)
                .tooltip(ctext::SCROLL_TO_TOP.get())
                .accessibility_label(ctext::SCROLL_TO_TOP.get())
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |_, _, _, cx| {
                    scroll_list.scroll_to(gpui::ListOffset {
                        item_ix: row_ix,
                        offset_in_item: gpui::px(0.),
                    });
                    cx.notify();
                }));

                let assistant_el = div()
                    .id(SharedString::from(item_selector))
                    .debug_selector(move || debug_selector.clone())
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(body)
                    .children(show_footer.then(|| {
                        div()
                            .mt_3()
                            .flex()
                            .items_center()
                            .gap_1()
                            .children(copy_button)
                            .children(open_file_button)
                            .child(scroll_top_btn)
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
                            full_width: true,
                            text_size: ui::TEXT_SM,
                            text_color: muted_fg,
                            epoch,
                            palette,
                        },
                        cx,
                    );
                    reasoning_el = reasoning_el.child(
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
                    );
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
        let card_border = if is_request && current {
            palette.agent_question
        } else {
            border
        };

        let mut card = div()
            .id(SharedString::from(item_selector))
            .debug_selector(move || debug_selector.clone())
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .p_3()
            .rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(card_border)
            .bg(palette.card)
            .flex()
            .flex_col()
            .gap_2()
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
                            .text_color(fg)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(if item.title.is_empty() {
                                SharedString::from(role_label)
                            } else {
                                SharedString::from(format!("{role_label} • {}", item.title))
                            }),
                    )
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
            .max_h(gpui::px(288.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2();

        if !item.text.trim().is_empty() {
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
                    full_width: true,
                    text_size: ui::TEXT_MD,
                    text_color: fg,
                    epoch,
                    palette,
                },
                cx,
            );
            scroll_body = scroll_body.child(body);
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
        } else if item.kind == ChatItemKind::Permission {
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

    fn render_pending_chat_card(
        &self,
        key: ChatKey,
        pane: PaneId,
        pending: &super::PendingChatPrompt,
        is_last_row: bool,
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

        let attachment_strip = (!pending.attachments.is_empty()).then(|| {
            div().flex().flex_wrap().items_center().gap_1().children(
                pending.attachments.iter().map(|attachment| {
                    let badge = if attachment.is_image {
                        format!(
                            "{} ({}, {})",
                            attachment.display_name,
                            ctext::ATTACH_IMAGE_BADGE.get(),
                            super::catalog::format_byte_len(attachment.byte_len)
                        )
                    } else {
                        format!(
                            "{} ({})",
                            attachment.display_name,
                            super::catalog::format_byte_len(attachment.byte_len)
                        )
                    };
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
                            gpui_component::Icon::new(if attachment.is_image {
                                IconName::Image
                            } else {
                                IconName::File
                            })
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
            div().w_full().flex().items_center().justify_end().child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(palette.muted_foreground)
                    .child(ctext::PENDING_SENDING.get()),
            )
        };

        div()
            .w_full()
            .flex()
            .justify_center()
            .pt_3()
            .when(is_last_row, gpui::Styled::pb_4)
            .child(
                div()
                    .w_full()
                    .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                    .min_w_0()
                    .px_3()
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
        if self.layout.focused() == Some(pane)
            && matches!(&self.chat.focus_pending, Some(ChatFocus::Composer(id)) if *id == key)
        {
            input_state.update(cx, |state, cx| state.focus(window, cx));
            self.chat.focus_pending = None;
        }
        let palette = ui::palette(cx);
        let bg = palette.background;
        let border = palette.border;
        let muted_fg = palette.muted_foreground;

        let host = self.layout.host(pane).clone();
        let agent_state = self.session_agent_state(&host, session, cx);
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
                            work_run::DisplayRow::Item { .. } => None,
                        })
                    } else {
                        None
                    };
                    (log.bound, log.revision, log.epoch, feed, elapsed)
                }
                None => (false, 0, 0, work_run::DisplayFeed::empty(), None),
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
                matches!(item.status, super::PendingStatus::ReviewRequired).hash(&mut hasher);
                let copied = self
                    .chat
                    .copied_pending
                    .as_ref()
                    .is_some_and(|(ck, cid, _)| *ck == key && *cid == item.id);
                copied.hash(&mut hasher);
            }
            hasher.finish()
        };

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
                    .flex_1()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .items_center()
                    .gap_3()
                    .p_4()
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
            );
        } else {
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
            let owner = cx.weak_entity();
            let rows = Arc::clone(&display_feed);
            let pending_rows = Arc::clone(&pending_cards);
            let total_rows = rows.len() + pending_len;
            let list_state_for_rows = list_state.clone();
            let pending_key = key.clone();
            feed = feed.child(
                gpui::list(list_state.clone(), move |row_ix, _, cx| {
                    if row_ix >= rows.len() {
                        let Some(pending_item) = pending_rows.get(row_ix - rows.len()) else {
                            return div().into_any_element();
                        };
                        let key_for_row = pending_key.clone();
                        let is_last = row_ix + 1 == total_rows;
                        return owner
                            .update(cx, |this, cx| {
                                this.render_pending_chat_card(
                                    key_for_row,
                                    pane,
                                    pending_item,
                                    is_last,
                                    cx,
                                )
                            })
                            .unwrap_or_else(|_| div().into_any_element());
                    }
                    let Some(row) = rows.get(row_ix) else {
                        return div().into_any_element();
                    };
                    owner
                        .update(cx, |this, cx| {
                            this.render_display_row(
                                pane,
                                session,
                                row,
                                row_ix,
                                total_rows,
                                &list_state_for_rows,
                                agent_state,
                                cx,
                            )
                        })
                        .unwrap_or_else(|_| div().into_any_element())
                })
                .size_full(),
            );
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
                                .child(
                                    Button::new("chat-jump-latest")
                                        .debug_selector(|| "chat-jump-latest".into())
                                        .label(ctext::JUMP_LATEST.get())
                                        .icon(IconName::ArrowDown)
                                        .rounded_full()
                                        .small()
                                        .ghost()
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            list_state.set_follow_mode(gpui::FollowMode::Tail);
                                            cx.notify();
                                        })),
                                ),
                        ),
                );
            }
        }

        let composer = self.render_chat_composer(pane, key.clone(), &input_state, cx);

        let live_line = self.pane_live_status_line(pane);
        let turn_status_line = (agent_state == krill_proto::control::AgentState::Working
            || live_line.is_some())
        .then(|| {
            let status_text = match (agent_state, &live_line) {
                (krill_proto::control::AgentState::Working, Some(line)) => {
                    format!("{} · {line}", ctext::STATE_WORKING.get())
                }
                (krill_proto::control::AgentState::Working, None) => {
                    ctext::STATE_WORKING.get().to_owned()
                }
                (_, Some(line)) => line.clone(),
                _ => String::new(),
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

        let switch_pill = div()
            .absolute()
            .top_2()
            .right_3()
            .rounded_full()
            .border_1()
            .border_color(border)
            .bg(palette.popover.opacity(0.92))
            .shadow_lg()
            .child(
                Button::new(SharedString::from(format!(
                    "desktop-chat-switch-term-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-switch-terminal".into())
                .icon(IconName::SquareTerminal)
                .label(ctext::TERM.get())
                .tooltip(krill_app::i18n::catalog::shell::TOGGLE_CHAT_VIEW.get())
                .rounded_full()
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.switch_pane_to_terminal(pane, window, cx);
                })),
            );

        let in_card = self.workspace_stage() == super::super::pane_frame::WorkspaceStage::CardStage;
        let overlay_weak = cx.weak_entity();
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
            .context_menu(move |menu, _, cx| {
                super::super::terminal_menu::build_chat_overlay(menu, &overlay_weak, pane, cx)
            })
            .child(feed)
            .children(self.render_chat_prompt_notices(&key, cx))
            .children(turn_status_line)
            .child(composer)
            .child(switch_pill);

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
                .child(inner)
                .into_any_element(),
        )
    }
}
