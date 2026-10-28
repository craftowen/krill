use super::*;
use crate::shell::tests::{layout_of, open, state_path};
use gpui::{Modifiers, TestAppContext};
use krill_proto::SessionId;

mod reconciliation;

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
    cx.simulate_keystrokes("ctrl-1");
    assert_eq!(layout_of(&view, cx).active(), 0);
    cx.run_until_parked();
    let tab = cx.debug_bounds("session-tab-2").unwrap().center();
    cx.simulate_click(tab, Modifiers::default());
    assert_eq!(layout_of(&view, cx).active(), 2);
    cx.simulate_keystrokes("ctrl-2");
    assert_eq!(layout_of(&view, cx).active(), 2);
    cx.simulate_keystrokes("ctrl-pagedown");
    assert_eq!(layout_of(&view, cx).active(), 0);
    cx.simulate_keystrokes("ctrl-pageup");
    assert_eq!(layout_of(&view, cx).active(), 2);
    cx.simulate_keystrokes("ctrl-tab");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 2);
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
        exited.status.source = Some(krill_proto::control::Source::Process);
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

#[gpui::test]
fn closing_remote_session_is_not_readded_after_interleaved_snapshots_and_externally_closed_session_closes_tab(
    cx: &mut TestAppContext,
) {
    use krill_app::hosts::HostStatus;
    let (view, cx) = open(cx, state_path("workspace-remote-close-loop"));
    let remote = HostId::Remote("macmini".into());
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "macmini",
                HostStatus::Connected,
                live_snapshot(&[41, 42], "/work/krill"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    click_host_card(&view, cx, &remote);
    view.update(cx, |s, cx| {
        assert_eq!(
            s.layout.sessions_for_host(&remote),
            vec![SessionId(41), SessionId(42)]
        );
        let (tab, _) = s.tab_pane_for_session_on(&remote, SessionId(42)).unwrap();
        s.do_close_tab(tab, cx);
        // 종료 직후 스냅샷에서 Exited로 잠깐 바뀌거나 일시 누락된 뒤 지연된 옛 스냅샷이 다시 도착해도
        // 닫은 원격 세션(42)이 새 Program::Login 탭으로 부활하면 안 된다.
        let mut exited_snap = live_snapshot(&[41], "/work/krill");
        let mut s42 = live_session(42, "/work/krill");
        s42.status.state = krill_proto::control::AgentState::Exited;
        exited_snap.sessions.push(s42);
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test("macmini", HostStatus::Connected, exited_snap, cx);
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "macmini",
                HostStatus::Connected,
                live_snapshot(&[41, 42], "/work/krill"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(s.layout.sessions_for_host(&remote), vec![SessionId(41)]);
        // 원격 클라이언트 등 외부에서 세션 41을 종료해 스냅샷에서 완전히 사라지면
        // 남아 있던 탭도 함께 정리되고 숨은 세션 복원(restore_hidden_sessions)도 돌지 않는다.
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "macmini",
                HostStatus::Connected,
                live_snapshot(&[], "/work/krill"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, _| {
        assert!(s.layout.sessions_for_host(&remote).is_empty());
        assert_eq!(s.closed_sessions, vec![SessionId(42)]);
    });
}

/// 종료 스냅샷은 비활성 탭도 정리한다. 호스트별 같은 ID와 셸 안의 `SessionEnd` 이벤트는 보존한다.
#[gpui::test]
fn exited_snapshot_closes_hidden_tab_without_trees_or_prior_observation(cx: &mut TestAppContext) {
    use krill_app::hosts::HostStatus;
    use krill_proto::control::{AgentState, Source};
    let (view, cx) = open(cx, state_path("workspace-exited-snapshot"));
    let remote = HostId::Remote("studio".into());
    let (local, ended, hook) = view.update(cx, |s, cx| {
        let local = s.layout.active_tab().focused;
        s.change(true, cx, |layout| {
            layout.set_session(local, SessionId(41));
            layout.new_tab();
            layout.set_host(layout.active_tab().focused, remote.clone());
            layout.set_session(layout.active_tab().focused, SessionId(41));
        });
        let ended = s.layout.active_tab().focused;
        s.change(true, cx, |layout| {
            layout.new_tab();
            layout.set_host(layout.active_tab().focused, remote.clone());
            layout.set_session(layout.active_tab().focused, SessionId(42));
        });
        let hook = s.layout.active_tab().focused;
        s.change(true, cx, |layout| layout.activate(0));
        assert!(
            !s.observed_sessions
                .contains(&(remote.clone(), SessionId(41)))
        );
        let mut exited = live_session(41, "/work/krill");
        exited.status.state = AgentState::Exited;
        exited.status.source = Some(Source::Process);
        let mut hook_exited = live_session(42, "/work/krill");
        hook_exited.status.state = AgentState::Exited;
        hook_exited.status.source = Some(Source::Hook);
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "studio",
                HostStatus::Connected,
                sidebar::Snapshot {
                    sessions: vec![exited, hook_exited],
                    ..Default::default()
                },
                cx,
            );
        });
        (local, ended, hook)
    });
    cx.run_until_parked();
    view.read_with(cx, |s, _| {
        assert!(!s.layout.pane_exists(ended));
        assert!(s.layout.pane_exists(hook));
        assert_eq!(s.layout.session(local), Some(SessionId(41)));
        assert_eq!(s.layout.focused(), Some(local));
        assert!(s.workspace_closing.contains(&(remote, SessionId(41))));
    });
}

#[gpui::test]
fn windows_workspace_aliases_share_tabs_and_new_tab_keeps_active_cwd(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("windows-workspace-identity"));
    view.update(cx, |s, cx| {
        s.sidebar
            .update(cx, |sidebar, cx| sidebar.select_worktree(None, cx));
        s.change(true, cx, |layout| {
            let pane = layout.active_tab().focused;
            layout.set_launch(
                pane,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: r"\\?\C:\Users\Owen\간단작업".as_bytes().to_vec(),
                    cols: 80,
                    rows: 24,
                },
            );
        });
        let original = s.layout.active();
        // No selected sidebar workspace yet: the active launch still owns the cwd.
        s.new_tab_on_launch_host(cx);
        assert_eq!(s.layout.tabs().len(), 2);
        let added = s.layout.active_tab().focused;
        assert_eq!(
            s.layout.launch(added).unwrap().cwd,
            r"\\?\C:\Users\Owen\간단작업".as_bytes()
        );
        s.change(true, cx, |layout| {
            let mut launch = layout.launch(added).unwrap().clone();
            launch.cwd = "c:/users/owen/간단작업/".as_bytes().to_vec();
            layout.set_launch(added, launch);
        });
        assert_eq!(s.workspace_tabs(cx), [original, s.layout.active()]);
        s.open_workspace_on(HostId::Local, PathBuf::from(r"C:\USERS\OWEN\간단작업"), cx);
        assert_eq!(s.layout.tabs().len(), 2);
        let local = Workspace {
            host: HostId::Local,
            folder: PathBuf::from(r"C:\Users\Owen\간단작업"),
        };
        let remote = Workspace {
            host: HostId::Remote("windows".into()),
            folder: local.folder.clone(),
        };
        assert_ne!(local, remote);
        assert_ne!(
            Workspace {
                host: HostId::Local,
                folder: "/Repo".into()
            },
            Workspace {
                host: HostId::Local,
                folder: "/repo".into()
            }
        );
    });
}

