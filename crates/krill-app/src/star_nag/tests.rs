use super::*;
use gpui::{Context, Modifiers, MouseButton, Render, Window};

fn fresh(version: &str) -> State {
    let mut s = State::default();
    s.ensure_version(version);
    s
}

#[test]
fn due_after_initial_threshold_only() {
    let mut s = fresh("1.0.0");
    s.total = INITIAL_THRESHOLD - 1;
    assert!(!s.due(0));
    s.total = INITIAL_THRESHOLD;
    assert!(s.due(0));
}

#[test]
fn defer_doubles_threshold_rebases_and_cools_down() {
    let mut s = fresh("1.0.0");
    s.total = 40;
    s.defer(1_000);
    assert_eq!(
        (s.next_threshold, s.baseline, s.deferred_until),
        (70, 40, 1_000 + COOLDOWN_SECS)
    );
    s.total = 40 + 70;
    assert!(!s.due(1_000 + COOLDOWN_SECS - 1), "cooldown");
    assert!(s.due(1_000 + COOLDOWN_SECS));
}

#[test]
fn completed_never_due_and_survives_version_change() {
    let mut s = fresh("1.0.0");
    s.total = 1_000;
    s.complete();
    assert!(!s.due(u64::MAX));
    assert!(s.ensure_version("2.0.0"));
    assert!(s.completed && !s.due(u64::MAX));
}

#[test]
fn version_change_rebases_count() {
    let mut s = fresh("1.0.0");
    s.total = 50;
    s.next_threshold = 140;
    assert!(!s.ensure_version("1.0.0"));
    assert!(s.ensure_version("1.1.0"));
    assert_eq!((s.baseline, s.next_threshold), (50, INITIAL_THRESHOLD));
    assert!(!s.due(0));
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owe905-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn load_save_roundtrip_and_garbage_starts_fresh() {
    let dir = temp_dir("roundtrip");
    let mut s = fresh("1.0.0");
    s.total = 7;
    s.defer(5);
    save(&dir, &s).unwrap();
    assert_eq!(load(&dir), s);
    std::fs::write(dir.join(FILE), "not = [toml").unwrap();
    assert_eq!(load(&dir), State::default());
}

#[gpui::test]
fn explicit_flush_waits_for_startup_and_early_session_count(cx: &mut gpui::TestAppContext) {
    let dir = temp_dir("startup-flush");
    let mut previous = fresh(env!("CARGO_PKG_VERSION"));
    previous.total = 9;
    save(&dir, &previous).unwrap();
    cx.update(|cx| {
        init(dir.clone(), cx);
        record_agent_start(cx);
    });
    let barrier = cx.update(flush);
    cx.spawn(async move |_| {
        barrier.await;
        assert_eq!(
            load(&dir).total,
            10,
            "flush must include the startup completion's save"
        );
    })
    .detach();
    cx.run_until_parked();
}

struct Host;
impl Render for Host {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().relative().size_full().children(card(cx))
    }
}

fn click(selector: &'static str, cx: &mut gpui::VisualTestContext) {
    let bounds = cx.debug_bounds(selector).unwrap();
    cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::default());
}

fn nag(cx: &mut gpui::VisualTestContext) -> (State, Card) {
    cx.update(|_, cx| {
        let n = cx.global::<StarNag>();
        (n.state.clone(), n.card)
    })
}

#[gpui::test]
fn card_appears_at_threshold_and_buttons_update_state(cx: &mut gpui::TestAppContext) {
    let dir = temp_dir("card");
    cx.update(gpui_component::init);
    cx.update(|cx| init(dir.clone(), cx));
    let (_host, cx) = cx.add_window_view(|_, _| Host);

    for _ in 0..INITIAL_THRESHOLD - 1 {
        cx.update(|_, cx| record_agent_start(cx));
    }
    cx.run_until_parked();
    assert!(cx.debug_bounds("star-nag-card").is_none());

    cx.update(|_, cx| record_agent_start(cx));
    cx.run_until_parked();
    assert_eq!(nag(cx).1, Card::Shown);
    assert!(cx.debug_bounds("star-nag-card").is_some());

    click("star-nag-later", cx);
    cx.run_until_parked();
    let (state, shown) = nag(cx);
    assert_eq!(shown, Card::Hidden);
    assert_eq!(state.next_threshold, INITIAL_THRESHOLD * 2);
    assert_eq!(state.baseline, INITIAL_THRESHOLD);
    assert!(state.deferred_until > now());
    assert_eq!(load(&dir), state, "persisted");
    assert!(cx.debug_bounds("star-nag-card").is_none());

    // 쉬는 동안은 문턱을 넘어도 안 뜬다.
    for _ in 0..INITIAL_THRESHOLD * 2 {
        cx.update(|_, cx| record_agent_start(cx));
    }
    cx.run_until_parked();
    assert_eq!(nag(cx).1, Card::Hidden);

    // 쉼이 끝나면 다시 뜨고, [다시 보지 않기]는 영구 완료.
    cx.update(|_, cx| cx.global_mut::<StarNag>().state.deferred_until = 0);
    cx.update(|_, cx| record_agent_start(cx));
    cx.run_until_parked();
    assert_eq!(nag(cx).1, Card::Shown);
    click("star-nag-never", cx);
    cx.run_until_parked();
    assert!(nag(cx).0.completed);
    assert!(load(&dir).completed);
    for _ in 0..100 {
        cx.update(|_, cx| record_agent_start(cx));
    }
    cx.run_until_parked();
    assert_eq!(nag(cx).1, Card::Hidden);
}

