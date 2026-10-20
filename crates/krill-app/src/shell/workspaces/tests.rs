use super::*;
use crate::shell::tests::{layout_of, open, state_path};
use gpui::{Modifiers, TestAppContext};
use krill_proto::SessionId;

/// A, B, A, B 순으로 섞인 저장 레이아웃. 화면 인덱스와 전체 인덱스가 다르다.
fn fixture(s: &mut Shell, cx: &mut Context<Shell>) -> Vec<PaneId> {
    let mut panes = Vec::new();
    s.change(true, cx, |l| {
        for (i, folder) in [
            "/workspace/a",
            "/workspace/b",
            "/workspace/a",
            "/workspace/b",
        ]
        .into_iter()
        .enumerate()
        {
            if i > 0 {
                l.new_tab();
            }
            let pane = l.active_tab().focused;
            l.set_launch(
                pane,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: folder.as_bytes().to_vec(),
                    cols: 80,
                    rows: 24,
                },
            );
            l.set_session(pane, SessionId(100 + i as u64));
            l.set_tab_title(i, Some("terminal"));
            panes.push(pane);
        }
        l.activate(2);
    });
    panes
}

#[gpui::test]
fn workspace_tabs_render_click_cycle_and_restore(cx: &mut TestAppContext) {
    let path = state_path("workspace-tabs");
    let (view, cx) = open(cx, path.clone());
    let panes = view.update(cx, fixture);
    cx.run_until_parked();
    assert!(cx.debug_bounds("session-tab-0").is_some());
    assert!(cx.debug_bounds("session-tab-2").is_some());
    assert!(cx.debug_bounds("session-tab-1").is_none());
    assert!(cx.debug_bounds("session-tab-3").is_none());
    view.update(cx, |s, cx| {
        assert_eq!(
            s.workspace_tab_labels(cx),
            vec![(0, "terminal".into()), (2, "terminal 2".into()),]
        );
    });

    // 화면의 두 번째 탭은 전체 index=2. 클릭과 번호 키가 같은 대상을 가리킨다.
    cx.simulate_keystrokes("cmd-1");
    assert_eq!(layout_of(&view, cx).active(), 0);
    cx.run_until_parked();
    let tab = cx.debug_bounds("session-tab-2").unwrap().center();
    cx.simulate_click(tab, Modifiers::default());
    assert_eq!(layout_of(&view, cx).active(), 2);
    cx.simulate_keystrokes("cmd-2");
    assert_eq!(layout_of(&view, cx).active(), 2);
    cx.simulate_keystrokes("ctrl-pagedown");
    assert_eq!(layout_of(&view, cx).active(), 0);
    cx.simulate_keystrokes("ctrl-pageup");
    assert_eq!(layout_of(&view, cx).active(), 2);
    cx.simulate_keystrokes("ctrl-tab");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 0);
    assert!(cx.debug_bounds("recent-tab-item-1").is_none());
    assert!(cx.debug_bounds("recent-tab-item-3").is_none());
    cx.simulate_modifiers_change(Modifiers::default());

    // 사이드바/알림의 세션 선택은 다른 워크스페이스로 전환할 수 있다.
    view.update(cx, |s, cx| s.open_session(SessionId(101), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("session-tab-0").is_none());
    assert!(cx.debug_bounds("session-tab-1").is_some());
    assert!(cx.debug_bounds("session-tab-3").is_some());
    let saved = layout_of(&view, cx);
    for (pane, id) in panes.iter().zip(100..104) {
        assert_eq!(saved.session(*pane), Some(SessionId(id)));
    }
    let restored = cx.update(|_, cx| cx.new(|cx| Shell::new(Some(path), cx)));
    cx.run_until_parked();
    restored.update(cx, |s, cx| assert_eq!(s.workspace_tabs(cx), vec![1, 3]));
}

#[gpui::test]
fn workspace_tabs_bulk_close_preserves_hidden_sessions(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("workspace-close"));
    let panes = view.update(cx, fixture);
    view.update(cx, |s, cx| s.close_other_tabs(2, cx));
    let layout = layout_of(&view, cx);
    assert_eq!(layout.tabs().len(), 3);
    assert!(!layout.pane_exists(panes[0]));
    assert!(layout.pane_exists(panes[1]));
    assert!(layout.pane_exists(panes[3]));
    assert_eq!(layout.focused(), Some(panes[2]));
    view.update(cx, |s, _| {
        assert_eq!(s.closed_sessions, vec![SessionId(100)]);
    });
}

#[gpui::test]
fn workspace_tabs_close_active_selects_another_tab_in_same_workspace(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("workspace-close-active"));
    let panes = view.update(cx, fixture);
    view.update(cx, |s, cx| {
        s.change(true, cx, |l| l.activate(0));
        s.do_close_tab(0, cx);
        assert_eq!(s.layout.focused(), Some(panes[2]));
        assert_eq!(s.workspace_tabs(cx), vec![1]);
    });
}

