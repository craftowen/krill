// Automation design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
// Shared schedule/preset and automation control behavior; independently implemented for Krill.
//! MK-101 file configuration, shared by the CLI and daemon; no IPC extension.
//! Cron uses five UTC fields (including weekday names): minute, hour, day, month, weekday (Sunday 0/7).

use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write as _};
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE: &str = "schedules.toml";
pub const RUNS: &str = "schedule-runs.jsonl";
pub const PREVIOUS_RUNS: &str = "schedule-runs.jsonl.1";
pub const RUN_NOW: &str = "schedule-run-now";
pub const MAX_FILE: u64 = 256 * 1024;
pub const MAX_RUNS: u64 = 1024 * 1024;
pub const MAX_SCHEDULES: usize = 64;
pub const MAX_OUTPUT_BYTES: usize = 8192;
pub const MAX_NAME: usize = 128;
pub const MAX_EXTRA_AGENT_ARGS: usize = 4_096;
pub const MAX_BASE_BRANCH: usize = 256;
pub const DEFAULT_PRECHECK_TIMEOUT_SECS: u64 = 60;
pub const MAX_PRECHECK_TIMEOUT_SECS: u64 = 600;
pub const DEFAULT_MISSED_RUN_GRACE_MINUTES: u32 = 720;
pub const MAX_MISSED_RUN_GRACE_MINUTES: u32 = 10_080;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    /// Reuse existing `cwd` worktree.
    #[default]
    #[serde(alias = "existing", alias = "reuse_worktree")]
    Reuse,
    /// Automatically create a new isolated git worktree per run.
    #[serde(
        alias = "new_worktree",
        alias = "isolated",
        alias = "create",
        alias = "new_per_run"
    )]
    Isolate,
}

impl WorkspaceMode {
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde skip_serializing_if requires &T signature"
    )]
    pub const fn is_reuse(&self) -> bool {
        matches!(self, Self::Reuse)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedules {
    #[serde(default)]
    pub schedule: Vec<Schedule>,
}

const fn default_enabled() -> bool {
    true
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires &T signature"
)]
const fn is_enabled(enabled: &bool) -> bool {
    *enabled
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub cron: String,
    pub cwd: String,
    /// Executable and fixed arguments. The prompt is appended as one literal argument when non-empty.
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_agent_args: Option<String>,
    #[serde(default)]
    pub prompt: String,
    /// Optional shell gate command (`/bin/sh -c <precheck>` in `cwd`). Non-zero exit records `skipped`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precheck: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precheck_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub reuse_session: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed_run_grace_minutes: Option<u32>,
    /// Workspace execution mode (`reuse` existing `cwd` vs `isolate` in a newly created worktree).
    #[serde(default, skip_serializing_if = "WorkspaceMode::is_reuse")]
    pub workspace_mode: WorkspaceMode,
    /// Whether automatic cron triggers are active for this schedule (default `true`).
    #[serde(default = "default_enabled", skip_serializing_if = "is_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRun {
    pub id: String,
    pub minute: u64,
    /// `started` means claimed, `skipped` means precheck exited non-zero,
    /// `spawned` means holder accepted the process, `finished` means exited, `failed` means launch error.
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timed_out: Option<bool>,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde callback signature"
)]
const fn is_false(v: &bool) -> bool {
    !*v
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: None,
            cron: String::new(),
            cwd: String::new(),
            command: Vec::new(),
            agent: None,
            extra_agent_args: None,
            prompt: String::new(),
            precheck: None,
            precheck_timeout_secs: None,
            workspace_mode: WorkspaceMode::Reuse,
            base_branch: None,
            reuse_session: false,
            missed_run_grace_minutes: None,
            enabled: true,
        }
    }
}

