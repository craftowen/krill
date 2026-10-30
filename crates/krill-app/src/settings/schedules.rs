//! 스케줄 자동화 GUI(OWE-740, MK-186): `Settings > Workflows` 탭의 스케줄 목록, 추가/편집 모달
//! (`precheck` 셸 게이트 · `workspace_mode` 격리 워크트리 선택 포함), 즉시 실행(`Run Now`), 최근 실행
//! 기록(`runs`) 배지 및 터미널 출력 로그 뷰어.

// Automations design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use gpui::{Context, Div, FontWeight, SharedString, Stateful, Window, div, prelude::*};
use gpui_component::Sizable as _;
use gpui_component::input::{Input, InputState};
use krill_proto::schedule::{
    FILE, Schedule, ScheduleRun, Schedules, WorkspaceMode, request_run_now,
};

use super::{SettingsView, layout, open_file, state_dir, text};

mod editor;
pub(super) mod model;
mod page;
pub(super) use model::SchedulesState;
pub use model::run_outcome_badge_spec;
use model::{
    AutomationsSubView, DetailTab, OutcomeFilter, RunOutcome, RunsStatusFilter, ScheduleSortField,
    StatusFilter, TEMPLATES, Template, latest_run, matches_run, matches_schedule, next_run,
    now_minute, retained_count,
};

/// 스케줄 추가/편집 모달 입력 상태.
pub(super) struct ScheduleModal {
    pub editing_id: Option<String>,
    pub id: gpui::Entity<InputState>,
    pub cron: gpui::Entity<InputState>,
    pub cwd: gpui::Entity<InputState>,
    pub command: gpui::Entity<InputState>,
    pub prompt: gpui::Entity<gpui_component::input::TextareaState>,
    pub name: gpui::Entity<InputState>,
    pub extra_args: gpui::Entity<InputState>,
    pub precheck_timeout: gpui::Entity<InputState>,
    pub base_branch: gpui::Entity<InputState>,
    pub grace: gpui::Entity<InputState>,
    pub agent: Option<String>,
    pub reuse_session: bool,
    pub preset: krill_proto::schedule::SchedulePreset,
    pub subscriptions: Vec<gpui::Subscription>,
    pub precheck: gpui::Entity<InputState>,
    pub workspace_mode: WorkspaceMode,
    pub enabled: bool,
    pub error: Option<SharedString>,
}

/// 선택된 실행 기록의 상세 로그 뷰어 상태.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleLogView {
    pub id: String,
    pub minute: u64,
    pub phase: String,
    pub cwd: Option<String>,
    pub output: String,
}

/// 실행 단계 문자열을 시맨틱 배지 톤과 다국어 라벨로 매핑한다.
#[must_use]
pub fn run_phase_badge_spec(phase: &str) -> (crate::ui::BadgeTone, &'static str) {
    use crate::ui::BadgeTone;
    match phase {
        "skipped" => (BadgeTone::Muted, text::SCHEDULE_STATUS_SKIPPED.get()),
        "spawned" | "started" => (BadgeTone::Accent, text::SCHEDULE_STATUS_SPAWNED.get()),
        "finished" => (BadgeTone::Success, text::SCHEDULE_STATUS_FINISHED.get()),
        "failed" => (BadgeTone::Destructive, text::SCHEDULE_STATUS_FAILED.get()),
        _ => (BadgeTone::Neutral, text::SCHEDULE_STATUS_SPAWNED.get()),
    }
}

/// 유닉스 epoch 분을 `YYYY-MM-DD HH:MM` 날짜·시각으로 푼다(OWE-883, UTC).
/// `chrono` 없이 윤년을 포함한 그레고리력 역산만 쓴다. 범위를 벗어나면 빈 문자열.
#[must_use]
pub fn format_run_minute(minute: u64) -> String {
    let secs = minute.saturating_mul(60);
    let days = secs / 86_400;
    let secs_of_day = secs % 86_400;
    // 1970-01-01 기준 일수를 연·월·일로 푼다(Howard Hinnant civil_from_days).
    let Ok(z) = i64::try_from(days.saturating_add(719_468)) else {
        return String::new();
    };
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    y += i64::from(m <= 2);
    if !(1970..=9_999).contains(&y) {
        return String::new();
    }
    let (hour, min) = (
        (secs_of_day / 3_600) as u32,
        (secs_of_day % 3_600 / 60) as u32,
    );
    let date = format!("{y:04}-{m:02}-{d:02}");
    let time = format!("{hour:02}:{min:02}");
    text::SCHEDULE_RUN_TIME.format(&[("date", &date), ("time", &time)])
}

