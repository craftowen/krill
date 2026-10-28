use super::*;
use gpui::{
    Context, FocusHandle, InteractiveElement as _, MouseButton, ParentElement as _, Render,
    TestAppContext, VisualTestContext,
};
use std::path::PathBuf;

struct InputProbe {
    focus: FocusHandle,
}
impl Render for InputProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::div()
            .id("telemetry-probe")
            .relative()
            .size(gpui::px(200.))
            .track_focus(&self.focus)
            .child(activity_observer())
            .child(
                gpui::div()
                    .id("telemetry-consumer")
                    .debug_selector(|| "telemetry-consumer".into())
                    .size_full()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
            )
            .on_key_down(|_, _, cx| cx.stop_propagation())
    }
}
fn state_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "krill-activity-policy-{}",
        telemetry::random_uuid().unwrap()
    ))
}
fn open(cx: &mut TestAppContext, activity: Arc<ActivityBuffer>) -> &mut VisualTestContext {
    cx.update(|cx| {
        let keys = observe_keys(cx);
        let task = cx.spawn(async |_| {});
        let upload = cx.spawn(async |_| {});
        cx.set_global(Analytics {
            activity,
            _keys: keys,
            _task: task,
            _upload: upload,
        });
    });
    let (_, cx) = cx.add_window_view(|window, cx| {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        window.activate_window();
        InputProbe { focus }
    });
    cx.run_until_parked();
    cx
}
fn click(cx: &mut VisualTestContext) {
    let bounds = cx.debug_bounds("telemetry-consumer").unwrap();
    cx.simulate_mouse_down(
        bounds.center(),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(
        bounds.center(),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
}
fn has_pending(buffer: &ActivityBuffer) -> bool {
    buffer.0.lock().unwrap().pending.is_some()
}
fn pending_timestamp(buffer: &ActivityBuffer) -> u64 {
    buffer.0.lock().unwrap().pending.as_ref().unwrap().timestamp
}
fn event_times(state: &Path) -> Vec<u64> {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state.join("telemetry/state.json")).unwrap())
            .unwrap();
    value["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["ts_unix"].as_u64().unwrap())
        .collect()
}

#[gpui::test]
fn idle_paint_does_not_count_but_consumed_foreground_input_does(cx: &mut TestAppContext) {
    let state = state_dir();
    let pending = Arc::new(ActivityBuffer::default());
    let generation = telemetry::activity_generation(&state, &Env::default()).unwrap();
    pending.refresh_and_take(generation.clone());
    let cx = open(cx, Arc::clone(&pending));
    assert!(
        !has_pending(&pending),
        "paint and activation alone must not count"
    );
    click(cx);
    assert!(
        pending.refresh_and_take(generation.clone()).is_some(),
        "capture must observe a consumed click"
    );
    cx.simulate_keystrokes("a");
    assert!(
        pending.refresh_and_take(generation).is_some(),
        "interceptor must observe a consumed key"
    );
    cx.run_until_parked();
    assert!(!has_pending(&pending), "idle must not create activity");
    std::fs::remove_dir_all(state).ok();
}

#[gpui::test]
fn pending_input_before_or_during_disable_cannot_cross_rapid_reenable(cx: &mut TestAppContext) {
    let state = state_dir();
    let buffer = Arc::new(ActivityBuffer::default());
    drain_activity(&state, &buffer, &Env::default()).unwrap();
    let cx = open(cx, Arc::clone(&buffer));
    for input_during_disable in [false, true] {
        if input_during_disable {
            telemetry::disable(&state).unwrap();
        }
        click(cx);
        assert!(
            has_pending(&buffer),
            "stale cache can retain only its old generation"
        );
        if !input_during_disable {
            telemetry::disable(&state).unwrap();
        }
        telemetry::enable(&state).unwrap();
        drain_activity(&state, &buffer, &Env::default()).unwrap();
        assert!(!has_pending(&buffer));
        assert_eq!(
            telemetry::status_with(&state, &Env::default())
                .unwrap()
                .pending,
            0
        );
    }
    cx.simulate_keystrokes("a");
    let original_time = pending_timestamp(&buffer);
    drain_activity(&state, &buffer, &Env::default()).unwrap();
    assert_eq!(
        event_times(&state),
        [original_time],
        "only post-refresh fresh input is counted"
    );
    std::fs::remove_dir_all(state).ok();
}

#[gpui::test]
fn initially_disabled_without_identity_does_not_retain_input_for_later_enable(
    cx: &mut TestAppContext,
) {
    let state = state_dir();
    telemetry::disable(&state).unwrap();
    let buffer = Arc::new(ActivityBuffer::default());
    drain_activity(&state, &buffer, &Env::default()).unwrap();
    assert!(
        telemetry::status_with(&state, &Env::default())
            .unwrap()
            .install_id
            .is_none()
    );
    let cx = open(cx, Arc::clone(&buffer));
    click(cx);
    cx.simulate_keystrokes("a");
    assert!(!has_pending(&buffer));
    telemetry::enable(&state).unwrap();
    click(cx);
    assert!(
        !has_pending(&buffer),
        "unknown cached policy is never a fallback"
    );
    drain_activity(&state, &buffer, &Env::default()).unwrap();
    assert!(event_times(&state).is_empty());
    cx.simulate_keystrokes("a");
    let original_time = pending_timestamp(&buffer);
    drain_activity(&state, &buffer, &Env::default()).unwrap();
    assert_eq!(event_times(&state), [original_time]);
    std::fs::remove_dir_all(state).ok();
}

#[gpui::test]
fn taken_input_is_rechecked_after_policy_refresh_before_queue_record(cx: &mut TestAppContext) {
    let state = state_dir();
    let buffer = Arc::new(ActivityBuffer::default());
    drain_activity(&state, &buffer, &Env::default()).unwrap();
    let cx = open(cx, Arc::clone(&buffer));
    click(cx);
    // The actual hook fed this signal. Pause the drain at its real policy/token
    // boundary, then toggle before the record API acquires queue.lock.
    let policy = telemetry::activity_generation(&state, &Env::default()).unwrap();
    let pending = buffer.refresh_and_take(policy).unwrap();
    telemetry::disable(&state).unwrap();
    telemetry::enable(&state).unwrap();
    assert!(
        !telemetry::record_activity_for_generation(
            &state,
            &Env::default(),
            pending.timestamp,
            &pending.generation
        )
        .unwrap()
    );
    assert!(event_times(&state).is_empty());
    std::fs::remove_dir_all(state).ok();
}

#[test]
fn deferred_input_preserves_original_utc_timestamp_and_env_veto_clears_cache() {
    let state = state_dir();
    let buffer = ActivityBuffer::default();
    drain_activity(&state, &buffer, &Env::default()).unwrap();
    buffer.retain(86_399);
    buffer.retain(86_400);
    drain_activity(&state, &buffer, &Env::default()).unwrap();
    buffer.retain(86_400);
    drain_activity(&state, &buffer, &Env::default()).unwrap();
    assert_eq!(event_times(&state), [86_399, 86_400]);
    buffer.retain(172_800);
    drain_activity(
        &state,
        &buffer,
        &Env::stub(&[(telemetry::DO_NOT_TRACK, "1")]),
    )
    .unwrap();
    assert!(!has_pending(&buffer));
    buffer.retain(172_800);
    assert!(!has_pending(&buffer));
    drain_activity(&state, &buffer, &Env::default()).unwrap();
    assert_eq!(event_times(&state), [86_399, 86_400]);
    std::fs::remove_dir_all(state).ok();
}
