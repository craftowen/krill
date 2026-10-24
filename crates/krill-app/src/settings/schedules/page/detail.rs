//! Detail surface for the Automations page.
use super::{
    AutomationsSubView, DetailTab, RunsStatusFilter, SettingsView, WorkspaceMode,
    format_command_args, format_run_minute, ghost, latest_run, layout, next_run,
    run_outcome_badge_spec, schedule_summary, status_badge, text,
};
use crate::ui;
use gpui::{Context, Div, FontWeight, div, prelude::*};

impl SettingsView {
    pub(super) fn render_automation_detail(
        &self,
        id: &str,
        tab: DetailTab,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = ui::palette(cx);
        let Some(entry) = self.services.schedules.iter().find(|e| e.id == id).cloned() else {
            return div().child(text::AUTOMATIONS_MISSING.get());
        };
        let mut body = div()
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
                            .child(entry.display_name().to_owned()),
                    )
                    .child(status_badge(&entry, p))
                    .child(self.automation_actions(&entry, "detail", cx)),
            )
            .child(
                div()
                    .font_family(ui::FONT_MONO)
                    .text_color(p.muted_foreground)
                    .child(entry.cwd.clone()),
            );
        let tabs = [
            (
                DetailTab::Overview,
                "automations-detail-tab-overview",
                text::AUTOMATIONS_OVERVIEW.get(),
            ),
            (
                DetailTab::Runs,
                "automations-detail-tab-runs",
                text::AUTOMATIONS_RUNS.get(),
            ),
        ]
        .map(|(value, selector, label)| {
            let id = id.to_owned();
            layout::segment(p, selector, label, tab == value)
                .debug_selector(move || selector.into())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.automation_navigate(
                        AutomationsSubView::Detail {
                            id: id.clone(),
                            tab: value,
                        },
                        cx,
                    );
                }))
        });
        body = body.child(layout::segmented_control(p, tabs));
        if tab == DetailTab::Runs {
            return body.child(self.automation_run_rows(
                Some(id),
                "automations-detail-run-row",
                "",
                RunsStatusFilter::All,
                cx,
            ));
        }
        let workspace = if entry.workspace_mode == WorkspaceMode::Isolate {
            text::AUTOMATIONS_ISOLATE_BASE
                .format(&[("base", entry.base_branch.as_deref().unwrap_or("HEAD"))])
        } else {
            entry.cwd.clone()
        };
        let grace = entry.effective_grace_minutes();
        let precheck = entry.precheck.as_ref().map_or_else(
            || text::AUTOMATIONS_NONE.get().to_owned(),
            |command| {
                text::AUTOMATIONS_PRECHECK_DETAIL.format(&[
                    ("command", command),
                    (
                        "seconds",
                        &entry.effective_precheck_timeout_secs().to_string(),
                    ),
                ])
            },
        );
        let mut metrics = layout::card(p)
            .id("automations-detail-metrics")
            .debug_selector(|| "automations-detail-metrics".into());
        let mut values = vec![
            (
                text::AUTOMATIONS_SCHEDULE.get(),
                format!("{} · {}", schedule_summary(&entry), entry.cron),
            ),
            (text::AUTOMATIONS_NEXT_RUN.get(), next_run(&entry)),
            (text::AUTOMATIONS_WORKSPACE.get(), workspace),
            (
                text::AUTOMATIONS_SESSION.get(),
                if entry.reuse_session {
                    text::AUTOMATIONS_REUSE_SESSION.get()
                } else {
                    text::AUTOMATIONS_FRESH.get()
                }
                .to_owned(),
            ),
            (
                text::AUTOMATIONS_GRACE.get(),
                if grace == 0 {
                    text::AUTOMATIONS_OFF.get().to_owned()
                } else {
                    text::AUTOMATIONS_MINUTES.format(&[("n", &grace.to_string())])
                },
            ),
            (text::SCHEDULE_FIELD_PRECHECK.get(), precheck),
            (
                text::SCHEDULE_FIELD_COMMAND.get(),
                format_command_args(&entry.command),
            ),
        ];
        if let Some(args) = &entry.extra_agent_args {
            values.push((text::AUTOMATIONS_EXTRA_ARGS.get(), args.clone()));
        }
        for (label, value) in values {
            metrics = metrics.child(
                layout::row(p)
                    .child(layout::label(p, label, None))
                    .child(div().flex_1().min_w_0().child(value)),
            );
        }
        metrics = metrics.child(
            layout::row(p)
                .child(layout::label(p, text::AUTOMATIONS_AGENT.get(), None))
                .child(Self::automation_agent(&entry, p)),
        );
        if let Some(run) = latest_run(&entry, &self.services.runs) {
            let (tone, label) = run_outcome_badge_spec(run);
            metrics = metrics.child(
                layout::row(p)
                    .child(layout::label(p, text::AUTOMATIONS_LAST_RUN.get(), None))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(format_run_minute(run.minute))
                            .child(ui::badge(p, tone, label)),
                    ),
            );
        }
        if let Some(error) = &self.services.runs_error {
            body = body.child(ui::error_banner(p, error.clone()));
        }
        let long = entry.prompt.lines().count() > 4 || entry.prompt.len() > 240;
        let prompt = if entry.prompt.is_empty() {
            text::AUTOMATIONS_NONE.get().to_owned()
        } else if long && !self.automations.prompt_expanded {
            let mut bytes = 0;
            entry
                .prompt
                .lines()
                .take(4)
                .collect::<Vec<_>>()
                .join("\n")
                .chars()
                .take_while(|ch| {
                    bytes += ch.len_utf8();
                    bytes <= 240
                })
                .collect::<String>()
                + "…"
        } else {
            entry.prompt.clone()
        };
        let mut prompt_card = layout::card(p)
            .id("automations-detail-prompt")
            .debug_selector(|| "automations-detail-prompt".into())
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text::AUTOMATIONS_PROMPT.get()),
            )
            .child(div().child(prompt));
        if long {
            prompt_card = prompt_card.child(
                ghost(
                    "automations-prompt-toggle",
                    if self.automations.prompt_expanded {
                        text::AUTOMATIONS_SHOW_LESS.get()
                    } else {
                        text::AUTOMATIONS_SHOW_MORE.get()
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.automations.prompt_expanded = !this.automations.prompt_expanded;
                    cx.notify();
                })),
            );
        }
        body.child(metrics).child(prompt_card)
    }
}
