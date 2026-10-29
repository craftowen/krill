//! Composer layout adapted from Orca `NewWorkspaceComposerCard`/`NameSection` and `CreateFromPicker`.
//! Copyright (c) 2026 Lovecast Inc. (MIT License; see docs/licensing/spdx/MIT.txt).
use super::*;
use gpui_component::popover::Popover;

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != crate::i18n::language() {
            self.language = crate::i18n::language();
            self.name.update(cx, |input, cx| {
                input.set_placeholder(text::NAME.get(), window, cx);
            });
            self.base.update(cx, |input, cx| {
                input.set_placeholder(text::BASE.get(), window, cx);
            });
            self.branch_query.update(cx, |input, cx| {
                input.set_placeholder(text::SEARCH_BRANCHES.get(), window, cx);
            });
            self.initial_prompt.update(cx, |input, cx| {
                input.set_placeholder(text::INITIAL_PROMPT_PLACEHOLDER.get(), window, cx);
            });
        }

        if self.picker_sync == PickerSync::Dirty {
            self.sync_pickers(window, cx);
        }
        let p = crate::ui::palette(cx);
        let (warning, muted) = (p.warning, p.muted_foreground);
        let close = Button::new("worktree-close")
            .disabled(matches!(self.phase, Phase::Creating))
            .ghost()
            .label(text::CLOSE.get())
            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close)));
        let footer = || div().flex_none().flex().flex_wrap().justify_end().gap_2();
        let repo_hooks = &self.hooks;
        let body = div()
            .id("worktree-dialog-body")
            .debug_selector(|| "worktree-dialog-body".into())
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4();
        let inner_width = (crate::ui::DIALOG_WIDTH
            .min((window.viewport_size().width - crate::ui::SPACE_4 * 2.).max(px(0.)))
            - crate::ui::SPACE_3 * 4.)
            .max(px(0.));
        let narrow = inner_width < crate::ui::SPACE_4 * 24.;
        let picker_width = if narrow {
            inner_width
        } else {
            crate::ui::DIALOG_WIDTH / 2.
        };
        let selected_agents = self.selected_agents.clone();
        let current_host = self.host.clone();
        let current_repo = self.repo.clone();
        let host_options = self.host_options.clone();
        let host_label = host_options
            .iter()
            .find(|o| o.host == current_host)
            .map(|o| o.label.clone());
        let repo_name = current_repo
            .file_name()
            .unwrap_or(current_repo.as_os_str())
            .to_string_lossy()
            .into_owned();
        let subtitle = match host_label {
            Some(ref hl) if !current_host.is_local() || host_options.len() > 1 => {
                format!("{hl} · {repo_name}")
            }
            _ => repo_name,
        };
        let has_repo_choice = !host_options.is_empty();
        let has_host_choice = host_options.len() > 1;
        let body = match &self.phase {
            Phase::Edit(error) => body
                .when(!host_options.is_empty(), |d| {
                    d.child(
                        div()
                            .id("worktree-repo-selector")
                            .debug_selector(|| "worktree-repo-selector".into())
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(crate::ui::field_label(text::REPO_LABEL.get(), None, p))
                            .child(
                                Select::new(&self.project_picker)
                                    .id("worktree-project-picker")
                                    .w_full()
                                    .menu_width(px(420.))
                                    .placeholder(text::PROJECT_PLACEHOLDER.get())
                                    .search_placeholder(text::PROJECT_SEARCH.get())
                                    .accessibility_label(text::REPO_LABEL.get()),
                            ),
                    )
                })
                .when(host_options.len() > 1, |d| {
                    d.child(
                        div()
                            .id("worktree-host-selector")
                            .debug_selector(|| "worktree-host-selector".into())
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(crate::ui::field_label(text::HOST_LABEL.get(), None, p))
                            .child(
                                Select::new(&self.host_picker)
                                    .id("worktree-host-picker")
                                    .w_full()
                                    .menu_width(px(420.))
                                    .search_placeholder(text::HOST_SEARCH.get())
                                    .accessibility_label(text::HOST_LABEL.get()),
                            ),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .when(narrow, |row| row.flex_col().items_start())
                                .child(
                                    crate::ui::field_label(
                                        text::NAME_LABEL.get(),
                                        Some(text::OPTIONAL.get()),
                                        p,
                                    )
                                    .flex_none(),
                                )
                                .child(self.render_base_picker(picker_width, cx)),
                        )
                        .child(Input::new(&self.name)),
                )
                .child(
                    div()
                        .id("worktree-fanout-agents")
                        .debug_selector(|| "worktree-fanout-agents".into())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(crate::ui::field_label(
                            text::AGENTS_LABEL.get(),
                            Some(text::OPTIONAL.get()),
                            p,
                        ))
                        .child(
                            div().flex().flex_wrap().gap_1().children(
                                FAN_OUT_AGENTS.iter().enumerate().map(
                                    |(idx, &(agent_id, label))| {
                                        let selected =
                                            selected_agents.iter().any(|a| a == agent_id);
                                        let btn = Button::new(("worktree-agent", idx))
                                            .debug_selector(move || {
                                                format!("worktree-agent-{agent_id}")
                                            })
                                            .xsmall()
                                            .icon(crate::ui::icons::agent_menu_icon(agent_id))
                                            .label(label)
                                            .selected(selected)
                                            .toggled(selected);
                                        let btn = btn.ghost();
                                        btn.on_click(cx.listener(move |d, _, _, cx| {
                                            d.toggle_agent(agent_id, cx);
                                        }))
                                    },
                                ),
                            ),
                        ),
                )
                .child(
                    Button::new("worktree-advanced")
                        .debug_selector(|| "worktree-advanced".into())
                        .ghost()
                        .small()
                        .label(text::ADVANCED.get())
                        .dropdown_caret(true)
                        .selected(self.advanced_open)
                        .on_click(cx.listener(|d, _, window, cx| {
                            if d.advanced_open {
                                d.name.update(cx, |input, cx| input.focus(window, cx));
                            }
                            d.advanced_open = !d.advanced_open;
                            cx.notify();
                        })),
                )
                .when(self.advanced_open, |body| {
                    body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(crate::ui::field_label(
                                text::CUSTOM_REF.get(),
                                Some(text::OPTIONAL.get()),
                                p,
                            ))
                            .child(Input::new(&self.base)),
                    )
                    .when(self.preparation.is_some(), |body| {
                        body.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(crate::ui::TEXT_2XS)
                                .text_color(muted)
                                .child(if self.preparation_ready {
                                    text::PREPARED.get()
                                } else {
                                    text::PREPARING.get()
                                })
                                .child(
                                    Button::new("cancel-preparation")
                                        .ghost()
                                        .xsmall()
                                        .label(text::CANCEL_PREPARATION.get())
                                        .on_click(
                                            cx.listener(|d, _, _, cx| d.cancel_preparation(cx)),
                                        ),
                                ),
                        )
                    })
                    .child(
                        div()
                            .id("worktree-initial-prompt")
                            .debug_selector(|| "worktree-initial-prompt".into())
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(crate::ui::field_label(
                                text::INITIAL_PROMPT_LABEL.get(),
                                Some(text::OPTIONAL.get()),
                                p,
                            ))
                            .child(Input::new(&self.initial_prompt)),
                    )
                    .when(!repo_hooks.effective_symlinks.is_empty(), |d| {
                        let paths = repo_hooks.effective_symlinks.join(", ");
                        d.child(
                            div()
                                .id("worktree-symlinks-preview")
                                .debug_selector(|| "worktree-symlinks-preview".into())
                                .font_family(crate::ui::FONT_MONO)
                                .text_size(crate::ui::TEXT_SM)
                                .text_color(muted)
                                .child(text::SYMLINKS_PREVIEW.format(&[("paths", &paths)])),
                        )
                    })
                    .when_some(repo_hooks.effective_setup.clone(), |d, script| {
                        d.child(
                            div()
                                .id("worktree-setup-preview")
                                .debug_selector(|| "worktree-setup-preview".into())
                                .font_family(crate::ui::FONT_MONO)
                                .text_size(crate::ui::TEXT_SM)
                                .text_color(muted)
                                .child(
                                    text::SETUP_PREVIEW.format(&[("script", &visible(&script))]),
                                ),
                        )
                    })
                    .when_some(
                        repo_hooks.effective_archive.clone(),
                        |d, script| {
                            d.child(
                                div()
                                    .id("worktree-archive-preview")
                                    .debug_selector(|| "worktree-archive-preview".into())
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_SM)
                                    .text_color(muted)
                                    .child(
                                        text::ARCHIVE_PREVIEW
                                            .format(&[("script", &visible(&script))]),
                                    ),
                            )
                        },
                    )
                })
                .children(
                    error
                        .clone()
                        .map(|e| crate::ui::error_banner(p, crate::i18n::cached_notice(&e))),
                ),
            Phase::Creating => body.child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(muted)
                    .child(text::CREATING.get()),
            ),
            Phase::Untrusted { script, hash } => body
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(warning)
                        .child(text::UNTRUSTED.get()),
                )
                .child(
                    div()
                        .id("setup-script")
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded(crate::ui::RADIUS_XS)
                        .bg(p.muted)
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_SM)
                        .child(visible(script)),
                )
                .child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_2XS)
                        .text_color(muted)
                        .child(
                            crate::i18n::catalog::general::HASH
                                .format(&[("hash", &hex_string(hash))]),
                        ),
                ),
            Phase::Done(w) => body
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.success)
                        .child(text::CREATED.get()),
                )
                .child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(muted)
                        .child(
                            PathBuf::from(std::ffi::OsStr::from_bytes(&w.path))
                                .display()
                                .to_string(),
                        ),
                )
                .child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_SM)
                        .child(format!("{} ← {}", w.branch, w.base)),
                )
                .children(w.setup.map(|id| format!("{} {}", text::SETUP.get(), id.0)))
                .children(w.warnings.iter().map(|e| {
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(warning)
                        .child(e.clone())
                })),
        };
        let actions = match &self.phase {
            Phase::Edit(_) => footer().child(close).child(
                Button::new("worktree-create")
                    .debug_selector(|| "worktree-create".into())
                    .primary()
                    .label(text::CREATE.get())
                    .on_click(cx.listener(|d, _, _, cx| d.submit(Setup::Ask, cx))),
            ),
            Phase::Untrusted { hash, .. } => {
                let approve = hash.clone();
                footer()
                    .child(close)
                    .child(
                        Button::new("setup-skip")
                            .ghost()
                            .label(text::SKIP.get())
                            .on_click(cx.listener(|d, _, _, cx| d.submit(Setup::Skip, cx))),
                    )
                    .child(
                        Button::new("setup-approve")
                            .danger()
                            .label(text::APPROVE.get())
                            .on_click(cx.listener(move |d, _, _, cx| {
                                d.submit(Setup::Approve(approve.clone()), cx);
                            })),
                    )
            }
            Phase::Creating | Phase::Done(_) => footer().child(close),
        };
        let card = div()
            .id("worktree-dialog")
            .debug_selector(|| "worktree-dialog".into())
            .occlude()
            .w(crate::ui::DIALOG_WIDTH)
            .max_w((window.viewport_size().width - crate::ui::SPACE_4 * 2.).max(px(0.)))
            .max_h((window.viewport_size().height - crate::ui::SPACE_4 * 2.).max(px(0.)))
            .role(gpui::Role::Dialog)
            .aria_label(text::TITLE.get())
            .p_6()
            .rounded(crate::ui::RADIUS_LG)
            .bg(p.popover)
            .font_family(crate::ui::FONT_UI)
            .text_size(crate::ui::TEXT_MD)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .capture_action(cx.listener(
                |dialog, action: &gpui_component::input::Enter, window, cx| {
                    if dialog.handle_branch_key("enter", window, cx) {
                        cx.stop_propagation();
                    } else if action.secondary && !dialog.input_is_composing(window, cx) {
                        dialog.submit(Setup::Ask, cx);
                        cx.stop_propagation();
                    } else if !dialog.input_is_composing(window, cx)
                        && dialog.name.read(cx).focus_handle(cx).is_focused(window)
                    {
                        window.focus_next(cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(
                cx.listener(|dialog, _: &gpui_component::input::Escape, window, cx| {
                    if dialog.input_is_composing(window, cx) {
                        // Let the input cancel composition, then skip this event's raw Escape fallback.
                        window.prevent_default();
                        return;
                    }
                    if dialog.handle_branch_key("escape", window, cx) {
                        cx.stop_propagation();
                    } else if !gpui_component::GlobalState::is_in_deferred_context(cx) {
                        cx.stop_propagation();
                        if !matches!(dialog.phase, Phase::Creating) {
                            cx.emit(Event::Close);
                        }
                    }
                }),
            )
            .capture_key_down(cx.listener(|d, e: &gpui::KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && window.default_prevented() {
                    return;
                }
                if d.on_key(e, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                if e.keystroke.key == "escape" && !d.input_is_composing(window, cx) {
                    cx.stop_propagation();
                    if !matches!(d.phase, Phase::Creating)
                        && !gpui_component::GlobalState::is_in_deferred_context(cx)
                    {
                        cx.emit(Event::Close);
                    }
                }
            }))
            .on_mouse_down_out(cx.listener(|d, _, _, cx| {
                if !matches!(d.phase, Phase::Creating)
                    && !gpui_component::GlobalState::is_in_deferred_context(cx)
                {
                    cx.emit(Event::Close);
                }
            }))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text::TITLE.get()),
                    )
                    .when(!has_repo_choice && !has_host_choice, |header| {
                        header.child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(muted)
                                .truncate()
                                .child(subtitle),
                        )
                    })
                    .when_some(self.parent.as_ref(), |d, parent| {
                        let parent_name = parent
                            .file_name()
                            .unwrap_or(parent.as_os_str())
                            .to_string_lossy()
                            .into_owned();
                        d.child(
                            div()
                                .id("worktree-parent")
                                .debug_selector(|| "worktree-parent".into())
                                .text_size(crate::ui::TEXT_2XS)
                                .text_color(muted)
                                .child(format!("{}: {parent_name}", text::PARENT_LABEL.get())),
                        )
                    }),
            )
            .child(body)
            .child(actions);
        crate::ui::modal_backdrop(
            window.viewport_size(),
            crate::ui::modal_focus_scope(card, &self.focus),
        )
    }
}

