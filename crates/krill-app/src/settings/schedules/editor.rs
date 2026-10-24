//! Automation editor. Design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
use super::*;
use crate::ui;
use gpui_component::Disableable as _;
use gpui_component::input::Textarea;
use krill_proto::schedule::{SchedulePreset, build_preset_cron, classify_schedule};

fn input_field(
    label: &'static str,
    selector: &'static str,
    input: &gpui::Entity<InputState>,
    p: &ui::Palette,
) -> Stateful<Div> {
    let legacy = match selector {
        "admin-schedule-id" => Some("admin-schedule-input-id"),
        "admin-schedule-cron" => Some("admin-schedule-input-cron"),
        "admin-schedule-cwd" => Some("admin-schedule-input-cwd"),
        "admin-schedule-command" => Some("admin-schedule-input-command"),
        "admin-schedule-precheck" => Some("admin-schedule-input-precheck"),
        _ => None,
    };
    let mut field = div()
        .id(selector)
        .debug_selector(move || selector.into())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(ui::field_label(label, None, p));
    if let Some(legacy) = legacy {
        field = field.child(
            div()
                .id(legacy)
                .debug_selector(move || legacy.into())
                .child(Input::new(input).small()),
        );
    } else {
        field = field.child(Input::new(input).small());
    }
    field
}

impl SettingsView {
    pub(super) fn apply_schedule_template(
        &mut self,
        template: Template,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = &mut self.schedule_modal else {
            return;
        };
        modal
            .name
            .update(cx, |s, cx| s.set_value(template.name.get(), window, cx));
        if modal.editing_id.is_none() {
            let ids = self
                .services
                .schedules
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>();
            let id = krill_proto::schedule::slugify_schedule_id(template.id, &ids);
            modal.id.update(cx, |s, cx| s.set_value(id, window, cx));
        }
        modal
            .cron
            .update(cx, |s, cx| s.set_value(template.cron, window, cx));
        modal.preset = classify_schedule(template.cron).preset;
        modal
            .prompt
            .update(cx, |s, cx| s.set_value(template.prompt.get(), window, cx));
        modal
            .precheck
            .update(cx, |s, cx| s.set_value(template.precheck, window, cx));
        modal.error = None;
        cx.notify();
    }

