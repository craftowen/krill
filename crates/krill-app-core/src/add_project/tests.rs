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

#[gpui::test]
fn tab_focused_host_chip_and_action_cards_activate_on_enter_and_space(cx: &mut TestAppContext) {
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
            HostId::Local,
        )
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&dialog, move |_, e: &Event, _| {
            sink.lock().unwrap().push(e.clone());
        })
        .detach();
    });
    cx.run_until_parked();

    // Tab 1: close button, Tab 2: local host chip, Tab 3: remote host chip ("mini")
    cx.simulate_keystrokes("tab tab tab space");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(dialog.read(cx).host(), &remote);
    });

    // Tab 4: Browse card -> Enter opens RemotePath(Browse) for remote host
    cx.simulate_keystrokes("tab enter");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(dialog.read(cx).step, Step::RemotePath(RemoteMode::Browse));
    });

    // Escape returns to Start, then Tab to Clone card (close=1, local=2, mini=3, browse=4, clone=5) and press Space
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(dialog.read(cx).step, Step::Start);
    });
    cx.simulate_keystrokes("tab tab tab tab tab space");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(dialog.read(cx).step, Step::Clone);
    });
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn remote_browser_path_helpers_handle_posix_and_windows() {
    assert_eq!(
        remote_breadcrumbs("/Users/macmini", false),
        vec![
            ("/".to_owned(), "/".to_owned()),
            ("Users".to_owned(), "/Users".to_owned()),
            ("macmini".to_owned(), "/Users/macmini".to_owned()),
        ]
    );
    assert_eq!(remote_parent_path("/Users/macmini", false), "/Users");
    assert_eq!(remote_parent_path("/Users", false), "/");
    assert_eq!(remote_parent_path("/", false), "/");
    assert_eq!(
        join_remote_path("/Users/macmini", "Documents", false),
        "/Users/macmini/Documents"
    );
    assert_eq!(join_remote_path("/", "Users", false), "/Users");

    assert_eq!(
        remote_breadcrumbs(r"C:\Users\dev", true),
        vec![
            ("/".to_owned(), "/".to_owned()),
            (r"C:\".to_owned(), r"C:\".to_owned()),
            ("Users".to_owned(), r"C:\Users".to_owned()),
            ("dev".to_owned(), r"C:\Users\dev".to_owned()),
        ]
    );
    assert_eq!(remote_parent_path(r"C:\Users\dev", true), r"C:\Users");
    assert_eq!(remote_parent_path(r"C:\Users", true), r"C:\");
    assert_eq!(remote_parent_path(r"C:\", true), "/");
    assert_eq!(join_remote_path("/", r"C:\", true), r"C:\");
    assert_eq!(join_remote_path(r"C:\", "Users", true), r"C:\Users");

    assert!(!looks_like_browse_path("haesol", false));
    assert!(looks_like_browse_path("/Users", false));
    assert!(looks_like_browse_path("~", false));
    assert!(looks_like_browse_path("..", false));
    assert!(looks_like_browse_path(r"C:\work", true));
}

#[gpui::test]
fn remote_folder_browser_loads_filters_navigates_and_selects_folder(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let remote = HostId::Remote("macmini".to_owned());
    let browser: RemoteBrowserFn = Arc::new(|_host, dir| {
        let dir_entry = |name: &str| WireBrowseEntry {
            name: name.to_owned(),
            is_dir: true,
            is_symlink: false,
        };
        match dir {
            "" | "~" | "/Users/macmini" => Ok(WireBrowseListing {
                resolved_path: "/Users/macmini".to_owned(),
                is_windows: false,
                entries: vec![dir_entry("Desktop"), dir_entry("Documents")],
            }),
            "/Users/macmini/Documents" => Ok(WireBrowseListing {
                resolved_path: "/Users/macmini/Documents".to_owned(),
                is_windows: false,
                entries: vec![dir_entry("wProject")],
            }),
            "/Users/macmini/Documents/wProject" => Ok(WireBrowseListing {
                resolved_path: "/Users/macmini/Documents/wProject".to_owned(),
                is_windows: false,
                entries: vec![dir_entry("haesol-tax-system")],
            }),
            other => Err(format!("not found: {other}")),
        }
    });
    let (dialog, cx) = cx.add_window_view(|window, cx| {
        Dialog::new_with_hosts(
            window,
            cx,
            vec![
                (HostId::Local, "local".to_owned()),
                (remote.clone(), "macmini".to_owned()),
            ],
            remote.clone(),
        )
        .with_remote_browser(browser)
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&dialog, move |_, e: &Event, _| {
            sink.lock().unwrap().push(e.clone());
        })
        .detach();
    });
    cx.run_until_parked();

    // Enter activates remote browse -> automatically loads remote home /Users/macmini
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let d = dialog.read(cx);
        assert_eq!(d.step, Step::RemotePath(RemoteMode::Browse));
        assert_eq!(d.browse_dir.as_deref(), Some("/Users/macmini"));
        assert_eq!(d.visible_browse_entries(cx).len(), 2);
    });

    // Typing filter narrows visible folders to "Documents", Enter navigates into it
    cx.simulate_input("doc");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let d = dialog.read(cx);
        let names: Vec<&str> = d
            .visible_browse_entries(cx)
            .into_iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, ["Documents"]);
    });
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(
            dialog.read(cx).browse_dir.as_deref(),
            Some("/Users/macmini/Documents")
        );
    });

    // Typing an explicit path jumps directly to /Users/macmini/Documents/wProject
    cx.simulate_input("/Users/macmini/Documents/wProject");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(
            dialog.read(cx).browse_dir.as_deref(),
            Some("/Users/macmini/Documents/wProject")
        );
    });

    // Confirming opens the currently browsed folder on the remote host
    cx.update(|window, cx| {
        dialog.update(cx, |d, cx| d.confirm_remote_browser(window, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        *events.lock().unwrap(),
        [Event::OpenPath(
            remote,
            PathBuf::from("/Users/macmini/Documents/wProject")
        )]
    );
}
