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
    assert_eq!(*events.lock().unwrap(), [Event::Browse, Event::Close]);
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
        [Event::Clone("git@github.com:o/repo.git".into())]
    );
}