impl Schedule {
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.id)
    }

    pub fn resolved_agent(&self) -> Option<&str> {
        if let Some(agent) = self
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(agent);
        }
        match self.command.first()?.rsplit('/').next()? {
            "claude" => Some("claude"),
            "codex" => Some("codex"),
            "agy" | "antigravity" => Some("agy"),
            "opencode" => Some("opencode"),
            "cursor" | "cursor-agent" => Some("cursor"),
            "gemini" => Some("gemini"),
            _ => None,
        }
    }

    pub fn effective_precheck_timeout_secs(&self) -> u64 {
        self.precheck_timeout_secs
            .unwrap_or(DEFAULT_PRECHECK_TIMEOUT_SECS)
            .clamp(1, MAX_PRECHECK_TIMEOUT_SECS)
    }

    pub fn effective_grace_minutes(&self) -> u32 {
        self.missed_run_grace_minutes
            .unwrap_or(0)
            .min(MAX_MISSED_RUN_GRACE_MINUTES)
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(invalid("invalid schedule ID"));
        }
        Cron::parse(&self.cron)?;
        if !Path::new(&self.cwd).is_absolute()
            || self.cwd.contains('\0')
            || self.command.is_empty()
            || self.command.first().is_none_or(String::is_empty)
            || self.command.len() > 128
            || self
                .command
                .iter()
                .any(|s| s.contains('\0') || s.len() > 8192)
            || self.prompt.contains('\0')
            || self.prompt.len() > 32 * 1024
            || self
                .precheck
                .as_ref()
                .is_some_and(|s| s.trim().is_empty() || s.contains('\0') || s.len() > 8192)
        {
            return Err(invalid("invalid schedule cwd, command or prompt"));
        }
        if self
            .name
            .as_ref()
            .is_some_and(|s| s.len() > MAX_NAME || s.chars().any(char::is_control))
        {
            return Err(invalid("invalid schedule name"));
        }
        if self.agent.as_ref().is_some_and(|s| {
            s.trim().is_empty()
                || s.len() > 64
                || s.chars().any(|c| c.is_control() || c.is_whitespace())
        }) {
            return Err(invalid("invalid schedule agent"));
        }
        if let Some(extra) = self
            .extra_agent_args
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            if extra.len() > MAX_EXTRA_AGENT_ARGS || extra.chars().any(char::is_control) {
                return Err(invalid("invalid extra_agent_args"));
            }
            let tokens = tokenize_command_line(extra)?;
            if tokens.first().is_none_or(|s| !s.starts_with('-'))
                || tokens.iter().any(|s| s == "--")
            {
                return Err(invalid(
                    "extra_agent_args requires flags and cannot contain --",
                ));
            }
            if self.reuse_session {
                return Err(invalid(
                    "extra_agent_args requires a fresh session (reuse_session = false)",
                ));
            }
        }
        if self.reuse_session && self.workspace_mode == WorkspaceMode::Isolate {
            return Err(invalid(
                "reuse_session cannot be enabled when workspace_mode is isolate",
            ));
        }
        if self
            .precheck_timeout_secs
            .is_some_and(|n| n == 0 || n > MAX_PRECHECK_TIMEOUT_SECS)
        {
            return Err(invalid(
                "precheck timeout must be between 1 and 600 seconds",
            ));
        }
        if self
            .missed_run_grace_minutes
            .is_some_and(|n| n > MAX_MISSED_RUN_GRACE_MINUTES)
        {
            return Err(invalid("missed run grace exceeds maximum"));
        }
        if self
            .base_branch
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .is_some_and(|s| {
                s.len() > MAX_BASE_BRANCH
                    || s.trim().starts_with('-')
                    || s.chars().any(|c| c.is_control() || c.is_whitespace())
            })
        {
            return Err(invalid("invalid base branch"));
        }
        Ok(())
    }
}

impl Schedules {
    pub fn parse(text: &str) -> io::Result<Self> {
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_FILE {
            return Err(invalid("schedule file too large"));
        }
        let value: Self = toml::from_str(text).map_err(invalid)?;
        if value.schedule.len() > MAX_SCHEDULES {
            return Err(invalid("too many schedules"));
        }
        let mut ids = HashSet::new();
        for entry in &value.schedule {
            if entry.id.is_empty()
                || entry.id.len() > 64
                || !entry
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || !ids.insert(&entry.id)
            {
                return Err(invalid(
                    "schedule IDs must be unique ASCII letters, digits, _ or -",
                ));
            }
            entry.validate()?;
        }
        Ok(value)
    }

