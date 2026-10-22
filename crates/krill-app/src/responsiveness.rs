//! Local evidence for intermittent macOS UI stalls. The observer must not run
//! on GPUI's foreground or shared blocking executor, which may themselves stall.

use std::io;
use std::mem;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gpui::{App, Global, Task};

const PULSE: Duration = Duration::from_millis(100);
/// Same size as the input delays macOS records as "slow hid response"; the old 5 s threshold
/// never saw the 0.5–3.4 s freezes users actually hit.
const STALL_MS: u64 = 500;
/// `sample` inspects the process for a second; keep it rare even when stalls are frequent.
const SAMPLE_EVERY: Duration = Duration::from_secs(600);
/// Spend that rare sample on a stall long enough to still be running while `sample` looks.
const SAMPLE_AFTER_MS: u64 = 1_500;
const SAMPLE_BYTES: u64 = 2 << 20;

struct Watchdog {
    _pulse: Task<()>,
    // Dropping the sender stops the observer without joining it on the UI.
    _stop: mpsc::Sender<()>,
}
impl Global for Watchdog {}

/// Start once for the entire application, independently of individual windows.
pub fn start(cx: &mut App) {
    // Only a focused window has input to delay; background timers are throttled by App Nap.
    start_with_state(cx, krill_proto::daemon::state_dir, |cx| {
        cx.active_window().is_some()
    });
}

fn start_with_state(
    cx: &mut App,
    state: impl FnOnce() -> Option<std::path::PathBuf> + Send + 'static,
    foreground: fn(&App) -> bool,
) {
    if cx.has_global::<Watchdog>() {
        return;
    }
    let epoch = Instant::now();
    let last = Arc::new(AtomicU64::new(0));
    let observed = Arc::clone(&last);
    let active = Arc::new(AtomicBool::new(false));
    let observed_active = Arc::clone(&active);
    let (stop, stopped) = mpsc::channel::<()>();
    let worker = std::thread::Builder::new()
        .name("ui-watchdog".into())
        .spawn(move || {
            let mut monitor = Monitor::default();
            let state = state();
            let mut quiet = false;
            let mut want_sample = false;
            let mut sampled_at: Option<Instant> = None;
            while matches!(
                stopped.recv_timeout(PULSE),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                let now = millis(epoch.elapsed());
                match monitor.check(now, observed.load(Ordering::Acquire)) {
                    Some(Event::Stalled(_)) if !observed_active.load(Ordering::Acquire) => {
                        quiet = true;
                    }
                    Some(Event::Recovered(_)) if mem::take(&mut quiet) => {}
                    Some(Event::Stalled(delay_ms)) => {
                        tracing::warn!(delay_ms, "UI heartbeat stalled");
                        want_sample = sampled_at.is_none_or(|at| at.elapsed() >= SAMPLE_EVERY);
                    }
                    Some(Event::Recovered(delay_ms)) => {
                        want_sample = false;
                        tracing::info!(delay_ms, "UI heartbeat recovered");
                    }
                    None => {}
                }
                if !want_sample || monitor.stalled_for(now) < SAMPLE_AFTER_MS {
                    continue;
                }
                want_sample = false;
                if let Some(state) = &state {
                    sampled_at = Some(Instant::now());
                    match capture(&state.join("diagnostics")) {
                        Ok(()) => tracing::info!("UI stall sample saved locally"),
                        Err(error) => {
                            tracing::warn!(kind = ?error.kind(), "UI stall sample failed");
                        }
                    }
                }
            }
        });
    if worker.is_err() {
        tracing::warn!("UI watchdog could not start");
        return;
    }
    let pulse = cx.spawn(async move |cx| {
        loop {
            last.store(millis(epoch.elapsed()), Ordering::Release);
            active.store(cx.update(|cx| foreground(cx)), Ordering::Release);
            cx.background_executor().timer(PULSE).await;
        }
    });
    cx.set_global(Watchdog {
        _pulse: pulse,
        _stop: stop,
    });
}

fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Stalled(u64),
    Recovered(u64),
}

