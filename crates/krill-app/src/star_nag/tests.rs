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
