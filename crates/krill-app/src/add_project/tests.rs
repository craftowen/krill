use std::sync::{Arc, Mutex};

use gpui::TestAppContext;

use super::*;

#[test]
fn clone_dir_follows_git_default_name() {
    assert_eq!(
        clone_dir("https://github.com/o/repo.git").as_deref(),
        Some("repo")
    );
    assert_eq!(
        clone_dir("git@github.com:o/repo.git").as_deref(),
        Some("repo")
    );
    assert_eq!(clone_dir("https://host/o/repo/").as_deref(), Some("repo"));
    assert_eq!(clone_dir("host:repo").as_deref(), Some("repo"));
    assert_eq!(clone_dir("  "), None);
    assert_eq!(clone_dir("https://host/.."), None);
}

fn open(
    cx: &mut TestAppContext,
) -> (
    Entity<Dialog>,
    Arc<Mutex<Vec<Event>>>,
    &mut gpui::VisualTestContext,
) {
    cx.update(gpui_component::init);
    let (dialog, cx) = cx.add_window_view(Dialog::new);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&dialog, move |_, e: &Event, _| {
            sink.lock().unwrap().push(e.clone());
        })
        .detach();
    });
    (dialog, events, cx)
}

#[gpui::test]
fn enter_browses_and_escape_closes(cx: &mut TestAppContext) {
    let (_dialog, events, cx) = open(cx);
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("escape");
    assert_eq!(
        *events.lock().unwrap(),
        [Event::Browse(HostId::Local), Event::Close]
    );
}

#[gpui::test]
fn owe852_arrow_keys_move_selection_and_enter_activates(cx: &mut TestAppContext) {
    let (dialog, events, cx) = open(cx);
    assert_eq!(cx.update(|_, cx| dialog.read(cx).selected()), 0);
    cx.simulate_keystrokes("down");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).selected()), 1);
    cx.simulate_keystrokes("down");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).selected()), 2);
    cx.simulate_keystrokes("down");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).selected()), 0);
    cx.simulate_keystrokes("up");
    assert_eq!(cx.update(|_, cx| dialog.read(cx).selected()), 2);
    // 선택 2(만들기)에서 Enter는 Create를 내보낸다.
    cx.simulate_keystrokes("enter");
    assert_eq!(*events.lock().unwrap(), [Event::Create(HostId::Local)]);
}

#[gpui::test]
fn owe852_escape_in_clone_returns_to_start(cx: &mut TestAppContext) {
    let (dialog, events, cx) = open(cx);
    cx.update(|window, cx| dialog.update(cx, |d, cx| d.show_clone(window, cx)));
    cx.simulate_keystrokes("escape");
    assert!(
        events.lock().unwrap().is_empty(),
        "escape in clone step must not close"
    );
    cx.update(|_, cx| assert_eq!(dialog.read(cx).step, Step::Start));
    cx.simulate_keystrokes("escape");
    assert_eq!(*events.lock().unwrap(), [Event::Close]);
}

#[gpui::test]
fn clone_step_requires_url_then_emits_it(cx: &mut TestAppContext) {
    let (dialog, events, cx) = open(cx);
    cx.update(|window, cx| dialog.update(cx, |d, cx| d.show_clone(window, cx)));
    cx.simulate_keystrokes("enter");
    assert!(
        events.lock().unwrap().is_empty(),
        "enter in clone step must not browse"
    );
    cx.update(|_, cx| assert_eq!(dialog.read(cx).error, Some(text::BAD_URL.get())));
    cx.simulate_input("git@github.com:o/repo.git");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        *events.lock().unwrap(),
        [Event::Clone {
            host: HostId::Local,
            url: "git@github.com:o/repo.git".into(),
        }]
    );
}

#[gpui::test]
fn default_dialog_targets_local_host_only(cx: &mut TestAppContext) {
    let (dialog, _, cx) = open(cx);
    assert_eq!(cx.update(|_, cx| dialog.read(cx).hosts_len()), 1);
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).host().clone()),
        HostId::Local
    );
}

#[gpui::test]
fn new_with_hosts_selects_initial_or_falls_back_to_local(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (dialog, cx) = cx.add_window_view(|window, cx| {
        Dialog::new_with_hosts(
            window,
            cx,
            vec![
                (HostId::Local, "local".to_owned()),
                (HostId::Remote("r1".to_owned()), "r1".to_owned()),
            ],
            HostId::Remote("missing".to_owned()),
        )
    });
    assert_eq!(cx.update(|_, cx| dialog.read(cx).hosts_len()), 2);
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).host().clone()),
        HostId::Local
    );
}
