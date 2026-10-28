//! Automations page (Orca `AutomationsPage` UI/UX parity).
//! Full in-app page for managing scheduled automations (`schedules.toml`), starter templates,
//! 2-column prompt/settings editor modal, automation detail overview & run history tabs,
//! global runs dashboard with 24h/7d summary cards, and run output log viewer.
//! UI/design adapted from Orca (src/renderer/src/components/automations/AutomationsPage.tsx).
//! Copyright (c) 2026 Lovecast Inc., MIT License; implemented here in Rust.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, FontWeight, KeyDownEvent, SharedString,
    Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::{IconName, Sizable as _};
use krill_proto::schedule::{
    Cron, Schedule, ScheduleRun, Schedules, WorkspaceMode, load_coalesced_runs, request_run_now,
};

use crate::i18n::catalog::automations as text;
use crate::i18n::catalog::settings as stext;
use crate::settings::schedules::{
    capture_session_output, format_command_args, format_run_minute, parse_command_args,
    run_phase_badge_spec,
};
use crate::ui;

// Keep scheduler options that the simplified editor does not expose.
fn apply_schedule_edit(
    schedules: &mut Schedules,
    editing_id: Option<&str>,
    edited: Schedule,
) -> std::io::Result<()> {
    if schedules
        .schedule
        .iter()
        .any(|s| s.id == edited.id && Some(s.id.as_str()) != editing_id)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "schedule ID already exists",
        ));
    }
    let existing = if let Some(id) = editing_id {
        let index = schedules
            .schedule
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "schedule no longer exists")
            })?;
        schedules.schedule.remove(index)
    } else {
        Schedule::default()
    };
    schedules.schedule.push(Schedule {
        id: edited.id,
        cron: edited.cron,
        cwd: edited.cwd,
        command: edited.command,
        prompt: edited.prompt,
        precheck: edited.precheck,
        workspace_mode: edited.workspace_mode,
        enabled: edited.enabled,
        ..existing
    });
    Ok(())
}

