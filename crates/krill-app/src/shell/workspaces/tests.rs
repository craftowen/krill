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
fn workspace_tabs_closing_last_tab_keeps_empty_workspace(cx: &mut TestAppContext) {
    let path = state_path("workspace-close-last");
    let (view, cx) = open(cx, path.clone());
    let panes = view.update(cx, fixture);
    view.update(cx, |s, cx| {
        s.do_close_tab(2, cx);
        s.do_close_tab(s.layout.active(), cx);
        // 다른 워크스페이스(B)로 넘어가지 않고 탭 없는 빈 화면으로 남는다.
        assert_eq!(s.layout.focused(), None);
        assert!(s.workspace_tabs(cx).is_empty());
        assert_eq!(s.layout.tabs().len(), 2);
        assert!(s.layout.pane_exists(panes[1]));
        assert!(s.layout.pane_exists(panes[3]));
        assert_eq!(s.closed_sessions, vec![SessionId(102), SessionId(100)]);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("empty-workspace").is_some());
    assert!(cx.debug_bounds("session-tab-1").is_none());
    assert!(cx.debug_bounds("session-tab-3").is_none());

    assert_eq!(layout_of(&view, cx).focused(), None);
    let restored = cx.update(|_, cx| cx.new(|cx| Shell::new(Some(path), cx)));
    cx.run_until_parked();
    restored.update(cx, |s, cx| {
        assert_eq!(s.layout.tabs().len(), 2);
        assert_eq!(s.layout.focused(), None);
        assert!(s.workspace_tabs(cx).is_empty());
    });

    // 빈 화면에서 새 탭을 열면 다시 탭이 생긴다.
    view.update(cx, |s, cx| {
        s.new_tab_on_launch_host(cx);
        assert_eq!(s.layout.tabs().len(), 3);
        assert!(s.layout.focused().is_some());
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

struct LiveSnapshot(sidebar::Snapshot);
impl sidebar::Source for LiveSnapshot {
    fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
        Ok(self.0.clone())
    }
}

fn live_session(id: u64, folder: &str) -> sidebar::Session {
    sidebar::Session {
        id: SessionId(id),
        status: krill_proto::control::AgentStatus::UNKNOWN,
        worktree: Some(folder.into()),
        cwd: Some(folder.into()),
        account: None,
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "test fixtures always use a workspace with an absolute parent"
)]
fn live_snapshot(ids: &[u64], folder: &str) -> sidebar::Snapshot {
    sidebar::Snapshot {
        sessions: ids.iter().map(|&id| live_session(id, folder)).collect(),
        trees: BTreeMap::from([(
            PathBuf::from(folder).parent().unwrap().to_path_buf(),
            vec![PathBuf::from(folder)],
        )]),
    }
}

fn click_host_card(view: &gpui::Entity<Shell>, cx: &mut gpui::VisualTestContext, host: &HostId) {
    click_host_card_at(view, cx, host, 0);
}

#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helper asserts that the requested rendered card exists"
)]
fn click_host_card_at(
    view: &gpui::Entity<Shell>,
    cx: &mut gpui::VisualTestContext,
    host: &HostId,
    ordinal: usize,
) {
    let candidates = view.read_with(cx, |s, cx| {
        let sidebar = s.sidebar.read(cx);
        (0..sidebar.row_count())
            .filter(|&i| sidebar.row_host(i) == host)
            .collect::<Vec<_>>()
    });
    let index = candidates
        .into_iter()
        .filter(|i| {
            cx.debug_bounds(format!("worktree-card-{i}").leak())
                .is_some()
        })
        .nth(ordinal)
        .expect("workspace card for the selected host");
    let at = cx
        .debug_bounds(format!("worktree-title-{index}").leak())
        .unwrap()
        .center();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

#[gpui::test]
fn selecting_workspace_reconciles_detached_and_late_sessions_preserving_layout(
    cx: &mut TestAppContext,
) {
    let path = state_path("workspace-live-reconcile");
    let (view, cx) = open(cx, path.clone());
    view.update(cx, |s, cx| {
        s.change(true, cx, |l| {
            let first = l.active_tab().focused;
            l.set_session(first, SessionId(101));
            l.set_launch(
                first,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: b"/work/a".to_vec(),
                    cols: 80,
                    rows: 24,
                },
            );
            l.split(Axis::Horizontal);
            l.set_session(l.active_tab().focused, SessionId(102));
            l.set_tab_title(0, Some("Keep my split title"));
            l.toggle_pin_tab(0);
        });
        let mut snapshot = live_snapshot(&[101, 102, 103, 103], "/work/a");
        let mut exited = live_session(104, "/work/a");
        exited.status.state = krill_proto::control::AgentState::Exited;
        snapshot.sessions.push(exited);
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(Arc::new(LiveSnapshot(snapshot)), cx);
        });
    });
    cx.run_until_parked();
    click_host_card(&view, cx, &HostId::Local);
    let before = view.update(cx, |s, cx| {
        assert_eq!(
            s.layout.tabs().len(),
            2,
            "one missing live session gets one tab"
        );
        assert_eq!(s.layout.sessions_for_host(&HostId::Local).len(), 3);
        assert_eq!(s.workspace_tabs(cx), vec![0, 1]);
        assert_eq!(s.layout.tab_title(0), Some("Keep my split title"));
        s.layout.clone()
    });
    // Sidebar polling while the same workspace stays selected adds only new IDs.
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(
                    &[101, 102, 103, 105],
                    "/work/a",
                ))),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(s.layout.tabs().len(), 3);
        assert_eq!(s.layout.active(), before.active());
        assert_eq!(s.layout.focused(), before.focused());
        assert_eq!(&s.layout.tabs()[0], &before.tabs()[0]);
        assert_eq!(&s.layout.tabs()[1], &before.tabs()[1]);
        assert_eq!(s.workspace_tabs(cx), vec![0, 1, 2]);
        for pane in s.layout.tab_panes(0) {
            assert_eq!(s.layout.session(pane), before.session(pane));
        }
        // Switching worktrees never moves or spawns an existing session.
        s.sidebar.update(cx, |sidebar, cx| {
            let mut snapshot = live_snapshot(&[101, 102, 103, 105], "/work/a");
            snapshot.sessions.push(live_session(201, "/work/b"));
            snapshot
                .trees
                .get_mut(&PathBuf::from("/work"))
                .unwrap()
                .push("/work/b".into());
            sidebar.connect_source(Arc::new(LiveSnapshot(snapshot)), cx);
        });
    });
    cx.run_until_parked();
    // Click B's actual card, then A's actual card; no direct session helper.
    click_host_card_at(&view, cx, &HostId::Local, 1);
    view.update(cx, |s, cx| assert_eq!(s.workspace_tabs(cx).len(), 1));
    click_host_card_at(&view, cx, &HostId::Local, 0);
    view.update(cx, |s, cx| {
        assert_eq!(s.workspace_tabs(cx).len(), 3);
        s.reconcile_workspace_sessions(cx);
        assert_eq!(s.layout.tabs().len(), 4);
        assert_eq!(&s.layout.tabs()[0].root, &before.tabs()[0].root);
        s.saver.flush();
    });
    let expected = layout_of(&view, cx);
    let restored = cx.update(|_, cx| cx.new(|cx| Shell::new(Some(path), cx)));
    cx.run_until_parked();
    restored.update(cx, |s, cx| {
        assert_eq!(s.layout.tabs(), expected.tabs());
        assert_eq!(s.layout.focused(), expected.focused());
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(
                    &[101, 102, 103, 105],
                    "/work/a",
                ))),
                cx,
            );
        });
    });
    cx.run_until_parked();
    restored.update(cx, |s, _| assert_eq!(s.layout.tabs().len(), 4));
}