    pub fn load(dir: &Path) -> io::Result<Self> {
        match File::open(dir.join(FILE)) {
            Ok(file) => {
                let mut text = String::new();
                file.take(MAX_FILE.saturating_add(1))
                    .read_to_string(&mut text)?;
                Self::parse(&text)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn exclusive_edit(dir: &Path) -> io::Result<File> {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(dir.join("schedules-edit.lock"))?;
        let mut attempts = 0_u32;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(fs::TryLockError::WouldBlock) if attempts < 500 => {
                    attempts = attempts.saturating_add(1);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(e) => return Err(io::Error::other(e)),
            }
        }
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        let text = toml::to_string(self).map_err(io::Error::other)?;
        Self::parse(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let tmp = dir.join("schedules.toml.tmp");
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_data()?;
            fs::rename(&tmp, dir.join(FILE))
        })();
        if result.is_err() {
            fs::remove_file(tmp).ok();
        }
        result
    }
}

#[must_use]
pub fn truncate_output(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= MAX_OUTPUT_BYTES {
        trimmed.to_owned()
    } else {
        let end = trimmed.floor_char_boundary(MAX_OUTPUT_BYTES);
        trimmed.get(..end).unwrap_or(trimmed).to_owned()
    }
}

pub fn load_runs(dir: &Path) -> io::Result<Vec<ScheduleRun>> {
    let mut out = Vec::new();
    for name in [PREVIOUS_RUNS, RUNS] {
        let file = match File::open(dir.join(name)) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        file.take(MAX_RUNS.saturating_add(4096))
            .read_to_end(&mut bytes)?;
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            if line.last() != Some(&b'\n') {
                break;
            }
            if let Ok(run) = serde_json::from_slice::<ScheduleRun>(line) {
                out.push(run);
            }
        }
    }
    Ok(out)
}

/// Coalesces raw journal lines (`started` -> `skipped`/`spawned`/`failed` -> `finished`)
/// into one summary entry per run, ordered newest-first.
pub fn load_coalesced_runs(dir: &Path) -> io::Result<Vec<ScheduleRun>> {
    Ok(coalesce_runs(&load_runs(dir)?))
}

pub fn coalesce_runs(raw: &[ScheduleRun]) -> Vec<ScheduleRun> {
    let mut entries: Vec<ScheduleRun> = Vec::new();
    let mut sessions: HashMap<u64, BTreeSet<usize>> = HashMap::new();
    let mut pending: HashMap<(String, u64), BTreeSet<usize>> = HashMap::new();
    let mut ordinals: HashMap<(String, u64), usize> = HashMap::new();
    for r in raw.iter().cloned() {
        let numbered_match = r
            .run_number
            .and_then(|number| ordinals.get(&(r.id.clone(), number)).copied());
        let session_match = r
            .session
            .and_then(|id| sessions.get(&id))
            .and_then(|set| set.last().copied());
        let key = (r.id.clone(), r.minute);
        let key_match = pending.get(&key).and_then(|set| set.last().copied());
        let target = if r.phase == "started" {
            None
        } else if r.run_number.is_some() {
            numbered_match
        } else {
            session_match.into_iter().chain(key_match).max()
        };
        let index = target.unwrap_or(entries.len());
        if let Some(existing) = target.and_then(|index| entries.get_mut(index)) {
            if let Some(session) = existing.session
                && let Some(set) = sessions.get_mut(&session)
            {
                set.remove(&index);
            }
            if let Some(set) = pending.get_mut(&(existing.id.clone(), existing.minute)) {
                set.remove(&index);
            }
            existing.phase = r.phase;
            if r.session.is_some() {
                existing.session = r.session;
            }
            if r.error.is_some() {
                existing.error = r.error;
            }
            if r.status.is_some() {
                existing.status = r.status;
            }
            if r.cwd.is_some() {
                existing.cwd = r.cwd;
            }
            if r.output.is_some() {
                existing.output = r.output;
            }
            if r.trigger.is_some() {
                existing.trigger = r.trigger;
            }
            if r.run_number.is_some() {
                existing.run_number = r.run_number;
            }
            if r.duration_ms.is_some() {
                existing.duration_ms = r.duration_ms;
            }
            if r.timed_out.is_some() {
                existing.timed_out = r.timed_out;
            }
        } else {
            entries.push(r);
        }
        if let Some(entry) = entries.get(index) {
            if let Some(number) = entry.run_number {
                ordinals.insert((entry.id.clone(), number), index);
            }
            if let Some(session) = entry.session {
                sessions.entry(session).or_default().insert(index);
            }
            if matches!(entry.phase.as_str(), "started" | "spawned") {
                pending
                    .entry((entry.id.clone(), entry.minute))
                    .or_default()
                    .insert(index);
            }
        }
    }
    entries.reverse();
    entries
}