#[derive(Debug)]
pub struct Close;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailTab {
    #[default]
    Overview,
    Runs,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PageView {
    #[default]
    List,
    Detail {
        id: String,
        tab: DetailTab,
    },
    RunsDashboard,
    RunDetails {
        run: Box<ScheduleRun>,
        return_to_detail: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusFilter {
    #[default]
    All,
    Enabled,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LastRunFilter {
    #[default]
    All,
    Failed,
    Succeeded,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentFilter {
    #[default]
    All,
    Claude,
    Codex,
    Antigravity,
    OpenCode,
    Cursor,
    Pi,
}

impl AgentFilter {
    pub const ALL_VARIANTS: [Self; 7] = [
        Self::All,
        Self::Claude,
        Self::Codex,
        Self::Antigravity,
        Self::OpenCode,
        Self::Cursor,
        Self::Pi,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => text::FILTER_ALL.get(),
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Antigravity => "Antigravity",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor",
            Self::Pi => "Pi",
        }
    }

    #[must_use]
    pub fn brand_id(self) -> &'static str {
        match self {
            Self::All | Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Antigravity => "agy",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
            Self::Pi => "pi",
        }
    }

    #[must_use]
    pub fn default_command(self) -> &'static str {
        match self {
            Self::All | Self::Claude => "claude -p",
            Self::Codex => "codex exec",
            Self::Antigravity => "agy",
            Self::OpenCode => "opencode run",
            Self::Cursor => "cursor",
            Self::Pi => "pi -p",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunStatusFilter {
    #[default]
    All,
    Successful,
    Failed,
    InProgress,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortField {
    #[default]
    Name,
    LastRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortDirection {
    #[default]
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScheduleCadence {
    Hourly,
    #[default]
    Daily,
    Weekdays,
    Weekly,
    Custom,
}

impl ScheduleCadence {
    #[must_use]
    pub fn from_cron(cron: &str) -> Self {
        match cron.trim() {
            "0 * * * *" | "15 * * * *" | "30 * * * *" => Self::Hourly,
            "0 9 * * *" | "30 16 * * *" => Self::Daily,
            "0 9 * * 1-5" => Self::Weekdays,
            "0 9 * * 1" | "0 14 * * 4" => Self::Weekly,
            _ => Self::Custom,
        }
    }

    #[must_use]
    pub fn default_cron(self) -> Option<&'static str> {
        match self {
            Self::Hourly => Some("0 * * * *"),
            Self::Daily => Some("0 9 * * *"),
            Self::Weekdays => Some("0 9 * * 1-5"),
            Self::Weekly => Some("0 9 * * 1"),
            Self::Custom => None,
        }
    }
}

/// Starter template definition matching Orca's `AUTOMATION_TEMPLATES`.
#[derive(Debug, Clone, Copy)]
pub struct AutomationTemplate {
    pub id: &'static str,
    pub cron: &'static str,
    pub prompt: &'static str,
}

pub const TEMPLATES: [AutomationTemplate; 4] = [
    AutomationTemplate {
        id: "weekday-repo-audit",
        cron: "0 9 * * 1-5",
        prompt: "Review the current repository status, recent commits, failing checks, and stale branches. Summarize top maintenance risks and concrete next actions.",
    },
    AutomationTemplate {
        id: "release-readiness",
        cron: "0 14 * * 4",
        prompt: "Inspect commits since the last tag, verify test/build health, and summarize release readiness, breaking changes, and remaining blockers.",
    },
    AutomationTemplate {
        id: "daily-change-review",
        cron: "30 16 * * *",
        prompt: "Scan today's commits and open diffs for regressions, missing tests, or security concerns. Report only actionable findings.",
    },
    AutomationTemplate {
        id: "hourly-queue-check",
        cron: "15 * * * *",
        prompt: "Check the workflow queue and recent run statuses. Report only if a job is stuck or failing repeatedly.",
    },
];

impl AutomationTemplate {
    #[must_use]
    pub fn category(self) -> &'static str {
        match self.id {
            "weekday-repo-audit" => text::TPL_AUDIT_CATEGORY.get(),
            "release-readiness" => text::TPL_RELEASE_CATEGORY.get(),
            "daily-change-review" => text::TPL_REVIEW_CATEGORY.get(),
            _ => text::TPL_QUEUE_CATEGORY.get(),
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self.id {
            "weekday-repo-audit" => text::TPL_AUDIT_NAME.get(),
            "release-readiness" => text::TPL_RELEASE_NAME.get(),
            "daily-change-review" => text::TPL_REVIEW_NAME.get(),
            _ => text::TPL_QUEUE_NAME.get(),
        }
    }

    #[must_use]
    pub fn summary(self) -> &'static str {
        match self.id {
            "weekday-repo-audit" => text::TPL_AUDIT_SUMMARY.get(),
            "release-readiness" => text::TPL_RELEASE_SUMMARY.get(),
            "daily-change-review" => text::TPL_REVIEW_SUMMARY.get(),
            _ => text::TPL_QUEUE_SUMMARY.get(),
        }
    }
}

/// Infer agent family from schedule command array.
#[must_use]
pub fn infer_agent_from_command(command: &[String]) -> (AgentFilter, &'static str) {
    let first = command
        .first()
        .map(|s| {
            Path::new(s)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(s.as_str())
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    if first.contains("codex") {
        (AgentFilter::Codex, "Codex")
    } else if first.contains("agy") || first.contains("antigravity") {
        (AgentFilter::Antigravity, "Antigravity")
    } else if first.contains("opencode") {
        (AgentFilter::OpenCode, "OpenCode")
    } else if first.contains("cursor") {
        (AgentFilter::Cursor, "Cursor")
    } else if first == "pi" || first.starts_with("pi-") {
        (AgentFilter::Pi, "Pi")
    } else {
        (AgentFilter::Claude, "Claude")
    }
}

/// Format a 5-field UTC cron expression into a human-friendly summary label like Orca.
#[must_use]
pub fn format_cron_summary(cron: &str) -> String {
    let parts: Vec<&str> = cron.split_whitespace().collect();
    let [min, hour, dom, month, dow] = parts.as_slice() else {
        return cron.to_owned();
    };
    if *dom == "*"
        && *month == "*"
        && let Ok(m) = min.parse::<u32>()
    {
        if *hour == "*" && *dow == "*" {
            return format!("{} :{m:02}", text::CADENCE_HOURLY.get());
        }
        if let Ok(h) = hour.parse::<u32>() {
            match *dow {
                "*" => return format!("{} {h:02}:{m:02} UTC", text::CADENCE_DAILY.get()),
                "1-5" => return format!("{} {h:02}:{m:02} UTC", text::CADENCE_WEEKDAYS.get()),
                "0" | "7" => return format!("Sun {h:02}:{m:02} UTC"),
                "1" => return format!("Mon {h:02}:{m:02} UTC"),
                "2" => return format!("Tue {h:02}:{m:02} UTC"),
                "3" => return format!("Wed {h:02}:{m:02} UTC"),
                "4" => return format!("Thu {h:02}:{m:02} UTC"),
                "5" => return format!("Fri {h:02}:{m:02} UTC"),
                "6" => return format!("Sat {h:02}:{m:02} UTC"),
                _ => {}
            }
        }
    }
    cron.to_owned()
}

/// Convert Unix epoch minute to `[minute, hour, day, month, weekday]` in UTC.
fn utc_fields_from_minute(epoch_minute: u64) -> Option<[u32; 5]> {
    let days = epoch_minute / 1_440;
    let min_of_day = (epoch_minute % 1_440) as u32;
    let hour = min_of_day / 60;
    let minute = min_of_day % 60;
    // 1970-01-01 was a Thursday (weekday 4, Sunday = 0).
    let weekday = ((days + 4) % 7) as u32;

    let z = i64::try_from(days.saturating_add(719_468)).ok()?;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).ok()?;
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).ok()?;
    Some([minute, hour, d, m, weekday])
}

/// Compute the next matching UTC epoch minute for `cron_expr` strictly after `now_minute`
/// (searches up to 8 days ahead for fast bounded UI rendering).
#[must_use]
pub fn next_run_minute(cron_expr: &str, now_minute: u64) -> Option<u64> {
    let cron = Cron::parse(cron_expr).ok()?;
    let start = now_minute.saturating_add(1);
    for offset in 0..=(8 * 1_440) {
        let candidate = start.saturating_add(offset);
        let fields = utc_fields_from_minute(candidate)?;
        if cron.matches(fields) {
            return Some(candidate);
        }
    }
    None
}

fn current_epoch_minute() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 60)
}

#[must_use]
pub fn project_label(cwd: &str) -> String {
    let trimmed = cwd.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return "—".into();
    }
    Path::new(trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
        .to_owned()
}

#[derive(Debug)]
pub struct EditorModal {
    pub editing_id: Option<String>,
    pub id: Entity<InputState>,
    pub cron: Entity<InputState>,
    pub cwd: Entity<InputState>,
    pub command: Entity<InputState>,
    pub prompt: Entity<InputState>,
    pub precheck: Entity<InputState>,
    pub cadence: ScheduleCadence,
    pub agent: AgentFilter,
    pub workspace_mode: WorkspaceMode,
    pub enabled: bool,
    pub error: Option<SharedString>,
}

pub struct View {
    focus: FocusHandle,
    modal_focus: FocusHandle,
    state_dir: Option<PathBuf>,
    default_cwd: String,
    screen: PageView,
    schedules: Vec<Schedule>,
    runs: Vec<ScheduleRun>,
    search_input: Option<Entity<InputState>>,
    search_sub: Option<Subscription>,
    query: String,
    runs_search_input: Option<Entity<InputState>>,
    runs_search_sub: Option<Subscription>,
    runs_query: String,
    status_filter: StatusFilter,
    last_run_filter: LastRunFilter,
    agent_filter: AgentFilter,
    run_status_filter: RunStatusFilter,
    sort_field: SortField,
    sort_dir: SortDirection,
    prompt_expanded: bool,
    filter_menu_open: bool,
    template_menu_open: bool,
    modal: Option<EditorModal>,
    run_log_output: String,
    run_log_generation: u64,
    form_generation: u64,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    language: crate::i18n::Language,
}

impl EventEmitter<Close> for View {}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Automations")
            .field("schedules", &self.schedules.len())
            .field("runs", &self.runs.len())
            .field("screen", &self.screen)
            .finish_non_exhaustive()
    }
}

impl View {
    pub fn new(
        state_dir: Option<PathBuf>,
        default_cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let fallback_cwd = default_cwd
            .or_else(|| std::env::current_dir().ok())
            .map_or_else(|| "/tmp".into(), |p| p.to_string_lossy().into_owned());
        let mut view = Self {
            focus: cx.focus_handle(),
            modal_focus: cx.focus_handle(),
            state_dir,
            default_cwd: fallback_cwd,
            screen: PageView::List,
            schedules: Vec::new(),
            runs: Vec::new(),
            search_input: None,
            search_sub: None,
            query: String::new(),
            runs_search_input: None,
            runs_search_sub: None,
            runs_query: String::new(),
            status_filter: StatusFilter::All,
            last_run_filter: LastRunFilter::All,
            agent_filter: AgentFilter::All,
            run_status_filter: RunStatusFilter::All,
            sort_field: SortField::Name,
            sort_dir: SortDirection::Asc,
            prompt_expanded: false,
            filter_menu_open: false,
            template_menu_open: false,
            modal: None,
            run_log_output: String::new(),
            run_log_generation: 0,
            form_generation: 0,
            error: None,
            task: None,
            language: crate::i18n::language(),
        };
        view.reload(cx);
        view
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    #[must_use]
    pub fn page_view(&self) -> &PageView {
        &self.screen
    }

    #[must_use]
    pub fn schedules(&self) -> &[Schedule] {
        &self.schedules
    }

    #[must_use]
    pub fn runs(&self) -> &[ScheduleRun] {
        &self.runs
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.state_dir.clone() else {
            return;
        };
        if let Ok(loaded) = Schedules::load(&dir) {
            self.schedules = loaded.schedule;
        }
        if let Ok(loaded_runs) = load_coalesced_runs(&dir) {
            self.runs = loaded_runs;
        }
        cx.notify();
    }

    pub fn open_detail(&mut self, id: String, cx: &mut Context<Self>) {
        self.prompt_expanded = false;
        self.screen = PageView::Detail {
            id,
            tab: DetailTab::Overview,
        };
        cx.notify();
    }

    pub fn set_detail_tab(&mut self, tab: DetailTab, cx: &mut Context<Self>) {
        if let PageView::Detail {
            tab: current_tab, ..
        } = &mut self.screen
        {
            *current_tab = tab;
            cx.notify();
        }
    }

    pub fn open_runs_dashboard(&mut self, cx: &mut Context<Self>) {
        self.screen = PageView::RunsDashboard;
        cx.notify();
    }

    pub fn open_run_details(
        &mut self,
        run: ScheduleRun,
        return_to_detail: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.run_log_generation = self.run_log_generation.wrapping_add(1);
        let generation = self.run_log_generation;
        let dir = self.state_dir.clone();
        self.run_log_output = run.output.clone().unwrap_or_default();
        let run_for_bg = run.clone();
        self.screen = PageView::RunDetails {
            run: Box::new(run),
            return_to_detail,
        };
        cx.spawn(async move |this, cx| {
            let output = cx
                .background_executor()
                .spawn(async move {
                    if let Some(out) = run_for_bg
                        .output
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                    {
                        out.to_owned()
                    } else if let Some(sid) = run_for_bg.session
                        && let Some(dir) = dir
                        && let Some(captured) =
                            capture_session_output(&dir, krill_proto::SessionId(sid))
                    {
                        captured
                    } else if let Some(err) =
                        run_for_bg.error.as_deref().filter(|s| !s.trim().is_empty())
                    {
                        err.to_owned()
                    } else if let Some(st) = run_for_bg.status {
                        format!("exit status {st}")
                    } else {
                        stext::SCHEDULE_LOG_EMPTY.get().to_owned()
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.run_log_generation == generation {
                    this.run_log_output = output;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn handle_escape(&mut self, cx: &mut Context<Self>) {
        if self.template_menu_open {
            self.template_menu_open = false;
            cx.notify();
            return;
        }
        if self.filter_menu_open {
            self.filter_menu_open = false;
            cx.notify();
            return;
        }
        if self.modal.is_some() {
            self.close_modal(cx);
            return;
        }
        match &self.screen {
            PageView::RunDetails {
                return_to_detail, ..
            } => {
                if let Some(id) = return_to_detail.clone() {
                    self.screen = PageView::Detail {
                        id,
                        tab: DetailTab::Runs,
                    };
                } else {
                    self.screen = PageView::RunsDashboard;
                }
                cx.notify();
            }
            PageView::Detail { .. } | PageView::RunsDashboard => {
                self.screen = PageView::List;
                cx.notify();
            }
            PageView::List => {
                cx.emit(Close);
            }
        }
    }

    pub fn open_editor_modal(
        &mut self,
        existing: Option<&Schedule>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.form_generation = self.form_generation.wrapping_add(1);
        self.template_menu_open = false;
        let editing_id = existing.map(|s| s.id.clone());
        let init_id = existing.map_or("", |s| s.id.as_str()).to_owned();
        let init_cron = existing.map_or("0 9 * * *", |s| s.cron.as_str()).to_owned();
        let init_cwd = existing
            .map_or(self.default_cwd.as_str(), |s| s.cwd.as_str())
            .to_owned();
        let init_cmd = existing.map_or_else(
            || "claude -p".to_owned(),
            |s| format_command_args(&s.command),
        );
        let init_prompt = existing.map_or("", |s| s.prompt.as_str()).to_owned();
        let init_precheck = existing
            .and_then(|s| s.precheck.as_deref())
            .unwrap_or("")
            .to_owned();
        let cadence = ScheduleCadence::from_cron(&init_cron);
        let (agent, _) = existing.map_or((AgentFilter::Claude, "Claude"), |s| {
            infer_agent_from_command(&s.command)
        });
        let workspace_mode = existing.map_or(WorkspaceMode::Isolate, |s| s.workspace_mode);
        let enabled = existing.is_none_or(|s| s.enabled);

        let make_input = |placeholder: &'static str,
                          value: String,
                          window: &mut Window,
                          cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                if !value.is_empty() {
                    state.set_value(value, window, cx);
                }
                state
            })
        };

        self.modal = Some(EditorModal {
            editing_id,
            id: make_input(text::NAME_PLACEHOLDER.get(), init_id, window, cx),
            cron: make_input(
                stext::SCHEDULE_FIELD_CRON_PLACEHOLDER.get(),
                init_cron,
                window,
                cx,
            ),
            cwd: make_input(
                stext::SCHEDULE_FIELD_CWD_PLACEHOLDER.get(),
                init_cwd,
                window,
                cx,
            ),
            command: make_input(
                stext::SCHEDULE_FIELD_COMMAND_PLACEHOLDER.get(),
                init_cmd,
                window,
                cx,
            ),
            prompt: make_input(
                stext::SCHEDULE_FIELD_PROMPT_PLACEHOLDER.get(),
                init_prompt,
                window,
                cx,
            ),
            precheck: make_input(
                stext::SCHEDULE_FIELD_PRECHECK_PLACEHOLDER.get(),
                init_precheck,
                window,
                cx,
            ),
            cadence,
            agent,
            workspace_mode,
            enabled,
            error: None,
        });
        cx.notify();
    }

    pub fn open_template_modal(
        &mut self,
        template: AutomationTemplate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_editor_modal(None, window, cx);
        self.apply_template(template, window, cx);
    }

    pub fn apply_template(
        &mut self,
        template: AutomationTemplate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = &mut self.modal else {
            return;
        };
        modal
            .id
            .update(cx, |s, cx| s.set_value(template.id.to_owned(), window, cx));
        modal.cron.update(cx, |s, cx| {
            s.set_value(template.cron.to_owned(), window, cx);
        });
        modal.prompt.update(cx, |s, cx| {
            s.set_value(template.prompt.to_owned(), window, cx);
        });
        modal.cadence = ScheduleCadence::from_cron(template.cron);
        modal.error = None;
        self.template_menu_open = false;
        cx.notify();
    }

    pub fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.form_generation = self.form_generation.wrapping_add(1);
        self.modal = None;
        self.template_menu_open = false;
        cx.notify();
    }

    pub fn submit_modal(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(modal) = &mut self.modal else {
            return;
        };
        let id = modal.id.read(cx).value().trim().to_owned();
        let cron = modal.cron.read(cx).value().trim().to_owned();
        let cwd = modal.cwd.read(cx).value().trim().to_owned();
        let cmd_raw = modal.command.read(cx).value().trim().to_owned();
        let prompt = modal.prompt.read(cx).value().trim().to_owned();
        let precheck_raw = modal.precheck.read(cx).value().trim().to_owned();
        let precheck = (!precheck_raw.is_empty()).then_some(precheck_raw);
        let workspace_mode = modal.workspace_mode;
        let enabled = modal.enabled;
        let editing_id = modal.editing_id.clone();

        let fail = |modal: &mut EditorModal, msg: String, cx: &mut Context<Self>| {
            modal.error = Some(msg.into());
            cx.notify();
        };

        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            fail(modal, stext::SCHEDULE_ERR_ID.get().to_owned(), cx);
            return;
        }
        if cwd.is_empty() || !Path::new(&cwd).is_absolute() || cwd.contains('\0') {
            fail(modal, stext::SCHEDULE_ERR_CWD.get().to_owned(), cx);
            return;
        }
        if Cron::parse(&cron).is_err() {
            fail(modal, stext::SCHEDULE_ERR_CRON.get().to_owned(), cx);
            return;
        }
        let command = match parse_command_args(&cmd_raw) {
            Ok(cmd) => cmd,
            Err(err) => {
                fail(modal, err, cx);
                return;
            }
        };
        if prompt.contains('\0') || prompt.len() > 32 * 1024 {
            fail(modal, stext::SCHEDULE_ERR_PROMPT.get().to_owned(), cx);
            return;
        }
        if precheck
            .as_ref()
            .is_some_and(|s| s.contains('\0') || s.len() > 8192)
        {
            fail(modal, stext::SCHEDULE_ERR_PRECHECK.get().to_owned(), cx);
            return;
        }

        let entry = Schedule {
            id: id.clone(),
            cron,
            cwd,
            command,
            prompt,
            precheck,
            workspace_mode,
            enabled,
            ..Schedule::default()
        };
        let Some(dir) = self.state_dir.clone() else {
            fail(modal, stext::NO_STATE_DIR.get().to_owned(), cx);
            return;
        };
        let generation = self.form_generation;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let _guard = Schedules::exclusive_edit(&dir)?;
                    let mut schedules = Schedules::load(&dir)?;
                    apply_schedule_edit(&mut schedules, editing_id.as_deref(), entry)?;
                    schedules.save(&dir)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.form_generation != generation {
                    this.reload(cx);
                    return;
                }
                match result {
                    Ok(()) => {
                        this.modal = None;
                        this.error = None;
                        if let PageView::Detail { id: detail_id, tab } = &this.screen
                            && detail_id != &id
                        {
                            this.screen = PageView::Detail { id, tab: *tab };
                        }
                    }
                    Err(err) => {
                        let msg: SharedString = err.to_string().into();
                        this.error = Some(msg.clone());
                        if let Some(m) = &mut this.modal {
                            m.error = Some(msg);
                        }
                    }
                }
                this.reload(cx);
            })
            .ok();
        }));
    }