impl Dialog {
    fn render_base_picker(&self, width: gpui::Pixels, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.base.read(cx).value().to_string();
        let label = if selected.is_empty() {
            text::DEFAULT_BRANCH.get().to_owned()
        } else {
            selected
        };
        let dialog = cx.entity();
        Popover::new("worktree-branch-popover")
            .open(self.branch_picker_open)
            .appearance(false)
            .track_focus(&self.branch_query.read(cx).focus_handle(cx))
            .trigger(
                Button::new("worktree-base-picker")
                    .debug_selector(|| "worktree-base-picker".into())
                    .small()
                    .ghost()
                    .dropdown_caret(true)
                    .min_w_0()
                    .w(width.max(px(0.)))
                    .flex_none()
                    .accessibility_label(format!("{}: {label}", text::BASE_LABEL.get()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(format!("{}: {label}", text::BASE_LABEL.get())),
                    ),
            )
            .on_open_change(cx.listener(|dialog, open, window, cx| {
                if *open {
                    dialog.open_branch_picker(window, cx);
                    cx.defer_in(window, |dialog, window, cx| {
                        if dialog.branch_picker_open {
                            dialog
                                .branch_query
                                .update(cx, |input, cx| input.focus(window, cx));
                        }
                    });
                } else {
                    dialog.close_branch_picker(window, cx);
                }
            }))
            .content(move |_, window, cx| {
                dialog.update(cx, |dialog, cx| dialog.render_branch_list(window, cx))
            })
    }

    fn render_branch_list(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let p = crate::ui::palette(cx);
        let count = self.filtered_bases(cx).len();
        let rows = u16::try_from(count.clamp(1, 7)).unwrap_or(7);
        div()
            .id("worktree-base-options")
            .debug_selector(|| "worktree-base-options".into())
            .w(crate::ui::DIALOG_WIDTH - crate::ui::SPACE_3 * 4.)
            .max_w((window.viewport_size().width - crate::ui::SPACE_4 * 2.).max(px(0.)))
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .rounded(crate::ui::RADIUS_MD)
            .shadow_lg()
            .occlude()
            .capture_action(
                cx.listener(|dialog, _: &gpui_component::input::Enter, window, cx| {
                    if dialog.handle_branch_key("enter", window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(cx.listener(
                |dialog, _: &gpui_component::input::MoveDown, window, cx| {
                    if dialog.handle_branch_key("down", window, cx) {
                        cx.stop_propagation();
                    }
                },
            ))
            .capture_action(
                cx.listener(|dialog, _: &gpui_component::input::MoveUp, window, cx| {
                    if dialog.handle_branch_key("up", window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(
                cx.listener(|dialog, _: &gpui_component::input::Escape, window, cx| {
                    if dialog.input_is_composing(window, cx) {
                        window.prevent_default();
                        return;
                    }
                    if dialog.handle_branch_key("escape", window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_key_down(
                cx.listener(|dialog, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" && window.default_prevented() {
                        return;
                    }
                    if dialog.on_key(event, window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .child(Input::new(&self.branch_query).small())
            .when(self.branch_error.is_some(), |list| {
                list.child(
                    div()
                        .p_2()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::BRANCHES_UNAVAILABLE.get()),
                )
            })
            .when(count == 0, |list| {
                list.child(
                    div()
                        .p_2()
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(p.muted_foreground)
                        .child(text::NO_BRANCHES.get()),
                )
            })
            .child(
                gpui::uniform_list(
                    "worktree-branch-rows",
                    count,
                    cx.processor(|dialog, range: std::ops::Range<usize>, _, cx| {
                        let p = crate::ui::palette(cx);
                        let candidates = dialog.filtered_bases(cx);
                        let selected = dialog.base.read(cx).value().to_string();
                        range
                            .filter_map(|index| {
                                let branch = candidates.get(index)?.clone();
                                let selector = if branch.is_empty() {
                                    "worktree-base-default".to_owned()
                                } else {
                                    format!("worktree-base-option-{branch}")
                                };
                                let label = if branch.is_empty() {
                                    text::DEFAULT_BRANCH.get().to_owned()
                                } else {
                                    branch.clone()
                                };
                                let active = dialog.highlight == Some(index);
                                Some(
                                    div()
                                        .id(("worktree-base-option", index))
                                        .debug_selector(move || selector.clone())
                                        .role(gpui::Role::ListBoxOption)
                                        .aria_selected(branch == selected)
                                        .h(crate::ui::SPACE_4 * 2.)
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .rounded(crate::ui::RADIUS_XS)
                                        .font_family(crate::ui::FONT_MONO)
                                        .text_size(crate::ui::TEXT_SM)
                                        .when(active, |row| {
                                            row.bg(p.accent).text_color(p.accent_foreground)
                                        })
                                        .hover(|row| {
                                            row.bg(p.accent).text_color(p.accent_foreground)
                                        })
                                        .cursor_pointer()
                                        .child(
                                            div().w(crate::ui::SPACE_4).flex_none().child(
                                                gpui_component::Icon::new(
                                                    gpui_component::IconName::Check,
                                                )
                                                .with_size(crate::ui::ICON_SM)
                                                .when(branch != selected, gpui::Styled::invisible),
                                            ),
                                        )
                                        .child(div().flex_1().min_w_0().truncate().child(label))
                                        .on_click(cx.listener(move |dialog, _, window, cx| {
                                            dialog.pick_base(&branch, window, cx);
                                        })),
                                )
                            })
                            .collect()
                    }),
                )
                .h(crate::ui::SPACE_4 * (2. * f32::from(rows)))
                .track_scroll(&self.branch_scroll),
            )
    }
}