pub fn request_run_now(dir: &Path, id: &str) -> io::Result<()> {
    let _guard = Schedules::exclusive_edit(dir)?;
    let schedules = Schedules::load(dir)?;
    if !schedules.schedule.iter().any(|s| s.id == id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown schedule ID",
        ));
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join(RUN_NOW))?;
    file.write_all(format!("{id}\n").as_bytes())?;
    file.sync_data()
}

pub fn take_run_now(dir: &Path) -> io::Result<Vec<String>> {
    let _guard = Schedules::exclusive_edit(dir)?;
    let path = dir.join(RUN_NOW);
    let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut text = String::new();
    (&mut file)
        .take(MAX_FILE.saturating_add(1))
        .read_to_string(&mut text)?;
    file.set_len(0)?;
    drop(file);
    fs::remove_file(&path).ok();
    let mut out = Vec::new();
    for line in text.lines() {
        let id = line.trim();
        if !id.is_empty() && !out.iter().any(|existing| existing == id) {
            out.push(id.to_owned());
        }
    }
    Ok(out)
}

/// Numeric cron with lists, inclusive ranges, and steps (`*/5`, `1-10/2`).
#[derive(Debug, Clone)]
pub struct Cron {
    fields: [u64; 5],
    day_wildcard: bool,
    weekday_wildcard: bool,
}

impl Cron {
    pub fn parse(text: &str) -> io::Result<Self> {
        let converted;
        let text = if text.contains('=') || text.trim().starts_with("RRULE:") {
            converted = rrule_to_cron(text)?;
            &converted
        } else {
            text
        };
        let parts: Vec<_> = text.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = parts.as_slice() else {
            return Err(invalid("cron requires five UTC fields"));
        };
        Ok(Self {
            fields: [
                field(minute, 0, 59)?,
                field(hour, 0, 23)?,
                field(day, 1, 31)?,
                field(month, 1, 12)?,
                field(&normalize_weekdays(weekday), 0, 7)?,
            ],
            day_wildcard: day.split(',').any(|s| s.starts_with('*')),
            weekday_wildcard: weekday.split(',').any(|s| s.starts_with('*')),
        })
    }

    /// Calendar values are minute, hour, day of month, month, weekday (Sunday 0).
    /// Restricted day-of-month and weekday fields use logical OR, as in traditional cron.
    pub fn matches(&self, values: [u32; 5]) -> bool {
        let [minute, hour, day, month, weekday] = self.fields;
        let [m, h, d, mo, w] = values;
        let has = |bits: u64, n| bits.checked_shr(n).is_some_and(|v| v & 1 != 0);
        let dom = has(day, d);
        let dow = has(weekday, w) || (w == 0 && has(weekday, 7));
        let days = if self.day_wildcard || self.weekday_wildcard {
            dom && dow
        } else {
            dom || dow
        };
        has(minute, m) && has(hour, h) && has(month, mo) && days
    }
}

