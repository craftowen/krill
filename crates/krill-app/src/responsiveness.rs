//! Local evidence for intermittent macOS UI stalls. The observer must not run
//! on GPUI's foreground or shared blocking executor, which may themselves stall.

use std::io;
use std::mem;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
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

/// What the UI thread saw at its last heartbeat. It freezes with the UI, so a stall record
/// shows the state just before the stall.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Seen {
    /// `[NSApp isActive]`.
    app_active: bool,
    /// Some window draws frames that reach the screen: not minimized, not fully covered
    /// (occlusion) and not on a sleeping display.
    window_visible: bool,
}

impl Seen {
    fn pack(self) -> u8 {
        u8::from(self.app_active) | u8::from(self.window_visible) << 1
    }

    const fn unpack(bits: u8) -> Self {
        Self {
            app_active: bits & 1 != 0,
            window_visible: bits & 2 != 0,
        }
    }
}

/// Start once for the entire application, independently of individual windows.
pub fn start(cx: &mut App) {
    start_with(
        cx,
        krill_proto::daemon::state_dir,
        seen,
        crate::platform::display_asleep,
    );
}

fn seen(cx: &mut App) -> Seen {
    let window_visible = cx.windows().into_iter().any(|handle| {
        handle
            .update(cx, |_, window, _| window.is_visible())
            .unwrap_or(false)
    });
    Seen {
        app_active: crate::platform::app_is_active(),
        window_visible,
    }
}

fn start_with(
    cx: &mut App,
    state: impl FnOnce() -> Option<std::path::PathBuf> + Send + 'static,
    probe: fn(&mut App) -> Seen,
    display_asleep: fn() -> bool,
) {
    if cx.has_global::<Watchdog>() {
        return;
    }
    let epoch = Instant::now();
    let last = Arc::new(AtomicU64::new(0));
    let observed = Arc::clone(&last);
    let seen = Arc::new(AtomicU8::new(0));
    let observed_seen = Arc::clone(&seen);
    let (stop, stopped) = mpsc::channel::<()>();
    let worker = std::thread::Builder::new()
        .name("ui-watchdog".into())
        .spawn(move || {
            let mut watch = Watch::default();
            let state = state();
            let mut want_sample = false;
            let mut sampled_at: Option<Instant> = None;
            while matches!(
                stopped.recv_timeout(PULSE),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                let now = millis(epoch.elapsed());
                let seen = Seen::unpack(observed_seen.load(Ordering::Acquire));
                match watch.observe(now, observed.load(Ordering::Acquire), seen, display_asleep) {
                    Some(Report::Stalled {
                        delay_ms,
                        late_ms,
                        observer_gap_ms,
                        whole_process,
                    }) => {
                        tracing::warn!(
                            delay_ms,
                            late_ms,
                            observer_gap_ms,
                            whole_process,
                            app_active = seen.app_active,
                            "UI heartbeat stalled"
                        );
                        want_sample = sampled_at.is_none_or(|at| at.elapsed() >= SAMPLE_EVERY);
                    }
                    Some(Report::Recovered {
                        delay_ms,
                        observer_gap_ms,
                        whole_process,
                    }) => {
                        want_sample = false;
                        tracing::info!(
                            delay_ms,
                            observer_gap_ms,
                            whole_process,
                            "UI heartbeat recovered"
                        );
                    }
                    Some(Report::ObserverLate { observer_gap_ms }) => {
                        tracing::warn!(
                            observer_gap_ms,
                            app_active = seen.app_active,
                            "UI watchdog woke late"
                        );
                    }
                    None => {}
                }
                if !want_sample || watch.monitor.stalled_for(now) < SAMPLE_AFTER_MS {
                    continue;
                }
                want_sample = false;
                if let Some(state) = &state {
                    sampled_at = Some(Instant::now());
                    let dir = state.join("diagnostics");
                    // `sample` takes over a second; on this thread it would delay the recovery record.
                    let sampler = std::thread::Builder::new()
                        .name("ui-stall-sample".into())
                        .spawn(move || match capture(&dir) {
                            Ok(()) => tracing::info!("UI stall sample saved locally"),
                            Err(error) => {
                                tracing::warn!(kind = ?error.kind(), "UI stall sample failed");
                            }
                        });
                    if sampler.is_err() {
                        tracing::warn!("UI stall sample could not start");
                    }
                }
            }
        });
    if worker.is_err() {
        tracing::warn!("UI watchdog could not start");
        return;
    }
    let pulse = cx.spawn(async move |cx| {
        let mut activity = None;
        loop {
            last.store(millis(epoch.elapsed()), Ordering::Release);
            let now = cx.update(probe);
            seen.store(now.pack(), Ordering::Release);
            hold_activity(&mut activity, now);
            cx.background_executor().timer(PULSE).await;
        }
    });
    cx.set_global(Watchdog {
        _pulse: pulse,
        _stop: stop,
    });
}