/// Actorjin(Windows): 새 세션(에이전트)도 지금 워크트리의 탭들 바로 옆에 열린다. 다른 워크스페이스 탭이 전체 목록
/// 사이에 있어도, 사이드바 선택이 아직 없고 경로 표기가 달라도 새 워크스페이스·목록을 시작하지 않는다.
#[gpui::test]
fn new_agent_session_opens_beside_tabs_of_the_same_windows_workspace(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("windows-agent-same-workspace"));
    view.update(cx, |s, cx| {
        s.sidebar
            .update(cx, |sidebar, cx| sidebar.select_worktree(None, cx));
        let login = |cwd: &str| Create {
            account: None,
            program: Program::Login,
            cwd: cwd.as_bytes().to_vec(),
            cols: 80,
            rows: 24,
        };
        s.change(true, cx, |layout| {
            let first = layout.active_tab().focused;
            layout.set_launch(first, login(r"\\?\C:\Users\Owen\간단작업"));
            layout.new_tab();
            let other = layout.active_tab().focused;
            layout.set_launch(other, login(r"C:\Users\Owen\다른작업"));
            layout.activate(0);
        });
        assert_eq!(s.workspace_tabs(cx), [0]);

        s.new_agent("codex".into(), cx);
        let added = s.layout.active_tab().focused;
        let launch = s.layout.launch(added).unwrap().clone();
        assert_eq!(launch.program, Program::Agent { id: "codex".into() });
        assert_eq!(launch.cwd, r"\\?\C:\Users\Owen\간단작업".as_bytes());
        assert_eq!(s.layout.host(added), &HostId::Local);
        // 새 세션은 같은 워크스페이스의 마지막 탭 바로 옆(화면상 두 번째)이고 활성이다.
        assert_eq!(s.workspace_tabs(cx), [0, s.layout.active()]);

        // 같은 폴더의 다른 표기로 시작한 칸에서 열어도 같은 탭 목록에 붙는다.
        s.change(true, cx, |layout| {
            let mut alias = layout.launch(added).unwrap().clone();
            alias.cwd = "c:/users/owen/간단작업/".as_bytes().to_vec();
            layout.set_launch(added, alias);
        });
        s.new_agent("claude".into(), cx);
        assert_eq!(s.layout.tabs().len(), 4);
        let tabs = s.workspace_tabs(cx);
        assert_eq!(tabs.len(), 3, "{tabs:?}");
        assert_eq!(tabs.last(), Some(&s.layout.active()));
    });
}

