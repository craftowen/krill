//! MK-101 file configuration, shared by the CLI and daemon; no IPC extension.
//! Cron uses five numeric UTC fields: minute, hour, day, month, weekday (Sunday 0/7).

use std::collections::HashSet;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub id: String,
    pub cron: String,
    pub cwd: String,
    /// Executable and fixed arguments. The prompt is appended as one literal argument.
    pub command: Vec<String>,
    pub prompt: String,
    /// Optional shell gate command (`/bin/sh -c <precheck>` in `cwd`). Non-zero exit records `skipped`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precheck: Option<String>,
    /// Workspace execution mode (`reuse` existing `cwd` vs `isolate` in a newly created worktree).
    #[serde(default, skip_serializing_if = "WorkspaceMode::is_reuse")]
    pub workspace_mode: WorkspaceMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRun {
    pub id: String,
    pub minute: u64,
    /// `started` means claimed, `skipped` means precheck exited non-zero,
    /// `spawned` means holder accepted the process, `finished` means exited, `failed` means launch error.
    pub phase: String,
    pub session: Option<u64>,
    pub error: Option<String>,
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
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
            Cron::parse(&entry.cron)?;
            if !Path::new(&entry.cwd).is_absolute()
                || entry.cwd.contains('\0')
                || entry.command.is_empty()
                || entry.command.first().is_none_or(String::is_empty)
                || entry.command.len() > 128
                || entry
                    .command
                    .iter()
                    .any(|s| s.contains('\0') || s.len() > 8192)
                || entry.prompt.is_empty()
                || entry.prompt.contains('\0')
                || entry.prompt.len() > 32 * 1024
                || entry
                    .precheck
                    .as_ref()
                    .is_some_and(|s| s.trim().is_empty() || s.contains('\0') || s.len() > 8192)
            {
                return Err(invalid("invalid schedule cwd, command or prompt"));
            }
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
        file.try_lock().map_err(io::Error::other)?;
        Ok(file)
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
    let raw = load_runs(dir)?;
    let mut entries: Vec<ScheduleRun> = Vec::new();
    for r in raw {
        if r.phase == "started" {
            entries.push(r);
            continue;
        }
        let target = entries.iter_mut().rev().find(|e| {
            (r.session.is_some() && e.session == r.session)
                || (e.id == r.id
                    && e.minute == r.minute
                    && matches!(e.phase.as_str(), "started" | "spawned"))
        });
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
            entries.push(r);
        }
    }
    entries.reverse();
    Ok(entries)
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
    let path = dir.join(RUN_NOW);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
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
                field(weekday, 0, 7)?,
            ],
            day_wildcard: day.starts_with('*'),
            weekday_wildcard: weekday.starts_with('*'),
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
            if part.contains('/') {
                return Err(invalid("cron steps require * or a range"));
            }
            let n = range.parse::<u32>().map_err(invalid)?;
            (n, n)
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

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

#[cfg(test)]
mod tests;