#[gpui::test]
fn workspace_tabs_close_active_prefers_adjacent_tabs_in_same_workspace(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("workspace-close-adjacent"));
    let panes = view.update(cx, fixture);
    let right = view.update(cx, |s, cx| {
        s.change(true, cx, |l| {
            l.new_tab_after(l.tabs().len() - 1);
            let pane = l.active_tab().focused;
            l.set_launch(
                pane,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: b"/workspace/a".to_vec(),
                    cols: 80,
                    rows: 24,
                },
            );
            l.set_session(pane, SessionId(104));
            l.activate(2);
        });
        s.layout.tabs()[4].focused
    });
    view.update(cx, |s, cx| {
        s.do_close_tab(2, cx);
        assert_eq!(s.layout.focused(), Some(right));
        s.do_close_tab(s.layout.active(), cx);
        assert_eq!(s.layout.focused(), Some(panes[0]));
        assert_eq!(s.closed_sessions, vec![SessionId(102), SessionId(104)]);
        for (pane, session) in [(panes[1], SessionId(101)), (panes[3], SessionId(103))] {
            assert_eq!(s.layout.session(pane), Some(session));
        }
    });
}

#[gpui::test]
fn workspace_tabs_close_single_pane_stays_in_workspace(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("workspace-close-pane"));
    let panes = view.update(cx, fixture);
    view.update(cx, |s, cx| {
        s.change(true, cx, |l| l.activate(0));
        s.do_close_pane(panes[0], cx);
        assert_eq!(s.layout.focused(), Some(panes[2]));
        assert_eq!(s.closed_sessions, vec![SessionId(100)]);
    });
}

#[gpui::test]
fn workspace_tabs_close_right_keeps_hidden_tabs_and_current_focus(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("workspace-close-right"));
    let panes = view.update(cx, fixture);
    view.update(cx, |s, cx| {
        s.change(true, cx, |l| l.activate(0));
        s.close_tabs_to_right(0, cx);
        assert_eq!(s.layout.focused(), Some(panes[0]));
        assert_eq!(s.closed_sessions, vec![SessionId(102)]);
        assert!(s.layout.pane_exists(panes[1]));
        assert!(s.layout.pane_exists(panes[3]));
    });
}

#[gpui::test]
fn workspace_tabs_resolve_nested_worktrees_without_following_live_cwd(cx: &mut TestAppContext) {
    struct Memory(sidebar::Snapshot);
    impl sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }
    let (view, cx) = open(cx, state_path("workspace-roots"));
    view.update(cx, |s, cx| {
        fixture(s, cx);
        s.change(true, cx, |l| {
            for (i, cwd) in [
                "/repo/src",
                "/repo/.worktrees/b",
                "/repo/tests",
                "/repo/.worktrees/b/src",
            ]
            .into_iter()
            .enumerate()
            {
                let pane = l.tabs()[i].focused;
                let mut launch = l.launch(pane).unwrap().clone();
                launch.cwd = cwd.as_bytes().to_vec();
                l.set_launch(pane, launch);
                l.set_respawn_cwd(pane, b"/somewhere/else".to_vec());
            }
        });
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(Memory(sidebar::Snapshot {
                    sessions: Vec::new(),
                    trees: BTreeMap::from([(
                        PathBuf::from("/repo"),
                        vec![PathBuf::from("/repo/.worktrees/b")],
                    )]),
                })),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(s.workspace_tabs(cx), vec![0, 2]);
        s.change(true, cx, |l| l.activate(1));
        assert_eq!(s.workspace_tabs(cx), vec![1, 3]);
    });
}

#[gpui::test]
fn workspace_tabs_keep_split_and_remote_host_identity(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("workspace-hosts"));
    view.update(cx, |s, cx| {
        fixture(s, cx);
        s.change(true, cx, |l| {
            l.set_host(l.tabs()[0].focused, HostId::Remote("server".into()));
            l.split(Axis::Horizontal);
            let right = l.focused().unwrap();
            let mut launch = l.launch(right).unwrap().clone();
            launch.cwd = b"/different/split-folder".to_vec();
            l.set_launch(right, launch);
        });
        assert_eq!(s.workspace_tabs(cx), vec![2]);
        s.change(true, cx, |l| l.activate(0));
        assert_eq!(s.workspace_tabs(cx), vec![0]);
    });
}

#[gpui::test]
fn workspace_tabs_preview_does_not_replace_other_workspace(cx: &mut TestAppContext) {
    let path = state_path("workspace-previews");
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let file_a = dir.join("a.rs");
    let file_b = dir.join("b.rs");
    std::fs::write(&file_a, "fn a() {}\n").unwrap();
    std::fs::write(&file_b, "fn b() {}\n").unwrap();
    let (view, cx) = open(cx, path);
    view.update(cx, |s, cx| {
        let panes = fixture(s, cx);
        s.open_file_preview(file_a.clone(), cx);
        let preview_a = s.layout.focused().unwrap();
        s.change(true, cx, |l| {
            l.activate(1);
            l.focus(panes[1]);
        });
        s.open_file_preview(file_b.clone(), cx);
        assert_ne!(s.layout.focused(), Some(preview_a));
        assert_eq!(s.layout.file(preview_a), Some(file_a.as_path()));
        assert!(
            !s.workspace_tabs(cx)
                .iter()
                .any(|&i| s.layout.tabs()[i].focused == preview_a)
        );
    });
}
