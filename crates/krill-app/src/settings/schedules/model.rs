//! Automations presentation state. Design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
use super::{ScheduleRun, text};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::settings) enum RunOutcome {
    Succeeded,
    Failed,
    Active,
    Skipped,
    Unknown,
}
pub(in crate::settings) fn run_outcome(run: &ScheduleRun) -> RunOutcome {
    match run.phase.as_str() {
        "finished" if run.status.is_none_or(|s| s == 0) => RunOutcome::Succeeded,
        "finished" | "failed" => RunOutcome::Failed,
        "spawned" | "started" => RunOutcome::Active,
        "skipped" => RunOutcome::Skipped,
        _ => RunOutcome::Unknown,
    }
}
pub fn run_outcome_badge_spec(run: &ScheduleRun) -> (crate::ui::BadgeTone, &'static str) {
    use crate::ui::BadgeTone;
    match run_outcome(run) {
        RunOutcome::Succeeded => (BadgeTone::Success, text::AUTOMATIONS_SUCCEEDED.get()),
        RunOutcome::Failed => (BadgeTone::Destructive, text::AUTOMATIONS_FAILED.get()),
        RunOutcome::Active => (BadgeTone::Warning, text::AUTOMATIONS_ACTIVE.get()),
        RunOutcome::Skipped => (BadgeTone::Muted, text::AUTOMATIONS_SKIPPED.get()),
        RunOutcome::Unknown => (BadgeTone::Neutral, text::AUTOMATIONS_NONE.get()),
    }
}

#[derive(Clone, Copy)]
pub(in crate::settings) struct Template {
    pub key: &'static str,
    pub id: &'static str,
    pub name: crate::i18n::Message,
    pub prompt: crate::i18n::Message,
    pub cron: &'static str,
    pub precheck: &'static str,
}
pub(in crate::settings) const TEMPLATES: [Template; 4] = [
    Template {
        key: "weekday-audit",
        id: "weekday-repo-audit",
        name: text::AUTOMATIONS_TEMPLATE_AUDIT,
        prompt: text::AUTOMATIONS_AUDIT_PROMPT,
        cron: "0 9 * * 1-5",
        precheck: "",
    },
    Template {
        key: "release-readiness",
        id: "release-readiness",
        name: text::AUTOMATIONS_TEMPLATE_RELEASE,
        prompt: text::AUTOMATIONS_RELEASE_PROMPT,
        cron: "0 10 * * 5",
        precheck: "",
    },
    Template {
        key: "daily-review",
        id: "daily-change-review",
        name: text::AUTOMATIONS_TEMPLATE_DAILY,
        prompt: text::AUTOMATIONS_DAILY_PROMPT,
        cron: "0 9 * * *",
        precheck: "",
    },
    Template {
        key: "hourly-queue",
        id: "hourly-queue-check",
        name: text::AUTOMATIONS_TEMPLATE_HOURLY,
        prompt: text::AUTOMATIONS_HOURLY_PROMPT,
        cron: "0 * * * *",
        precheck: "git status --porcelain | grep -q .",
    },
];
