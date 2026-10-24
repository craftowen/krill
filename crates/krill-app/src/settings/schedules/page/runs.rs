//! Runs surface for the Automations page.
use super::*;

impl SettingsView {
    pub(super) fn render_automations_runs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = ui::palette(cx);
        let input = self.automation_search(true, window, cx);
        let query = input.read(cx).value().trim().to_owned();
        let mut body = div().w_full().min_w_0().flex().flex_col().gap_4().child(
            div()
                .id("automations-runs-search")
                .debug_selector(|| "automations-runs-search".into())
                .child(Input::new(&input).small()),
        );
        let mut filters = div().flex().flex_wrap().gap_2();
        for (key, label, value) in [
            ("all", text::AUTOMATIONS_ALL.get(), RunsStatusFilter::All),
            (
                "succeeded",
                text::AUTOMATIONS_SUCCEEDED.get(),
                RunsStatusFilter::Succeeded,
            ),
            (
                "failed",
                text::AUTOMATIONS_FAILED.get(),
                RunsStatusFilter::Failed,
            ),
            (
                "active",
                text::AUTOMATIONS_ACTIVE.get(),
                RunsStatusFilter::Active,
            ),
            (
                "skipped",
                text::AUTOMATIONS_SKIPPED.get(),
                RunsStatusFilter::Skipped,
            ),
        ] {
            let selector = format!("automations-runs-filter-{key}");
            filters = filters.child(
                layout::segment(
                    p,
                    gpui::ElementId::Name(selector.clone().into()),
                    label,
                    self.automations.runs_status_filter == value,
                )
                .debug_selector(move || selector.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.automations.runs_status_filter = value;
                    cx.notify();
                })),
            );
        }
        body = body.child(filters);
        if let Some(error) = &self.services.runs_error {
            return body.child(ui::error_banner(p, error.clone()));
        }
        let mut summary = div().flex().flex_wrap().gap_3();
        for (key, label, minutes, outcome) in [
            (
                "succeeded-24h",
                text::AUTOMATIONS_SUCCESS_24H.get(),
                24 * 60,
                RunOutcome::Succeeded,
            ),
            (
                "failed-24h",
                text::AUTOMATIONS_FAILED_24H.get(),
                24 * 60,
                RunOutcome::Failed,
            ),
            (
                "succeeded-7d",
                text::AUTOMATIONS_SUCCESS_7D.get(),
                7 * 24 * 60,
                RunOutcome::Succeeded,
            ),
            (
                "failed-7d",
                text::AUTOMATIONS_FAILED_7D.get(),
                7 * 24 * 60,
                RunOutcome::Failed,
            ),
        ] {
            let selector = format!("automations-runs-summary-{key}");
            summary = summary.child(
                layout::card(p)
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex_1()
                    .child(label)
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(
                                retained_count(&self.services.runs, now_minute(), minutes, outcome)
                                    .to_string(),
                            ),
                    ),
            );
        }
        body.child(summary)
            .child(
                div()
                    .text_size(TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::AUTOMATIONS_RETAINED.get()),
            )
            .child(self.automation_run_rows(
                None,
                "automations-dashboard-run-row",
                &query,
                self.automations.runs_status_filter,
                cx,
            ))
    }

    pub(super) fn automation_run_rows(
        &self,
        schedule_id: Option<&str>,
        prefix: &str,
        query: &str,
        status: RunsStatusFilter,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = ui::palette(cx);
        if let Some(error) = &self.services.runs_error {
            return div().child(ui::error_banner(p, error.clone()));
        }
        let mut rows = div().flex().flex_col().gap_2();
        let mut runs: Vec<_> = self
            .services
            .runs
            .iter()
            .filter(|r| {
                let name = self
                    .services
                    .schedules
                    .iter()
                    .find(|s| s.id == r.id)
                    .map_or(r.id.as_str(), Schedule::display_name);
                schedule_id.is_none_or(|id| r.id == id) && matches_run(r, query, status, name)
            })
            .collect();
        runs.sort_by_key(|r| (std::cmp::Reverse(r.minute), std::cmp::Reverse(r.run_number)));
        if runs.is_empty() {
            return rows.child(text::AUTOMATIONS_NO_MATCHES.get());
        }
        for (idx, run) in runs.into_iter().enumerate() {
            let selector = format!("{prefix}-{idx}");
            let selected = run.clone();
            let (tone, label) = run_outcome_badge_spec(run);
            let name = self
                .services
                .schedules
                .iter()
                .find(|s| s.id == run.id)
                .map_or(run.id.as_str(), Schedule::display_name);
            rows = rows.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .role(gpui::Role::Button)
                    .aria_label(format!("{name} {}", format_run_minute(run.minute)))
                    .cursor_pointer()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .items_center()
                    .p_3()
                    .rounded(ui::RADIUS_SM)
                    .hover(|d| d.bg(p.accent))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.automation_open_run(&selected, cx)),
                    )
                    .child(format!(
                        "{} · {} · {}",
                        run_number(run),
                        name,
                        format_run_minute(run.minute)
                    ))
                    .child(ui::badge(p, BadgeTone::Muted, trigger(run)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(ui::FONT_MONO)
                            .text_color(p.muted_foreground)
                            .child(run.cwd.clone().unwrap_or_default()),
                    )
                    .children(run.status.map(|code| {
                        div().child(text::SCHEDULE_RUN_EXIT.format(&[("code", &code.to_string())]))
                    }))
                    .child(ui::badge(p, tone, label)),
            );
        }
        rows
    }

    pub(super) fn render_automation_run_detail(
        &self,
        id: &str,
        minute: u64,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = ui::palette(cx);
        let Some(run) = self.automation_selected_run(id, minute) else {
            return div().child(text::AUTOMATIONS_MISSING.get());
        };
        let name = self
            .services
            .schedules
            .iter()
            .find(|s| s.id == id)
            .map_or(id, Schedule::display_name);
        let (tone, label) = run_outcome_badge_spec(run);
        let rerun = id.to_owned();
        let mut actions = div().flex().gap_2().child(
            ghost("automations-run-detail-back", text::AUTOMATIONS_BACK.get())
                .on_click(cx.listener(|this, _, window, cx| this.automation_back(window, cx))),
        );
        if self.services.schedules.iter().any(|s| s.id == id) {
            actions = actions.child(
                ghost(
                    "automations-run-detail-rerun",
                    text::AUTOMATIONS_RERUN.get(),
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| this.trigger_schedule_run_now(&rerun, cx)),
                ),
            );
        }
        let output = run
            .output
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.schedule_log
                    .as_ref()
                    .filter(|l| l.id == id && l.minute == minute)
                    .map(|l| l.output.clone())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| text::SCHEDULE_LOG_EMPTY.get().to_owned());
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{name} · {}", run_number(run))),
                    )
                    .child(ui::badge(p, tone, label))
                    .child(actions),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(format_run_minute(minute))
                    .child(ui::badge(p, BadgeTone::Muted, trigger(run)))
                    .child(run.cwd.clone().unwrap_or_default())
                    .children(run.status.map(|code| {
                        div().child(text::SCHEDULE_RUN_EXIT.format(&[("code", &code.to_string())]))
                    })),
            )
            .child(
                layout::card(p)
                    .id("automations-run-detail-output")
                    .debug_selector(|| "automations-run-detail-output".into())
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text::AUTOMATIONS_OUTPUT.get()),
                    )
                    .child(
                        div()
                            .font_family(ui::FONT_MONO)
                            .text_size(TEXT_SM)
                            .child(output),
                    )
                    .children(
                        run.error
                            .as_ref()
                            .map(|error| ui::error_banner(p, error.clone())),
                    ),
            )
    }
}