/// App Nap and timer coalescing can delay this very heartbeat and the frames behind it.
/// Prevent them only while a window is visible and the app is active, so the power cost
/// is paid only while the user is looking at Krill.
fn hold_activity(activity: &mut Option<crate::platform::LatencyActivity>, seen: Seen) {
    let wanted = seen.app_active && seen.window_visible;
    if wanted != activity.is_some() {
        *activity = wanted.then(crate::platform::LatencyActivity::begin);
    }
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

#[derive(Debug, PartialEq, Eq)]
enum Report {
    Stalled {
        delay_ms: u64,
        /// How long after the expected detection (`last + STALL_MS`) the observer noticed.
        /// Normally under one pulse; more means the observer thread was not running either.
        late_ms: u64,
        observer_gap_ms: u64,
        /// The observer itself slept through a stall-sized interval: the whole process was
        /// not scheduled (external suspension, App Nap, CPU starvation), not just the UI.
        whole_process: bool,
    },
    Recovered {
        delay_ms: u64,
        observer_gap_ms: u64,
        whole_process: bool,
    },
    /// The observer overslept while the UI heartbeat kept up (or was already stalled).
    ObserverLate { observer_gap_ms: u64 },
}

#[derive(Default)]
struct Watch {
    monitor: Monitor,
    /// The current stall began while nothing was on screen; drop its recovery too.
    quiet: bool,
    tick: Option<u64>,
}

impl Watch {
    fn observe(
        &mut self,
        now: u64,
        last: u64,
        seen: Seen,
        display_asleep: impl Fn() -> bool,
    ) -> Option<Report> {
        let observer_gap_ms = self
            .tick
            .replace(now)
            .map_or(0, |previous| now.saturating_sub(previous));
        let whole_process = observer_gap_ms >= STALL_MS;
        // Hidden, covered or on a sleeping display: macOS may throttle us and no one waits on input.
        let shown = || seen.window_visible && !display_asleep();
        match self.monitor.check(now, last) {
            Some(Event::Stalled(_)) if !shown() => {
                self.quiet = true;
                None
            }
            Some(Event::Recovered(_)) if mem::take(&mut self.quiet) => None,
            Some(Event::Stalled(delay_ms)) => Some(Report::Stalled {
                delay_ms,
                late_ms: delay_ms.saturating_sub(STALL_MS),
                observer_gap_ms,
                whole_process,
            }),
            Some(Event::Recovered(delay_ms)) => Some(Report::Recovered {
                delay_ms,
                observer_gap_ms,
                whole_process,
            }),
            None => (whole_process && !self.quiet && shown())
                .then_some(Report::ObserverLate { observer_gap_ms }),
        }
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

    const SHOWN: Seen = Seen {
        app_active: true,
        window_visible: true,
    };

    #[test]
    fn stall_record_carries_detection_delay_and_observer_gap() {
        let mut watch = Watch::default();
        for now in (100..=600).step_by(100) {
            assert_eq!(watch.observe(now, 200.min(now), SHOWN, || false), None);
        }
        assert_eq!(
            watch.observe(700, 200, SHOWN, || false),
            Some(Report::Stalled {
                delay_ms: 500,
                late_ms: 0,
                observer_gap_ms: 100,
                whole_process: false,
            })
        );
        assert_eq!(
            watch.observe(800, 750, SHOWN, || false),
            Some(Report::Recovered {
                delay_ms: 550,
                observer_gap_ms: 100,
                whole_process: false,
            })
        );
    }

    #[test]
    fn observer_that_slept_through_the_stall_marks_the_whole_process() {
        let mut watch = Watch::default();
        assert_eq!(watch.observe(100, 100, SHOWN, || false), None);
        assert_eq!(
            watch.observe(2_800, 100, SHOWN, || false),
            Some(Report::Stalled {
                delay_ms: 2_700,
                late_ms: 2_200,
                observer_gap_ms: 2_700,
                whole_process: true,
            })
        );
        assert_eq!(
            watch.observe(2_900, 2_850, SHOWN, || false),
            Some(Report::Recovered {
                delay_ms: 2_750,
                observer_gap_ms: 100,
                whole_process: false,
            })
        );
        // The heartbeat ran first after the pause: no UI stall, but the observer overslept.
        assert_eq!(
            watch.observe(3_900, 3_850, SHOWN, || false),
            Some(Report::ObserverLate {
                observer_gap_ms: 1_000
            })
        );
    }

    #[test]
    fn hidden_or_sleeping_display_is_not_recorded_as_a_stall() {
        let hidden = Seen {
            app_active: true,
            window_visible: false,
        };
        for (seen, asleep) in [(hidden, false), (SHOWN, true)] {
            let mut watch = Watch::default();
            assert_eq!(watch.observe(100, 100, seen, || asleep), None);
            assert_eq!(watch.observe(3_000, 100, seen, || asleep), None);
            assert_eq!(watch.observe(3_100, 3_050, SHOWN, || false), None);
            assert_eq!(watch.observe(3_200, 3_150, SHOWN, || false), None);
        }
    }

    #[test]
    fn app_nap_activity_is_held_only_while_visible_and_active() {
        let mut activity = None;
        hold_activity(&mut activity, SHOWN);
        assert!(activity.is_some());
        hold_activity(&mut activity, SHOWN);
        assert!(activity.is_some());
        for seen in [
            Seen {
                app_active: false,
                window_visible: true,
            },
            Seen {
                app_active: true,
                window_visible: false,
            },
        ] {
            hold_activity(&mut activity, SHOWN);
            hold_activity(&mut activity, seen);
            assert!(activity.is_none(), "{seen:?}");
        }
    }

    #[test]
    fn seen_state_round_trips_through_the_atomic() {
        for app_active in [false, true] {
            for window_visible in [false, true] {
                let seen = Seen {
                    app_active,
                    window_visible,
                };
                assert_eq!(Seen::unpack(seen.pack()), seen);
            }
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
        cx.update(|cx| start_with(cx, move || Some(owned), |_| SHOWN, || false));
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