#[gpui::test]
fn repeated_prompt_rechecks_stars_added_outside_app(cx: &mut gpui::TestAppContext) {
    let dir = temp_dir("external-star");
    let mut state = fresh(env!("CARGO_PKG_VERSION"));
    state.total = INITIAL_THRESHOLD - 1;
    save(&dir, &state).unwrap();
    cx.update(gpui_component::init);
    cx.update(|cx| init(dir.clone(), cx));
    cx.run_until_parked();
    cx.update(|cx| {
        cx.global_mut::<StarNag>().mock_gh = Some((Some(false), false));
    });
    let (_host, cx) = cx.add_window_view(|_, _| Host);
    cx.update(|_, cx| record_agent_start(cx));
    cx.run_until_parked();
    assert_eq!(nag(cx).1, Card::Shown);

    click("star-nag-later", cx);
    cx.run_until_parked();
    // The user stars on GitHub between prompts. Expire the cooldown and reach
    // the next threshold without restarting the app or clearing its cache.
    cx.update(|_, cx| {
        let nag = cx.global_mut::<StarNag>();
        nag.mock_gh = Some((Some(true), false));
        nag.state.deferred_until = 0;
        nag.state.total = nag.state.baseline + nag.state.next_threshold - 1;
        record_agent_start(cx);
    });
    cx.run_until_parked();
    assert_eq!(nag(cx).1, Card::Hidden);
    assert!(nag(cx).0.completed);
    assert!(
        load(&dir).completed,
        "verified star suppression is persisted"
    );
    assert!(cx.debug_bounds("star-nag-card").is_none());
}

#[gpui::test]
fn star_without_gh_opens_web_and_defers(cx: &mut gpui::TestAppContext) {
    let dir = temp_dir("star");
    let mut s = fresh(env!("CARGO_PKG_VERSION"));
    s.total = INITIAL_THRESHOLD - 1;
    save(&dir, &s).unwrap();
    cx.update(gpui_component::init);
    cx.update(|cx| init(dir.clone(), cx));
    let (_host, cx) = cx.add_window_view(|_, _| Host);
    cx.update(|_, cx| record_agent_start(cx));
    cx.run_until_parked();
    assert_eq!(nag(cx).1, Card::Shown);

    click("star-nag-star", cx);
    cx.run_until_parked();
    let (state, shown) = nag(cx);
    assert_eq!(shown, Card::Hidden);
    assert!(!state.completed, "web handoff is not a verified star");
    assert_eq!(state.next_threshold, INITIAL_THRESHOLD * 2);
    assert!(state.deferred_until > now());
}

#[test]
fn parse_gh_starred_output_distinguishes_three_states() {
    assert_eq!(
        parse_gh_starred_output(true, b"HTTP/2.0 204 No Content\r\n", b""),
        Some(true)
    );
    assert_eq!(
        parse_gh_starred_output(
            false,
            b"HTTP/2.0 404 Not Found\r\n",
            b"gh: Not Found (HTTP 404)"
        ),
        Some(false)
    );
    assert_eq!(
        parse_gh_starred_output(false, b"", b"gh: Not Found (HTTP 404)\n"),
        Some(false)
    );
    assert_eq!(
        parse_gh_starred_output(false, b"", b"error connecting to api.github.com"),
        None
    );
}