/// 숨은 칸 재개를 받는 가짜 krilld. `List`에 `eof`면 답 없이 끊고, 아니면 빈 목록을 준다. `Resume`은 닫힌
/// 세션처럼 `NoSession`으로 거부한다. (List, Resume) 요청 수를 센다.
fn fake_restore_daemon(
    listener: std::os::unix::net::UnixListener,
    eof: bool,
) -> Arc<[std::sync::atomic::AtomicUsize; 2]> {
    use krill_proto::control::{
        FEAT_CONTROL, FEAT_RESUME, FEAT_RESUME_HISTORY, Failure, Reply, Request,
    };
    use krill_proto::screen::Msg as _;
    use std::io::Write as _;
    use std::sync::atomic::Ordering::SeqCst;
    let counts = Arc::new([0, 0].map(std::sync::atomic::AtomicUsize::new));
    let seen = Arc::clone(&counts);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                let hello = krill_proto::ipc::ServerHello {
                    proto_min: krill_proto::ipc::PROTO_MIN,
                    proto_max: krill_proto::ipc::PROTO_MAX,
                    features: FEAT_CONTROL | FEAT_RESUME | FEAT_RESUME_HISTORY,
                    build: "test".into(),
                    instance_id: 1,
                };
                let Ok(hello) = hello.encode() else { return };
                if stream.write_all(&hello).is_err() {
                    return;
                }
                let Ok(Some(_)) = krill_proto::ipc::read_frame(&mut stream) else {
                    return;
                };
                while let Ok(Some((kind, body))) = krill_proto::ipc::read_frame(&mut stream) {
                    let reply = match Request::decode(kind, &body) {
                        Ok(Request::List) => {
                            seen[0].fetch_add(1, SeqCst);
                            if eof {
                                return;
                            }
                            Reply::Sessions(Vec::new())
                        }
                        Ok(Request::Resume { .. }) => {
                            seen[1].fetch_add(1, SeqCst);
                            Reply::Failed(Failure::NoSession)
                        }
                        _ => Reply::Failed(Failure::BadRequest),
                    };
                    let Ok(frame) = reply.encode() else { return };
                    stream.write_all(&frame).ok();
                }
            });
        }
    });
    counts
}

