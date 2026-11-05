//! Opt-in error reporting. Network work runs in a bounded helper process, outside
//! GPUI and the session daemon. Captured payloads use the shared closed schema.

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Global, SharedString, Task};
use krill_proto::error_reporting::{self, Code, Event, Process, Status, runs::RunGuard};

use crate::i18n::catalog::settings as text;

const POLL: Duration = Duration::from_secs(60);
const HELPER_DEADLINE: Duration = Duration::from_secs(45);
pub const DAEMON_CONNECT_REPORT_GRACE: Duration = Duration::from_secs(10);

#[must_use]
#[derive(Debug)]
pub struct PendingConnectFailure {
    cancelled: Arc<AtomicBool>,
    _task: Task<()>,
}

impl Drop for PendingConnectFailure {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

pub(crate) fn record_app_error_with_env(
    state: &Path,
    env: &error_reporting::Env,
    code: Code,
    detail: &str,
    file: &str,
    line: u32,
) {
    let Ok(Some(consent)) = error_reporting::consent_with(state, env) else {
        return;
    };
    let loc = error_reporting::source_location(file, line);
    let Ok(mut event) = Event::new(Process::App, code, loc) else {
        return;
    };
    event.detail = error_reporting::sanitize_detail(detail);
    error_reporting::record_with_consent(state, event, &consent).ok();
}

#[track_caller]
pub fn defer_daemon_connection_failed(
    cx: &mut App,
    error: &dyn std::fmt::Display,
) -> PendingConnectFailure {
    defer_daemon_connection_failed_with(
        cx,
        crate::settings::state_dir(cx),
        error_reporting::Env::live(),
        DAEMON_CONNECT_REPORT_GRACE,
        &error.to_string(),
        std::panic::Location::caller(),
    )
}

#[cfg(test)]
fn defer_daemon_connection_failed_at(
    cx: &mut App,
    state_dir: Option<std::path::PathBuf>,
    grace: Duration,
    detail: &str,
    loc: &'static std::panic::Location<'static>,
) -> PendingConnectFailure {
    defer_daemon_connection_failed_with(
        cx,
        state_dir,
        error_reporting::Env::stub(&[]),
        grace,
        detail,
        loc,
    )
}

fn defer_daemon_connection_failed_with(
    cx: &mut App,
    state_dir: Option<std::path::PathBuf>,
    env: error_reporting::Env,
    grace: Duration,
    detail: &str,
    loc: &'static std::panic::Location<'static>,
) -> PendingConnectFailure {
    let cancelled = Arc::new(AtomicBool::new(false));
    let detail = detail.to_owned();
    let file = loc.file();
    let line = loc.line();
    let task_cancelled = Arc::clone(&cancelled);
    let task = cx.spawn(async move |cx| {
        cx.background_executor().timer(grace).await;
        if task_cancelled.load(Ordering::Acquire) {
            return;
        }
        let Some(state) = state_dir else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                if task_cancelled.load(Ordering::Acquire) {
                    return;
                }
                record_app_error_with_env(
                    &state,
                    &env,
                    Code::DaemonConnectFailed,
                    &detail,
                    file,
                    line,
                );
            })
            .await;
    });
    PendingConnectFailure {
        cancelled,
        _task: task,
    }
}

#[derive(Debug, Default)]
pub(crate) struct Panel {
    pub status: Option<Status>,
    pub pending: bool,
    pub started: Option<Instant>,
    pub message: Option<SharedString>,
    pub revision: u64,
}

impl Panel {
    pub fn description(&self) -> Option<String> {
        if self.pending
            && self
                .started
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(1))
        {
            return Some(text::ERROR_REPORTS_SAVING.get().to_owned());
        }
        if let Some(message) = &self.message {
            return Some(message.to_string());
        }
        let status = self.status.as_ref()?;
        Some(if let Some(var) = &status.veto {
            text::ERROR_REPORTS_VETO.format(&[("variable", var)])
        } else if status.enabled {
            text::ERROR_REPORTS_ON.format(&[("count", &status.pending.to_string())])
        } else {
            text::ERROR_REPORTS_OFF.get().to_owned()
        })
    }
}