#[gpui::test]
fn selected_remote_workspace_reconciles_same_path_and_ids_without_crossing_hosts(
    cx: &mut TestAppContext,
) {
    use krill_app::hosts::HostStatus;
    let (view, cx) = open(cx, state_path("workspace-live-hosts"));
    let remote = HostId::Remote("server".into());
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[41, 42], "/work/shared"))),
                cx,
            );
            sidebar.set_remote_snapshot_for_test(
                "server",
                HostStatus::Connected,
                live_snapshot(&[41, 43], "/work/shared"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    click_host_card(&view, cx, &HostId::Local);
    view.update(cx, |s, _| {
        assert_eq!(s.layout.sessions_for_host(&HostId::Local).len(), 2);
        assert!(s.layout.sessions_for_host(&remote).is_empty());
    });
    click_host_card(&view, cx, &remote);
    let before = view.update(cx, |s, cx| {
        assert_eq!(
            s.sidebar.read(cx).selected_workspace(),
            Some((remote.clone(), "/work/shared".into()))
        );
        assert_eq!(s.layout.sessions_for_host(&HostId::Local).len(), 2);
        assert_eq!(s.layout.sessions_for_host(&remote).len(), 2);
        assert_eq!(s.workspace_tabs(cx).len(), 2);
        assert!(
            s.workspace_tabs(cx)
                .into_iter()
                .all(|i| s.layout.host(s.layout.tabs()[i].focused) == &remote)
        );
        s.layout.clone()
    });
    // A reconnect may relist the exact same IDs in another order.
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "server",
                HostStatus::Disconnected,
                live_snapshot(&[41, 43], "/work/shared"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "server",
                HostStatus::Connected,
                live_snapshot(&[43, 41, 44], "/work/shared"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, _| {
        assert_eq!(s.layout.sessions_for_host(&remote).len(), 3);
        assert_eq!(s.layout.sessions_for_host(&HostId::Local).len(), 2);
        assert_eq!(s.layout.focused(), before.focused());
        assert_eq!(&s.layout.tabs()[..before.tabs().len()], before.tabs());
    });
}

#[gpui::test]
fn terminating_a_workspace_tab_is_not_undone_by_a_stale_sidebar_snapshot(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("workspace-live-close"));
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[41, 42], "/work/a"))),
                cx,
            );
        });
    });
    cx.run_until_parked();
    click_host_card(&view, cx, &HostId::Local);
    view.update(cx, |s, cx| {
        let (tab, _) = s
            .tab_pane_for_session_on(&HostId::Local, SessionId(42))
            .unwrap();
        s.do_close_tab(tab, cx);
        s.reconcile_workspace_sessions(cx);
        assert_eq!(
            s.layout.sessions_for_host(&HostId::Local),
            vec![SessionId(41)]
        );
    });
    cx.run_until_parked();
    view.update(cx, |s, _| {
        assert!(
            s.tab_pane_for_session_on(&HostId::Local, SessionId(42))
                .is_none()
        );
    });
}

