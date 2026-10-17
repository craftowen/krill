//! MK-101 daemon-owned scheduling. A durable claim precedes holder spawn; uncertain
//! attempts are never automatically replayed. No catch-up of past cron minutes.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::control::CaptureFormat;
use krill_proto::schedule::{
    Cron, MAX_RUNS, PREVIOUS_RUNS, RUN_NOW, RUNS, ScheduleRun as Run, Schedules, WorkspaceMode,
    take_run_now, truncate_output,
};

use super::{Create, Peer, Program, Registry, Reply, Scope, lock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecheckResult {
    pub passed: bool,
    pub status: Option<i32>,
    pub output: Option<String>,
    pub error: Option<String>,
}

/// Runs `/bin/sh -c <command>` in `cwd` as a precheck gate (MK-186).
/// Exit code `0` passes; any non-zero exit or spawn error marks the run as skipped.
#[must_use]
pub fn run_precheck(cwd: &Path, command: &str) -> PrecheckResult {
    match Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) => {
            let code = output.status.code();
            let passed = output.status.success();
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }
            let out = truncate_output(&combined);
            PrecheckResult {
                passed,
                status: code,
                output: (!out.is_empty()).then_some(out),
                error: (!passed)
                    .then(|| format!("precheck exited with status {}", code.unwrap_or(-1))),
            }
        }
        Err(e) => PrecheckResult {
            passed: false,
            status: None,
            output: None,
            error: Some(format!("precheck failed: {e}")),
        },
    }
}

impl Registry {
    /// Evaluate the current UTC minute at most once per daemon lifetime (plus any explicit
    /// `RUN_NOW` triggers). Restart deduplication comes from the journal under a cross-process
    /// scheduler lock. Errors leave sessions untouched and suppress this minute rather than retrying.
    pub fn poll_schedules(&self, now: SystemTime) -> io::Result<()> {
        let Some(dir) = self.dir.as_deref() else {
            return Ok(());
        };
        // Serialize against upgrades; an old daemon must not spawn during handover.
        let upgrading = lock(&self.upgrading);
        if *upgrading {
            return Ok(());
        }
        if self
            .holder
            .as_ref()
            .is_some_and(|holder| !holder.is_connected())
        {
            return Ok(());
        }
        let seconds = now
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        let minute = seconds / 60;
        let has_run_now = dir.join(RUN_NOW).exists();
        let mut last = lock(&self.schedule_minute);
        if !has_run_now && last.is_some_and(|previous| previous >= minute) {
            return Ok(());
        }
        let calendar = crate::sys::cron_calendar(seconds)?;
        let _guard = exclusive(dir)?;
        let forced_ids = take_run_now(dir)?;
        let cron_due = last.is_none_or(|previous| previous < minute);
        *last = Some(minute);
        let schedules = Schedules::load(dir)?;
        if schedules.schedule.is_empty() && forced_ids.is_empty() && !dir.join(RUNS).exists() {
            return Ok(());
        }
        let mut journal = Journal::open(dir)?;
        // Reap only processes whose exit the holder has already reported. Live
        // PTYs are never closed by scheduling. Segment output remains on disk.
        for (session, mut run) in journal.sessions.clone() {
            let id = krill_proto::SessionId(session);
            // 세션도 종료 기록도 없이 사라진 옛 실행 기록은 여기서 tombstone을 남긴다.
            // 두지 않으면 id가 재사용됐을 때 무관한 새 세션을 close한다(K-30).
            // 종료 기록이 남은 경우는 아래 수거 경로가 상태와 함께 마무리한다.
            let gone = {
                let state = lock(&self.state);
                !state.sessions.contains_key(&id) && !state.exited.contains_key(&id)
            };
            if gone {
                run.phase = "finished".into();
                journal.append(&run)?;
                continue;
            }
            let (status, captured) = {
                let state = lock(&self.state);
                // An exited root can leave background children holding the PTY.
                // Wait for reader EOF before releasing that session's fds.
                if state
                    .sessions
                    .get(&id)
                    .is_none_or(|entry| entry.session.is_closed())
                {
                    let out = state
                        .sessions
                        .get(&id)
                        .and_then(|entry| {
                            entry.session.tap().capture_cli(CaptureFormat::Plain).ok()
                        })
                        .map(|(_, bytes)| truncate_output(&String::from_utf8_lossy(&bytes)))
                        .filter(|s| !s.is_empty());
                    (state.exited.get(&id).copied(), out)
                } else {
                    (None, None)
                }
            };
            if let Some(status) = status {
                run.phase = "finished".into();
                run.status = Some(status);
                if captured.is_some() {
                    run.output = captured;
                }
                journal.append(&run)?;
                self.close(
                    &Peer {
                        pid: None,
                        scope: Scope::Full,
                    },
                    id,
                )
                .ok();
            }
        }
        for entry in schedules.schedule {
            let forced = forced_ids.iter().any(|id| id == &entry.id);
            let due = cron_due
                && Cron::parse(&entry.cron)?.matches(calendar)
                && !journal.claimed(&entry.id, minute);
            if !forced && !due {
                continue;
            }
            let mut run = Run {
                id: entry.id.clone(),
                minute,
                phase: "started".into(),
                session: None,
                error: None,
                status: None,
                cwd: None,
                output: None,
            };
            journal.append(&run)?;
            if let Some(precheck) = entry.precheck.as_deref().filter(|s| !s.trim().is_empty()) {
                let res = run_precheck(Path::new(&entry.cwd), precheck);
                if !res.passed {
                    run.phase = "skipped".into();
                    run.status = res.status;
                    run.output = res.output;
                    run.error = res.error;
                    journal.append(&run)?;
                    continue;
                }
            }
            let run_cwd = match entry.workspace_mode {
                WorkspaceMode::Reuse => PathBuf::from(&entry.cwd),
                WorkspaceMode::Isolate => {
                    let wt_name = format!("schedule-{}-{minute}", entry.id);
                    match crate::git::worktree::create_isolated(
                        Path::new(&entry.cwd),
                        &wt_name,
                        dir,
                    ) {
                        Ok(path) => {
                            run.cwd = Some(path.to_string_lossy().into_owned());
                            path
                        }
                        Err(err) => {
                            run.phase = "failed".into();
                            run.error = Some(err);
                            journal.append(&run)?;
                            continue;
                        }
                    }
                }
            };
            let mut command = entry.command.into_iter();
            let Some(program) = command.next() else {
                continue;
            };
            let args = command
                .chain(std::iter::once(entry.prompt))
                .map(String::into_bytes)
                .collect();
            let create = Create {
                account: None,
                program: Program::Custom {
                    program: program.into_bytes(),
                    args,
                },
                cwd: run_cwd.as_os_str().as_bytes().to_vec(),
                cols: 80,
                rows: 24,
            };
            let peer = Peer {
                pid: None,
                scope: Scope::Full,
            };
            if let Ok(Reply::Created { session, .. }) = self.create(&peer, create) {
                run.phase = "spawned".into();
                run.session = Some(session.0);
            } else {
                run.phase = "failed".into();
                run.error = Some("session creation failed".into());
            }
            journal.append(&run)?;
        }
        Ok(())
    }
}