pub(crate) fn capture_session_output(
    dir: &Path,
    session: krill_proto::SessionId,
) -> Option<String> {
    use krill_proto::control::{CaptureFormat, FEAT_CLI, FEAT_CONTROL, Reply, Request};
    use krill_proto::screen::Msg as _;
    use std::path::PathBuf;

    let socket = krill_proto::daemon::socket_path(dir).ok()?;
    if !socket.exists() {
        return None;
    }
    let paths = krill_proto::daemon::Paths {
        socket,
        holder: PathBuf::new(),
        krilld: PathBuf::new(),
    };
    let target = crate::hosts::HostTarget::from(paths);
    let mut hello = krill_proto::daemon::client_hello(
        krill_proto::ipc::ClientRole::Cli,
        "krill-app schedule-log",
    );
    hello.features = FEAT_CONTROL | FEAT_CLI;
    let mut conn = target.connect(&hello).ok()?;
    if conn.negotiated.features & FEAT_CLI == 0 {
        return None;
    }
    let timeout = Some(Duration::from_secs(2));
    conn.stream.set_read_timeout(timeout).ok();
    conn.stream.set_write_timeout(timeout).ok();
    let frame = (Request::Capture {
        session,
        format: CaptureFormat::Plain,
    })
    .encode()
    .ok()?;
    conn.stream.write_all(&frame).ok()?;
    let (kind, body) = krill_proto::ipc::read_frame(&mut conn.stream).ok()??;
    if let Ok(Reply::Captured { bytes, .. }) = Reply::decode(kind, &body) {
        let text = String::from_utf8_lossy(&bytes);
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    Single,
    Double,
}

/// 명령 인자 배열을 직렬화한다. Windows의 경로·이스케이프 문자는 JSON으로 보존한다.
#[must_use]
pub fn format_command_args(args: &[String]) -> String {
    if cfg!(windows)
        && args
            .iter()
            .any(|arg| arg.contains(['\\', '\'', '"', '\n', '\r', '\t']))
    {
        return serde_json::Value::Array(
            args.iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        )
        .to_string();
    }
    args.iter()
        .enumerate()
        .map(|(idx, arg)| {
            let looks_like_json_array =
                idx == 0 && args.len() == 1 && arg.starts_with('[') && arg.ends_with(']');
            if !arg.is_empty()
                && !looks_like_json_array
                && !arg
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'))
            {
                arg.clone()
            } else if !arg.is_empty()
                && !arg.contains(['\'', '\n', '\r', '\t'])
                && !looks_like_json_array
            {
                format!("'{arg}'")
            } else {
                let mut out = String::with_capacity(arg.len() + 2);
                out.push('"');
                for ch in arg.chars() {
                    match ch {
                        '\\' => out.push_str("\\\\"),
                        '"' => out.push_str("\\\""),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        _ => out.push(ch),
                    }
                }
                out.push('"');
                out
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 셸 따옴표 또는 JSON 문자열 배열을 파싱한다. Windows에서는 `\`를 경로 문자로 보존한다.
pub fn parse_command_args(raw: &str) -> Result<Vec<String>, String> {
    parse_command_args_for(raw, cfg!(windows))
}

fn parse_command_args_for(raw: &str, windows: bool) -> Result<Vec<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(text::SCHEDULE_ERR_COMMAND_EMPTY.get().to_owned());
    }
    if trimmed.starts_with('[')
        && trimmed.ends_with(']')
        && let Ok(arr) = serde_json::from_str::<Vec<String>>(trimmed)
    {
        if arr.is_empty()
            || arr.len() > 128
            || arr.first().is_none_or(|s| s.trim().is_empty())
            || arr.iter().any(|s| s.contains('\0') || s.len() > 8192)
        {
            return Err(text::SCHEDULE_ERR_COMMAND_INVALID.get().to_owned());
        }
        return Ok(arr);
    }

    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut quote: Option<Quote> = None;
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(Quote::Single) => {
                if ch == '\'' {
                    if windows && chars.peek() == Some(&'\'') {
                        chars.next();
                        cur.push('\'');
                    } else {
                        quote = None;
                    }
                } else {
                    cur.push(ch);
                }
            }
            Some(Quote::Double) => {
                if ch == '"' {
                    quote = None;
                } else if ch == '\\' && !windows {
                    let Some(next) = chars.next() else {
                        return Err(text::SCHEDULE_ERR_COMMAND_ESCAPE.get().to_owned());
                    };
                    match next {
                        '\\' | '"' => cur.push(next),
                        'n' => cur.push('\n'),
                        'r' => cur.push('\r'),
                        't' => cur.push('\t'),
                        _ => {
                            cur.push('\\');
                            cur.push(next);
                        }
                    }
                } else {
                    cur.push(ch);
                }
            }
            None => {
                if ch.is_whitespace() {
                    if in_token {
                        args.push(std::mem::take(&mut cur));
                        in_token = false;
                    }
                } else if ch == '\'' {
                    in_token = true;
                    quote = Some(Quote::Single);
                } else if ch == '"' {
                    in_token = true;
                    quote = Some(Quote::Double);
                } else if ch == '\\' && !windows {
                    let Some(next) = chars.next() else {
                        return Err(text::SCHEDULE_ERR_COMMAND_ESCAPE.get().to_owned());
                    };
                    in_token = true;
                    cur.push(next);
                } else {
                    in_token = true;
                    cur.push(ch);
                }
            }
        }
    }
    if quote.is_some() {
        return Err(text::SCHEDULE_ERR_COMMAND_QUOTE.get().to_owned());
    }
    if in_token {
        args.push(cur);
    }
    if args.is_empty() || args.first().is_none_or(|s| s.trim().is_empty()) {
        return Err(text::SCHEDULE_ERR_COMMAND_EMPTY.get().to_owned());
    }
    if args.len() > 128 || args.iter().any(|s| s.contains('\0') || s.len() > 8192) {
        return Err(text::SCHEDULE_ERR_COMMAND_INVALID.get().to_owned());
    }
    Ok(args)
}

impl SettingsView {
    /// 스케줄 추가(`existing == None`) 또는 편집(`existing == Some(..)`) 모달을 연다.
    pub fn open_schedule_modal(
        &mut self,
        existing: Option<&Schedule>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_return_focus.is_none() {
            self.modal_return_focus =
                Some(window.focused(cx).unwrap_or_else(|| self.focus.clone()));
        }
        self.schedule_form_generation = self.schedule_form_generation.wrapping_add(1);
        let editing_id = existing.map(|s| s.id.clone());
        let init_id = existing.map_or("", |s| s.id.as_str()).to_owned();
        let init_cron = existing.map_or("0 9 * * *", |s| s.cron.as_str()).to_owned();
        let init_cwd = existing.map_or("", |s| s.cwd.as_str()).to_owned();
        let init_cmd = existing.map_or_else(
            || "claude -p".to_owned(),
            |s| format_command_args(&s.command),
        );
        let init_prompt = existing.map_or("", |s| s.prompt.as_str()).to_owned();
        let init_precheck = existing
            .and_then(|s| s.precheck.as_deref())
            .unwrap_or("")
            .to_owned();
        let workspace_mode = existing.map_or(WorkspaceMode::Reuse, |s| s.workspace_mode);
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

        let name = make_input(
            text::AUTOMATIONS_NAME.get(),
            existing.and_then(|s| s.name.clone()).unwrap_or_default(),
            window,
            cx,
        );
        let extra_args = make_input(
            text::AUTOMATIONS_EXTRA_ARGS.get(),
            existing
                .and_then(|s| s.extra_agent_args.clone())
                .unwrap_or_default(),
            window,
            cx,
        );
        let precheck_timeout = make_input(
            text::AUTOMATIONS_PRECHECK_TIMEOUT.get(),
            existing
                .and_then(|s| s.precheck_timeout_secs)
                .map(|n| n.to_string())
                .unwrap_or_default(),
            window,
            cx,
        );
        let base_branch = make_input(
            text::AUTOMATIONS_BASE_BRANCH.get(),
            existing
                .and_then(|s| s.base_branch.clone())
                .unwrap_or_default(),
            window,
            cx,
        );
        let grace = make_input(
            text::AUTOMATIONS_GRACE.get(),
            existing
                .and_then(|s| s.missed_run_grace_minutes)
                .map(|n| n.to_string())
                .unwrap_or_default(),
            window,
            cx,
        );
        let prompt = cx.new(|cx| {
            gpui_component::input::TextareaState::new(window, cx)
                .auto_grow(6, 16)
                .default_value(init_prompt)
        });
        let preset = krill_proto::schedule::classify_schedule(&init_cron).preset;
        self.schedule_modal = Some(ScheduleModal {
            name,
            extra_args,
            precheck_timeout,
            base_branch,
            grace,
            prompt,
            preset,
            subscriptions: Vec::new(),
            agent: existing.and_then(|s| s.agent.clone()),
            reuse_session: existing.is_some_and(|s| s.reuse_session),
            editing_id,
            id: make_input(
                text::SCHEDULE_FIELD_ID_PLACEHOLDER.get(),
                init_id,
                window,
                cx,
            ),
            cron: make_input(
                text::SCHEDULE_FIELD_CRON_PLACEHOLDER.get(),
                init_cron,
                window,
                cx,
            ),
            cwd: make_input(
                text::SCHEDULE_FIELD_CWD_PLACEHOLDER.get(),
                init_cwd,
                window,
                cx,
            ),
            command: make_input(
                text::SCHEDULE_FIELD_COMMAND_PLACEHOLDER.get(),
                init_cmd,
                window,
                cx,
            ),
            precheck: make_input(
                text::SCHEDULE_FIELD_PRECHECK_PLACEHOLDER.get(),
                init_precheck,
                window,
                cx,
            ),
            workspace_mode,
            enabled,
            error: None,
        });
        if let Some(modal) = &self.schedule_modal {
            let cron_input = modal.cron.clone();
            let extra_input = modal.extra_args.clone();
            let cron_sub = cx.subscribe(
                &cron_input,
                |this, input, event: &gpui_component::input::InputEvent, cx| {
                    if matches!(event, gpui_component::input::InputEvent::Change) {
                        let classified = krill_proto::schedule::classify_schedule(
                            input.read(cx).value().as_ref(),
                        );
                        if let Some(modal) = &mut this.schedule_modal {
                            modal.preset = classified.preset;
                        }
                        cx.notify();
                    }
                },
            );
            let extra_sub = cx.subscribe(
                &extra_input,
                |_, _, _: &gpui_component::input::InputEvent, cx| cx.notify(),
            );
            if let Some(modal) = &mut self.schedule_modal {
                modal.subscriptions = vec![cron_sub, extra_sub];
            }
        }
        if let Some(modal) = &self.schedule_modal {
            modal.name.update(cx, |s, cx| s.focus(window, cx));
        }
        cx.notify();
    }

    pub fn close_schedule_modal(&mut self, cx: &mut Context<Self>) {
        self.schedule_form_generation = self.schedule_form_generation.wrapping_add(1);
        if self.schedule_modal.is_some() {
            self.schedule_modal = None;
            cx.notify();
        }
    }

    #[must_use]
    pub fn schedule_modal_error(&self) -> Option<&str> {
        self.schedule_modal
            .as_ref()
            .and_then(|m| m.error.as_ref().map(SharedString::as_ref))
    }

    /// 열린 스케줄 모달의 입력 값을 일괄 설정한다.
    #[expect(
        clippy::too_many_arguments,
        reason = "스케줄 모달의 7개 필드를 한 번에 채우는 헬퍼"
    )]
    pub fn set_schedule_modal_values(
        &mut self,
        id: &str,
        cron: &str,
        cwd: &str,
        command: &str,
        prompt: &str,
        precheck: &str,
        workspace_mode: WorkspaceMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(modal) = &mut self.schedule_modal else {
            return;
        };
        modal
            .id
            .update(cx, |s, cx| s.set_value(id.to_owned(), window, cx));
        modal
            .cron
            .update(cx, |s, cx| s.set_value(cron.to_owned(), window, cx));
        modal
            .cwd
            .update(cx, |s, cx| s.set_value(cwd.to_owned(), window, cx));
        modal
            .command
            .update(cx, |s, cx| s.set_value(command.to_owned(), window, cx));
        modal
            .prompt
            .update(cx, |s, cx| s.set_value(prompt.to_owned(), window, cx));
        modal
            .precheck
            .update(cx, |s, cx| s.set_value(precheck.to_owned(), window, cx));
        modal.workspace_mode = workspace_mode;
        modal.error = None;
        cx.notify();
    }

    /// 모달 입력을 검증해 `schedules.toml`에 저장한다.
    pub fn submit_schedule_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_jobs.contains_key("schedule-edit") {
            return;
        }
        let Some(modal) = &mut self.schedule_modal else {
            return;
        };
        let name_raw = modal.name.read(cx).value().trim().to_owned();
        let name = (!name_raw.is_empty()).then_some(name_raw.clone());
        let mut id = modal.id.read(cx).value().trim().to_owned();
        if id.is_empty() && modal.editing_id.is_none() && !name_raw.is_empty() {
            let ids = self
                .services
                .schedules
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>();
            id = krill_proto::schedule::slugify_schedule_id(&name_raw, &ids);
        }
        let agent = modal.agent.clone();
        let extra_args = modal.extra_args.read(cx).value().trim().to_owned();
        let extra_agent_args = (!extra_args.is_empty()).then_some(extra_args);
        let base = modal.base_branch.read(cx).value().trim().to_owned();
        let base_branch = (!base.is_empty()).then_some(base);
        let reuse_session = modal.reuse_session
            && modal.workspace_mode == WorkspaceMode::Reuse
            && extra_agent_args.is_none();
        let precheck_timeout_secs =
            parse_optional_number::<u64>(modal.precheck_timeout.read(cx).value().as_ref());
        let missed_run_grace_minutes =
            parse_optional_number::<u32>(modal.grace.read(cx).value().as_ref());
        let cron = modal.cron.read(cx).value().trim().to_owned();
        let cwd = modal.cwd.read(cx).value().trim().to_owned();
        let cmd_raw = modal.command.read(cx).value().trim().to_owned();
        let prompt = modal.prompt.read(cx).value().trim().to_owned();
        let precheck_raw = modal.precheck.read(cx).value().trim().to_owned();
        let precheck = (!precheck_raw.is_empty()).then_some(precheck_raw);
        let workspace_mode = modal.workspace_mode;
        let enabled = modal.enabled;
        let editing_id = modal.editing_id.clone();

        let fail = |modal: &mut ScheduleModal,
                    schedule_error: &mut Option<SharedString>,
                    msg: String,
                    cx: &mut Context<Self>| {
            let shared: SharedString = msg.into();
            modal.error = Some(shared.clone());
            *schedule_error = Some(shared);
            cx.notify();
        };

        let (Ok(precheck_timeout_secs), Ok(missed_run_grace_minutes)) =
            (precheck_timeout_secs, missed_run_grace_minutes)
        else {
            fail(
                modal,
                &mut self.schedule_error,
                text::AUTOMATIONS_INVALID_NUMBER.get().to_owned(),
                cx,
            );
            return;
        };
        if precheck_timeout_secs
            .is_some_and(|n| n == 0 || n > krill_proto::schedule::MAX_PRECHECK_TIMEOUT_SECS)
            || missed_run_grace_minutes
                .is_some_and(|n| n > krill_proto::schedule::MAX_MISSED_RUN_GRACE_MINUTES)
        {
            fail(
                modal,
                &mut self.schedule_error,
                text::AUTOMATIONS_INVALID_NUMBER.get().to_owned(),
                cx,
            );
            return;
        }
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            fail(
                modal,
                &mut self.schedule_error,
                text::SCHEDULE_ERR_ID.get().to_owned(),
                cx,
            );
            return;
        }
        if cwd.is_empty() || !Path::new(&cwd).is_absolute() || cwd.contains('\0') {
            fail(
                modal,
                &mut self.schedule_error,
                text::SCHEDULE_ERR_CWD.get().to_owned(),
                cx,
            );
            return;
        }
        if krill_proto::schedule::Cron::parse(&cron).is_err() {
            fail(
                modal,
                &mut self.schedule_error,
                text::SCHEDULE_ERR_CRON.get().to_owned(),
                cx,
            );
            return;
        }
        let command = match parse_command_args(&cmd_raw) {
            Ok(cmd) => cmd,
            Err(err) => {
                fail(modal, &mut self.schedule_error, err, cx);
                return;
            }
        };
        if prompt.contains('\0') || prompt.len() > 32 * 1024 {
            fail(
                modal,
                &mut self.schedule_error,
                text::SCHEDULE_ERR_PROMPT.get().to_owned(),
                cx,
            );
            return;
        }
        if precheck
            .as_ref()
            .is_some_and(|s| s.contains('\0') || s.len() > 8192)
        {
            fail(
                modal,
                &mut self.schedule_error,
                text::SCHEDULE_ERR_PRECHECK.get().to_owned(),
                cx,
            );
            return;
        }

        let entry = Schedule {
            id,
            name,
            agent,
            extra_agent_args,
            base_branch,
            reuse_session,
            precheck_timeout_secs,
            missed_run_grace_minutes,
            cron,
            cwd,
            command,
            prompt,
            precheck,
            workspace_mode,
            enabled,
        };
        if let Err(error) = entry.validate() {
            fail(modal, &mut self.schedule_error, error.to_string(), cx);
            return;
        }
        let Some(dir) = state_dir(cx) else {
            fail(
                modal,
                &mut self.schedule_error,
                text::NO_STATE_DIR.get().to_owned(),
                cx,
            );
            return;
        };
        let generation = self.schedule_form_generation;
        window.focus(&self.focus, cx);
        self.file_job(
            "schedule-edit",
            move || {
                let _guard = Schedules::exclusive_edit(&dir)?;
                let mut schedules = Schedules::load(&dir)?;
                if let Some(old_id) = editing_id.as_deref() {
                    schedules.schedule.retain(|s| s.id != old_id);
                }
                schedules.schedule.retain(|s| s.id != entry.id);
                schedules.schedule.push(entry);
                schedules.save(&dir)
            },
            move |this, result: std::io::Result<()>, cx| {
                if this.schedule_form_generation != generation {
                    this.refresh_service_cache(cx);
                    return;
                }
                match result {
                    Ok(()) => {
                        this.schedule_modal = None;
                        this.schedule_error = None;
                    }
                    Err(error) => {
                        this.schedule_error = Some(error.to_string().into());
                        if let Some(modal) = &mut this.schedule_modal {
                            modal.error.clone_from(&this.schedule_error);
                        }
                    }
                }
                this.refresh_service_cache(cx);
            },
            cx,
        );
    }

    /// 지정한 스케줄의 활성/일시정지 상태(`enabled`)를 토글해 `schedules.toml`에 저장한다.
    pub fn toggle_schedule_enabled(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(dir) = state_dir(cx) else {
            return;
        };
        let id = id.to_owned();
        self.file_job(
            "schedule-edit",
            move || {
                let _guard = Schedules::exclusive_edit(&dir)?;
                let mut schedules = Schedules::load(&dir)?;
                if let Some(sched) = schedules.schedule.iter_mut().find(|s| s.id == id) {
                    sched.enabled = !sched.enabled;
                    schedules.save(&dir)?;
                }
                Ok(())
            },
            |this, result: std::io::Result<()>, cx| {
                this.schedule_error = result.err().map(|error| error.to_string().into());
                this.refresh_service_cache(cx);
            },
            cx,
        );
    }

    pub fn remove_schedule(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(dir) = state_dir(cx) else {
            return;
        };
        let id = id.to_owned();
        let bg_id = id.clone();
        self.file_job(
            "schedule-edit",
            move || {
                let _guard = Schedules::exclusive_edit(&dir)?;
                let mut schedules = Schedules::load(&dir)?;
                schedules.schedule.retain(|s| s.id != bg_id);
                schedules.save(&dir)
            },
            move |this, result, cx| {
                match result {
                    Ok(()) => {
                        if this.schedule_log.as_ref().is_some_and(|log| log.id == id) {
                            this.schedule_log = None;
                        }
                    }
                    Err(error) => this.schedule_error = Some(error.to_string().into()),
                }
                this.refresh_service_cache(cx);
            },
            cx,
        );
    }

    /// 스케줄을 즉시 실행(`Run Now`)한다. 데몬 소켓이 살아 있을 때만 `request_run_now`로
    /// 데몬에 실행을 요청하며, 데몬이 없으면 데몬 필요 오류를 표시한다.
    pub fn trigger_schedule_run_now(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(dir) = state_dir(cx) else {
            self.schedule_error = Some(text::NO_STATE_DIR.get().into());
            cx.notify();
            return;
        };
        let id = id.to_owned();
        self.file_job(
            "schedule-run",
            move || {
                let socket = krill_proto::daemon::socket_path(&dir)?;
                if !socket.exists() || krill_proto::os::net::UnixStream::connect(&socket).is_err() {
                    return Err(std::io::Error::other(text::ACCOUNTS_DAEMON_REQUIRED.get()));
                }
                request_run_now(&dir, &id)
            },
            |this, result, _| {
                this.schedule_error = result.err().map(|error| error.to_string().into());
            },
            cx,
        );
    }

    /// 실행 기록 항목의 터미널 출력 로그 뷰어를 연다.
    pub fn open_schedule_run_log(&mut self, run: &ScheduleRun, cx: &mut Context<Self>) {
        self.schedule_log_generation = self.schedule_log_generation.wrapping_add(1);
        let generation = self.schedule_log_generation;
        let run = run.clone();
        let dir = state_dir(cx);
        // Selecting another log or closing it fences this completion by the full run key.
        self.schedule_log = Some(ScheduleLogView {
            id: run.id.clone(),
            minute: run.minute,
            phase: run.phase.clone(),
            cwd: run.cwd.clone(),
            output: run.output.clone().unwrap_or_default(),
        });
        cx.spawn(async move |this, cx| {
            let key = (run.id.clone(), run.minute, run.phase.clone());
            let output = cx
                .background_executor()
                .spawn(async move {
                    if let Some(out) = run.output.as_deref().filter(|s| !s.trim().is_empty()) {
                        out.to_owned()
                    } else if let Some(sid) = run.session
                        && let Some(dir) = dir
                        && let Some(captured) =
                            capture_session_output(&dir, krill_proto::SessionId(sid))
                    {
                        captured
                    } else if let Some(err) = run.error.as_deref().filter(|s| !s.trim().is_empty())
                    {
                        err.to_owned()
                    } else if let Some(st) = run.status {
                        format!("exit status {st}")
                    } else {
                        text::SCHEDULE_LOG_EMPTY.get().to_owned()
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.schedule_log_generation != generation {
                    return;
                }
                if let Some(log) = &mut this.schedule_log
                    && (log.id.clone(), log.minute, log.phase.clone()) == key
                {
                    log.output = output;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn close_schedule_run_log(&mut self, cx: &mut Context<Self>) {
        self.schedule_log_generation = self.schedule_log_generation.wrapping_add(1);
        if self.schedule_log.is_some() {
            self.schedule_log = None;
            cx.notify();
        }
    }

    #[must_use]
    pub fn schedule_log(&self) -> Option<&ScheduleLogView> {
        self.schedule_log.as_ref()
    }

    pub(super) fn render_schedules_section(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = crate::ui::palette(cx);
        let schedules = self.services.schedules.clone();
        let runs = self.services.runs.clone();

        let add_btn = layout::primary("admin-schedule-add", text::SCHEDULE_ADD_BTN.get())
            .debug_selector(|| "admin-schedule-add".into())
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_schedule_modal(None, window, cx);
            }));

        let open_toml_btn =
            layout::secondary("admin-schedule-open-toml", text::SCHEDULE_OPEN_TOML.get())
                .debug_selector(|| "admin-schedule-open-toml".into())
                .on_click(|_, _, cx| {
                    open_file(FILE, "# [[schedule]]\n", cx);
                });

        let mut card =
            layout::card(p)
                .id("admin-schedules-card")
                .debug_selector(|| "admin-schedules-card".into())
                .child(
                    layout::row(p)
                        .child(layout::label(
                            p,
                            text::AUTOMATIONS_SHOW_BTN_TITLE.get(),
                            Some(text::AUTOMATIONS_SHOW_BTN_DESC.get()),
                        ))
                        .child(
                            layout::controls()
                                .child(layout::switch(
                                    p,
                                    "admin-show-automations-button",
                                    super::current(cx).show_automations_button,
                                    false,
                                    |on, _, cx| {
                                        super::edit(cx, move |s| s.show_automations_button = on);
                                    },
                                ))
                                .child(
                                    layout::secondary(
                                        "admin-open-automations-page",
                                        text::AUTOMATIONS_OPEN.get(),
                                    )
                                    .debug_selector(|| "admin-open-automations-page".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| this.set_automations_page(true, cx),
                                    )),
                                ),
                        ),
                )
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(layout::label(
                            p,
                            text::SCHEDULES_TITLE.get(),
                            Some(text::SCHEDULES_DESC.get()),
                        ))
                        .child(layout::controls().child(open_toml_btn).child(add_btn)),
                );

        if let Some(err) = &self.schedule_error {
            card = card.child(
                crate::ui::error_banner(p, err.clone())
                    .id("admin-schedule-error")
                    .debug_selector(|| "admin-schedule-error".into()),
            );
        }

        if schedules.is_empty() {
            card = card.child(
                div()
                    .id("admin-schedules-empty")
                    .debug_selector(|| "admin-schedules-empty".into())
                    .p_2p5()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.background)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::SCHEDULE_EMPTY.get()),
            );
        } else {
            let mut list = div().flex().flex_col().gap_2();
            for sched in &schedules {
                let row_id = format!("admin-schedule-row-{}", sched.id);
                let toggle_id = format!("admin-schedule-toggle-{}", sched.id);
                let status_id = format!("admin-schedule-status-{}", sched.id);
                let run_now_id = format!("admin-schedule-run-now-{}", sched.id);
                let edit_id = format!("admin-schedule-edit-{}", sched.id);
                let rm_id = format!("admin-schedule-remove-{}", sched.id);
                let sched_for_edit = sched.clone();
                let id_for_toggle = sched.id.clone();
                let id_for_run = sched.id.clone();
                let id_for_rm = sched.id.clone();

                let status_badge = if sched.enabled {
                    crate::ui::badge(
                        p,
                        crate::ui::BadgeTone::Success,
                        text::SCHEDULE_ACTIVE.get(),
                    )
                } else {
                    crate::ui::badge(p, crate::ui::BadgeTone::Muted, text::SCHEDULE_PAUSED.get())
                }
                .id(SharedString::from(status_id.clone()))
                .debug_selector(move || status_id.clone());

                let mode_badge = match sched.workspace_mode {
                    WorkspaceMode::Reuse => crate::ui::badge(
                        p,
                        crate::ui::BadgeTone::Neutral,
                        text::SCHEDULE_MODE_REUSE.get(),
                    ),
                    WorkspaceMode::Isolate => crate::ui::badge(
                        p,
                        crate::ui::BadgeTone::Accent,
                        text::SCHEDULE_MODE_ISOLATE.get(),
                    ),
                };

                let cmd_str = format_command_args(&sched.command);
                let sched_agent = sched
                    .command
                    .first()
                    .and_then(|bin| {
                        std::path::Path::new(bin)
                            .file_name()
                            .and_then(|n| n.to_str())
                    })
                    .filter(|b| {
                        matches!(
                            *b,
                            "claude"
                                | "codex"
                                | "agy"
                                | "antigravity"
                                | "gemini"
                                | "cursor"
                                | "cursor-agent"
                                | "opencode"
                        )
                    });
                let mut meta_line = format!("{} · {} · {}", sched.cron, cmd_str, sched.cwd);
                if let Some(pre) = &sched.precheck {
                    meta_line.push_str(" · precheck: ");
                    meta_line.push_str(pre);
                }

                list = list.child(
                    div()
                        .id(SharedString::from(row_id.clone()))
                        .debug_selector(move || row_id.clone())
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2p5()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .border_1()
                        .border_color(p.border)
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
                                        .flex_wrap()
                                        .items_center()
                                        .gap_2()
                                        .children(sched_agent.map(|ag| {
                                            crate::ui::icons::agent_brand_icon(
                                                ag,
                                                crate::ui::ICON_MD,
                                                p.foreground,
                                            )
                                        }))
                                        .child(
                                            div()
                                                .text_size(crate::ui::TEXT_SM)
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(sched.id.clone()),
                                        )
                                        .child(status_badge)
                                        .child(mode_badge),
                                )
                                .child(
                                    div()
                                        .font_family(crate::ui::FONT_MONO)
                                        .text_size(crate::ui::TEXT_SM)
                                        .text_color(p.muted_foreground)
                                        .truncate()
                                        .child(meta_line),
                                ),
                        )
                        .child({
                            let view = cx.entity();
                            layout::controls()
                                .flex_wrap()
                                .child(
                                    layout::switch(
                                        p,
                                        toggle_id.clone(),
                                        sched.enabled,
                                        false,
                                        move |_, _, cx| {
                                            view.update(cx, |this, cx| {
                                                this.toggle_schedule_enabled(&id_for_toggle, cx);
                                            });
                                        },
                                    )
                                    .aria_label(format!(
                                        "{} · {}",
                                        text::SCHEDULES_TITLE.get(),
                                        sched.id
                                    ))
                                    .debug_selector(move || toggle_id.clone()),
                                )
                                .child(
                                    layout::primary(
                                        gpui::ElementId::Name(run_now_id.clone().into()),
                                        text::SCHEDULE_RUN_NOW.get(),
                                    )
                                    .debug_selector(move || run_now_id.clone())
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.trigger_schedule_run_now(&id_for_run, cx);
                                        },
                                    )),
                                )
                                .child(
                                    layout::secondary(
                                        gpui::ElementId::Name(edit_id.clone().into()),
                                        text::SCHEDULE_EDIT.get(),
                                    )
                                    .debug_selector(move || edit_id.clone())
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            this.open_schedule_modal(
                                                Some(&sched_for_edit),
                                                window,
                                                cx,
                                            );
                                        },
                                    )),
                                )
                                .child(
                                    layout::ghost(
                                        gpui::ElementId::Name(rm_id.clone().into()),
                                        text::SCHEDULE_REMOVE.get(),
                                    )
                                    .debug_selector(move || rm_id.clone())
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.remove_schedule(&id_for_rm, cx);
                                        },
                                    )),
                                )
                        }),
                );
            }
            card = card.child(list);
        }

        // 최근 실행 기록(Runs) 섹션
        let mut runs_col = div()
            .id("admin-schedule-runs")
            .debug_selector(|| "admin-schedule-runs".into())
            .flex()
            .flex_col()
            .gap_1p5()
            .pt_2()
            .border_t_1()
            .border_color(p.border)
            .child(
                div()
                    .text_size(crate::ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.muted_foreground)
                    .child(text::SCHEDULE_RUNS_TITLE.get()),
            );

        if runs.is_empty() {
            runs_col = runs_col.child(
                div()
                    .id("admin-schedule-runs-empty")
                    .debug_selector(|| "admin-schedule-runs-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::SCHEDULE_RUNS_EMPTY.get()),
            );
        } else {
            for (idx, run) in runs.iter().take(15).enumerate() {
                let (tone, badge_label) = run_outcome_badge_spec(run);
                let badge_id = format!("admin-schedule-run-badge-{idx}");
                let log_btn_id = format!("admin-schedule-run-log-{idx}");
                let row_id = format!("admin-schedule-run-row-{idx}");
                let run_for_row = run.clone();
                let run_for_btn = run.clone();

                let mut summary = format!("{} · {}", run.id, format_run_minute(run.minute));
                if let Some(sid) = run.session {
                    summary.push_str(" · ");
                    summary
                        .push_str(&text::SCHEDULE_RUN_SESSION.format(&[("id", &sid.to_string())]));
                }
                if let Some(cwd) = &run.cwd {
                    summary.push_str(" · ");
                    summary.push_str(cwd);
                }
                if let Some(st) = run.status {
                    summary.push_str(" · ");
                    summary.push_str(&text::SCHEDULE_RUN_EXIT.format(&[("code", &st.to_string())]));
                }
                if let Some(err) = &run.error {
                    summary.push_str(" · ");
                    summary.push_str(err);
                }

                runs_col = runs_col.child(
                    div()
                        .id(SharedString::from(row_id.clone()))
                        .debug_selector(move || row_id.clone())
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .hover(|d| d.bg(p.accent))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_schedule_run_log(&run_for_row, cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    crate::ui::badge(p, tone, badge_label)
                                        .id(SharedString::from(badge_id.clone()))
                                        .debug_selector(move || badge_id.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .font_family(crate::ui::FONT_MONO)
                                        .text_size(crate::ui::TEXT_SM)
                                        .truncate()
                                        .child(summary),
                                ),
                        )
                        .child(
                            layout::secondary(
                                gpui::ElementId::Name(log_btn_id.clone().into()),
                                text::SCHEDULE_VIEW_LOG.get(),
                            )
                            .debug_selector(move || log_btn_id.clone())
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.open_schedule_run_log(&run_for_btn, cx);
                                },
                            )),
                        ),
                );
            }
        }
        card = card.child(runs_col);

        // 실행 상세 로그 뷰어
        if let Some(log) = &self.schedule_log {
            let (tone, badge_label) = run_phase_badge_spec(&log.phase);
            let title = text::SCHEDULE_LOG_TITLE.format(&[
                ("id", &log.id),
                ("datetime", &format_run_minute(log.minute)),
            ]);
            let mut log_box = div()
                .id("admin-schedule-log-viewer")
                .debug_selector(|| "admin-schedule-log-viewer".into())
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded(crate::ui::RADIUS_SM)
                .bg(p.background)
                .border_1()
                .border_color(p.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(crate::ui::TEXT_SM)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .child(crate::ui::badge(p, tone, badge_label)),
                        )
                        .child(
                            layout::ghost(
                                "admin-schedule-log-close",
                                text::SCHEDULE_CLOSE_LOG.get(),
                            )
                            .debug_selector(|| "admin-schedule-log-close".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_schedule_run_log(cx);
                            })),
                        ),
                );
            if let Some(cwd) = &log.cwd {
                log_box = log_box.child(
                    div()
                        .id("admin-schedule-log-cwd")
                        .debug_selector(|| "admin-schedule-log-cwd".into())
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(p.muted_foreground)
                        .child(cwd.clone()),
                );
            }
            log_box = log_box.child(
                div()
                    .id("admin-schedule-log-content")
                    .debug_selector(|| "admin-schedule-log-content".into())
                    .max_h(gpui::px(280.))
                    .min_h_0()
                    .overflow_y_scroll()
                    .whitespace_normal()
                    .p_2()
                    .rounded(crate::ui::RADIUS_XS)
                    .bg(p.card)
                    .border_1()
                    .border_color(p.border)
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_SM)
                    .children(
                        log.output
                            .lines()
                            .map(|line| div().min_h(crate::ui::TEXT_SM).child(line.to_owned())),
                    ),
            );
            card = card.child(log_box);
        }

        card
    }

    pub(super) fn render_schedule_modal(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        self.render_automation_editor(window, cx)
    }
}

fn parse_optional_number<T: std::str::FromStr>(raw: &str) -> Result<Option<T>, T::Err> {
    if raw.trim().is_empty() {
        Ok(None)
    } else {
        raw.trim().parse().map(Some)
    }
}

#[cfg(test)]
mod command_path_tests {
    use super::*;

    #[test]
    fn windows_command_paths_keep_backslashes_and_apostrophes() {
        for raw in [
            r"C:\tools\runner.exe --flag",
            r#""C:\tools\runner.exe" --flag"#,
        ] {
            assert_eq!(
                parse_command_args_for(raw, true).unwrap(),
                [r"C:\tools\runner.exe", "--flag"]
            );
        }
        assert_eq!(
            parse_command_args_for(r#""\\server\share\tool.exe" "C:\temp\new\report""#, true)
                .unwrap(),
            [r"\\server\share\tool.exe", r"C:\temp\new\report"]
        );
        assert_eq!(
            parse_command_args_for("powershell -Command 'it''s literal'", true).unwrap(),
            ["powershell", "-Command", "it's literal"]
        );
    }

    #[test]
    fn posix_command_and_json_argv_still_preserve_arguments() {
        assert_eq!(
            parse_command_args_for(r"echo hello\ world", false).unwrap(),
            ["echo", "hello world"]
        );
        let args = vec![
            r"C:\tools\runner.exe".to_owned(),
            "a\nb\t'quoted'".to_owned(),
        ];
        let json = serde_json::to_string(&args).unwrap();
        assert_eq!(parse_command_args_for(&json, true).unwrap(), args);
        assert_eq!(parse_command_args_for(&json, false).unwrap(), args);
        assert_eq!(
            parse_command_args(&format_command_args(&args)).unwrap(),
            args
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_command_formatter_roundtrips_paths() {
        let args = vec![
            r"C:\Program Files\Krill\runner.exe".to_owned(),
            r"\\server\share\O'Brien\report.txt".to_owned(),
            "say \"hello\"".to_owned(),
            String::new(),
        ];
        assert_eq!(
            parse_command_args(&format_command_args(&args)).unwrap(),
            args
        );
    }
}
