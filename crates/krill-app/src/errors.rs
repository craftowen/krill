//! Opt-in error reporting. Network work runs in a bounded helper process, outside
//! GPUI and the session daemon. Captured payloads use the shared closed schema.

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Global, SharedString, Task};
use krill_proto::error_reporting::{self, Code, Event, Process, Status, runs::RunGuard};

use crate::i18n::catalog::settings as text;

const POLL: Duration = Duration::from_secs(60);
const HELPER_DEADLINE: Duration = Duration::from_secs(45);

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
pub fn daemon_connection_failed(cx: &mut App) {
    let Some(state) = crate::settings::state_dir(cx) else {
        return;
    };
    cx.background_spawn(async move {
        if let Ok(event) = Event::new(
            Process::App,
            Code::DaemonConnectFailed,
            error_reporting::source_location(file!(), line!()),
        ) {
            error_reporting::record_at(&state, event).ok();
        }
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
}