    pub fn toggle_enabled(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(dir) = self.state_dir.clone() else {
            return;
        };
        let id = id.to_owned();
        self.task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let _guard = Schedules::exclusive_edit(&dir)?;
                    let mut schedules = Schedules::load(&dir)?;
                    if let Some(s) = schedules.schedule.iter_mut().find(|s| s.id == id) {
                        s.enabled = !s.enabled;
                        schedules.save(&dir)?;
                    }
                    Ok::<(), std::io::Error>(())
                })
                .await;
            this.update(cx, |this, cx| {
                this.error = res.err().map(|e| e.to_string().into());
                this.reload(cx);
            })
            .ok();
        }));
    }

    pub fn delete_schedule(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(dir) = self.state_dir.clone() else {
            return;
        };
        let id = id.to_owned();
        let id_check = id.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let _guard = Schedules::exclusive_edit(&dir)?;
                    let mut schedules = Schedules::load(&dir)?;
                    schedules.schedule.retain(|s| s.id != id);
                    schedules.save(&dir)
                })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(()) => {
                        if let PageView::Detail { id: cur, .. } = &this.screen
                            && cur == &id_check
                        {
                            this.screen = PageView::List;
                        }
                        this.error = None;
                    }
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                    }
                }
                this.reload(cx);
            })
            .ok();
        }));
    }

    pub fn run_now(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(dir) = self.state_dir.clone() else {
            return;
        };
        let id = id.to_owned();
        self.task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let socket = dir.join("run/krilld.sock");
                    if !socket.exists()
                        || krill_proto::os::net::UnixStream::connect(&socket).is_err()
                    {
                        return Err(std::io::Error::other(stext::ACCOUNTS_DAEMON_REQUIRED.get()));
                    }
                    request_run_now(&dir, &id)
                })
                .await;
            this.update(cx, |this, cx| {
                this.error = res.err().map(|e| e.to_string().into());
                this.reload(cx);
            })
            .ok();
        }));
    }

    fn latest_run_for(&self, id: &str) -> Option<&ScheduleRun> {
        self.runs.iter().find(|r| r.id == id)
    }

    fn has_active_filters(&self) -> bool {
        self.status_filter != StatusFilter::All
            || self.last_run_filter != LastRunFilter::All
            || self.agent_filter != AgentFilter::All
    }

    pub fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.status_filter = StatusFilter::All;
        self.last_run_filter = LastRunFilter::All;
        self.agent_filter = AgentFilter::All;
        cx.notify();
    }

    fn matches_schedule(&self, sched: &Schedule) -> bool {
        match self.status_filter {
            StatusFilter::Enabled if !sched.enabled => return false,
            StatusFilter::Paused if sched.enabled => return false,
            StatusFilter::All | StatusFilter::Enabled | StatusFilter::Paused => {}
        }
        let last_run = self.latest_run_for(&sched.id);
        match self.last_run_filter {
            LastRunFilter::All => {}
            LastRunFilter::Failed => {
                if last_run.is_none_or(|r| r.phase != "failed") {
                    return false;
                }
            }
            LastRunFilter::Succeeded => {
                if last_run.is_none_or(|r| r.phase != "finished") {
                    return false;
                }
            }
            LastRunFilter::Never => {
                if last_run.is_some() {
                    return false;
                }
            }
        }
        if self.agent_filter != AgentFilter::All {
            let (agent, _) = infer_agent_from_command(&sched.command);
            if agent != self.agent_filter {
                return false;
            }
        }
        if !self.query.is_empty() {
            let haystack = format!(
                "{} {} {} {}",
                sched.id,
                sched.cwd,
                sched.prompt,
                format_command_args(&sched.command)
            )
            .to_lowercase();
            if !self
                .query
                .split_whitespace()
                .all(|term| haystack.contains(term))
            {
                return false;
            }
        }
        true
    }

    fn render_top_bar(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let mut breadcrumbs = div().flex().items_center().gap_1p5();

        let is_root = matches!(self.screen, PageView::List);
        breadcrumbs = breadcrumbs.child(
            div()
                .id("automations-breadcrumb-root")
                .debug_selector(|| "automations-breadcrumb-root".into())
                .text_size(ui::TEXT_LG)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if is_root {
                    p.foreground
                } else {
                    p.muted_foreground
                })
                .when(!is_root, |d| {
                    let fg = p.foreground;
                    d.cursor_pointer()
                        .hover(move |s| s.text_color(fg))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.screen = PageView::List;
                            cx.notify();
                        }))
                })
                .child(text::TITLE.get()),
        );

        match &self.screen {
            PageView::List => {}
            PageView::Detail { id, .. } => {
                breadcrumbs = breadcrumbs
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child("/"),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(id.clone()),
                    );
            }
            PageView::RunsDashboard => {
                breadcrumbs = breadcrumbs
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child("/"),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(text::RUNS_BTN.get()),
                    );
            }
            PageView::RunDetails {
                return_to_detail, ..
            } => {
                let parent_label = return_to_detail
                    .clone()
                    .unwrap_or_else(|| text::RUNS_BTN.get().to_owned());
                let ret = return_to_detail.clone();
                let fg = p.foreground;
                breadcrumbs = breadcrumbs
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child("/"),
                    )
                    .child(
                        div()
                            .id("automations-breadcrumb-parent")
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.muted_foreground)
                            .cursor_pointer()
                            .hover(move |s| s.text_color(fg))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(id) = ret.clone() {
                                    this.screen = PageView::Detail {
                                        id,
                                        tab: DetailTab::Runs,
                                    };
                                } else {
                                    this.screen = PageView::RunsDashboard;
                                }
                                cx.notify();
                            }))
                            .child(parent_label),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child("/"),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(text::RUN_DETAILS_BREADCRUMB.get()),
                    );
            }
        }

        div()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px_5()
            .pt_5()
            .pb_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        ui::ghost("automations-back", text::BACK.get())
                            .debug_selector(|| "automations-back".into())
                            .icon(IconName::ArrowLeft)
                            .on_click(cx.listener(|this, _, _, cx| this.handle_escape(cx))),
                    )
                    .child(breadcrumbs),
            )
            .child(
                ui::ghost("automations-open-toml", stext::SCHEDULE_OPEN_TOML.get())
                    .debug_selector(|| "automations-open-toml".into())
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(dir) = &this.state_dir {
                            let path = dir.join(krill_proto::schedule::FILE);
                            cx.open_with_system(&path);
                        }
                    })),
            )
    }

    fn render_list_view(
        &mut self,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let now_min = current_epoch_minute();

        let toolbar = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .children(self.search_input.as_ref().map(|input| {
                        div()
                            .id("automations-search")
                            .debug_selector(|| "automations-search".into())
                            .w(px(224.))
                            .child(Input::new(input).small())
                    }))
                    .child(
                        ui::secondary("automations-filter-btn", text::FILTERS.get())
                            .debug_selector(|| "automations-filter-btn".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.filter_menu_open = !this.filter_menu_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        ui::secondary("automations-refresh", text::REFRESH.get())
                            .debug_selector(|| "automations-refresh".into())
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        ui::secondary("automations-runs-btn", text::RUNS_BTN.get())
                            .debug_selector(|| "automations-runs-btn".into())
                            .on_click(cx.listener(|this, _, _, cx| this.open_runs_dashboard(cx))),
                    )
                    .child(
                        ui::primary("automations-new-btn", text::NEW_AUTOMATION.get())
                            .debug_selector(|| "automations-new-btn".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_editor_modal(None, window, cx);
                            })),
                    ),
            );

        // Filter bar (when filter_menu_open or active filters)
        let filter_bar = (self.filter_menu_open || self.has_active_filters()).then(|| {
            let status_btn = |sf: StatusFilter, label: &'static str, id: &'static str| {
                let active = self.status_filter == sf;
                ui::segment(p, id, label, active).on_click(cx.listener(move |this, _, _, cx| {
                    this.status_filter = sf;
                    cx.notify();
                }))
            };
            let run_btn = |lf: LastRunFilter, label: &'static str, id: &'static str| {
                let active = self.last_run_filter == lf;
                ui::segment(p, id, label, active).on_click(cx.listener(move |this, _, _, cx| {
                    this.last_run_filter = lf;
                    cx.notify();
                }))
            };
            let agent_btn = |af: AgentFilter, id: &'static str| {
                let active = self.agent_filter == af;
                ui::segment(p, id, af.label(), active).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.agent_filter = af;
                        cx.notify();
                    },
                ))
            };

            div()
                .id("automations-filter-bar")
                .debug_selector(|| "automations-filter-bar".into())
                .rounded(ui::RADIUS_MD)
                .border_1()
                .border_color(p.border)
                .bg(p.muted.opacity(0.25))
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .w(px(80.))
                                .text_size(ui::TEXT_XS)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.muted_foreground)
                                .child(text::FILTER_STATUS.get()),
                        )
                        .child(status_btn(
                            StatusFilter::All,
                            text::FILTER_ALL.get(),
                            "automations-filter-status-all",
                        ))
                        .child(status_btn(
                            StatusFilter::Enabled,
                            text::STATUS_ENABLED.get(),
                            "automations-filter-status-enabled",
                        ))
                        .child(status_btn(
                            StatusFilter::Paused,
                            text::STATUS_PAUSED.get(),
                            "automations-filter-status-paused",
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .w(px(80.))
                                .text_size(ui::TEXT_XS)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.muted_foreground)
                                .child(text::FILTER_LAST_RUN.get()),
                        )
                        .child(run_btn(
                            LastRunFilter::All,
                            text::FILTER_ALL.get(),
                            "automations-filter-run-all",
                        ))
                        .child(run_btn(
                            LastRunFilter::Failed,
                            text::LAST_RUN_FAILED.get(),
                            "automations-filter-run-failed",
                        ))
                        .child(run_btn(
                            LastRunFilter::Succeeded,
                            text::LAST_RUN_SUCCEEDED.get(),
                            "automations-filter-run-succeeded",
                        ))
                        .child(run_btn(
                            LastRunFilter::Never,
                            text::LAST_RUN_NEVER.get(),
                            "automations-filter-run-never",
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    div()
                                        .w(px(80.))
                                        .text_size(ui::TEXT_XS)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.muted_foreground)
                                        .child(text::FILTER_AGENT.get()),
                                )
                                .child(agent_btn(AgentFilter::All, "automations-filter-agent-all"))
                                .child(agent_btn(
                                    AgentFilter::Claude,
                                    "automations-filter-agent-claude",
                                ))
                                .child(agent_btn(
                                    AgentFilter::Codex,
                                    "automations-filter-agent-codex",
                                ))
                                .child(agent_btn(
                                    AgentFilter::Antigravity,
                                    "automations-filter-agent-agy",
                                ))
                                .child(agent_btn(
                                    AgentFilter::OpenCode,
                                    "automations-filter-agent-opencode",
                                ))
                                .child(agent_btn(
                                    AgentFilter::Cursor,
                                    "automations-filter-agent-cursor",
                                ))
                                .child(agent_btn(AgentFilter::Pi, "automations-filter-agent-pi")),
                        )
                        .when(self.has_active_filters(), |d| {
                            d.child(
                                ui::ghost("automations-clear-filters", text::CLEAR_FILTERS.get())
                                    .debug_selector(|| "automations-clear-filters".into())
                                    .on_click(cx.listener(|this, _, _, cx| this.clear_filters(cx))),
                            )
                        }),
                )
        });

        if self.schedules.is_empty() && self.query.is_empty() && !self.has_active_filters() {
            // Empty state + Orca starter templates
            let mut tpl_grid = div().flex().flex_col().gap_2();
            for tpl in TEMPLATES {
                let tpl_id = format!("automation-template-{}", tpl.id);
                let tpl_sel = tpl_id.clone();
                tpl_grid = tpl_grid.child(
                    div()
                        .id(tpl_id)
                        .debug_selector(move || tpl_sel.clone())
                        .rounded(ui::RADIUS_MD)
                        .border_1()
                        .border_color(p.border)
                        .bg(p.muted.opacity(0.2))
                        .px_4()
                        .py_3()
                        .cursor_pointer()
                        .hover(|s| s.bg(p.muted.opacity(0.4)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_template_modal(tpl, window, cx);
                        }))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_size(ui::TEXT_SM)
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(p.foreground)
                                                .child(tpl.name()),
                                        )
                                        .child(ui::badge(p, ui::BadgeTone::Muted, tpl.category()))
                                        .child(
                                            div()
                                                .font_family(ui::FONT_MONO)
                                                .text_size(ui::TEXT_XS)
                                                .text_color(p.muted_foreground)
                                                .child(format_cron_summary(tpl.cron)),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(tpl.summary()),
                                ),
                        )
                        .child(ui::icons::svg_box(
                            ui::icons::ARROW_RIGHT_SVG,
                            ui::icons::ICON_SM,
                            p.muted_foreground,
                        )),
                );
            }

            return div()
                .id("automations-empty-scroll")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_4()
                .px_5()
                .pb_5()
                .overflow_y_scroll()
                .child(toolbar)
                .children(filter_bar)
                .child(
                    div()
                        .id("automations-empty")
                        .debug_selector(|| "automations-empty".into())
                        .rounded(ui::RADIUS_MD)
                        .border_1()
                        .border_color(p.border)
                        .bg(p.muted.opacity(0.15))
                        .p_6()
                        .flex()
                        .flex_col()
                        .items_center()
                        .text_center()
                        .gap_2()
                        .child(ui::icons::svg_box(
                            ui::icons::CALENDAR_CLOCK_SVG,
                            28.0,
                            p.muted_foreground,
                        ))
                        .child(
                            div()
                                .text_size(ui::TEXT_MD)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.foreground)
                                .child(text::EMPTY_TITLE.get()),
                        )
                        .child(
                            div()
                                .max_w(px(460.))
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::EMPTY_DESC.get()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_size(ui::TEXT_XS)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.muted_foreground)
                                .child(ui::icons::svg_box(
                                    ui::icons::SPARKLES_SVG,
                                    ui::icons::ICON_XS,
                                    p.muted_foreground,
                                ))
                                .child(text::TEMPLATES_HEADING.get()),
                        )
                        .child(tpl_grid),
                )
                .into_any_element();
        }

        // Filter & Sort rows
        let mut filtered: Vec<Schedule> = self
            .schedules
            .iter()
            .filter(|s| self.matches_schedule(s))
            .cloned()
            .collect();
        filtered.sort_by(|a, b| {
            let ord = match self.sort_field {
                SortField::Name => a.id.cmp(&b.id),
                SortField::LastRun => {
                    let ma = self.latest_run_for(&a.id).map_or(0, |r| r.minute);
                    let mb = self.latest_run_for(&b.id).map_or(0, |r| r.minute);
                    ma.cmp(&mb)
                }
            };
            match self.sort_dir {
                SortDirection::Asc => ord,
                SortDirection::Desc => ord.reverse(),
            }
        });

        let header = div()
            .h(px(32.))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .border_b_1()
            .border_color(p.border)
            .bg(p.muted.opacity(0.4))
            .text_size(ui::TEXT_XS)
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.muted_foreground)
            .child(
                div()
                    .id("automations-sort-name")
                    .flex_1()
                    .min_w(px(140.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.sort_field == SortField::Name {
                            this.sort_dir = match this.sort_dir {
                                SortDirection::Asc => SortDirection::Desc,
                                SortDirection::Desc => SortDirection::Asc,
                            };
                        } else {
                            this.sort_field = SortField::Name;
                            this.sort_dir = SortDirection::Asc;
                        }
                        cx.notify();
                    }))
                    .child(text::COL_NAME.get()),
            )
            .child(
                div()
                    .w(px(136.))
                    .flex_none()
                    .child(text::COL_SCHEDULE.get()),
            )
            .child(div().w(px(108.)).flex_none().child(text::COL_PROJECT.get()))
            .child(
                div()
                    .w(px(116.))
                    .flex_none()
                    .child(text::COL_NEXT_RUN.get()),
            )
            .child(
                div()
                    .id("automations-sort-last-run")
                    .w(px(124.))
                    .flex_none()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.sort_field == SortField::LastRun {
                            this.sort_dir = match this.sort_dir {
                                SortDirection::Asc => SortDirection::Desc,
                                SortDirection::Desc => SortDirection::Asc,
                            };
                        } else {
                            this.sort_field = SortField::LastRun;
                            this.sort_dir = SortDirection::Desc;
                        }
                        cx.notify();
                    }))
                    .child(text::COL_LAST_RUN.get()),
            )
            .child(div().w(px(84.)).flex_none().child(text::COL_STATUS.get()))
            .child(div().w(px(88.)).flex_none().child(text::COL_AGENT.get()))
            .child(
                div()
                    .w(px(196.))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .child(text::COL_ACTIONS.get()),
            );

        let mut list_body = div()
            .id("automations-list")
            .debug_selector(|| "automations-list".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();

        if filtered.is_empty() {
            list_body = list_body.child(
                div()
                    .id("automations-no-matches")
                    .px_3()
                    .py_6()
                    .text_center()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::NO_MATCHES.get()),
            );
        } else {
            for sched in filtered {
                let id = sched.id.clone();
                let id_click = id.clone();
                let id_run = id.clone();
                let id_toggle = id.clone();
                let id_delete = id.clone();
                let sched_for_edit = sched.clone();
                let row_id = format!("automation-row-{id}");
                let row_sel = row_id.clone();
                let (agent_kind, agent_name) = infer_agent_from_command(&sched.command);
                let next_run_str = if !sched.enabled {
                    text::NEXT_RUN_PAUSED.get().to_owned()
                } else if let Some(next_m) = next_run_minute(&sched.cron, now_min) {
                    format_run_minute(next_m)
                } else {
                    sched.cron.clone()
                };
                let last_run = self.latest_run_for(&id);
                let (last_run_text, last_run_color) = if let Some(r) = last_run {
                    let (_, phase_label) = run_phase_badge_spec(&r.phase);
                    let color = if r.phase == "failed" {
                        p.destructive
                    } else if r.phase == "finished" {
                        p.success
                    } else {
                        p.muted_foreground
                    };
                    (
                        format!("{phase_label} · {}", format_run_minute(r.minute)),
                        color,
                    )
                } else {
                    (text::LAST_RUN_NONE.get().to_owned(), p.muted_foreground)
                };

                list_body = list_body.child(
                    div()
                        .id(row_id)
                        .debug_selector(move || row_sel.clone())
                        .w_full()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_2p5()
                        .border_b_1()
                        .border_color(p.border)
                        .cursor_pointer()
                        .hover(|s| s.bg(p.muted.opacity(0.25)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_detail(id_click.clone(), cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(140.))
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_size(ui::TEXT_SM)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.foreground)
                                        .truncate()
                                        .child(sched.id.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .truncate()
                                        .child(if sched.prompt.trim().is_empty() {
                                            format_command_args(&sched.command)
                                        } else {
                                            sched.prompt.clone()
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .w(px(136.))
                                .flex_none()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.foreground)
                                        .truncate()
                                        .child(format_cron_summary(&sched.cron)),
                                )
                                .child(
                                    div()
                                        .font_family(ui::FONT_MONO)
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .truncate()
                                        .child(sched.cron.clone()),
                                ),
                        )
                        .child(
                            div()
                                .w(px(108.))
                                .flex_none()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .truncate()
                                .child(project_label(&sched.cwd)),
                        )
                        .child(
                            div()
                                .w(px(116.))
                                .flex_none()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .truncate()
                                .child(next_run_str),
                        )
                        .child(
                            div()
                                .w(px(124.))
                                .flex_none()
                                .text_size(ui::TEXT_XS)
                                .text_color(last_run_color)
                                .truncate()
                                .child(last_run_text),
                        )
                        .child(
                            div()
                                .w(px(84.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_size(ui::TEXT_XS)
                                .child(div().size(ui::STATE_DOT).rounded_full().bg(
                                    if sched.enabled {
                                        p.success
                                    } else {
                                        p.muted_foreground
                                    },
                                ))
                                .child(
                                    div()
                                        .text_color(if sched.enabled {
                                            p.foreground
                                        } else {
                                            p.muted_foreground
                                        })
                                        .child(if sched.enabled {
                                            text::STATUS_ENABLED.get()
                                        } else {
                                            text::STATUS_PAUSED.get()
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .w(px(88.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.foreground)
                                .child(ui::icons::agent_brand_icon(
                                    agent_kind.brand_id(),
                                    ui::icons::ICON_SM,
                                    p.foreground,
                                ))
                                .child(div().truncate().child(agent_name)),
                        )
                        .child(
                            div()
                                .w(px(196.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_1()
                                .child(
                                    ui::secondary(
                                        format!("automation-run-{id}"),
                                        text::ACT_RUN_NOW.get(),
                                    )
                                    .debug_selector({
                                        let id = id.clone();
                                        move || format!("automation-run-{id}")
                                    })
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.run_now(&id_run, cx);
                                        },
                                    )),
                                )
                                .child(
                                    ui::ghost(format!("automation-edit-{id}"), "")
                                        .debug_selector({
                                            let id = id.clone();
                                            move || format!("automation-edit-{id}")
                                        })
                                        .icon(IconName::Settings)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.open_editor_modal(
                                                Some(&sched_for_edit),
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                                .child(
                                    ui::ghost(
                                        format!("automation-toggle-{id}"),
                                        if sched.enabled {
                                            text::ACT_PAUSE.get()
                                        } else {
                                            text::ACT_RESUME.get()
                                        },
                                    )
                                    .debug_selector({
                                        let id = id.clone();
                                        move || format!("automation-toggle-{id}")
                                    })
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.toggle_enabled(&id_toggle, cx);
                                        },
                                    )),
                                )
                                .child(
                                    ui::ghost(format!("automation-delete-{id}"), "")
                                        .debug_selector({
                                            let id = id.clone();
                                            move || format!("automation-delete-{id}")
                                        })
                                        .icon(IconName::Delete)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.delete_schedule(&id_delete, cx);
                                        })),
                                ),
                        ),
                );
            }
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .px_5()
            .pb_5()
            .child(toolbar)
            .children(filter_bar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.muted.opacity(0.2))
                    .child(header)
                    .child(list_body),
            )
            .into_any_element()
    }

    fn render_detail_view(
        &self,
        id: &str,
        tab: DetailTab,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(sched) = self.schedules.iter().find(|s| s.id == id).cloned() else {
            return div()
                .flex_1()
                .p_6()
                .text_size(ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(text::NO_MATCHES.get())
                .into_any_element();
        };

        let matching_runs: Vec<ScheduleRun> =
            self.runs.iter().filter(|r| r.id == id).cloned().collect();
        let completed_count = matching_runs
            .iter()
            .filter(|r| r.phase == "finished")
            .count();

        let tab_bar = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_4()
            .px_5()
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .id("automations-tab-overview")
                    .debug_selector(|| "automations-tab-overview".into())
                    .py_2()
                    .border_b_2()
                    .border_color(if tab == DetailTab::Overview {
                        p.primary
                    } else {
                        ui::TRANSPARENT
                    })
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if tab == DetailTab::Overview {
                        p.foreground
                    } else {
                        p.muted_foreground
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_detail_tab(DetailTab::Overview, cx);
                    }))
                    .child(text::TAB_OVERVIEW.get()),
            )
            .child(
                div()
                    .id("automations-tab-runs")
                    .debug_selector(|| "automations-tab-runs".into())
                    .py_2()
                    .border_b_2()
                    .border_color(if tab == DetailTab::Runs {
                        p.primary
                    } else {
                        ui::TRANSPARENT
                    })
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if tab == DetailTab::Runs {
                        p.foreground
                    } else {
                        p.muted_foreground
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_detail_tab(DetailTab::Runs, cx);
                    }))
                    .child(format!(
                        "{} ({})",
                        text::TAB_RUNS.get(),
                        matching_runs.len()
                    )),
            );

        let body = match tab {
            DetailTab::Overview => {
                let now_min = current_epoch_minute();
                let next_run_str = if !sched.enabled {
                    text::NEXT_RUN_PAUSED.get().to_owned()
                } else if let Some(next_m) = next_run_minute(&sched.cron, now_min) {
                    format_run_minute(next_m)
                } else {
                    sched.cron.clone()
                };
                let (agent_kind, agent_name) = infer_agent_from_command(&sched.command);
                let last_run_str = matching_runs.first().map_or_else(
                    || text::LAST_RUN_NONE.get().to_owned(),
                    |r| {
                        let (_, lbl) = run_phase_badge_spec(&r.phase);
                        format!("{lbl} · {}", format_run_minute(r.minute))
                    },
                );
                let id_run = sched.id.clone();
                let id_toggle = sched.id.clone();
                let id_delete = sched.id.clone();
                let sched_edit = sched.clone();

                let metric_item = |label: &'static str, value: String| {
                    div()
                        .min_w(px(180.))
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(ui::TEXT_XS)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.muted_foreground)
                                .child(label),
                        )
                        .child(
                            div()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.foreground)
                                .child(value),
                        )
                };

                let prompt_text = if sched.prompt.trim().is_empty() {
                    format_command_args(&sched.command)
                } else {
                    sched.prompt.clone()
                };
                let prompt_lines: Vec<&str> = prompt_text.lines().collect();
                let collapsible = prompt_lines.len() > 8 || prompt_text.len() > 360;
                let displayed_prompt = if collapsible && !self.prompt_expanded {
                    let preview: String = prompt_lines
                        .iter()
                        .take(8)
                        .copied()
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("{preview}\n…")
                } else {
                    prompt_text
                };

                div()
                    .id("automations-detail-overview-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .py_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        // Detail Header Row
                        div()
                            .flex()
                            .flex_wrap()
                            .items_start()
                            .justify_between()
                            .gap_4()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_size(ui::TEXT_LG)
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(p.foreground)
                                                    .child(sched.id.clone()),
                                            )
                                            .child(ui::badge(
                                                p,
                                                if sched.enabled {
                                                    ui::BadgeTone::Success
                                                } else {
                                                    ui::BadgeTone::Muted
                                                },
                                                if sched.enabled {
                                                    text::STATUS_ENABLED.get()
                                                } else {
                                                    text::STATUS_PAUSED.get()
                                                },
                                            )),
                                    )
                                    .child(
                                        div()
                                            .font_family(ui::FONT_MONO)
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(format!(
                                                "{} · {}",
                                                project_label(&sched.cwd),
                                                sched.cwd
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(
                                        ui::secondary(
                                            "automations-detail-run-now",
                                            text::ACT_RUN_NOW.get(),
                                        )
                                        .debug_selector(|| "automations-detail-run-now".into())
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.run_now(&id_run, cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        ui::secondary(
                                            "automations-detail-edit",
                                            text::ACT_EDIT.get(),
                                        )
                                        .debug_selector(|| "automations-detail-edit".into())
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.open_editor_modal(
                                                    Some(&sched_edit),
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    )
                                    .child(
                                        ui::secondary(
                                            "automations-detail-toggle",
                                            if sched.enabled {
                                                text::ACT_PAUSE.get()
                                            } else {
                                                text::ACT_RESUME.get()
                                            },
                                        )
                                        .debug_selector(|| "automations-detail-toggle".into())
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.toggle_enabled(&id_toggle, cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        Button::new("automations-detail-delete")
                                            .small()
                                            .danger()
                                            .label(text::ACT_DELETE.get())
                                            .debug_selector(|| "automations-detail-delete".into())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.delete_schedule(&id_delete, cx);
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        // Metrics Grid Card
                        div()
                            .rounded(ui::RADIUS_MD)
                            .border_1()
                            .border_color(p.border)
                            .bg(p.muted.opacity(0.25))
                            .px_4()
                            .py_3()
                            .flex()
                            .flex_wrap()
                            .gap_4()
                            .child(metric_item(
                                text::META_SCHEDULE.get(),
                                format!("{} ({})", format_cron_summary(&sched.cron), sched.cron),
                            ))
                            .child(metric_item(text::META_NEXT_RUN.get(), next_run_str))
                            .child(metric_item(
                                text::META_RUN_LOCATION.get(),
                                match sched.workspace_mode {
                                    WorkspaceMode::Isolate => {
                                        text::LOCATION_ISOLATE.get().to_owned()
                                    }
                                    WorkspaceMode::Reuse => text::LOCATION_REUSE.get().to_owned(),
                                },
                            ))
                            .child(metric_item(
                                text::META_PRECHECK.get(),
                                sched
                                    .precheck
                                    .clone()
                                    .unwrap_or_else(|| text::PRECHECK_NONE.get().to_owned()),
                            ))
                            .child(
                                div()
                                    .min_w(px(180.))
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.muted_foreground)
                                            .child(text::META_AGENT.get()),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .text_size(ui::TEXT_SM)
                                            .text_color(p.foreground)
                                            .child(ui::icons::agent_brand_icon(
                                                agent_kind.brand_id(),
                                                ui::icons::ICON_SM,
                                                p.foreground,
                                            ))
                                            .child(agent_name),
                                    ),
                            )
                            .child(metric_item(
                                text::META_COMMAND.get(),
                                format_command_args(&sched.command),
                            ))
                            .child(metric_item(text::META_LAST_RUN.get(), last_run_str)),
                    )
                    .child(
                        // AutomationPromptDisclosure Card
                        div()
                            .rounded(ui::RADIUS_MD)
                            .border_1()
                            .border_color(p.border)
                            .bg(p.muted.opacity(0.25))
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .px_4()
                                    .py_2p5()
                                    .border_b_1()
                                    .border_color(p.border)
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.muted_foreground)
                                            .child(text::PROMPT_HEADING.get()),
                                    )
                                    .when(collapsible, |d| {
                                        d.child(
                                            ui::ghost(
                                                "automations-prompt-toggle",
                                                if self.prompt_expanded {
                                                    text::SHOW_LESS.get()
                                                } else {
                                                    text::SHOW_MORE.get()
                                                },
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.prompt_expanded = !this.prompt_expanded;
                                                    cx.notify();
                                                }),
                                            ),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .px_4()
                                    .py_3()
                                    .font_family(ui::FONT_MONO)
                                    .text_size(ui::TEXT_SM)
                                    .text_color(p.foreground)
                                    .child(displayed_prompt),
                            ),
                    )
                    .into_any_element()
            }
            DetailTab::Runs => {
                let mut rows = div()
                    .id("automations-detail-runs-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col();
                if matching_runs.is_empty() {
                    rows = rows.child(
                        div()
                            .px_4()
                            .py_6()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(stext::SCHEDULE_RUNS_EMPTY.get()),
                    );
                } else {
                    for (idx, run) in matching_runs.iter().enumerate() {
                        let (tone, badge_label) = run_phase_badge_spec(&run.phase);
                        let run_clone = run.clone();
                        let ret_id = sched.id.clone();
                        rows = rows.child(
                            div()
                                .id(SharedString::from(format!("automation-detail-run-{idx}")))
                                .px_4()
                                .py_3()
                                .border_b_1()
                                .border_color(p.border)
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_4()
                                .cursor_pointer()
                                .hover(|s| s.bg(p.muted.opacity(0.25)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_run_details(
                                        run_clone.clone(),
                                        Some(ret_id.clone()),
                                        cx,
                                    );
                                }))
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
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(p.foreground)
                                                .child(format_run_minute(run.minute)),
                                        )
                                        .child(
                                            div()
                                                .font_family(ui::FONT_MONO)
                                                .text_size(ui::TEXT_XS)
                                                .text_color(p.muted_foreground)
                                                .truncate()
                                                .child(
                                                    run.cwd
                                                        .clone()
                                                        .unwrap_or_else(|| sched.cwd.clone()),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(ui::badge(p, tone, badge_label))
                                        .child(ui::icons::svg_box(
                                            ui::icons::ARROW_RIGHT_SVG,
                                            ui::icons::ICON_SM,
                                            p.muted_foreground,
                                        )),
                                ),
                        );
                    }
                }

                div()
                    .flex_1()
                    .min_h_0()
                    .px_5()
                    .py_4()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .rounded(ui::RADIUS_MD)
                            .border_1()
                            .border_color(p.border)
                            .bg(p.muted.opacity(0.2))
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .px_4()
                                    .py_2p5()
                                    .border_b_1()
                                    .border_color(p.border)
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.muted_foreground)
                                            .child(text::RUN_HISTORY_HEADING.get()),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(text::RUN_HISTORY_SUMMARY.format(&[
                                                ("total", &matching_runs.len().to_string()),
                                                ("completed", &completed_count.to_string()),
                                            ])),
                                    ),
                            )
                            .child(rows),
                    )
                    .into_any_element()
            }
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(tab_bar)
            .child(body)
            .into_any_element()
    }

    fn render_runs_dashboard(
        &self,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let now_min = current_epoch_minute();
        let min_24h = now_min.saturating_sub(24 * 60);
        let min_7d = now_min.saturating_sub(7 * 24 * 60);

        let mut success_24h = 0usize;
        let mut failed_24h = 0usize;
        let mut success_7d = 0usize;
        let mut failed_7d = 0usize;
        for run in &self.runs {
            if run.minute >= min_7d {
                if run.phase == "finished" {
                    success_7d += 1;
                    if run.minute >= min_24h {
                        success_24h += 1;
                    }
                } else if run.phase == "failed" {
                    failed_7d += 1;
                    if run.minute >= min_24h {
                        failed_24h += 1;
                    }
                }
            }
        }

        let summary_card = |label: &'static str, value: usize, value_color: gpui::Hsla| {
            div()
                .flex_1()
                .min_w(px(140.))
                .rounded(ui::RADIUS_MD)
                .border_1()
                .border_color(p.border)
                .bg(p.muted.opacity(0.2))
                .px_3()
                .py_2p5()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .text_size(ui::TEXT_LG)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(value_color)
                        .child(value.to_string()),
                )
        };

        let status_chip = |filter: RunStatusFilter, label: &'static str, id: &'static str| {
            let active = self.run_status_filter == filter;
            ui::segment(p, id, label, active).on_click(cx.listener(move |this, _, _, cx| {
                this.run_status_filter = filter;
                cx.notify();
            }))
        };

        let filtered_runs: Vec<ScheduleRun> = self
            .runs
            .iter()
            .filter(|r| {
                let status_ok = match self.run_status_filter {
                    RunStatusFilter::All => true,
                    RunStatusFilter::Successful => r.phase == "finished",
                    RunStatusFilter::Failed => r.phase == "failed",
                    RunStatusFilter::InProgress => {
                        matches!(r.phase.as_str(), "started" | "spawned")
                    }
                    RunStatusFilter::Skipped => r.phase == "skipped",
                };
                if !status_ok {
                    return false;
                }
                if !self.runs_query.is_empty() {
                    let hay = format!(
                        "{} {} {}",
                        r.id,
                        r.cwd.as_deref().unwrap_or_default(),
                        r.phase
                    )
                    .to_lowercase();
                    return self.runs_query.split_whitespace().all(|t| hay.contains(t));
                }
                true
            })
            .cloned()
            .collect();

        let mut rows = div()
            .id("automations-runs-table")
            .debug_selector(|| "automations-runs-table".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();

        if filtered_runs.is_empty() {
            rows = rows.child(
                div()
                    .px_4()
                    .py_6()
                    .text_center()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(stext::SCHEDULE_RUNS_EMPTY.get()),
            );
        } else {
            for (idx, run) in filtered_runs.into_iter().enumerate() {
                let (tone, badge_label) = run_phase_badge_spec(&run.phase);
                let run_click = run.clone();
                rows = rows.child(
                    div()
                        .id(SharedString::from(format!(
                            "automations-dashboard-run-{idx}"
                        )))
                        .w_full()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_2p5()
                        .border_b_1()
                        .border_color(p.border)
                        .cursor_pointer()
                        .hover(|s| s.bg(p.muted.opacity(0.25)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_run_details(run_click.clone(), None, cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(140.))
                                .text_size(ui::TEXT_SM)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.foreground)
                                .truncate()
                                .child(run.id.clone()),
                        )
                        .child(
                            div()
                                .w(px(160.))
                                .flex_none()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(format_run_minute(run.minute)),
                        )
                        .child(
                            div()
                                .w(px(180.))
                                .flex_none()
                                .font_family(ui::FONT_MONO)
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .truncate()
                                .child(run.cwd.clone().unwrap_or_else(|| "—".into())),
                        )
                        .child(
                            div()
                                .w(px(120.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(ui::badge(p, tone, badge_label))
                                .child(ui::icons::svg_box(
                                    ui::icons::ARROW_RIGHT_SVG,
                                    ui::icons::ICON_SM,
                                    p.muted_foreground,
                                )),
                        ),
                );
            }
        }

        div()
            .flex_1()
            .min_h_0()
            .px_5()
            .pb_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1p5()
                            .children(self.runs_search_input.as_ref().map(|input| {
                                div()
                                    .id("automations-runs-search")
                                    .w(px(224.))
                                    .child(Input::new(input).small())
                            }))
                            .child(status_chip(
                                RunStatusFilter::All,
                                text::FILTER_ALL_STATUSES.get(),
                                "runs-filter-all",
                            ))
                            .child(status_chip(
                                RunStatusFilter::Successful,
                                text::FILTER_SUCCESSFUL.get(),
                                "runs-filter-success",
                            ))
                            .child(status_chip(
                                RunStatusFilter::Failed,
                                text::LAST_RUN_FAILED.get(),
                                "runs-filter-failed",
                            ))
                            .child(status_chip(
                                RunStatusFilter::InProgress,
                                text::FILTER_IN_PROGRESS.get(),
                                "runs-filter-progress",
                            ))
                            .child(status_chip(
                                RunStatusFilter::Skipped,
                                text::FILTER_SKIPPED.get(),
                                "runs-filter-skipped",
                            )),
                    )
                    .child(
                        ui::secondary("automations-runs-refresh", text::REFRESH.get())
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_wrap()
                    .gap_2p5()
                    .child(summary_card(
                        text::SUMMARY_SUCCESS_24H.get(),
                        success_24h,
                        p.foreground,
                    ))
                    .child(summary_card(
                        text::SUMMARY_FAILED_24H.get(),
                        failed_24h,
                        if failed_24h > 0 {
                            p.destructive
                        } else {
                            p.foreground
                        },
                    ))
                    .child(summary_card(
                        text::SUMMARY_SUCCESS_7D.get(),
                        success_7d,
                        p.foreground,
                    ))
                    .child(summary_card(
                        text::SUMMARY_FAILED_7D.get(),
                        failed_7d,
                        if failed_7d > 0 {
                            p.destructive
                        } else {
                            p.foreground
                        },
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.muted.opacity(0.2))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(32.))
                            .w_full()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .border_b_1()
                            .border_color(p.border)
                            .bg(p.muted.opacity(0.4))
                            .text_size(ui::TEXT_XS)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.muted_foreground)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(140.))
                                    .child(text::COL_AUTOMATION.get()),
                            )
                            .child(
                                div()
                                    .w(px(160.))
                                    .flex_none()
                                    .child(text::COL_TRIGGERED.get()),
                            )
                            .child(
                                div()
                                    .w(px(180.))
                                    .flex_none()
                                    .child(text::COL_WORKSPACE.get()),
                            )
                            .child(
                                div()
                                    .w(px(120.))
                                    .flex_none()
                                    .flex()
                                    .justify_end()
                                    .child(text::COL_STATUS.get()),
                            ),
                    )
                    .child(rows),
            )
            .into_any_element()
    }

    fn render_run_details_view(
        &self,
        run: &ScheduleRun,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let (tone, badge_label) = run_phase_badge_spec(&run.phase);
        let run_id = run.id.clone();

        div()
            .id("automations-run-details")
            .debug_selector(|| "automations-run-details".into())
            .flex_1()
            .min_h_0()
            .px_5()
            .pb_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .pb_3()
                    .border_b_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_size(ui::TEXT_MD)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.foreground)
                                    .child(run.id.clone()),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(format!(
                                        "{} · {}",
                                        format_run_minute(run.minute),
                                        run.cwd.as_deref().unwrap_or("—")
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ui::badge(p, tone, badge_label))
                            .child(
                                ui::secondary("automations-run-rerun", text::RERUN_BTN.get())
                                    .debug_selector(|| "automations-run-rerun".into())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.run_now(&run_id, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .id("automations-run-output")
                    .debug_selector(|| "automations-run-output".into())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.editor_surface)
                    .p_4()
                    .font_family(ui::FONT_MONO)
                    .text_size(ui::TEXT_XS)
                    .text_color(p.foreground)
                    .child(self.run_log_output.clone()),
            )
            .into_any_element()
    }

    fn render_editor_modal(
        &self,
        window: &Window,
        p: &'static ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        let modal = self.modal.as_ref()?;
        let is_editing = modal.editing_id.is_some();
        let heading = if is_editing {
            text::MODAL_EDIT_TITLE.get()
        } else {
            text::MODAL_CREATE_TITLE.get()
        };

        let cadence_btn = |cad: ScheduleCadence, label: &'static str, id: &'static str| {
            let active = modal.cadence == cad;
            ui::segment(p, id, label, active).on_click(cx.listener(move |this, _, window, cx| {
                if let Some(m) = &mut this.modal {
                    m.cadence = cad;
                    if let Some(preset) = cad.default_cron() {
                        m.cron
                            .update(cx, |s, cx| s.set_value(preset.to_owned(), window, cx));
                    }
                    cx.notify();
                }
            }))
        };

        let agent_btn = |ag: AgentFilter, id: &'static str| {
            let active = modal.agent == ag;
            ui::segment(p, id, ag.label(), active).on_click(cx.listener(
                move |this, _, window, cx| {
                    if let Some(m) = &mut this.modal {
                        m.agent = ag;
                        m.command.update(cx, |s, cx| {
                            s.set_value(ag.default_command().to_owned(), window, cx);
                        });
                        cx.notify();
                    }
                },
            ))
        };

        let reuse_active = modal.workspace_mode == WorkspaceMode::Reuse;

        let dialog = div()
            .id("automations-editor-modal")
            .debug_selector(|| "automations-editor-modal".into())
            .occlude()
            .role(gpui::Role::Dialog)
            .aria_label(heading)
            .w(px(960.))
            .max_w(window.viewport_size().width - ui::SPACE_4 * 2.)
            .max_h(window.viewport_size().height - ui::SPACE_4 * 2.)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_modal(cx)))
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                // Header (AutomationEditorDialogHeader)
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_5()
                    .py_3p5()
                    .border_b_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(heading),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(!is_editing, |d| {
                                d.child(
                                    ui::secondary(
                                        "automations-modal-template-btn",
                                        text::USE_TEMPLATE.get(),
                                    )
                                    .debug_selector(|| "automations-modal-template-btn".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.template_menu_open = !this.template_menu_open;
                                            cx.notify();
                                        },
                                    )),
                                )
                            })
                            .child(
                                ui::ghost("automations-modal-close", "")
                                    .icon(IconName::Close)
                                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                            ),
                    ),
            )
            .when(self.template_menu_open, |d| {
                let mut bar = div()
                    .px_5()
                    .py_2p5()
                    .border_b_1()
                    .border_color(p.border)
                    .bg(p.muted.opacity(0.3))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2();
                for tpl in TEMPLATES {
                    bar = bar.child(
                        ui::secondary(format!("modal-tpl-{}", tpl.id), tpl.name()).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.apply_template(tpl, window, cx);
                            }),
                        ),
                    );
                }
                d.child(bar)
            })
            .child(
                // 2-Column Body (AutomationEditorPromptSection + AutomationEditorSettingsSidebar)
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        // Left Column: Name + Prompt
                        div()
                            .id("automations-modal-left-scroll")
                            .flex_1()
                            .min_w_0()
                            .p_5()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .overflow_y_scroll()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(ui::field_label(stext::SCHEDULE_FIELD_ID.get(), None, p))
                                    .child(
                                        div()
                                            .id("automations-modal-id")
                                            .debug_selector(|| "automations-modal-id".into())
                                            .child(Input::new(&modal.id)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(ui::field_label(text::PROMPT_HEADING.get(), None, p))
                                    .child(
                                        div()
                                            .id("automations-modal-prompt")
                                            .debug_selector(|| "automations-modal-prompt".into())
                                            .child(Input::new(&modal.prompt)),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(text::PROMPT_HINT.get()),
                                    ),
                            )
                            .children(modal.error.clone().map(|err| {
                                div()
                                    .id("automations-modal-error")
                                    .debug_selector(|| "automations-modal-error".into())
                                    .child(ui::error_banner(p, err))
                            })),
                    )
                    .child(
                        // Right Column: Settings Sidebar (320px)
                        div()
                            .id("automations-modal-right-scroll")
                            .w(px(320.))
                            .flex_none()
                            .border_l_1()
                            .border_color(p.border)
                            .bg(p.muted.opacity(0.2))
                            .p_4()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_3p5()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1p5()
                                    .child(ui::field_label(text::META_AGENT.get(), None, p))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .gap_1()
                                            .child(agent_btn(
                                                AgentFilter::Claude,
                                                "modal-agent-claude",
                                            ))
                                            .child(agent_btn(
                                                AgentFilter::Codex,
                                                "modal-agent-codex",
                                            ))
                                            .child(agent_btn(
                                                AgentFilter::Antigravity,
                                                "modal-agent-agy",
                                            ))
                                            .child(agent_btn(
                                                AgentFilter::OpenCode,
                                                "modal-agent-opencode",
                                            ))
                                            .child(agent_btn(
                                                AgentFilter::Cursor,
                                                "modal-agent-cursor",
                                            ))
                                            .child(agent_btn(AgentFilter::Pi, "modal-agent-pi")),
                                    )
                                    .child(
                                        div()
                                            .id("automations-modal-command")
                                            .debug_selector(|| "automations-modal-command".into())
                                            .child(Input::new(&modal.command).small()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(ui::field_label(
                                        stext::SCHEDULE_FIELD_CWD.get(),
                                        None,
                                        p,
                                    ))
                                    .child(
                                        div()
                                            .id("automations-modal-cwd")
                                            .debug_selector(|| "automations-modal-cwd".into())
                                            .child(Input::new(&modal.cwd).small()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1p5()
                                    .child(ui::field_label(
                                        stext::SCHEDULE_FIELD_WORKSPACE_MODE.get(),
                                        None,
                                        p,
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .gap_1()
                                            .child(
                                                ui::segment(
                                                    p,
                                                    "automations-modal-mode-isolate",
                                                    text::LOCATION_ISOLATE.get(),
                                                    !reuse_active,
                                                )
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    if let Some(m) = &mut this.modal {
                                                        m.workspace_mode = WorkspaceMode::Isolate;
                                                        cx.notify();
                                                    }
                                                })),
                                            )
                                            .child(
                                                ui::segment(
                                                    p,
                                                    "automations-modal-mode-reuse",
                                                    text::LOCATION_REUSE.get(),
                                                    reuse_active,
                                                )
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    if let Some(m) = &mut this.modal {
                                                        m.workspace_mode = WorkspaceMode::Reuse;
                                                        cx.notify();
                                                    }
                                                })),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1p5()
                                    .child(ui::field_label(
                                        stext::SCHEDULE_FIELD_CRON.get(),
                                        None,
                                        p,
                                    ))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .gap_1()
                                            .child(cadence_btn(
                                                ScheduleCadence::Hourly,
                                                text::CADENCE_HOURLY.get(),
                                                "modal-cadence-hourly",
                                            ))
                                            .child(cadence_btn(
                                                ScheduleCadence::Daily,
                                                text::CADENCE_DAILY.get(),
                                                "modal-cadence-daily",
                                            ))
                                            .child(cadence_btn(
                                                ScheduleCadence::Weekdays,
                                                text::CADENCE_WEEKDAYS.get(),
                                                "modal-cadence-weekdays",
                                            ))
                                            .child(cadence_btn(
                                                ScheduleCadence::Weekly,
                                                text::CADENCE_WEEKLY.get(),
                                                "modal-cadence-weekly",
                                            ))
                                            .child(cadence_btn(
                                                ScheduleCadence::Custom,
                                                text::CADENCE_CUSTOM.get(),
                                                "modal-cadence-custom",
                                            )),
                                    )
                                    .child(
                                        div()
                                            .id("automations-modal-cron")
                                            .debug_selector(|| "automations-modal-cron".into())
                                            .child(Input::new(&modal.cron).small()),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(stext::SCHEDULE_FIELD_CRON_HINT.get()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(ui::field_label(
                                        stext::SCHEDULE_FIELD_PRECHECK.get(),
                                        None,
                                        p,
                                    ))
                                    .child(
                                        div()
                                            .id("automations-modal-precheck")
                                            .debug_selector(|| "automations-modal-precheck".into())
                                            .child(Input::new(&modal.precheck).small()),
                                    ),
                            ),
                    ),
            )
            .child(
                // Footer (AutomationEditorDialogFooter)
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .px_5()
                    .py_3p5()
                    .border_t_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(ui::icons::svg_box(
                                ui::icons::SPARKLES_SVG,
                                ui::icons::ICON_XS,
                                p.muted_foreground,
                            ))
                            .child(text::FOOTER_AUTO_NOTE.get()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                ui::ghost("automations-modal-cancel", stext::SCHEDULE_CANCEL.get())
                                    .debug_selector(|| "automations-modal-cancel".into())
                                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                            )
                            .child(
                                ui::primary(
                                    "automations-modal-save",
                                    if is_editing {
                                        text::SAVE_CHANGES_BTN.get()
                                    } else {
                                        text::CREATE_BTN.get()
                                    },
                                )
                                .debug_selector(|| "automations-modal-save".into())
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.submit_modal(window, cx);
                                    },
                                )),
                            ),
                    ),
            );

        Some(ui::modal_backdrop(
            window.viewport_size(),
            ui::modal_focus_scope(dialog, &self.modal_focus),
        ))
    }
}

