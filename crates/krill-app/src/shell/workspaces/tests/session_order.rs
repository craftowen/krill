use super::*;

fn ordered_sessions(s: &Shell, cx: &App) -> Vec<SessionId> {
    s.sidebar
        .read(cx)
        .live_sessions_in_workspace(&HostId::Local, Path::new("/work/a"))
}

#[gpui::test]
fn session_order_split_focus_is_stable_and_pointer_drag_moves_the_group(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("session-order-split-drag"));
    let (left, right) = view.update(cx, |s, cx| {
        let mut panes = None;
        s.change(true, cx, |l| {
            let left = l.active_tab().focused;
            let launch = Create {
                account: None,
                program: Program::Login,
                cwd: b"/work/a".to_vec(),
                cols: 80,
                rows: 24,
            };
            l.set_launch(left, launch.clone());
            l.set_session(left, SessionId(3));
            l.split(Axis::Horizontal);
            let right = l.active_tab().focused;
            l.set_session(right, SessionId(1));
            l.set_launch(right, launch.clone());
            l.new_tab();
            let last = l.active_tab().focused;
            l.set_session(last, SessionId(2));
            l.set_launch(last, launch);
            panes = Some((left, right));
        });
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[1, 2, 3], "/work/a"))),
                cx,
            );
        });
        panes.unwrap()
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(
            ordered_sessions(s, cx),
            vec![SessionId(3), SessionId(1), SessionId(2)]
        );
        for pane in [left, right] {
            s.change(true, cx, |l| {
                l.activate(0);
                l.focus(pane);
            });
            assert_eq!(
                ordered_sessions(s, cx),
                vec![SessionId(3), SessionId(1), SessionId(2)]
            );
        }
    });
    cx.run_until_parked();
    let from = cx.debug_bounds("session-tab-0").unwrap().center();
    let to = cx.debug_bounds("session-tab-1").unwrap().center();
    let modifiers = Modifiers::default();
    cx.simulate_mouse_down(from, MouseButton::Left, modifiers);
    cx.simulate_mouse_move(
        point((from.x + to.x) / 2., (from.y + to.y) / 2.),
        MouseButton::Left,
        modifiers,
    );
    cx.simulate_mouse_move(to, MouseButton::Left, modifiers);
    cx.simulate_mouse_up(to, MouseButton::Left, modifiers);
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(s.layout.tab_panes(1), vec![left, right]);
        assert_eq!(
            ordered_sessions(s, cx),
            vec![SessionId(2), SessionId(3), SessionId(1)]
        );
    });
}

#[gpui::test]
fn session_order_follows_tabs_through_move_create_poll_and_restore(cx: &mut TestAppContext) {
    let path = state_path("session-order-parity");
    let (view, cx) = open(cx, path.clone());
    view.update(cx, |s, cx| {
        s.change(true, cx, |l| {
            for (i, id) in [3, 1, 2].into_iter().enumerate() {
                if i > 0 {
                    l.new_tab();
                }
                let pane = l.active_tab().focused;
                l.set_session(pane, SessionId(id));
                l.set_launch(
                    pane,
                    Create {
                        account: None,
                        program: Program::Login,
                        cwd: b"/work/a".to_vec(),
                        cols: 80,
                        rows: 24,
                    },
                );
            }
        });
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[1, 2, 3], "/work/a"))),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(
            ordered_sessions(s, cx),
            vec![SessionId(3), SessionId(1), SessionId(2)]
        );
        let focused = s.layout.focused();
        s.change(true, cx, |l| l.reorder_tab(0, 2));
        assert_eq!(s.layout.focused(), focused);
        assert_eq!(
            ordered_sessions(s, cx),
            vec![SessionId(1), SessionId(2), SessionId(3)]
        );
        s.change(true, cx, |l| {
            l.new_tab_after(0);
            let pane = l.active_tab().focused;
            l.set_launch(
                pane,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: b"/work/a".to_vec(),
                    cols: 80,
                    rows: 24,
                },
            );
            l.set_session(pane, SessionId(4));
        });
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[1, 2, 3, 4], "/work/a"))),
                cx,
            );
        });
    });
    cx.run_until_parked();
    let expected = vec![SessionId(1), SessionId(4), SessionId(2), SessionId(3)];
    view.update(cx, |s, cx| {
        assert_eq!(ordered_sessions(s, cx), expected);
        assert!(s.flush_layout_snapshot());
        s.saver.flush();
    });
    let restored = cx.update(|_, cx| cx.new(|cx| Shell::new(Some(path), cx)));
    cx.run_until_parked();
    restored.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[1, 2, 3, 4, 5], "/work/a"))),
                cx,
            );
        });
    });
    cx.run_until_parked();
    restored.update(cx, |s, cx| {
        let mut expected = expected;
        expected.push(SessionId(5));
        assert_eq!(ordered_sessions(s, cx), expected);
        let tabs: Vec<_> = s
            .workspace_tabs(cx)
            .into_iter()
            .filter_map(|i| s.layout.session(s.layout.tabs()[i].focused))
            .collect();
        assert_eq!(tabs, expected);
    });
}
