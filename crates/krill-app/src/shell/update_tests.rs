use gpui::TestAppContext;
use krill_app::updater::{CheckStatus, RestartStatus, StagedUpdate, UpdateState};

use super::tests::{open, state_path};

fn ready() -> StagedUpdate {
    StagedUpdate {
        state_dir: "/test/update-card".into(),
        app_bundle: "/test/Krill.app".into(),
        from_version: "0.1.0".into(),
        to_version: "0.2.0".into(),
        manifest_url: "https://example.test/latest.json".into(),
        relaunched: false,
    }
}

#[gpui::test]
fn update_restart_failure_stays_visible_after_reopening_card(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("update-restart-error"));
    let staged = ready();
    cx.update(|_, cx| {
        cx.set_global(UpdateState {
            status: CheckStatus::Ready(staged.clone()),
            staged: Some(staged.clone()),
            restart: RestartStatus::Failed("launcher permission denied".into()),
            card_collapsed: false,
        });
        view.update(cx, |_, cx| cx.notify());
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("update-card-restart-error").is_some());
    assert!(cx.debug_bounds("update-card-restart").is_some());
    let minimize = cx.debug_bounds("update-card-minimize").unwrap();
    cx.simulate_click(minimize.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("update-card").is_none());
    let badge = cx.debug_bounds("status-update-ready").unwrap();
    cx.simulate_click(badge.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("update-card-restart-error").is_some());
    cx.update(|_, cx| {
        let state = cx.global::<UpdateState>();
        assert_eq!(state.staged.as_ref(), Some(&staged));
        assert_eq!(state.restart.error(), Some("launcher permission denied"));
    });
}

#[gpui::test]
fn update_restart_busy_card_does_not_launch_again(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("update-restart-busy"));
    let staged = ready();
    cx.update(|_, cx| {
        cx.set_global(UpdateState {
            status: CheckStatus::Ready(staged.clone()),
            staged: Some(staged.clone()),
            restart: RestartStatus::Starting,
            card_collapsed: false,
        });
        view.update(cx, |_, cx| cx.notify());
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("update-card-restart-error").is_none());
    let restart = cx.debug_bounds("update-card-restart").unwrap();
    cx.simulate_click(restart.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    cx.update(|_, cx| {
        let state = cx.global::<UpdateState>();
        assert!(state.restart.is_starting());
        assert_eq!(state.staged.as_ref(), Some(&staged));
    });
}
