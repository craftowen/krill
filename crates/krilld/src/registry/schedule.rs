// Automation design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
// Shared schedule/preset and automation control behavior; independently implemented for Krill.
//! MK-101 daemon-owned scheduling. A durable claim precedes holder spawn; uncertain
//! attempts are never automatically replayed. Catch-up is opt-in and only claims the latest due occurrence.

use krill_proto::os::ffi::OsStrExt as _;
use krill_proto::os::fs::OpenOptionsExt as _;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::control::CaptureFormat;
use krill_proto::schedule::{
    Cron, MAX_OUTPUT_BYTES, MAX_PRECHECK_TIMEOUT_SECS, MAX_RUNS, PREVIOUS_RUNS, RUN_NOW, RUNS,
    ScheduleRun as Run, Schedules, WorkspaceMode, coalesce_runs, load_runs, take_run_now,
    tokenize_command_line, truncate_output,
};

use crate::child::{PipeReader, own_group, spawn};

use super::{Create, Peer, Program, Registry, Reply, Scope, lock};

/// Default execution timeout for schedule `precheck` shell commands.
#[cfg(not(test))]
pub const PRECHECK_TIMEOUT: Duration =
    Duration::from_secs(krill_proto::schedule::DEFAULT_PRECHECK_TIMEOUT_SECS);
#[cfg(test)]
pub const PRECHECK_TIMEOUT: Duration = Duration::from_millis(500);

const PRECHECK_STREAM_LIMIT: usize = MAX_OUTPUT_BYTES.saturating_add(64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecheckResult {
    pub passed: bool,
    pub status: Option<i32>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub timed_out: bool,
}

/// Runs `sh -c <command>` in `cwd` as a precheck gate (MK-186): Unix `/bin/sh`, Windows the Git for
/// Windows `sh.exe` (`git var GIT_SHELL_PATH`, daemon-03).
/// Exit code `0` passes; any non-zero exit, timeout, or spawn error marks the run as skipped.
#[must_use]
pub fn run_precheck(cwd: &Path, command: &str, timeout_override: Option<u64>) -> PrecheckResult {
    run_precheck_with_timeout(cwd, command, precheck_timeout(timeout_override))
}

/// The shell that runs precheck commands. Windows needs Git for Windows (`sh.exe`); `cmd.exe` would give the
/// same command text different semantics, so a missing Git fails the precheck explicitly instead.
#[cfg_attr(
    unix,
    expect(
        clippy::unnecessary_wraps,
        reason = "Windows resolves the Git shell and can fail"
    )
)]
fn precheck_shell() -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        Ok(PathBuf::from("/bin/sh"))
    }
    #[cfg(windows)]
    {
        let mut git = Command::new("git");
        {
            use std::os::windows::process::CommandExt as _;
            git.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = git
            .args(["var", "GIT_SHELL_PATH"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("precheck requires Git for Windows sh: {e}"),
                )
            })?;
        let mut path = out.stdout;
        while path.last().is_some_and(|b| matches!(b, b'\n' | b'\r')) {
            path.pop();
        }
        if !out.status.success() || path.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "precheck requires Git for Windows sh (git var GIT_SHELL_PATH)",
            ));
        }
        Ok(PathBuf::from(String::from_utf8_lossy(&path).into_owned()))
    }
}

