//! MK-101 daemon-owned scheduling. A durable claim precedes holder spawn; uncertain
//! attempts are never automatically replayed. No catch-up of past cron minutes.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::control::CaptureFormat;
use krill_proto::schedule::{
    Cron, MAX_OUTPUT_BYTES, MAX_RUNS, PREVIOUS_RUNS, RUN_NOW, RUNS, ScheduleRun as Run, Schedules,
    WorkspaceMode, take_run_now, truncate_output,
};

use crate::child::{kill_and_reap as kill_precheck, nonblocking as set_nonblocking};

use super::{Create, Peer, Program, Registry, Reply, Scope, lock};

/// Default execution timeout for schedule `precheck` shell commands.
#[cfg(not(test))]
pub const PRECHECK_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(test)]
pub const PRECHECK_TIMEOUT: Duration = Duration::from_millis(500);

const PRECHECK_STREAM_LIMIT: usize = MAX_OUTPUT_BYTES.saturating_add(64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecheckResult {
    pub passed: bool,
    pub status: Option<i32>,
    pub output: Option<String>,
    pub error: Option<String>,
}

/// Runs `/bin/sh -c <command>` in `cwd` as a precheck gate (MK-186).
/// Exit code `0` passes; any non-zero exit, timeout, or spawn error marks the run as skipped.
#[must_use]
pub fn run_precheck(cwd: &Path, command: &str) -> PrecheckResult {
    run_precheck_with_timeout(cwd, command, PRECHECK_TIMEOUT)
}

#[must_use]
pub fn run_precheck_with_timeout(cwd: &Path, command: &str, timeout: Duration) -> PrecheckResult {
    let mut child = match Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return PrecheckResult {
                passed: false,
                status: None,
                output: None,
                error: Some(format!("precheck failed: {e}")),
            };
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        kill_precheck(&mut child);
        return PrecheckResult {
            passed: false,
            status: None,
            output: None,
            error: Some("precheck failed: missing stdout pipe".into()),
        };
    };
    let Some(mut stderr) = child.stderr.take() else {
        kill_precheck(&mut child);
        return PrecheckResult {
            passed: false,
            status: None,
            output: None,
            error: Some("precheck failed: missing stderr pipe".into()),
        };
    };
    if let Err(e) = set_nonblocking(&stdout).and_then(|()| set_nonblocking(&stderr)) {
        kill_precheck(&mut child);
        return PrecheckResult {
            passed: false,
            status: None,
            output: None,
            error: Some(format!("precheck failed: {e}")),
        };
    }

    let mut out_bytes = Vec::new();
    let mut err_bytes = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        if let Err(e) = drain_pipe(&mut stdout, &mut out_bytes, PRECHECK_STREAM_LIMIT, deadline)
            .and_then(|()| drain_pipe(&mut stderr, &mut err_bytes, PRECHECK_STREAM_LIMIT, deadline))
        {
            kill_precheck(&mut child);
            return PrecheckResult {
                passed: false,
                status: None,
                output: combine_precheck_output(&out_bytes, &err_bytes),
                error: Some(format!("precheck failed: {e}")),
            };
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let post_deadline = Instant::now() + Duration::from_millis(50);
                drain_pipe(
                    &mut stdout,
                    &mut out_bytes,
                    PRECHECK_STREAM_LIMIT,
                    post_deadline,
                )
                .ok();
                drain_pipe(
                    &mut stderr,
                    &mut err_bytes,
                    PRECHECK_STREAM_LIMIT,
                    post_deadline,
                )
                .ok();
                kill_precheck(&mut child);
                let code = status.code();
                let passed = status.success();
                return PrecheckResult {
                    passed,
                    status: code,
                    output: combine_precheck_output(&out_bytes, &err_bytes),
                    error: (!passed)
                        .then(|| format!("precheck exited with status {}", code.unwrap_or(-1))),
                };
            }
            Ok(None) => {}
            Err(e) => {
                kill_precheck(&mut child);
                return PrecheckResult {
                    passed: false,
                    status: None,
                    output: combine_precheck_output(&out_bytes, &err_bytes),
                    error: Some(format!("precheck failed: {e}")),
                };
            }
        }
        if Instant::now() >= deadline {
            kill_precheck(&mut child);
            return PrecheckResult {
                passed: false,
                status: None,
                output: combine_precheck_output(&out_bytes, &err_bytes),
                error: Some("precheck timed out".into()),
            };
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn combine_precheck_output(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let mut combined = String::from_utf8_lossy(stdout).into_owned();
    let err = String::from_utf8_lossy(stderr);
    if !err.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&err);
    }
    let out = truncate_output(&combined);
    (!out.is_empty()).then_some(out)
}

fn drain_pipe(
    pipe: &mut impl io::Read,
    dst: &mut Vec<u8>,
    limit: usize,
    deadline: Instant,
) -> io::Result<()> {
    let mut discard = [0_u8; 4096];
    loop {
        let remaining = u64::try_from(limit.saturating_sub(dst.len())).unwrap_or(0);
        let res = if remaining > 0 {
            pipe.by_ref().take(remaining).read_to_end(dst)
        } else {
            pipe.read(&mut discard)
        };
        match res {
            Ok(0) => return Ok(()),
            Ok(_) => {
                if Instant::now() >= deadline {
                    return Ok(());
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        }
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
        let cron_due = last.is_none_or(|previous| previous < minute);
        *last = Some(minute);
        let schedules = Schedules::load(dir)?;
        if schedules.schedule.is_empty() && !dir.join(RUNS).exists() {
            if dir.join(RUN_NOW).exists() {
                take_run_now(dir)?;
            }
            return Ok(());
        }
        let mut journal = Journal::open(dir)?;
        let forced_ids = take_run_now(dir)?;
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
                .chain((!entry.prompt.is_empty()).then_some(entry.prompt))
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
                valid = valid.saturating_add(u64::try_from(line.len()).map_err(io::Error::other)?);
                let run: Run = match serde_json::from_slice(line) {
                    Ok(run) => run,
                    Err(error) => {
                        tracing::warn!(
                            file = name,
                            ?error,
                            "skipping corrupted schedule journal line"
                        );
                        continue;
                    }
                };
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