#[derive(Default)]
struct Monitor {
    stalled_at: Option<u64>,
}

impl Monitor {
    fn stalled_for(&self, now: u64) -> u64 {
        self.stalled_at.map_or(0, |start| now.saturating_sub(start))
    }

    fn check(&mut self, now: u64, last: u64) -> Option<Event> {
        if let Some(start) = self.stalled_at {
            if last > start {
                self.stalled_at = None;
                return Some(Event::Recovered(last.saturating_sub(start)));
            }
        } else if now.saturating_sub(last) >= STALL_MS {
            self.stalled_at = Some(last);
            return Some(Event::Stalled(now.saturating_sub(last)));
        }
        None
    }
}

fn capture(dir: &Path) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    // No terminal contents or heap dump. Keep only two bounded stack samples,
    // private to this user; no upload and no new file per process/restart.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut child = Command::new("/usr/bin/sample")
        .args([
            std::process::id().to_string(),
            "1".into(),
            "-file".into(),
            "/dev/stdout".into(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| {
        let stdout = child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?;
        let read = crate::process_io::read(stdout, SAMPLE_BYTES, deadline)?;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let output = read.finish(deadline)?;
        if output.timed_out || output.bytes.is_empty() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if !status.success() && u64::try_from(output.bytes.len()).unwrap_or(0) < SAMPLE_BYTES {
            return Err(io::Error::other("sample failed"));
        }
        let path = dir.join("app-hang-latest.sample.txt");
        match std::fs::rename(&path, dir.join("app-hang-previous.sample.txt")) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?
            .write_all(&output.bytes)
    })();
    child.kill().ok();
    child.wait().ok();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_once_per_stall_then_records_recovery_and_next_stall() {
        let mut monitor = Monitor::default();
        assert_eq!(monitor.check(499, 0), None);
        assert_eq!(monitor.check(500, 0), Some(Event::Stalled(500)));
        assert_eq!(monitor.check(30_000, 0), None);
        assert_eq!(
            monitor.check(30_500, 30_000),
            Some(Event::Recovered(30_000))
        );
        assert_eq!(monitor.check(30_600, 30_500), None);
        assert_eq!(monitor.check(31_000, 30_500), Some(Event::Stalled(500)));
    }

    #[test]
    fn continuing_heartbeats_do_not_report_a_stall() {
        let mut monitor = Monitor::default();
        for now in (100..60_000).step_by(100) {
            assert_eq!(monitor.check(now, now - 100), None);
        }
    }

    #[test]
    fn native_sample_is_bounded_and_rotates_in_place() {
        let dir = std::env::temp_dir().join(format!("krill-hang-sample-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app-hang-latest.sample.txt"), "previous").unwrap();
        capture(&dir).unwrap();
        let bytes = std::fs::read(dir.join("app-hang-latest.sample.txt")).unwrap();
        assert!(bytes.len() <= usize::try_from(SAMPLE_BYTES).unwrap());
        assert!(String::from_utf8_lossy(&bytes).contains("Call graph:"));
        assert_eq!(
            std::fs::read_to_string(dir.join("app-hang-previous.sample.txt")).unwrap(),
            "previous"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[gpui::test]
    fn foreground_stall_is_sampled_without_executor_progress(cx: &mut gpui::TestAppContext) {
        let state = std::env::temp_dir().join(format!("krill-watchdog-{}", std::process::id()));
        let sample = state.join("diagnostics/app-hang-latest.sample.txt");
        let owned = state.clone();
        cx.update(|cx| start_with_state(cx, move || Some(owned), |_| true));
        cx.run_until_parked();
        // Deliberately do not pump either GPUI executor. Only the native observer
        // can collect the blocked foreground's stack during this interval.
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let ready =
                std::fs::read_to_string(&sample).is_ok_and(|text| text.contains("Call graph:"));
            if ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "watchdog did not sample the stalled UI"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        cx.update(App::remove_global::<Watchdog>);
        std::fs::remove_dir_all(state).unwrap();
    }
}