/// 숨은 로컬 칸 하나를 두고 가짜 데몬에 재개를 한 번 끝까지 돌린다.
fn restore_once<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    socket: &std::path::Path,
    old: SessionId,
) -> (gpui::Entity<Shell>, &'a mut gpui::VisualTestContext) {
    let (view, cx) = open(cx, state_path(name));
    view.update(cx, |s, cx| {
        let pane = s.layout.active_tab().focused;
        s.layout.set_session(pane, old);
        s.layout.deactivate();
        s.paths = Some(Paths {
            socket: socket.to_path_buf(),
            holder: PathBuf::new(),
            krilld: PathBuf::new(),
        });
        s.link = Link::Up;
        s.restore_hidden_sessions(&[], cx);
        assert!(s.restoring_sessions.contains(&old));
    });
    cx.run_until_parked();
    (view, cx)
}

#[gpui::test]
fn hidden_restore_retries_after_presend_failure_on_reconnect(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    // 재부팅 직후 데몬이 바뀌며 읽기 전용 List가 끊기면 Resume은 나가지 않았다. 재접속(Up) 뒤 다시 시도해야 한다.
    let socket = PathBuf::from(format!("/tmp/kr-hidden-eof-{}.sock", std::process::id()));
    std::fs::remove_file(&socket).ok();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let counts = fake_restore_daemon(listener, true);
    let old = SessionId(900);
    let (view, cx) = restore_once(cx, "reboot-restore-presend", &socket, old);
    view.update(cx, |s, cx| {
        assert_eq!(counts[0].load(SeqCst), 1, "List까지 가서 끊겨야 한다");
        assert_eq!(counts[1].load(SeqCst), 0);
        assert!(!s.restoring_sessions.contains(&old));
        assert!(s.outcome_unknown.is_empty(), "재개 요청은 나가지 않았다");
        s.set_link(Link::Down, cx);
        s.set_link(Link::Up, cx);
        assert!(
            s.restoring_sessions.contains(&old),
            "요청 전 실패는 재접속 뒤 다시 시도한다"
        );
    });
    std::fs::remove_file(socket).ok();
}

#[gpui::test]
fn hidden_restore_does_not_retry_daemon_refusal(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    // 정상 종료(closed)된 세션은 데몬이 Resume을 거부한다. 재접속해도 다시 되살리려 하지 않는다.
    let socket = PathBuf::from(format!("/tmp/kr-hidden-closed-{}.sock", std::process::id()));
    std::fs::remove_file(&socket).ok();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let counts = fake_restore_daemon(listener, false);
    let old = SessionId(901);
    let (view, cx) = restore_once(cx, "reboot-restore-refused", &socket, old);
    view.update(cx, |s, cx| {
        assert_eq!(counts[1].load(SeqCst), 1, "Resume이 거부돼야 한다");
        assert!(!s.restoring_sessions.contains(&old));
        s.set_link(Link::Down, cx);
        s.set_link(Link::Up, cx);
        assert!(
            !s.restoring_sessions.contains(&old),
            "확정 거부는 다시 시도하지 않는다"
        );
    });
    std::fs::remove_file(socket).ok();
}

#[gpui::test]
fn hidden_restore_ignores_visible_remote_id_collision(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("reboot-restore-remote-collision"));
    view.update(cx, |s, cx| {
        // 원격 호스트의 SessionId는 로컬과 독립이라 숫자가 같을 수 있다.
        let remote = s.layout.active_tab().focused;
        let old = SessionId(910);
        s.layout.set_host(remote, HostId::Remote("server".into()));
        s.layout.set_session(remote, old);
        s.layout.new_tab();
        let local = s.layout.active_tab().focused;
        s.layout.set_session(local, old);
        s.layout.activate(0);
        s.paths = Some(Paths {
            socket: PathBuf::new(),
            holder: PathBuf::new(),
            krilld: PathBuf::new(),
        });
        s.link = Link::Up;
        let visible = s.layout.visible();
        assert!(visible.contains(&remote));
        assert!(!visible.contains(&local));
        s.restore_hidden_sessions(&visible, cx);
        assert!(
            s.restore_attempted.contains(&old),
            "보이는 원격 칸의 같은 ID가 숨은 로컬 칸 복원을 막으면 안 된다"
        );
    });
}
