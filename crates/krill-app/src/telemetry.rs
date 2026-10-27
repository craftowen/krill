//! App usage only. UI hooks retain input timestamp + cached policy generation; IO stays off GPUI.
use gpui::{App, Global, IntoElement, Styled as _, Subscription, Task, Window};
use krill_proto::telemetry::{self, ActivityGeneration, Env};
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Analytics {
    activity: Arc<ActivityBuffer>,
    _keys: Subscription,
    _task: Task<()>,
    _upload: Task<()>,
}
impl Global for Analytics {}

#[derive(Default)]
struct BufferedActivity {
    generation: Option<ActivityGeneration>,
    pending: Option<PendingActivity>,
}
struct PendingActivity {
    generation: ActivityGeneration,
    timestamp: u64,
}
#[derive(Default)]
struct ActivityBuffer(Mutex<BufferedActivity>);

impl ActivityBuffer {
    /// Input path: no IO, no waiting, and disabled/unknown policies retain nothing.
    fn retain(&self, timestamp: u64) {
        let Ok(mut state) = self.0.try_lock() else {
            return;
        };
        if state.pending.is_none()
            && let Some(generation) = state.generation.clone()
        {
            state.pending = Some(PendingActivity {
                generation,
                timestamp,
            });
        }
    }
    /// Background path: a changed policy drops every pending signal before it
    /// publishes the new generation. No timestamp/identity fallback is allowed.
    fn refresh_and_take(&self, generation: Option<ActivityGeneration>) -> Option<PendingActivity> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.generation != generation {
            state.pending = None;
            state.generation = generation;
        }
        state.pending.take()
    }
}

/// No key/button/content enters the buffer. Its first pending timestamp retains
/// the real interaction's UTC day even if background work is delayed.
pub fn user_activity(cx: &mut App) {
    if let Some(analytics) = cx.try_global::<Analytics>() {
        analytics.activity.retain(telemetry::now_unix());
    }
}

fn drain_activity(state: &Path, activity: &ActivityBuffer, env: &Env) -> io::Result<()> {
    let generation = match telemetry::activity_generation(state, env) {
        Ok(generation) => generation,
        Err(error) => {
            activity.refresh_and_take(None);
            return Err(error);
        }
    };
    if let Some(pending) = activity.refresh_and_take(generation) {
        // A disable/enable after the refresh above is rejected under queue.lock.
        telemetry::record_activity_for_generation(
            state,
            env,
            pending.timestamp,
            &pending.generation,
        )?;
    }
    Ok(())
}

/// Paint before page children so capture-phase input is observed even when a child
/// consumes it. The canvas adds no hitbox and no layout space.
pub fn activity_observer() -> impl IntoElement {
    gpui::canvas(|_, _, _| {}, |_, (), window, _| observe_window(window))
        .absolute()
        .size_full()
}

fn observe_window(window: &mut Window) {
    window.on_mouse_event(|_: &gpui::MouseDownEvent, phase, window, cx| {
        if phase == gpui::DispatchPhase::Capture && window.is_window_active() {
            user_activity(cx);
        }
    });
    window.on_mouse_event(|_: &gpui::ScrollWheelEvent, phase, window, cx| {
        if phase == gpui::DispatchPhase::Capture && window.is_window_active() {
            user_activity(cx);
        }
    });
}

fn observe_keys(cx: &mut App) -> Subscription {
    cx.intercept_keystrokes(|_, window, cx| {
        if window.is_window_active() {
            user_activity(cx);
        }
    })
}

pub fn start(cx: &mut App) {
    if cx.has_global::<Analytics>() {
        return;
    }
    let Some(state) = crate::settings::state_dir(cx) else {
        return;
    };
    let activity = Arc::new(ActivityBuffer::default());
    let keys = observe_keys(cx);
    let upload_state = state.clone();
    let upload = cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(2)).await;
        loop {
            let state = upload_state.clone();
            cx.background_executor()
                .spawn(async move {
                    send_pending(&state).ok();
                })
                .await;
            cx.background_executor()
                .timer(Duration::from_secs(60))
                .await;
        }
    });
    let pending = Arc::clone(&activity);
    let task = cx.spawn(async move |cx| {
        let startup = state.clone();
        let initial = Arc::clone(&pending);
        cx.background_executor()
            .spawn(async move {
                telemetry::record_open(&startup, &Env::live(), telemetry::now_unix()).ok();
                drain_activity(&startup, &initial, &Env::live()).ok();
            })
            .await;
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let state = state.clone();
            let pending = Arc::clone(&pending);
            cx.background_executor()
                .spawn(async move {
                    drain_activity(&state, &pending, &Env::live()).ok();
                })
                .await;
        }
    });
    cx.set_global(Analytics {
        activity,
        _keys: keys,
        _task: task,
        _upload: upload,
    });
}

fn send_pending(state: &Path) -> io::Result<()> {
    let status = telemetry::status_with(state, &Env::live())?;
    let now = telemetry::now_unix();
    if !status.enabled || status.pending == 0 || status.next_attempt_unix.is_some_and(|at| at > now)
    {
        return Ok(());
    }
    let mut child = krill_proto::os::background_command(krill_proto::daemon::sibling_exe("krill")?)
        .args(["telemetry", "flush", "--json"])
        .env("KRILL_STATE_DIR", state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other("telemetry helper failed"))
                };
            }
            Ok(None) if start.elapsed() < Duration::from_secs(40) => {
                std::thread::sleep(Duration::from_millis(25));
            }
            result => {
                child.kill().ok();
                child.wait().ok();
                return Err(result
                    .err()
                    .unwrap_or_else(|| io::ErrorKind::TimedOut.into()));
            }
        }
    }
}

#[cfg(test)]
mod tests;
