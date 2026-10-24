//! Automations presentation state. Design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::settings) enum AutomationsSubView {
    #[default]
    List,
    Detail {
        id: String,
        tab: DetailTab,
    },
    RunsDashboard,
    RunDetail {
        id: String,
        minute: u64,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::settings) enum DetailTab {
    #[default]
    Overview,
    Runs,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::settings) enum StatusFilter {
    #[default]
    All,
    Enabled,
    Paused,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::settings) enum OutcomeFilter {
    #[default]
    All,
    Succeeded,
    Failed,
    Skipped,
    Never,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::settings) enum ScheduleSortField {
    #[default]
    Name,
    LastRun,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::settings) enum RunsStatusFilter {
    #[default]
    All,
    Succeeded,
    Failed,
    Active,
    Skipped,
}

pub(in crate::settings) struct SchedulesState {
    pub sub_view: AutomationsSubView,
    pub run_origin: AutomationsSubView,
    pub selected_run_number: Option<u64>,
    pub search_input: Option<gpui::Entity<InputState>>,
    pub status_filter: StatusFilter,
    pub outcome_filter: OutcomeFilter,
    pub agent_filter: Option<String>,
    pub sort_by: ScheduleSortField,
    pub sort_asc: bool,
    pub prompt_expanded: bool,
    pub runs_search_input: Option<gpui::Entity<InputState>>,
    pub runs_status_filter: RunsStatusFilter,
    pub subscriptions: Vec<gpui::Subscription>,
}
impl Default for SchedulesState {
    fn default() -> Self {
        Self {
            sub_view: AutomationsSubView::List,
            run_origin: AutomationsSubView::List,
            selected_run_number: None,
            search_input: None,
            status_filter: StatusFilter::All,
            outcome_filter: OutcomeFilter::All,
            agent_filter: None,
            sort_by: ScheduleSortField::Name,
            sort_asc: true,
            prompt_expanded: false,
            runs_search_input: None,
            runs_status_filter: RunsStatusFilter::All,
            subscriptions: Vec::new(),
        }
    }
}

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
pub(in crate::settings) fn now_minute() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 60
}
pub(in crate::settings) fn next_run(entry: &Schedule) -> String {
    if !entry.enabled {
        return text::AUTOMATIONS_PAUSED.get().to_owned();
    }
    krill_proto::schedule::Cron::parse(&entry.cron)
        .ok()
        .and_then(|c| c.next_minute_after(now_minute()))
        .map_or_else(
            || text::AUTOMATIONS_NONE.get().to_owned(),
            format_run_minute,
        )
}
pub(in crate::settings) fn latest_run<'a>(
    entry: &Schedule,
    runs: &'a [ScheduleRun],
) -> Option<&'a ScheduleRun> {
    runs.iter().filter(|r| r.id == entry.id).max_by(|a, b| {
        (a.minute, a.run_number.unwrap_or(0))
            .cmp(&(b.minute, b.run_number.unwrap_or(0)))
            .then(std::cmp::Ordering::Greater)
    })
}
pub(in crate::settings) fn matches_schedule(
    entry: &Schedule,
    runs: &[ScheduleRun],
    query: &str,
    state: &SchedulesState,
) -> bool {
    let haystack = format!(
        "{} {} {} {}",
        entry.display_name(),
        entry.id,
        entry.cwd,
        format_command_args(&entry.command)
    )
    .to_lowercase();
    if !haystack.contains(&query.to_lowercase()) {
        return false;
    }
    if matches!(state.status_filter, StatusFilter::Enabled) && !entry.enabled
        || matches!(state.status_filter, StatusFilter::Paused) && entry.enabled
    {
        return false;
    }
    if state
        .agent_filter
        .as_deref()
        .is_some_and(|a| entry.resolved_agent() != Some(a))
    {
        return false;
    }
    let outcome = latest_run(entry, runs).map(run_outcome);
    match state.outcome_filter {
        OutcomeFilter::All => true,
        OutcomeFilter::Never => outcome.is_none(),
        OutcomeFilter::Succeeded => outcome == Some(RunOutcome::Succeeded),
        OutcomeFilter::Failed => outcome == Some(RunOutcome::Failed),
        OutcomeFilter::Skipped => outcome == Some(RunOutcome::Skipped),
    }
}
pub(in crate::settings) fn matches_run(
    run: &ScheduleRun,
    query: &str,
    status: RunsStatusFilter,
    name: &str,
) -> bool {
    let haystack = format!(
        "{} {} {}",
        run.id,
        name,
        run.cwd.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    haystack.contains(&query.to_lowercase())
        && match status {
            RunsStatusFilter::All => true,
            RunsStatusFilter::Succeeded => run_outcome(run) == RunOutcome::Succeeded,
            RunsStatusFilter::Failed => run_outcome(run) == RunOutcome::Failed,
            RunsStatusFilter::Active => run_outcome(run) == RunOutcome::Active,
            RunsStatusFilter::Skipped => run_outcome(run) == RunOutcome::Skipped,
        }
}
pub(in crate::settings) fn retained_count(
    runs: &[ScheduleRun],
    now: u64,
    window: u64,
    outcome: RunOutcome,
) -> usize {
    runs.iter()
        .filter(|r| {
            r.minute >= now.saturating_sub(window) && r.minute <= now && run_outcome(r) == outcome
        })
        .count()
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