#[must_use]
pub fn run_precheck_with_timeout(cwd: &Path, command: &str, timeout: Duration) -> PrecheckResult {
    let started = Instant::now();
    let failed = |error: String| PrecheckResult {
        duration_ms: elapsed_ms(started),
        timed_out: false,
        passed: false,
        status: None,
        output: None,
        error: Some(error),
    };
    let shell = match precheck_shell() {
        Ok(shell) => shell,
        Err(e) => return failed(format!("precheck failed: {e}")),
    };
    let mut process = Command::new(&shell);
    #[cfg(windows)]
    if let Some(bin) = shell.parent() {
        // Git's `cmd` PATH entry does not expose sh utilities such as sleep and grep.
        let mut paths = vec![bin.to_owned()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = match std::env::join_paths(paths) {
            Ok(path) => path,
            Err(e) => return failed(format!("precheck PATH failed: {e}")),
        };
        process.env("PATH", path);
    }
    // `spawn` kills descendants on drop (Unix process group, Windows Job) on every exit path below.
    let mut child = match spawn(
        own_group(&mut process)
            .arg("-c")
            .arg(command)
            .current_dir(krill_proto::os::shell_path(cwd))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    ) {
        Ok(child) => child,
        Err(e) => return failed(format!("precheck failed: {e}")),
    };
    // `PipeReader` gives both platforms `WouldBlock` reads (Unix O_NONBLOCK, Windows PeekNamedPipe).
    let pipes = child
        .stdout
        .take()
        .zip(child.stderr.take())
        .ok_or_else(|| io::Error::other("missing stdout/stderr pipe"))
        .and_then(|(out, err)| Ok((PipeReader::new(out)?, PipeReader::new(err)?)));
    let (mut stdout, mut stderr) = match pipes {
        Ok(pipes) => pipes,
        Err(e) => return failed(format!("precheck failed: {e}")),
    };

    let mut out_bytes = Vec::new();
    let mut err_bytes = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        if let Err(e) = drain_pipe(&mut stdout, &mut out_bytes, PRECHECK_STREAM_LIMIT, deadline)
            .and_then(|()| drain_pipe(&mut stderr, &mut err_bytes, PRECHECK_STREAM_LIMIT, deadline))
        {
            return PrecheckResult {
                duration_ms: elapsed_ms(started),
                timed_out: false,
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
                let code = status.code();
                let passed = status.success();
                return PrecheckResult {
                    duration_ms: elapsed_ms(started),
                    timed_out: false,
                    passed,
                    status: code,
                    output: combine_precheck_output(&out_bytes, &err_bytes),
                    error: (!passed)
                        .then(|| format!("precheck exited with status {}", code.unwrap_or(-1))),
                };
            }
            Ok(None) => {}
            Err(e) => {
                return PrecheckResult {
                    duration_ms: elapsed_ms(started),
                    timed_out: false,
                    passed: false,
                    status: None,
                    output: combine_precheck_output(&out_bytes, &err_bytes),
                    error: Some(format!("precheck failed: {e}")),
                };
            }
        }
        if Instant::now() >= deadline {
            return PrecheckResult {
                duration_ms: elapsed_ms(started),
                timed_out: true,
                passed: false,
                status: None,
                output: combine_precheck_output(&out_bytes, &err_bytes),
                error: Some("precheck timed out".into()),
            };
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn precheck_timeout(override_secs: Option<u64>) -> Duration {
    override_secs.map_or(PRECHECK_TIMEOUT, |secs| {
        Duration::from_secs(secs.clamp(1, MAX_PRECHECK_TIMEOUT_SECS))
    })
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
        let mut history = load_runs(dir)?;
        // Reap only processes whose exit the holder has already reported. Live
        // PTYs are never closed by scheduling. Segment output remains on disk.
        for (session, runs) in journal.sessions.clone() {
            let id = krill_proto::SessionId(session);
            // 세션도 종료 기록도 없이 사라진 옛 실행 기록은 여기서 tombstone을 남긴다.
            // 두지 않으면 id가 재사용됐을 때 무관한 새 세션을 close한다(K-30).
            // 종료 기록이 남은 경우는 아래 수거 경로가 상태와 함께 마무리한다.
            let gone = {
                let state = lock(&self.state);
                !state.sessions.contains_key(&id) && !state.exited.contains_key(&id)
            };
            if gone {
                for mut run in runs {
                    run.phase = "finished".into();
                    journal.append(&run)?;
                }
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
                for mut run in runs {
                    run.phase = "finished".into();
                    run.status = Some(status);
                    if let Some(out) = &captured {
                        run.output = Some(out.clone());
                    }
                    journal.append(&run)?;
                }
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
            let cron = Cron::parse(&entry.cron)?;
            let target_minute = if forced {
                Some(minute)
            } else if entry.enabled && cron_due {
                if cron.matches(calendar) && !journal.claimed(&entry.id, minute) {
                    Some(minute)
                } else if entry.effective_grace_minutes() > 0 {
                    cron.latest_minute_at_or_before(minute, entry.effective_grace_minutes())
                        .filter(|due| !journal.claimed(&entry.id, *due))
                } else {
                    None
                }
            } else {
                None
            };
            let Some(target_minute) = target_minute else {
                continue;
            };
            let summaries = coalesce_runs(&history);
            let previous = summaries
                .iter()
                .filter(|r| r.id == entry.id)
                .collect::<Vec<_>>();
            let run_number = previous
                .iter()
                .filter_map(|r| r.run_number)
                .max()
                .unwrap_or(0)
                .max(u64::try_from(previous.len()).unwrap_or(u64::MAX))
                .saturating_add(1);
            let mut run = Run {
                id: entry.id.clone(),
                minute: target_minute,
                phase: "started".into(),
                trigger: Some(if forced { "manual" } else { "scheduled" }.into()),
                run_number: Some(run_number),
                ..Default::default()
            };
            history.push(run.clone());
            journal.append(&run)?;
            if let Some(precheck) = entry.precheck.as_deref().filter(|s| !s.trim().is_empty()) {
                let res =
                    run_precheck(Path::new(&entry.cwd), precheck, entry.precheck_timeout_secs);
                if !res.passed {
                    run.phase = "skipped".into();
                    run.duration_ms = Some(res.duration_ms);
                    run.timed_out = Some(res.timed_out);
                    run.status = res.status;
                    run.output = res.output;
                    run.error = res.error;
                    journal.append(&run)?;
                    continue;
                }
            }
            if entry.workspace_mode == WorkspaceMode::Reuse
                && entry.reuse_session
                && !entry.prompt.is_empty()
            {
                let prior = previous.iter().find_map(|r| r.session);
                if let Some(id) = prior.map(krill_proto::SessionId) {
                    let peer = Peer {
                        pid: None,
                        scope: Scope::Full,
                    };
                    let alive = {
                        let state = lock(&self.state);
                        !state.exited.contains_key(&id)
                            && state
                                .sessions
                                .get(&id)
                                .is_some_and(|e| !e.session.is_closed())
                    };
                    if alive && let Some(tap) = self.tap(&peer, id) {
                        if let Err(e) =
                            Registry::deliver_input_to_tap(&tap, entry.prompt.as_bytes(), true)
                        {
                            run.phase = "failed".into();
                            run.error = Some(format!("session input failed: {e}"));
                        } else {
                            run.phase = "spawned".into();
                            run.session = Some(id.0);
                            run.cwd = Some(entry.cwd.clone());
                        }
                        journal.append(&run)?;
                        continue;
                    }
                }
            }
            let run_cwd = match entry.workspace_mode {
                WorkspaceMode::Reuse => PathBuf::from(&entry.cwd),
                WorkspaceMode::Isolate => {
                    let wt_name = format!("schedule-{}-{target_minute}", entry.id);
                    match create_isolated(
                        &entry.cwd,
                        &wt_name,
                        dir,
                        entry
                            .base_branch
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty()),
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
            run.cwd = Some(run_cwd.to_string_lossy().into_owned());
            let launch_as_agent = entry.agent.is_some()
                || (entry.command.len() == 1 && entry.resolved_agent().is_some());
            let extra_tokens = entry
                .extra_agent_args
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(tokenize_command_line)
                .transpose()?
                .unwrap_or_default();
            let mut command = entry.command.into_iter();
            let Some(program) = command.next() else {
                continue;
            };
            let mut extra_args: Vec<String> = command.collect();
            if launch_as_agent {
                extra_args.extend(extra_tokens);
            }
            let resolved_agent = self
                .agents
                .agents()
                .ok()
                .and_then(|a| a.resolve(&program).map(|(c, p)| (c.to_owned(), p.clone())));
            let cwd_bytes = run_cwd.as_os_str().as_bytes().to_vec();
            let is_agent_launch = launch_as_agent;
            let prompt = entry.prompt;
            if let Some((_, profile)) = &resolved_agent {
                crate::agent::pre_trust_workspace(profile, &cwd_bytes, None);
            }
            let create = Create {
                account: None,
                program: schedule_program(
                    &program,
                    extra_args,
                    &prompt,
                    is_agent_launch,
                    resolved_agent.as_ref(),
                ),
                cwd: cwd_bytes,
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
                if is_agent_launch
                    && !prompt.is_empty()
                    && let Some(tap) = self.tap(&peer, session)
                {
                    let prompt_bytes = prompt.into_bytes();
                    std::thread::Builder::new()
                        .name(format!("sched-prompt-{}", session.0))
                        .spawn(move || {
                            let deadline = Instant::now() + Duration::from_secs(15);
                            while Instant::now() < deadline {
                                let p = tap.pulse();
                                if p.closed || (p.end > 0 && tap.quiet_ms() >= 150) {
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            if !tap.pulse().closed {
                                drop(Registry::deliver_input_to_tap(&tap, &prompt_bytes, true));
                            }
                        })
                        .ok();
                }
            } else {
                run.phase = "failed".into();
                run.error = Some("session creation failed".into());
            }
            journal.append(&run)?;
        }
        Ok(())
    }
}

fn schedule_program(
    program: &str,
    extra_args: Vec<String>,
    prompt: &str,
    is_agent: bool,
    resolved: Option<&(String, krill_proto::agent::Profile)>,
) -> Program {
    if is_agent
        && extra_args.is_empty()
        && let Some((canonical, _)) = resolved
    {
        return Program::Agent {
            id: canonical.clone(),
        };
    }
    let mut args = Vec::new();
    let executable = if is_agent && let Some((_, profile)) = resolved {
        args.extend(profile.launch.iter().skip(1).cloned());
        profile.program().unwrap_or(program)
    } else {
        program
    };
    args.extend(extra_args);
    if !is_agent && !prompt.is_empty() {
        args.push(prompt.into());
    }
    Program::Custom {
        program: executable.as_bytes().to_vec(),
        args: args.into_iter().map(String::into_bytes).collect(),
    }
}

fn create_isolated(
    cwd: &str,
    name: &str,
    dir: &Path,
    base: Option<&str>,
) -> Result<PathBuf, String> {
    use krill_proto::control::{NewWorktree, Setup};
    use krill_proto::os::ffi::OsStringExt as _;
    let root = std::env::var_os("KRILL_WORKTREES_DIR")
        .map_or_else(|| dir.join("worktrees"), PathBuf::from);
    let req = NewWorktree {
        repo: cwd.as_bytes().to_vec(),
        name: name.into(),
        base: base.map(str::to_owned),
        setup: Setup::Skip,
    };
    let outcome = crate::git::worktree::create(
        crate::git::queue()?,
        &req,
        &root,
        &dir.join(crate::git::worktree::TRUST_FILE),
    )?;
    match outcome {
        crate::git::worktree::Outcome::Created { worktree, .. } => {
            Ok(PathBuf::from(std::ffi::OsString::from_vec(worktree.path)))
        }
        crate::git::worktree::Outcome::Untrusted { .. } => Err("worktree setup untrusted".into()),
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
    sessions: HashMap<u64, Vec<Run>>,
}

fn track_session_run(sessions: &mut HashMap<u64, Vec<Run>>, run: &Run) {
    let Some(session) = run.session else {
        return;
    };
    if run.phase == "spawned" {
        let active = sessions.entry(session).or_default();
        if let Some(existing) = active
            .iter_mut()
            .find(|r| r.id == run.id && r.run_number == run.run_number && r.minute == run.minute)
        {
            *existing = run.clone();
        } else {
            active.push(run.clone());
        }
    } else if run.run_number.is_some() {
        let remove_key = if let Some(active) = sessions.get_mut(&session) {
            active.retain(|r| !(r.id == run.id && r.run_number == run.run_number));
            active.is_empty()
        } else {
            false
        };
        if remove_key {
            sessions.remove(&session);
        }
    } else {
        sessions.remove(&session);
    }
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
                track_session_run(&mut sessions, &run);
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
        track_session_run(&mut self.sessions, run);
        if self.file.metadata()?.len() >= MAX_RUNS {
            fs::rename(self.dir.join(RUNS), self.dir.join(PREVIOUS_RUNS))?;
            self.file = Self::writer(self.dir)?;
            for active in self.sessions.values().flatten() {
                let same_as_current = run.phase == "spawned"
                    && active.session == run.session
                    && active.id == run.id
                    && active.run_number == run.run_number
                    && active.minute == run.minute;
                if !same_as_current {
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
#[cfg(unix)]
mod tests;
#[cfg(all(test, windows))]
#[path = "schedule/windows_tests.rs"]
mod windows_tests;