    pub(super) fn render_automation_editor(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        let modal = self.schedule_modal.as_ref()?;
        let p = ui::palette(cx);
        let pending = self.file_jobs.contains_key("schedule-edit");
        let heading = modal.editing_id.as_ref().map_or_else(
            || text::AUTOMATIONS_NEW_TITLE.get().to_owned(),
            |id| text::AUTOMATIONS_EDIT_TITLE.format(&[("id", id)]),
        );
        let mut templates = div().flex().flex_wrap().gap_1();
        for template in TEMPLATES {
            let id = format!("admin-schedule-template-{}", template.key);
            templates = templates.child(
                layout::ghost(
                    gpui::ElementId::Name(id.clone().into()),
                    template.name.get(),
                )
                .debug_selector(move || id.clone())
                .disabled(pending)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_schedule_template(template, window, cx);
                })),
            );
        }
        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(input_field(
                text::AUTOMATIONS_NAME.get(),
                "admin-schedule-name",
                &modal.name,
                p,
            ))
            .child(input_field(
                text::SCHEDULE_FIELD_ID.get(),
                "admin-schedule-id",
                &modal.id,
                p,
            ))
            .child(
                div()
                    .id("admin-schedule-prompt")
                    .debug_selector(|| "admin-schedule-prompt".into())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(ui::field_label(text::SCHEDULE_FIELD_PROMPT.get(), None, p))
                    .child(
                        div()
                            .id("admin-schedule-input-prompt")
                            .debug_selector(|| "admin-schedule-input-prompt".into())
                            .child(Textarea::new(&modal.prompt).small()),
                    ),
            )
            .child(input_field(
                text::SCHEDULE_FIELD_PRECHECK.get(),
                "admin-schedule-precheck",
                &modal.precheck,
                p,
            ))
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::SCHEDULE_FIELD_PRECHECK_HINT.get()),
            )
            .child(input_field(
                text::AUTOMATIONS_PRECHECK_TIMEOUT.get(),
                "admin-schedule-precheck-timeout",
                &modal.precheck_timeout,
                p,
            ));
        let mut agents = div().flex().flex_wrap().gap_1();
        for (agent, label) in [
            ("claude", "Claude"),
            ("codex", "Codex"),
            ("agy", "Antigravity"),
            ("opencode", "OpenCode"),
            ("cursor", "Cursor"),
        ] {
            let id = format!("admin-schedule-agent-{agent}");
            agents = agents.child(
                layout::segment(
                    p,
                    gpui::ElementId::Name(id.clone().into()),
                    label,
                    modal.agent.as_deref() == Some(agent),
                )
                .debug_selector(move || id.clone())
                .disabled(pending)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(modal) = &mut this.schedule_modal {
                        modal.agent = Some(agent.to_owned());
                        modal
                            .command
                            .update(cx, |s, cx| s.set_value(agent, window, cx));
                        cx.notify();
                    }
                })),
            );
        }
        let mode_buttons = [
            (
                WorkspaceMode::Reuse,
                "admin-schedule-mode-reuse",
                text::SCHEDULE_MODE_REUSE.get(),
            ),
            (
                WorkspaceMode::Isolate,
                "admin-schedule-mode-isolate",
                text::SCHEDULE_MODE_ISOLATE.get(),
            ),
        ]
        .map(|(mode, id, label)| {
            layout::segment(p, id, label, modal.workspace_mode == mode)
                .debug_selector(move || id.into())
                .disabled(pending)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(modal) = &mut this.schedule_modal {
                        modal.workspace_mode = mode;
                        if mode == WorkspaceMode::Isolate {
                            modal.reuse_session = false;
                        }
                        cx.notify();
                    }
                }))
        });
        let narrow = window.viewport_size().width < gpui::px(720.);
        let mut right = div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .when(narrow, Styled::w_full)
            .when(!narrow, |d| d.w(gpui::px(300.)).flex_none())
            .child(ui::field_label(text::AUTOMATIONS_AGENT.get(), None, p))
            .child(agents)
            .child(input_field(
                text::SCHEDULE_FIELD_COMMAND.get(),
                "admin-schedule-command",
                &modal.command,
                p,
            ))
            .child(input_field(
                text::AUTOMATIONS_EXTRA_ARGS.get(),
                "admin-schedule-extra-args",
                &modal.extra_args,
                p,
            ))
            .child(input_field(
                text::SCHEDULE_FIELD_CWD.get(),
                "admin-schedule-cwd",
                &modal.cwd,
                p,
            ))
            .child(ui::field_label(
                text::SCHEDULE_FIELD_WORKSPACE_MODE.get(),
                None,
                p,
            ))
            .child(layout::segmented_control(p, mode_buttons));
        if modal.workspace_mode == WorkspaceMode::Isolate {
            right = right.child(input_field(
                text::AUTOMATIONS_BASE_BRANCH.get(),
                "admin-schedule-base-branch",
                &modal.base_branch,
                p,
            ));
        } else {
            let reuse_available = modal.extra_args.read(cx).value().trim().is_empty();
            let session_buttons = [
                (
                    false,
                    "admin-schedule-session-fresh",
                    text::AUTOMATIONS_FRESH.get(),
                ),
                (
                    true,
                    "admin-schedule-session-reuse",
                    text::AUTOMATIONS_REUSE_SESSION.get(),
                ),
            ]
            .map(|(reuse, id, label)| {
                layout::segment(
                    p,
                    id,
                    label,
                    (modal.reuse_session && reuse_available) == reuse,
                )
                .debug_selector(move || id.into())
                .disabled(pending || (reuse && !reuse_available))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(modal) = &mut this.schedule_modal {
                        modal.reuse_session = reuse;
                        cx.notify();
                    }
                }))
            });
            right = right
                .child(ui::field_label(text::AUTOMATIONS_SESSION.get(), None, p))
                .child(layout::segmented_control(p, session_buttons));
        }
        let mut presets = div().flex().flex_wrap().gap_1();
        for (preset, key, label) in [
            (
                SchedulePreset::Hourly,
                "hourly",
                text::AUTOMATIONS_HOURLY.get(),
            ),
            (
                SchedulePreset::Daily,
                "daily",
                text::AUTOMATIONS_DAILY.get(),
            ),
            (
                SchedulePreset::Weekdays,
                "weekdays",
                text::AUTOMATIONS_WEEKDAYS.get(),
            ),
            (
                SchedulePreset::Weekly,
                "weekly",
                text::AUTOMATIONS_WEEKLY.get(),
            ),
            (
                SchedulePreset::Custom,
                "custom",
                text::AUTOMATIONS_CUSTOM.get(),
            ),
        ] {
            let id = format!("admin-schedule-preset-{key}");
            presets = presets.child(
                layout::segment(
                    p,
                    gpui::ElementId::Name(id.clone().into()),
                    label,
                    modal.preset == preset,
                )
                .debug_selector(move || id.clone())
                .disabled(pending)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(modal) = &mut this.schedule_modal {
                        let current = classify_schedule(modal.cron.read(cx).value().as_ref());
                        modal.preset = preset;
                        if let Ok(cron) = build_preset_cron(
                            preset,
                            current.hour,
                            current.minute,
                            current.day_of_week,
                        ) {
                            modal.cron.update(cx, |s, cx| s.set_value(cron, window, cx));
                        }
                        cx.notify();
                    }
                })),
            );
        }
        right = right
            .child(ui::field_label(text::AUTOMATIONS_SCHEDULE.get(), None, p))
            .child(presets)
            .child(input_field(
                text::SCHEDULE_FIELD_CRON.get(),
                "admin-schedule-cron",
                &modal.cron,
                p,
            ))
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::SCHEDULE_FIELD_CRON_HINT.get()),
            )
            .child(input_field(
                text::AUTOMATIONS_GRACE.get(),
                "admin-schedule-grace",
                &modal.grace,
                p,
            ));
        let mut grace = div().flex().flex_wrap().gap_1();
        for (minutes, label) in [
            (0, text::AUTOMATIONS_OFF.get().to_owned()),
            (60, "1h".to_owned()),
            (720, "12h".to_owned()),
            (1440, "24h".to_owned()),
        ] {
            let id = format!("admin-schedule-grace-{minutes}");
            grace = grace.child(
                layout::ghost(gpui::ElementId::Name(id.clone().into()), label)
                    .debug_selector(move || id.clone())
                    .disabled(pending)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(modal) = &mut this.schedule_modal {
                            modal
                                .grace
                                .update(cx, |s, cx| s.set_value(minutes.to_string(), window, cx));
                            cx.notify();
                        }
                    })),
            );
        }
        right = right.child(grace);
        let body = div()
            .id("admin-schedule-modal-scroll")
            .min_h_0()
            .overflow_y_scroll()
            .max_h((window.viewport_size().height - gpui::px(240.)).max(gpui::px(160.)))
            .child(
                div()
                    .flex()
                    .when(narrow, Styled::flex_col)
                    .gap_4()
                    .child(left)
                    .child(right),
            );
        let dialog = div()
            .id("admin-schedule-modal")
            .debug_selector(|| "admin-schedule-modal".into())
            .occlude()
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.close_schedule_modal(cx);
                window.focus(&this.focus, cx);
                cx.stop_propagation();
            }))
            .role(gpui::Role::Dialog)
            .w(gpui::px(860.))
            .max_w(window.viewport_size().width - ui::SPACE_4 * 2.)
            .max_h(window.viewport_size().height - ui::SPACE_4 * 2.)
            .p_6()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(heading),
            )
            .child(templates)
            .child(body)
            .children(modal.error.as_ref().map(|err| {
                ui::error_banner(p, err.clone())
                    .id("admin-schedule-modal-error")
                    .debug_selector(|| "admin-schedule-modal-error".into())
            }))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(
                        layout::ghost("admin-schedule-modal-cancel", text::SCHEDULE_CANCEL.get())
                            .debug_selector(|| "admin-schedule-modal-cancel".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_schedule_modal(cx);
                                window.focus(&this.focus, cx);
                            })),
                    )
                    .child(
                        layout::primary("admin-schedule-modal-save", text::SCHEDULE_SAVE.get())
                            .debug_selector(|| "admin-schedule-modal-save".into())
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit_schedule_modal(window, cx);
                            })),
                    ),
            );
        Some(ui::modal_backdrop(window.viewport_size(), dialog))
    }
}