fn field(text: &str, min: u32, max: u32) -> io::Result<u64> {
    let mut bits = 0_u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (range, step.parse::<u32>().map_err(invalid)?),
            None => (part, 1),
        };
        if step == 0 || step > max.saturating_sub(min).saturating_add(1) {
            return Err(invalid("invalid cron step"));
        }
        let (start, end) = if range == "*" {
            (min, max)
        } else if let Some((start, end)) = range.split_once('-') {
            (
                start.parse::<u32>().map_err(invalid)?,
                end.parse::<u32>().map_err(invalid)?,
            )
        } else {
            let n = range.parse::<u32>().map_err(invalid)?;
            (n, if part.contains('/') { max } else { n })
        };
        if start < min || end > max || start > end {
            return Err(invalid("cron value out of range"));
        }
        for n in start..=end {
            if n.checked_sub(start).and_then(|v| v.checked_rem(step)) == Some(0) {
                bits |= 1_u64
                    .checked_shl(n)
                    .ok_or_else(|| invalid("invalid cron value"))?;
            }
        }
    }
    Ok(bits)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulePreset {
    Hourly,
    Daily,
    Weekdays,
    Weekly,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedSchedule {
    pub preset: SchedulePreset,
    pub hour: u8,
    pub minute: u8,
    pub day_of_week: u8,
    pub cron: String,
}

fn normalize_weekdays(text: &str) -> String {
    let mut text = text.to_ascii_uppercase();
    for (day, n) in [
        ("SUN", "0"),
        ("MON", "1"),
        ("TUE", "2"),
        ("WED", "3"),
        ("THU", "4"),
        ("FRI", "5"),
        ("SAT", "6"),
    ] {
        text = text.replace(day, n);
    }
    text
}

fn rrule_to_cron(expr: &str) -> io::Result<String> {
    let text = expr.trim().strip_prefix("RRULE:").unwrap_or(expr.trim());
    let mut fields = HashMap::new();
    for part in text.split(';') {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| invalid("invalid RRULE"))?;
        let key = key.trim().to_ascii_uppercase();
        if !matches!(
            key.as_str(),
            "FREQ" | "INTERVAL" | "BYMINUTE" | "BYHOUR" | "BYDAY"
        ) || fields
            .insert(key, value.trim().to_ascii_uppercase())
            .is_some()
        {
            return Err(invalid("unsupported or duplicate RRULE field"));
        }
    }
    let value = |key: &str, default: &str| {
        fields
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.to_owned())
    };
    let minute = value("BYMINUTE", "0");
    let hour = value("BYHOUR", "9");
    let interval = value("INTERVAL", "1").parse::<u32>().map_err(invalid)?;
    if interval == 0 {
        return Err(invalid("invalid RRULE interval"));
    }
    let cron = match value("FREQ", "").as_str() {
        "MINUTELY"
            if !fields.contains_key("BYMINUTE")
                && !fields.contains_key("BYHOUR")
                && !fields.contains_key("BYDAY") =>
        {
            if interval == 60 {
                "0 * * * *".to_owned()
            } else if 60_u32.checked_rem(interval) == Some(0) {
                format!("*/{interval} * * * *")
            } else {
                return Err(invalid("unsupported RRULE minutely interval"));
            }
        }
        "HOURLY" if !fields.contains_key("BYHOUR") && !fields.contains_key("BYDAY") => {
            if interval == 1 {
                format!("{minute} * * * *")
            } else if 24_u32.checked_rem(interval) == Some(0) {
                format!("{minute} */{interval} * * *")
            } else {
                return Err(invalid("unsupported RRULE hourly interval"));
            }
        }
        "DAILY" if interval == 1 && !fields.contains_key("BYDAY") => {
            format!("{minute} {hour} * * *")
        }
        "WEEKLY" if interval == 1 => {
            let mut days = Vec::new();
            for day in value("BYDAY", "MO").split(',') {
                days.push(match day {
                    "SU" => "0",
                    "MO" => "1",
                    "TU" => "2",
                    "WE" => "3",
                    "TH" => "4",
                    "FR" => "5",
                    "SA" => "6",
                    _ => return Err(invalid("invalid RRULE weekday")),
                });
            }
            format!("{minute} {hour} * * {}", days.join(","))
        }
        _ => return Err(invalid("unsupported RRULE frequency or interval")),
    };
    Cron::parse(&cron)?;
    Ok(cron)
}