impl Render for View {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        if self.search_input.is_none() && cx.has_global::<gpui_component::Theme>() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(text::SEARCH.get()));
            self.search_sub = Some(cx.observe(&input, |this, input, cx| {
                this.query = input.read(cx).value().to_lowercase();
                cx.notify();
            }));
            self.search_input = Some(input);
        }
        if self.runs_search_input.is_none() && cx.has_global::<gpui_component::Theme>() {
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder(text::SEARCH_RUNS.get()));
            self.runs_search_sub = Some(cx.observe(&input, |this, input, cx| {
                this.runs_query = input.read(cx).value().to_lowercase();
                cx.notify();
            }));
            self.runs_search_input = Some(input);
        }

        let language_changed = self.language != crate::i18n::language();
        self.language = crate::i18n::language();
        if language_changed {
            if let Some(input) = &self.search_input {
                input.update(cx, |input, cx| {
                    input.set_placeholder(text::SEARCH.get(), window, cx);
                });
            }
            if let Some(input) = &self.runs_search_input {
                input.update(cx, |input, cx| {
                    input.set_placeholder(text::SEARCH_RUNS.get(), window, cx);
                });
            }
        }

        let top_bar = self.render_top_bar(p, cx);
        let main_content = match self.screen.clone() {
            PageView::List => self.render_list_view(p, cx),
            PageView::Detail { id, tab } => self.render_detail_view(&id, tab, p, cx),
            PageView::RunsDashboard => self.render_runs_dashboard(p, cx),
            PageView::RunDetails { run, .. } => self.render_run_details_view(&run, p, cx),
        };
        let modal = self.render_editor_modal(window, p, cx);

        div()
            .id("automations-page")
            .debug_selector(|| "automations-page".into())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    this.handle_escape(cx);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(p.background)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_MD)
            .text_color(p.foreground)
            .child(top_bar)
            .children(
                self.error
                    .clone()
                    .map(|err| div().px_5().pb_2().child(ui::error_banner(p, err))),
            )
            .child(main_content)
            .children(modal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_summary_next_run_and_agent_inference_work() {
        assert_eq!(
            infer_agent_from_command(&["codex".into(), "exec".into()]).0,
            AgentFilter::Codex
        );
        assert_eq!(
            infer_agent_from_command(&["/usr/local/bin/claude".into(), "-p".into()]).0,
            AgentFilter::Claude
        );
        assert_eq!(
            infer_agent_from_command(&["agy".into()]).0,
            AgentFilter::Antigravity
        );
        assert_eq!(project_label("/Users/me/wProject/krill/"), "krill");
        assert!(format_cron_summary("0 9 * * 1-5").contains("09:00 UTC"));

        // 2026-10-25 00:00 UTC = 29,882,880 minutes since epoch.
        let base_min = 29_882_880_u64;
        let next = next_run_minute("0 9 * * *", base_min).unwrap();
        assert_eq!(next, base_min + 9 * 60);
    }

    #[test]
    fn editor_preserves_hidden_options_and_rejects_id_collisions() {
        let original = Schedule {
            id: "old".into(),
            name: Some("Daily audit".into()),
            agent: Some("claude".into()),
            extra_agent_args: Some("--verbose".into()),
            precheck_timeout_secs: Some(42),
            base_branch: Some("dev".into()),
            reuse_session: false,
            missed_run_grace_minutes: Some(7),
            ..Schedule::default()
        };
        let mut schedules = Schedules {
            schedule: vec![
                original.clone(),
                Schedule {
                    id: "other".into(),
                    ..Schedule::default()
                },
            ],
        };
        let edited = Schedule {
            id: "renamed".into(),
            prompt: "new prompt".into(),
            ..Schedule::default()
        };
        apply_schedule_edit(&mut schedules, Some("old"), edited.clone()).unwrap();
        let result = schedules.schedule.last().unwrap();
        assert_eq!(
            result,
            &Schedule {
                id: edited.id,
                prompt: edited.prompt,
                ..original
            }
        );
        let before = schedules.clone();
        let collision = Schedule {
            id: "other".into(),
            ..Schedule::default()
        };
        assert_eq!(
            apply_schedule_edit(&mut schedules, Some("renamed"), collision)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(schedules, before);
    }

    #[gpui::test]
    fn automations_view_navigates_detail_runs_and_handles_escape(cx: &mut gpui::TestAppContext) {
        let dir = std::env::temp_dir().join(format!(
            "krill-automations-view-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let schedules = Schedules {
            schedule: vec![Schedule {
                id: "daily-audit".into(),
                cron: "0 9 * * 1-5".into(),
                cwd: "/tmp".into(),
                command: vec!["claude".into(), "-p".into()],
                prompt: "Audit repository".into(),
                precheck: None,
                workspace_mode: WorkspaceMode::Isolate,
                enabled: true,
                ..Schedule::default()
            }],
        };
        schedules.save(&dir).unwrap();
        let run_entry = ScheduleRun {
            id: "daily-audit".into(),
            minute: 29_882_880,
            phase: "finished".into(),
            session: None,
            status: Some(0),
            error: None,
            cwd: Some("/tmp".into()),
            output: Some("All checks passed.".into()),
            ..ScheduleRun::default()
        };
        std::fs::write(
            dir.join(krill_proto::schedule::RUNS),
            format!("{}\n", serde_json::to_string(&run_entry).unwrap()),
        )
        .unwrap();

        let dir_clone = dir.clone();
        let view = cx.new(|cx| View::new(Some(dir_clone), Some(PathBuf::from("/tmp")), cx));
        view.read_with(cx, |v, _| {
            assert_eq!(v.schedules().len(), 1);
            assert_eq!(v.runs().len(), 1);
            assert_eq!(v.page_view(), &PageView::List);
        });

        // Navigate to detail -> runs tab -> run details -> escape back to detail -> escape back to list
        view.update(cx, |v, cx| v.open_detail("daily-audit".into(), cx));
        view.read_with(cx, |v, _| {
            assert_eq!(
                v.page_view(),
                &PageView::Detail {
                    id: "daily-audit".into(),
                    tab: DetailTab::Overview
                }
            );
        });
        view.update(cx, |v, cx| v.set_detail_tab(DetailTab::Runs, cx));
        let run = view.read_with(cx, |v, _| v.runs()[0].clone());
        view.update(cx, |v, cx| {
            v.open_run_details(run, Some("daily-audit".into()), cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert!(matches!(v.page_view(), PageView::RunDetails { .. }));
            assert_eq!(v.run_log_output, "All checks passed.");
        });

        view.update(cx, View::handle_escape);
        view.read_with(cx, |v, _| {
            assert_eq!(
                v.page_view(),
                &PageView::Detail {
                    id: "daily-audit".into(),
                    tab: DetailTab::Runs
                }
            );
        });

        view.update(cx, View::handle_escape);
        view.read_with(cx, |v, _| {
            assert_eq!(v.page_view(), &PageView::List);
        });

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