#[derive(Default)]
struct RunSlot {
    generation: Option<String>,
    guard: Option<RunGuard>,
    closed: bool,
}

struct Reporter {
    run: Arc<Mutex<RunSlot>>,
    _poll: Task<()>,
}
impl Global for Reporter {}

/// Capture the failed connection boundary without running disk work on GPUI.
#[track_caller]
pub fn daemon_connection_failed(cx: &mut App, error: &dyn std::fmt::Display) {
    let loc = std::panic::Location::caller();
    let detail = error.to_string();
    let Some(state) = crate::settings::state_dir(cx) else {
        return;
    };
    cx.background_spawn(async move {
        error_reporting::capture_detailed_at(
            &state,
            Process::App,
            Code::DaemonConnectFailed,
            Some(&detail),
            &[],
            loc.file(),
            loc.line(),
        );
    })
    .detach();
}

fn refresh_run_at(state: &Path, slot: &Mutex<RunSlot>) {
    let Ok(consent) = error_reporting::consent_at(state) else {
        return;
    };
    let Ok(mut slot) = slot.try_lock() else {
        return;
    };
    if slot.closed {
        return;
    }
    let generation = consent.map(|consent| consent.generation);
    if slot.generation == generation && (slot.guard.is_some() || generation.is_none()) {
        return;
    }
    // Normal replacement writes clean in place. The owner never unlinks its lock.
    slot.guard = None;
    slot.generation = generation;
    if slot.generation.is_some() {
        slot.guard = RunGuard::start(state, Process::App).ok().flatten();
    }
}

fn finish_run(slot: &Mutex<RunSlot>) {
    // Join an in-flight refresh off GPUI, then fence late refresh jobs.
    // Skipping a busy slot or allowing a new marker after this point
    // would turn a normal app quit into an uncertain-exit report.
    let mut run = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    run.closed = true;
    if let Some(guard) = run.guard.take() {
        guard.finish();
    }
}

/// Refresh the current process marker after a consent change, off the UI thread.
pub(crate) fn refresh_run(cx: &mut App) {
    let Some(state) = crate::settings::state_dir(cx) else {
        return;
    };
    let Some(reporter) = cx.try_global::<Reporter>() else {
        return;
    };
    let slot = Arc::clone(&reporter.run);
    cx.background_spawn(async move { refresh_run_at(&state, &slot) })
        .detach();
}

/// Start once, after settings initialization. Disabled reporting does not spawn
/// the uploader. The next loop observes consent changes made through the CLI.
pub fn start(cx: &mut App) {
    if cx.has_global::<Reporter>() {
        return;
    }
    let Some(state) = crate::settings::state_dir(cx) else {
        return;
    };
    let run = Arc::new(Mutex::new(RunSlot::default()));
    let quitting_run = Arc::clone(&run);
    cx.on_app_quit(move |cx| {
        let run = Arc::clone(&quitting_run);
        cx.background_spawn(async move {
            finish_run(&run);
        })
    })
    .detach();
    let poll_run = Arc::clone(&run);
    let poll = cx.spawn(async move |cx| {
        loop {
            let state = state.clone();
            let run = Arc::clone(&poll_run);
            cx.background_executor()
                .spawn(async move {
                    refresh_run_at(&state, &run);
                    send_pending(&state).ok();
                })
                .await;
            cx.background_executor().timer(POLL).await;
        }
    });
    cx.set_global(Reporter { run, _poll: poll });
}