pub fn parse_schedule_to_cron(expr: &str) -> io::Result<String> {
    if expr.contains('=') || expr.trim().starts_with("RRULE:") {
        return rrule_to_cron(expr);
    }
    Cron::parse(expr)?;
    Ok(expr.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub fn build_preset_cron(
    preset: SchedulePreset,
    hour: u8,
    minute: u8,
    day_of_week: u8,
) -> io::Result<String> {
    if hour > 23 || minute > 59 || day_of_week > 6 {
        return Err(invalid("invalid preset time or weekday"));
    }
    Ok(match preset {
        SchedulePreset::Hourly => format!("{minute} * * * *"),
        SchedulePreset::Daily => format!("{minute} {hour} * * *"),
        SchedulePreset::Weekdays => format!("{minute} {hour} * * 1-5"),
        SchedulePreset::Weekly => format!("{minute} {hour} * * {day_of_week}"),
        SchedulePreset::Custom => {
            return Err(invalid(
                "custom preset requires an explicit cron expression",
            ));
        }
    })
}

pub fn classify_schedule(expr: &str) -> ClassifiedSchedule {
    let mut result = ClassifiedSchedule {
        preset: SchedulePreset::Custom,
        hour: 9,
        minute: 0,
        day_of_week: 1,
        cron: expr.trim().to_owned(),
    };
    let Ok(cron) = parse_schedule_to_cron(expr) else {
        return result;
    };
    let parts = cron.split_whitespace().collect::<Vec<_>>();
    if let [m, h, "*", "*", d] = parts.as_slice()
        && let Ok(minute) = m.parse::<u8>()
    {
        let weekday = normalize_weekdays(d);
        let preset = if *h == "*" && *d == "*" {
            Some(SchedulePreset::Hourly)
        } else if let Ok(hour) = h.parse::<u8>() {
            result.hour = hour;
            match weekday.as_str() {
                "*" => Some(SchedulePreset::Daily),
                "1-5" | "1,2,3,4,5" => Some(SchedulePreset::Weekdays),
                day => day.parse::<u8>().ok().map(|day| {
                    result.day_of_week = day.checked_rem(7).unwrap_or(0);
                    SchedulePreset::Weekly
                }),
            }
        } else {
            None
        };
        if let Some(preset) = preset {
            result.preset = preset;
            result.minute = minute;
            result.cron = cron;
            return result;
        }
    }
    result.hour = 9;
    result
}

pub fn format_schedule_summary(expr: &str, korean: bool) -> String {
    let c = classify_schedule(expr);
    let (h, m) = (c.hour, c.minute);
    match (c.preset, korean) {
        (SchedulePreset::Hourly, true) => format!("매시간 :{m:02} (UTC)"),
        (SchedulePreset::Hourly, false) => format!("Every hour at :{m:02} (UTC)"),
        (SchedulePreset::Daily, true) => format!("매일 {h:02}:{m:02} (UTC)"),
        (SchedulePreset::Daily, false) => format!("Daily at {h:02}:{m:02} (UTC)"),
        (SchedulePreset::Weekdays, true) => format!("평일 {h:02}:{m:02} (UTC)"),
        (SchedulePreset::Weekdays, false) => format!("Weekdays at {h:02}:{m:02} (UTC)"),
        (SchedulePreset::Weekly, ko) => {
            let days = if ko {
                [
                    "일요일",
                    "월요일",
                    "화요일",
                    "수요일",
                    "목요일",
                    "금요일",
                    "토요일",
                ]
            } else {
                ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
            };
            let day = days.get(usize::from(c.day_of_week)).copied().unwrap_or("");
            if ko {
                format!("매주 {day} {h:02}:{m:02} (UTC)")
            } else {
                format!("Weekly on {day} at {h:02}:{m:02} (UTC)")
            }
        }
        (SchedulePreset::Custom, _) => format!("{} (UTC)", c.cron),
    }
}

/// UTC Gregorian calendar from an epoch minute. Years beyond i32 are saturated.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Gregorian conversion intermediates are bounded by u64 minutes / 1440, within i64; divisors are nonzero"
)]
pub fn utc_calendar_from_epoch_minute(epoch_minute: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = i64::try_from(epoch_minute / 1440).unwrap_or(i64::MAX / 2);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (
        i32::try_from(year).unwrap_or(i32::MAX),
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
        u32::try_from(epoch_minute / 60 % 24).unwrap_or(0),
        u32::try_from(epoch_minute % 60).unwrap_or(0),
        u32::try_from((days + 4) % 7).unwrap_or(0),
    )
}

impl Cron {
    pub fn matches_epoch_minute(&self, epoch_minute: u64) -> bool {
        let (_, month, day, hour, minute, weekday) = utc_calendar_from_epoch_minute(epoch_minute);
        self.matches([minute, hour, day, month, weekday])
    }

    pub fn next_minute_after(&self, after_epoch_minute: u64) -> Option<u64> {
        let mut next = after_epoch_minute.checked_add(1)?;
        let end = after_epoch_minute.saturating_add(366 * 24 * 60 * 5);
        let [_, hours, _, _, _] = self.fields;
        while next <= end {
            let (_, month, day, hour, _, weekday) = utc_calendar_from_epoch_minute(next);
            // A day can be tested independently of its hour/minute restrictions.
            if !self.matches([self.first_minute(), self.first_hour(), day, month, weekday]) {
                next = next.checked_add(1440_u64.saturating_sub(next.checked_rem(1440)?))?;
            } else if hours.checked_shr(hour).unwrap_or(0) & 1 == 0 {
                next = next.checked_add(60_u64.saturating_sub(next.checked_rem(60)?))?;
            } else if self.matches_epoch_minute(next) {
                return Some(next);
            } else {
                next = next.checked_add(1)?;
            }
        }
        None
    }