#[gpui::test]
fn gh_mode_direct_star_succeeds_or_falls_back_to_web_mode(cx: &mut gpui::TestAppContext) {
    let dir = temp_dir("gh-mode");
    let mut s = fresh(env!("CARGO_PKG_VERSION"));
    s.total = INITIAL_THRESHOLD - 1;
    save(&dir, &s).unwrap();
    cx.update(gpui_component::init);
    cx.update(|cx| init(dir.clone(), cx));
    cx.run_until_parked();
    cx.update(|cx| {
        cx.global_mut::<StarNag>().mock_gh = Some((Some(false), false));
    });
    let (_host, cx) = cx.add_window_view(|_, _| Host);

    cx.update(|_, cx| record_agent_start(cx));
    cx.run_until_parked();
    assert_eq!(nag(cx).1, Card::Shown);
    cx.update(|_, cx| {
        let n = cx.global::<StarNag>();
        assert_eq!(n.mode, Mode::Gh);
        assert_eq!(n.repo_star, RepoStar::NotStarred);
    });

    // Direct star failure switches card to Mode::Web instead of auto-opening browser.
    click("star-nag-star", cx);
    cx.run_until_parked();
    cx.update(|_, cx| {
        let n = cx.global::<StarNag>();
        assert_eq!(n.card, Card::Shown);
        assert_eq!(n.mode, Mode::Web);
        assert_eq!(n.repo_star, RepoStar::WebFallback);
        assert!(!n.state.completed);
    });

    // Now test direct star success when Mode::Gh succeeds.
    cx.update(|_, cx| {
        let n = cx.global_mut::<StarNag>();
        n.mode = Mode::Gh;
        n.repo_star = RepoStar::NotStarred;
        n.mock_gh = Some((Some(false), true));
    });
    click("star-nag-star", cx);
    cx.run_until_parked();
    cx.update(|_, cx| {
        let n = cx.global::<StarNag>();
        assert!(n.state.completed);
        assert_eq!(n.repo_star, RepoStar::Starred);
        assert_eq!(n.card, Card::Thanks);
    });
    cx.background_executor.advance_clock(THANKS_FOR);
    cx.run_until_parked();
    assert_eq!(nag(cx).1, Card::Hidden);
}

#[gpui::test]
fn agent_value_moment_shows_once_per_version_after_quiet_delay(cx: &mut gpui::TestAppContext) {
    let dir = temp_dir("value-moment");
    cx.update(gpui_component::init);
    cx.update(|cx| init(dir.clone(), cx));
    cx.run_until_parked();
    cx.update(|cx| {
        cx.global_mut::<StarNag>().mock_gh = Some((Some(false), true));
    });
    let (_host, cx) = cx.add_window_view(|_, _| Host);

    // Trigger value moment, then cancel by becoming active before delay finishes.
    cx.update(|_, cx| observe_agent_activity(false, true, cx));
    cx.background_executor
        .advance_clock(Duration::from_millis(600));
    cx.update(|_, cx| observe_agent_activity(true, false, cx));
    cx.background_executor.advance_clock(VALUE_MOMENT_DELAY);
    cx.run_until_parked();
    assert_eq!(nag(cx).1, Card::Hidden);

    // Trigger value moment and stay idle for 1200ms -> card shows once for this version.
    cx.update(|_, cx| observe_agent_activity(false, true, cx));
    cx.background_executor.advance_clock(VALUE_MOMENT_DELAY);
    cx.run_until_parked();
    let (state, shown) = nag(cx);
    assert_eq!(shown, Card::Shown);
    assert_eq!(
        state.agent_value_moment_app_version,
        env!("CARGO_PKG_VERSION")
    );
}

struct SurfaceHost {
    has_projects: bool,
}
impl Render for SurfaceHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .children(landing_star_pill(self.has_projects, cx))
            .child(settings_support_control(cx))
    }
}

#[gpui::test]
fn landing_pill_and_settings_support_share_star_state(cx: &mut gpui::TestAppContext) {
    let dir = temp_dir("surfaces");
    cx.update(gpui_component::init);
    cx.update(|cx| init(dir.clone(), cx));
    cx.run_until_parked();
    cx.update(|cx| {
        cx.global_mut::<StarNag>().mock_gh = Some((Some(false), true));
        ensure_checked(cx);
    });
    let (host, cx) = cx.add_window_view(|_, _| SurfaceHost {
        has_projects: false,
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("landing-star-pill").is_some());
    assert!(cx.debug_bounds("settings-star-github").is_some());
    assert!(cx.debug_bounds("settings-star-thanks").is_none());

    // Clicking settings star button stars via gh and updates both settings and landing pill.
    click("settings-star-github", cx);
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-star-thanks").is_some());
    assert!(cx.debug_bounds("landing-star-pill").is_some());
    assert!(nag(cx).0.completed);

    // Clicking starred landing pill opens Hide menu; clicking Hide dismisses the pill.
    click("landing-star-pill", cx);
    cx.run_until_parked();
    assert!(cx.debug_bounds("landing-star-hide").is_some());
    click("landing-star-hide", cx);
    cx.run_until_parked();
    assert!(cx.debug_bounds("landing-star-pill").is_none());

    // When unhidden and has_projects is true while Starred, landing pill is hidden automatically.
    cx.update(|_, cx| {
        cx.global_mut::<StarNag>().landing_hidden = false;
        host.update(cx, |h, cx| {
            h.has_projects = true;
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("landing-star-pill").is_none());
}