#[gpui::test]
fn selecting_workspace_keeps_a_session_in_its_existing_mixed_split(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("workspace-live-mixed-split"));
    let split = view.update(cx, |s, cx| {
        s.change(true, cx, |l| {
            let first = l.active_tab().focused;
            l.set_launch(
                first,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: b"/work/b".to_vec(),
                    cols: 80,
                    rows: 24,
                },
            );
            l.set_session(first, SessionId(201));
            l.split(Axis::Horizontal);
            let second = l.active_tab().focused;
            l.set_launch(
                second,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: b"/work/a".to_vec(),
                    cols: 80,
                    rows: 24,
                },
            );
            l.set_session(second, SessionId(101));
            l.set_tab_title(0, Some("Mixed split"));
        });
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[101, 102], "/work/a"))),
                cx,
            );
        });
        s.layout.tabs()[0].root.clone()
    });
    cx.run_until_parked();
    click_host_card(&view, cx, &HostId::Local);
    view.update(cx, |s, cx| {
        assert_eq!(s.layout.tabs()[0].root, split);
        assert_eq!(s.layout.tab_title(0), Some("Mixed split"));
        assert_eq!(s.workspace_tabs(cx), vec![0, 1]);
        assert_eq!(s.layout.sessions_for_host(&HostId::Local).len(), 3);
        assert_eq!(
            s.layout.session(s.layout.focused().unwrap()),
            Some(SessionId(101))
        );
    });
}

/// 재부팅 직후 데몬 목록이 비어도 저장된 에이전트 칸을 찾아야 하며 Login 탭을 만들면 안 된다.
#[gpui::test]
fn reboot_empty_sidebar_opens_saved_agent_workspace_without_creating_shell(
    cx: &mut TestAppContext,
) {
    let path = state_path("reboot-workspace");
    let (view, cx) = open(cx, path.clone());
    let panes = view.update(cx, fixture);
    let saved = layout_of(&view, cx);
    assert!(!saved.visible().contains(&panes[1]));
    view.update(cx, |s, cx| {
        s.open_saved_workspace(HostId::Local, Path::new("/workspace/b"), cx);
    });
    cx.run_until_parked();
    let layout = layout_of(&view, cx);
    assert_eq!(layout.tabs().len(), saved.tabs().len());
    assert_eq!(layout.session(panes[1]), Some(SessionId(101)));
    assert!(layout.visible().contains(&panes[1]));
    assert_eq!(layout.sessions(), saved.sessions());
}
