//! List surface for the Automations page.
use super::*;

impl SettingsView {
    pub(super) fn render_automations_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = ui::palette(cx);
        let search = self.automation_search(false, window, cx);
        let query = search.read(cx).value().trim().to_owned();
        let mut toolbar = div().flex().flex_col().gap_2().child(
            div()
                .id("automations-search-input")
                .debug_selector(|| "automations-search-input".into())
                .child(Input::new(&search).small()),
        );
        let mut filters = div().flex().flex_wrap().items_center().gap_2();
        for (key, label, value) in [
            ("all", text::AUTOMATIONS_ALL.get(), StatusFilter::All),
            (
                "enabled",
                text::AUTOMATIONS_ENABLED.get(),
                StatusFilter::Enabled,
            ),
            (
                "paused",
                text::AUTOMATIONS_PAUSED.get(),
                StatusFilter::Paused,
            ),
        ] {
            let id = format!("automations-filter-status-{key}");
            filters = filters.child(
                layout::segment(
                    p,
                    gpui::ElementId::Name(id.clone().into()),
                    label,
                    self.automations.status_filter == value,
                )
                .debug_selector(move || id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.automations.status_filter = value;
                    cx.notify();
                })),
            );
        }
        for (key, label, value) in [
            ("all", text::AUTOMATIONS_ALL.get(), OutcomeFilter::All),
            (
                "succeeded",
                text::AUTOMATIONS_SUCCEEDED.get(),
                OutcomeFilter::Succeeded,
            ),
            (
                "failed",
                text::AUTOMATIONS_FAILED.get(),
                OutcomeFilter::Failed,
            ),
            (
                "skipped",
                text::AUTOMATIONS_SKIPPED.get(),
                OutcomeFilter::Skipped,
            ),
            ("never", text::AUTOMATIONS_NEVER.get(), OutcomeFilter::Never),
        ] {
            let id = format!("automations-filter-outcome-{key}");
            filters = filters.child(
                layout::segment(
                    p,
                    gpui::ElementId::Name(id.clone().into()),
                    label,
                    self.automations.outcome_filter == value,
                )
                .debug_selector(move || id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.automations.outcome_filter = value;
                    cx.notify();
                })),
            );
        }
        for (key, label) in [
            ("all", text::AUTOMATIONS_ALL.get()),
            ("claude", "Claude"),
            ("codex", "Codex"),
            ("agy", "Antigravity"),
            ("opencode", "OpenCode"),
            ("cursor", "Cursor"),
        ] {
            let value = (key != "all").then(|| key.to_owned());
            let id = format!("automations-filter-agent-{key}");
            filters = filters.child(
                layout::segment(
                    p,
                    gpui::ElementId::Name(id.clone().into()),
                    label,
                    self.automations.agent_filter == value,
                )
                .debug_selector(move || id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.automations.agent_filter.clone_from(&value);
                    cx.notify();
                })),
            );
        }
        toolbar = toolbar.child(filters);
        let has_filters = self.automations.status_filter != StatusFilter::All
            || self.automations.outcome_filter != OutcomeFilter::All
            || self.automations.agent_filter.is_some()
            || !query.is_empty();
        if has_filters {
            let mut pills = div().flex().flex_wrap().gap_2();
            if self.automations.status_filter != StatusFilter::All {
                pills = pills.child(
                    ghost(
                        "automations-active-pill-status",
                        format!(
                            "{} ×",
                            if self.automations.status_filter == StatusFilter::Enabled {
                                text::AUTOMATIONS_ENABLED.get()
                            } else {
                                text::AUTOMATIONS_PAUSED.get()
                            }
                        ),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.automations.status_filter = StatusFilter::All;
                        cx.notify();
                    })),
                );
            }
            if self.automations.outcome_filter != OutcomeFilter::All {
                let label = match self.automations.outcome_filter {
                    OutcomeFilter::Succeeded => text::AUTOMATIONS_SUCCEEDED.get(),
                    OutcomeFilter::Failed => text::AUTOMATIONS_FAILED.get(),
                    OutcomeFilter::Skipped => text::AUTOMATIONS_SKIPPED.get(),
                    _ => text::AUTOMATIONS_NEVER.get(),
                };
                pills = pills.child(
                    ghost("automations-active-pill-outcome", format!("{label} ×")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.automations.outcome_filter = OutcomeFilter::All;
                            cx.notify();
                        }),
                    ),
                );
            }
            if let Some(agent) = &self.automations.agent_filter {
                pills = pills.child(
                    ghost("automations-active-pill-agent", format!("{agent} ×")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.automations.agent_filter = None;
                            cx.notify();
                        }),
                    ),
                );
            }
            if !query.is_empty() {
                pills = pills.child(
                    ghost("automations-active-pill-search", format!("{query} ×")).on_click(
                        cx.listener(|this, _, window, cx| {
                            if let Some(input) = &this.automations.search_input {
                                input.update(cx, |s, cx| s.set_value("", window, cx));
                            }
                            cx.notify();
                        }),
                    ),
                );
            }
            toolbar = toolbar.child(
                pills.child(
                    ghost(
                        "automations-clear-filters",
                        text::AUTOMATIONS_CLEAR_FILTERS.get(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.automations.status_filter = StatusFilter::All;
                        this.automations.outcome_filter = OutcomeFilter::All;
                        this.automations.agent_filter = None;
                        if let Some(input) = &this.automations.search_input {
                            input.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                        cx.notify();
                    })),
                ),
            );
        }
        let mut body = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .child(toolbar);
        if let Some(error) = &self.services.schedules_error {
            return body.child(ui::error_banner(p, error.clone()));
        }
        if let Some(error) = &self.services.runs_error {
            body = body.child(ui::error_banner(p, error.clone()));
        }
        if self.services_pending
            && self.services_checked.is_none()
            && self.services.schedules.is_empty()
        {
            return body.child(text::AUTOMATIONS_LOADING.get());
        }
        if self.services.runs_error.is_some()
            && self.automations.outcome_filter != OutcomeFilter::All
        {
            return body;
        }
        if self.services.schedules.is_empty() {
            body = body
                .child(
                    div()
                        .text_size(ui::TEXT_LG)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(text::AUTOMATIONS_EMPTY.get()),
                )
                .child(
                    div()
                        .text_color(p.muted_foreground)
                        .child(text::AUTOMATIONS_EMPTY_DESC.get()),
                );
            let mut templates = div().flex().flex_wrap().gap_3();
            for template in TEMPLATES {
                let id = format!("automations-template-{}", template.key);
                templates = templates.child(
                    layout::card(p)
                        .id(SharedString::from(id.clone()))
                        .debug_selector(move || id.clone())
                        .cursor_pointer()
                        .w(gpui::px(280.))
                        .hover(|d| d.bg(p.accent))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_schedule_modal(None, window, cx);
                            this.apply_schedule_template(template, window, cx);
                        }))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(template.name.get()),
                        )
                        .child(
                            div()
                                .text_size(TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(template.cron),
                        )
                        .child(template.prompt.get()),
                );
            }
            return body.child(templates);
        }
        let mut entries: Vec<_> = self
            .services
            .schedules
            .iter()
            .filter(|e| matches_schedule(e, &self.services.runs, &query, &self.automations))
            .cloned()
            .collect();
        entries.sort_by(|a, b| {
            let order = match self.automations.sort_by {
                ScheduleSortField::Name => a
                    .display_name()
                    .to_lowercase()
                    .cmp(&b.display_name().to_lowercase()),
                ScheduleSortField::LastRun => latest_run(a, &self.services.runs)
                    .map(|r| r.minute)
                    .cmp(&latest_run(b, &self.services.runs).map(|r| r.minute)),
            }
            .then_with(|| a.id.cmp(&b.id));
            if self.automations.sort_asc {
                order
            } else {
                order.reverse()
            }
        });
        if entries.is_empty() {
            return body.child(text::AUTOMATIONS_NO_MATCHES.get());
        }
        let mut table = div().min_w(gpui::px(1540.)).flex().flex_col().gap_1();
        let mut header = div()
            .flex()
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(p.border)
            .py_2();
        for (label, width, sort) in [
            (
                text::AUTOMATIONS_NAME.get(),
                160.,
                Some(ScheduleSortField::Name),
            ),
            (text::AUTOMATIONS_SCHEDULE.get(), 150., None),
            (text::AUTOMATIONS_PROJECT.get(), 150., None),
            (text::AUTOMATIONS_NEXT_RUN.get(), 150., None),
            (
                text::AUTOMATIONS_LAST_RUN.get(),
                210.,
                Some(ScheduleSortField::LastRun),
            ),
            (text::AUTOMATIONS_STATUS.get(), 80., None),
            (text::AUTOMATIONS_AGENT.get(), 110., None),
            (text::AUTOMATIONS_ACTIONS.get(), 300., None),
        ] {
            header = header.child(if let Some(sort) = sort {
                let id = if sort == ScheduleSortField::Name {
                    "automations-sort-name"
                } else {
                    "automations-sort-last-run"
                };
                cell("", width).child(
                    ghost(
                        id,
                        format!(
                            "{label} {}",
                            if self.automations.sort_by == sort {
                                if self.automations.sort_asc {
                                    "↑"
                                } else {
                                    "↓"
                                }
                            } else {
                                ""
                            }
                        ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.automations.sort_by == sort {
                            this.automations.sort_asc = !this.automations.sort_asc;
                        } else {
                            this.automations.sort_by = sort;
                            this.automations.sort_asc = sort == ScheduleSortField::Name;
                        }
                        cx.notify();
                    })),
                )
            } else {
                cell(label, width).text_color(p.muted_foreground)
            });
        }
        table = table.child(header);
        for entry in entries {
            let selector = format!("automations-row-{}", entry.id);
            let id = entry.id.clone();
            let mut name = cell(entry.display_name().to_owned(), 160.);
            if entry.display_name() != entry.id {
                name = name.child(
                    div()
                        .text_size(TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(entry.id.clone()),
                );
            }
            let project = Path::new(&entry.cwd)
                .file_name()
                .map_or_else(|| entry.cwd.clone(), |n| n.to_string_lossy().into_owned());
            let mut project_cell = cell(project, 150.).child(
                div()
                    .text_size(TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(entry.cwd.clone()),
            );
            if entry.workspace_mode == WorkspaceMode::Isolate {
                project_cell = project_cell.child(ui::badge(
                    p,
                    BadgeTone::Muted,
                    text::SCHEDULE_MODE_ISOLATE.get(),
                ));
            }
            let mut last = cell(
                if self.services.runs_error.is_some() {
                    text::AUTOMATIONS_UNAVAILABLE.get()
                } else {
                    text::AUTOMATIONS_NEVER.get()
                },
                210.,
            );
            if let Some(run) = latest_run(&entry, &self.services.runs) {
                let (tone, label) = run_outcome_badge_spec(run);
                last = cell(format_run_minute(run.minute), 210.).child(ui::badge(p, tone, label));
            }
            let actions = self.automation_actions(&entry, "row", cx);
            table = table.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .role(gpui::Role::Button)
                    .aria_label(entry.display_name().to_owned())
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_3()
                    .rounded(ui::RADIUS_SM)
                    .hover(|d| d.bg(p.accent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.automation_navigate(
                            AutomationsSubView::Detail {
                                id: id.clone(),
                                tab: DetailTab::Overview,
                            },
                            cx,
                        );
                    }))
                    .child(name)
                    .child(cell(schedule_summary(&entry), 150.))
                    .child(project_cell)
                    .child(cell(next_run(&entry), 150.))
                    .child(last)
                    .child(cell("", 80.).child(status_badge(&entry, p)))
                    .child(Self::automation_agent(&entry, p))
                    .child(actions),
            );
        }
        body.child(
            div()
                .id("automations-table-scroll")
                .min_w_0()
                .overflow_x_scroll()
                .child(table),
        )
    }

    pub(super) fn automation_agent(entry: &Schedule, p: &ui::Palette) -> Div {
        let agent = entry.resolved_agent().unwrap_or_default();
        let label = if agent.is_empty() {
            format_command_args(&entry.command)
        } else {
            agent.to_owned()
        };
        let mut element = cell("", 110.).flex().items_center().gap_2();
        if !agent.is_empty() {
            element = element.child(ui::icons::svg_icon(
                ui::icons::agent_svg_bytes(agent),
                ui::ICON_SM,
                p.muted_foreground,
            ));
        }
        element.child(div().min_w_0().child(label))
    }

    pub(super) fn automation_actions(
        &self,
        entry: &Schedule,
        surface: &str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = entry.id.clone();
        let edit = entry.clone();
        let toggle = entry.id.clone();
        let delete = entry.id.clone();
        let selector = |action: &str| {
            if surface == "row" {
                format!("automations-row-{action}-{}", entry.id)
            } else {
                format!("automations-detail-{action}")
            }
        };
        div()
            .id(SharedString::from(format!(
                "automations-{surface}-actions-{}",
                entry.id
            )))
            .flex_none()
            .flex()
            .flex_wrap()
            .gap_1()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                ghost(
                    selector(if surface == "row" { "run" } else { "run-now" }),
                    text::SCHEDULE_RUN_NOW.get(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.trigger_schedule_run_now(&id, cx);
                })),
            )
            .child(
                ghost(selector("edit"), text::SCHEDULE_EDIT.get()).on_click(cx.listener(
                    move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_schedule_modal(Some(&edit), window, cx);
                    },
                )),
            )
            .child(
                ghost(
                    selector("toggle"),
                    if entry.enabled {
                        text::AUTOMATIONS_PAUSE.get()
                    } else {
                        text::AUTOMATIONS_RESUME.get()
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_schedule_enabled(&toggle, cx);
                })),
            )
            .child(self.confirm_button(
                selector("delete"),
                text::SCHEDULE_REMOVE.get(),
                cx,
                move |this, cx| {
                    this.remove_schedule(&delete, cx);
                    if matches!(this.automations.sub_view, AutomationsSubView::Detail { .. }) {
                        this.automation_navigate(AutomationsSubView::List, cx);
                    }
                },
            ))
    }
}