    fn first_minute(&self) -> u32 {
        let [minute, _, _, _, _] = self.fields;
        minute.trailing_zeros()
    }
    fn first_hour(&self) -> u32 {
        let [_, hour, _, _, _] = self.fields;
        hour.trailing_zeros()
    }

    pub fn latest_minute_at_or_before(
        &self,
        at_epoch_minute: u64,
        max_lookback_minutes: u32,
    ) -> Option<u64> {
        let start = at_epoch_minute.saturating_sub(u64::from(max_lookback_minutes));
        (start..=at_epoch_minute)
            .rev()
            .find(|m| self.matches_epoch_minute(*m))
    }
}

pub fn slugify_schedule_id(name: &str, existing_ids: &[String]) -> String {
    let mut base = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if !base.ends_with('-') {
            base.push('-');
        }
    }
    let mut base = base.trim_matches('-').chars().take(56).collect::<String>();
    base = base.trim_end_matches('-').to_owned();
    if base.is_empty() {
        base = "automation".into();
    }
    let mut candidate = base.clone();
    let mut n = 2_u64;
    while existing_ids.contains(&candidate) {
        candidate = format!("{base}-{n}");
        n = n.saturating_add(1);
    }
    candidate
}

pub fn tokenize_command_line(raw: &str) -> io::Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for c in raw.chars() {
        if escaped {
            token.push(c);
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
        } else if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && matches!(c, '\'' | '"') {
            quote = Some(c);
            started = true;
        } else if quote.is_none() && c.is_whitespace() {
            if started {
                tokens.push(std::mem::take(&mut token));
                started = false;
            }
        } else {
            token.push(c);
            started = true;
        }
    }
    if quote.is_some() || escaped {
        return Err(invalid("unclosed quote or trailing escape"));
    }
    if started {
        tokens.push(token);
    }
    Ok(tokens)
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod indexed_run_tests {
    use super::*;
    #[test]
    fn indexed_coalescing_matches_reverse_scan_across_duplicate_minutes_and_sessions() {
        let dir = std::env::temp_dir().join(format!("krill-index-runs-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut raw = Vec::new();
        for n in 0_u64..300 {
            raw.push(ScheduleRun {
                id: format!("job-{}", n % 5),
                minute: n % 7,
                phase: ["started", "spawned", "finished", "failed", "skipped"]
                    .get(usize::try_from(n % 5).unwrap())
                    .unwrap()
                    .to_string(),
                session: (n % 3 != 0).then_some(n % 11),
                error: (n % 5 == 3).then(|| "error".into()),
                status: (n % 5 == 2).then_some(0),
                cwd: None,
                output: (n % 5 == 2).then(|| "한글 output".into()),
                ..Default::default()
            });
        }
        let mut expected: Vec<ScheduleRun> = Vec::new();
        for r in raw.iter().cloned() {
            let target = if r.phase == "started" {
                None
            } else {
                expected.iter_mut().rev().find(|e| {
                    r.session.is_some() && e.session == r.session
                        || e.id == r.id
                            && e.minute == r.minute
                            && matches!(e.phase.as_str(), "started" | "spawned")
                })
            };
            if let Some(existing) = target {
                existing.phase = r.phase;
                if r.session.is_some() {
                    existing.session = r.session;
                }
                if r.error.is_some() {
                    existing.error = r.error;
                }
                if r.status.is_some() {
                    existing.status = r.status;
                }
                if r.cwd.is_some() {
                    existing.cwd = r.cwd;
                }
                if r.output.is_some() {
                    existing.output = r.output;
                }
            } else {
                expected.push(r);
            }
        }
        expected.reverse();
        let mut file = File::create(dir.join(RUNS)).unwrap();
        for r in raw {
            serde_json::to_writer(&mut file, &r).unwrap();
            file.write_all(b"\n").unwrap();
        }
        drop(file);
        assert_eq!(load_coalesced_runs(&dir).unwrap(), expected);
        fs::remove_dir_all(dir).ok();
    }
}
