use std::sync::{Arc, Mutex};

use gpui::TestAppContext;

use super::*;

#[test]
fn remote_project_paths_keep_the_target_platform_syntax() {
    for raw in [
        "/srv/repo",
        r"C:\remote\repo",
        "c:/remote/repo",
        r"\\server\share\repo",
    ] {
        assert_eq!(host_path(raw), Some(PathBuf::from(raw)), "{raw}");
    }
    assert_eq!(
        host_path(r"  C:\remote\repo  "),
        Some(PathBuf::from(r"C:\remote\repo"))
    );
    for raw in [
        "",
        "repo",
        "C:repo",
        r"\repo",
        r"C:\",
        r"\\server\share",
        "/",
        "/..",
        "/repo\0bad",
    ] {
        assert!(host_path(raw).is_none(), "{raw:?}");
    }
}

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

#[gpui::test]
fn add_project_clone_actions_stay_inside_small_viewport_at_large_zoom(cx: &mut TestAppContext) {
    let (dialog, _, cx) = open(cx);
    cx.update(|window, cx| dialog.update(cx, |d, cx| d.show_clone(window, cx)));
    cx.simulate_resize(gpui::size(gpui::px(640.), gpui::px(480.)));
    cx.update(|window, _| window.set_rem_size(gpui::px(32.)));
    cx.run_until_parked();
    let card = cx.debug_bounds("add-project-dialog").unwrap();
    let actions = cx.debug_bounds("add-project-actions").unwrap();
    let body = cx.debug_bounds("add-project-body").unwrap();
    assert!(body.size.height > gpui::px(0.) && body.bottom() <= actions.top());
    let submit = cx.debug_bounds("add-project-clone-submit").unwrap();
    assert!(
        card.size.width <= gpui::px(608.) && card.size.height <= gpui::px(448.),
        "{card:?}"
    );
    assert!(
        actions.left() >= card.left()
            && actions.right() <= card.right()
            && actions.top() >= card.top()
            && actions.bottom() <= card.bottom(),
        "{actions:?} outside {card:?}"
    );
    assert!(
        submit.left() >= card.left()
            && submit.right() <= card.right()
            && submit.top() >= card.top()
            && submit.bottom() <= card.bottom(),
        "{submit:?} outside {card:?}"
    );
}

#[gpui::test]
fn add_project_ignores_escape_during_ime_preedit(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;

    cx.update(gpui_component::init);
    let remote = HostId::Remote("mini".to_owned());
    let (dialog, cx) = cx.add_window_view(|window, cx| {
        Dialog::new_with_hosts(
            window,
            cx,
            vec![
                (HostId::Local, "local".to_owned()),
                (remote.clone(), "mini".to_owned()),
            ],
            remote,
        )
    });
    cx.run_until_parked();

    // Clone step: url & dest IME preedit must not return to Step::Start on Escape.
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.show_clone(window, cx);
            d.url.update(cx, |input, cx| {
                input.focus(window, cx);
                input.replace_and_mark_text_in_range(None, "ㅎ", None, window, cx);
            });
            assert!(d.input_is_composing(window, cx));
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|_, cx| assert_eq!(dialog.read(cx).step, Step::Clone));

    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.url.update(cx, |input, cx| input.unmark_text(window, cx));
            d.dest.update(cx, |input, cx| {
                input.focus(window, cx);
                input.replace_and_mark_text_in_range(None, "한", None, window, cx);
            });
            assert!(d.input_is_composing(window, cx));
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|_, cx| assert_eq!(dialog.read(cx).step, Step::Clone));

    // RemotePath step: remote_path IME preedit must not return to Step::Start on Escape.
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.dest.update(cx, |input, cx| input.unmark_text(window, cx));
            d.show_start(window, cx);
            d.browse_activated(window, cx);
            d.remote_path.update(cx, |input, cx| {
                input.focus(window, cx);
                input.replace_and_mark_text_in_range(None, "경", None, window, cx);
            });
            assert!(d.input_is_composing(window, cx));
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(dialog.read(cx).step, Step::RemotePath(RemoteMode::Browse));
    });

    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.remote_path
                .update(cx, |input, cx| input.unmark_text(window, cx));
            assert!(!d.input_is_composing(window, cx));
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|_, cx| assert_eq!(dialog.read(cx).step, Step::Start));
}