fn send_pending(state: &Path) -> io::Result<()> {
    if std::env::var_os(error_reporting::ENDPOINT_VAR).is_none()
        && !error_reporting::is_production_state_dir(state)
    {
        return Ok(());
    }
    if error_reporting::consent_at(state)?.is_none() {
        return Ok(());
    }
    let executable = krill_proto::daemon::sibling_exe("krill")?;
    let mut child = krill_proto::os::background_command(executable)
        .args(["errors", "flush", "--json"])
        .env("KRILL_STATE_DIR", state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + HELPER_DEADLINE;
    loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                child.kill().ok();
                child.wait().ok();
                return Err(error);
            }
        };
        if let Some(status) = status {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other("error reporting helper failed"))
            };
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            return Err(io::ErrorKind::TimedOut.into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn error_reporting_quit_waits_for_refresh_and_fences_late_refresh() {
        let state =
            std::env::temp_dir().join(format!("krill-error-reporting-quit-{}", std::process::id()));
        std::fs::remove_dir_all(&state).ok();
        error_reporting::enable(&state).unwrap();
        let slot = Arc::new(Mutex::new(RunSlot::default()));
        let refreshing = slot.lock().unwrap();
        let finishing = Arc::clone(&slot);
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let quit = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            finish_run(&finishing);
            finished_tx.send(()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(finished_rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(refreshing);
        finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        quit.join().unwrap();
        assert!(slot.lock().unwrap().closed);

        refresh_run_at(&state, &slot);
        let slot = slot.lock().unwrap();
        assert!(slot.guard.is_none());
        assert!(slot.generation.is_none());
        std::fs::remove_dir_all(state).unwrap();
    }

    fn drain_events(state: &Path) -> Vec<Event> {
        let consent = error_reporting::consent_with(state, &error_reporting::Env::stub(&[]))
            .unwrap()
            .unwrap();
        let lock = error_reporting::UploadLock::try_acquire(state)
            .unwrap()
            .unwrap();
        let mut events = Vec::new();
        error_reporting::flush(state, &consent, &lock, |body| {
            let req: error_reporting::Request = serde_json::from_slice(body).unwrap();
            let ack = error_reporting::Ack {
                schema_version: error_reporting::SCHEMA_VERSION,
                accepted_ids: req.events.iter().map(|e| e.event_id.clone()).collect(),
                duplicate_ids: Vec::new(),
            };
            events.extend(req.events);
            error_reporting::Reply::Http {
                status: 200,
                retry_after_secs: None,
                body: serde_json::to_vec(&ack).unwrap(),
            }
        })
        .unwrap();
        events
    }

    #[gpui::test]
    fn defer_daemon_connect_failure_respects_grace_and_redacts_detail(
        cx: &mut gpui::TestAppContext,
    ) {
        let state =
            std::env::temp_dir().join(format!("krill-defer-daemon-err-{}", std::process::id()));
        std::fs::remove_dir_all(&state).ok();
        error_reporting::enable(&state).unwrap();
        let grace = Duration::from_millis(80);
        let raw_error = "/Users/alice/.krill/krilld.sock: connection refused (os error 61)";

        let pending = cx.update(|cx| {
            defer_daemon_connection_failed_at(
                cx,
                Some(state.clone()),
                grace,
                raw_error,
                std::panic::Location::caller(),
            )
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(40));
        cx.run_until_parked();
        drop(pending);
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        assert!(
            drain_events(&state).is_empty(),
            "connect failure recovered within grace must record 0 events"
        );

        let pending = cx.update(|cx| {
            defer_daemon_connection_failed_at(
                cx,
                Some(state.clone()),
                grace,
                raw_error,
                std::panic::Location::caller(),
            )
        });
        cx.run_until_parked();
        cx.executor().advance_clock(grace);
        cx.run_until_parked();
        let events = drain_events(&state);
        drop(pending);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].process, Process::App);
        assert_eq!(events[0].error_code, Code::DaemonConnectFailed);
        let detail = events[0].detail.as_deref().expect("redacted detail");
        assert!(detail.contains("[USER]"), "{detail}");
        assert!(!detail.contains("alice"), "{detail}");
        let source_loc = events[0].source_loc.as_deref().expect("source_loc");
        assert!(
            source_loc.starts_with("crates/krill-app/src/"),
            "{source_loc}"
        );
        std::fs::remove_dir_all(state).unwrap();
    }
}