fn exclusive(dir: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("scheduler.lock"))?;
    file.try_lock().map_err(io::Error::other)?;
    Ok(file)
}

struct Journal<'a> {
    dir: &'a Path,
    file: File,
    latest: HashMap<String, u64>,
    sessions: HashMap<u64, Run>,
}

impl<'a> Journal<'a> {
    fn open(dir: &'a Path) -> io::Result<Self> {
        let mut latest = HashMap::<String, u64>::new();
        let mut sessions = HashMap::new();
        for name in [PREVIOUS_RUNS, RUNS] {
            let file = match File::open(dir.join(name)) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let mut bytes = Vec::new();
            file.take(MAX_RUNS.saturating_add(4096))
                .read_to_end(&mut bytes)?;
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) >= MAX_RUNS.saturating_add(4096) {
                return Err(io::Error::other("schedule journal too large"));
            }
            let mut valid = 0_u64;
            for line in bytes.split_inclusive(|b| *b == b'\n') {
                if line.last() != Some(&b'\n') {
                    break;
                }
                let run: Run = serde_json::from_slice(line).map_err(io::Error::other)?;
                latest
                    .entry(run.id.clone())
                    .and_modify(|m| *m = (*m).max(run.minute))
                    .or_insert(run.minute);
                if let Some(session) = run.session {
                    if run.phase == "spawned" {
                        sessions.insert(session, run);
                    } else {
                        sessions.remove(&session);
                    }
                }
                valid = valid.saturating_add(u64::try_from(line.len()).map_err(io::Error::other)?);
            }
            // Discard a SIGKILL-truncated final record before the next append.
            if name == RUNS {
                OpenOptions::new()
                    .write(true)
                    .open(dir.join(name))?
                    .set_len(valid)?;
            }
        }
        let file = Self::writer(dir)?;
        Ok(Self {
            dir,
            file,
            latest,
            sessions,
        })
    }

    fn writer(dir: &Path) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(dir.join(RUNS))
    }

    fn claimed(&self, id: &str, minute: u64) -> bool {
        self.latest.get(id).is_some_and(|last| *last >= minute)
    }

    fn append(&mut self, run: &Run) -> io::Result<()> {
        if let Some(session) = run.session {
            if run.phase == "spawned" {
                self.sessions.insert(session, run.clone());
            } else {
                self.sessions.remove(&session);
            }
        }
        if self.file.metadata()?.len() >= MAX_RUNS {
            fs::rename(self.dir.join(RUNS), self.dir.join(PREVIOUS_RUNS))?;
            self.file = Self::writer(self.dir)?;
            for active in self.sessions.values() {
                if active.session != run.session {
                    let mut line = serde_json::to_vec(active).map_err(io::Error::other)?;
                    line.push(b'\n');
                    self.file.write_all(&line)?;
                }
            }
        }
        let mut bytes = serde_json::to_vec(run).map_err(io::Error::other)?;
        bytes.push(b'\n');
        self.file.write_all(&bytes)?;
        self.file.sync_data()?;
        self.latest
            .entry(run.id.clone())
            .and_modify(|minute| *minute = (*minute).max(run.minute))
            .or_insert(run.minute);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
