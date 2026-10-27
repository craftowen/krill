use super::*;

#[gpui::test]
fn restored_workspace_imports_late_sessions_without_sidebar_selection(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("workspace-no-selection-reconcile"));
    let original = view.update(cx, |s, cx| {
        let pane = s.layout.active_tab().focused;
        s.layout.set_session(pane, SessionId(41));
        s.layout.set_launch(
            pane,
            Create {
                account: None,
                program: Program::Login,
                cwd: b"/work/a".to_vec(),
                cols: 80,
                rows: 24,
            },
        );
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.select_worktree(None, cx);
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[41, 42], "/work/a"))),
                cx,
            );
        });
        pane
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert!(s.sidebar.read(cx).selected_workspace().is_none());
        assert_eq!(
            s.layout.sessions_for_host(&HostId::Local),
            vec![SessionId(41), SessionId(42)]
        );
        assert_eq!(s.layout.focused(), Some(original));
        // An incomplete local list during daemon update must not erase either tab.
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(Arc::new(LiveSnapshot(live_snapshot(&[], "/work/a"))), cx);
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(
            s.layout.sessions_for_host(&HostId::Local),
            vec![SessionId(41), SessionId(42)]
        );
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[41, 42, 43], "/work/a"))),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(
            s.layout.sessions_for_host(&HostId::Local),
            vec![SessionId(41), SessionId(42), SessionId(43)]
        );
        assert_eq!(s.layout.focused(), Some(original));
        assert_eq!(s.workspace_tabs(cx).len(), 3);
    });
}

#[gpui::test]
fn closed_remote_tab_stays_closed_after_empty_and_stale_snapshots(cx: &mut TestAppContext) {
    use krill_app::hosts::HostStatus;
    let (view, cx) = open(cx, state_path("workspace-remote-close-relist"));
    let remote = HostId::Remote("server".into());
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "server",
                HostStatus::Connected,
                live_snapshot(&[41, 42], "/work/a"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    click_host_card(&view, cx, &remote);
    view.update(cx, |s, cx| {
        let (tab, _) = s.tab_pane_for_session_on(&remote, SessionId(42)).unwrap();
        s.do_close_tab(tab, cx);
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "server",
                HostStatus::Disconnected,
                live_snapshot(&[], "/work/a"),
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
                live_snapshot(&[41, 42, 43], "/work/a"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, _| {
        assert_eq!(
            s.layout.sessions_for_host(&remote),
            vec![SessionId(41), SessionId(43)]
        );
    });
}

#[gpui::test]
fn remote_creation_is_not_duplicated_by_an_early_sidebar_snapshot(cx: &mut TestAppContext) {
    use krill_app::hosts::HostStatus;
    let (view, cx) = open(cx, state_path("workspace-remote-create-relist"));
    let remote = HostId::Remote("server".into());
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "server",
                HostStatus::Connected,
                live_snapshot(&[41], "/work/a"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    click_host_card(&view, cx, &remote);
    let pending = view.update(cx, |s, cx| {
        s.new_folder_tab_on(remote.clone(), Path::new("/work/a"), Program::Login, cx);
        let pending = s.layout.focused().unwrap();
        // The daemon can list a new session before Create/Attach returns to its pane.
        s.live.insert(pending, Live::new(Task::ready(())));
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "server",
                HostStatus::Connected,
                live_snapshot(&[41, 42, 43], "/work/a"),
                cx,
            );
        });
        pending
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(
            s.workspace_tabs(cx).len(),
            2,
            "the pending pane already owns this launch"
        );
        s.settled(
            pending,
            Ok(Got::Live {
                session: SessionId(42),
                linked: Err(std::io::ErrorKind::ConnectionReset.into()),
                prior_cold: None,
            }),
            cx,
        );
        assert_eq!(
            s.layout.sessions_for_host(&remote),
            vec![SessionId(41), SessionId(42), SessionId(43)]
        );
        assert_eq!(
            s.workspace_tabs(cx).len(),
            3,
            "other sessions are imported after Create settles"
        );
    });
}

#[gpui::test]
fn closing_remote_creation_before_layout_receives_id_prevents_reimport(cx: &mut TestAppContext) {
    use krill_app::hosts::HostStatus;
    let (view, cx) = open(cx, state_path("workspace-remote-close-create"));
    let remote = HostId::Remote("server".into());
    view.update(cx, |s, cx| {
        s.close_call = Some(Arc::new(|request| {
            Ok(match request {
                krill_proto::control::Request::List => {
                    krill_proto::control::Reply::Sessions(Vec::new())
                }
                _ => krill_proto::control::Reply::Closed,
            })
        }));
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "server",
                HostStatus::Connected,
                live_snapshot(&[41], "/work/a"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    click_host_card(&view, cx, &remote);
    view.update(cx, |s, cx| {
        s.new_folder_tab_on(remote.clone(), Path::new("/work/a"), Program::Login, cx);
        let pane = s.layout.focused().unwrap();
        let live = Live::new(Task::ready(()));
        assert!(close::Creation::publish(&live.creation, SessionId(42)));
        s.live.insert(pane, live);
        s.do_close_pane(pane, cx);
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_remote_snapshot_for_test(
                "server",
                HostStatus::Connected,
                live_snapshot(&[41, 42], "/work/a"),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, _| {
        assert_eq!(s.layout.sessions_for_host(&remote), vec![SessionId(41)]);
        assert!(
            s.workspace_closing
                .contains(&(remote.clone(), SessionId(42)))
        );
        assert!(
            !s.workspace_closing
                .contains(&(HostId::Local, SessionId(42)))
        );
    });
}

#[gpui::test]
fn restored_workspace_imports_late_sessions_without_sidebar_selection(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("workspace-no-selection-reconcile"));
    let original = view.update(cx, |s, cx| {
        let pane = s.layout.active_tab().focused;
        s.layout.set_session(pane, SessionId(41));
        s.layout.set_launch(
            pane,
            Create {
                account: None,
                program: Program::Login,
                cwd: b"/work/a".to_vec(),
                cols: 80,
                rows: 24,
            },
        );
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.select_worktree(None, cx);
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[41, 42], "/work/a"))),
                cx,
            );
        });
        pane
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert!(s.sidebar.read(cx).selected_workspace().is_none());
        assert_eq!(
            s.layout.sessions_for_host(&HostId::Local),
            vec![SessionId(41), SessionId(42)]
        );
        assert_eq!(s.layout.focused(), Some(original));
        // An incomplete local list during daemon update must not erase either tab.
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(Arc::new(LiveSnapshot(live_snapshot(&[], "/work/a"))), cx);
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(
            s.layout.sessions_for_host(&HostId::Local),
            vec![SessionId(41), SessionId(42)]
        );
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(LiveSnapshot(live_snapshot(&[41, 42, 43], "/work/a"))),
                cx,
            );
        });
    });
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        assert_eq!(
            s.layout.sessions_for_host(&HostId::Local),
            vec![SessionId(41), SessionId(42), SessionId(43)]
        );
        assert_eq!(s.layout.focused(), Some(original));
        assert_eq!(s.workspace_tabs(cx).len(), 3);
    });
}

/// APP-RUNTIME-004 (`81fbb04c` / `e54d84b9` 상호작용): `layout_loading` 중 초기 `Cmd+T`(`$HOME`)가 열려
/// `restore_layout` 뒤 활성 워크스페이스가 `$HOME`이 되고 사이드바 선택도 비어 있어도,
/// 저장본의 모든 워크스페이스(`/work/a`, `/work/b`)는 데몬의 유효 스냅샷이 도착할 때 늦게 뜬 세션을
/// 빠짐없이 가져오고, 명시적으로 닫힌 세션(`workspace_closing`)과 무관한 워크스페이스(`/work/unrelated`)는
/// 가져오지 않으며, 초기 `$HOME` 탭의 활성·포커스 상태도 그대로 유지한다.
#[gpui::test]
fn saved_workspaces_import_late_sessions_even_after_early_startup_new_tab(cx: &mut TestAppContext) {
    let path = state_path("startup-saved-workspaces-reconcile");
    let mut saved = Layout::default();
    let s0 = saved.focused().unwrap();
    saved.set_session(s0, SessionId(41));
    saved.set_launch(
        s0,
        Create {
            account: None,
            program: Program::Login,
            cwd: b"/work/a".to_vec(),
            cols: 80,
            rows: 24,
        },
    );
    saved.new_tab();
    let s1 = saved.focused().unwrap();
    saved.set_session(s1, SessionId(51));
    saved.set_launch(
        s1,
        Create {
            account: None,
            program: Program::Login,
            cwd: b"/work/b".to_vec(),
            cols: 80,
            rows: 24,
        },
    );
    saved.activate(0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    saved.save(&path).unwrap();

    let state_dir = path.parent().map(std::path::Path::to_path_buf);
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::shell::bind_keys(cx);
        krill_app::settings::init_at(state_dir, cx);
    });
    let (release, gate) = tokio::sync::oneshot::channel();
    let (view, cx) = cx.add_window_view(|_, cx| {
        let shell = Shell::new_with_restore_gate(Some(path.clone()), gate, cx);
        assert!(shell.layout_loading);
        shell
    });

    let early_home_pane = cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            assert!(s.layout_loading);
            s.new_folder_tab_on(HostId::Local, Path::new("/Users/home"), Program::Login, cx);
            s.workspace_closing.insert((HostId::Local, SessionId(43)));
            s.layout.focused().unwrap()
        })
    });

    release.send(()).unwrap();
    cx.run_until_parked();

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            assert!(!s.layout_loading);
            s.sidebar.update(cx, |sidebar, cx| {
                sidebar.select_worktree(None, cx);
            });
            assert_eq!(s.sidebar.read(cx).selected_workspace(), None);
            assert_eq!(s.layout.focused(), Some(early_home_pane));
            assert_eq!(s.startup_reconcile_panes.len(), 2);
            // 데몬 재기동 직후 아직 세션 목록이 비어 있는 불완전 스냅샷이 먼저 와도
            // 저장 워크스페이스 큐가 조기에 비워지거나 저장 탭이 닫히면 안 된다.
            s.sidebar.update(cx, |sidebar, cx| {
                sidebar.connect_source(Arc::new(LiveSnapshot(live_snapshot(&[], "/work/a"))), cx);
            });
        });
    });
    cx.run_until_parked();

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            assert_eq!(
                s.layout.sessions_for_host(&HostId::Local),
                vec![SessionId(41), SessionId(51)]
            );
            assert_eq!(s.startup_reconcile_panes.len(), 2);

            let snapshot = sidebar::Snapshot {
                sessions: vec![
                    live_session(41, "/work/a"),
                    live_session(42, "/work/a"),
                    live_session(43, "/work/a"),
                    live_session(51, "/work/b"),
                    live_session(52, "/work/b"),
                    live_session(99, "/work/unrelated"),
                ],
                trees: BTreeMap::from([(
                    PathBuf::from("/work"),
                    vec![
                        PathBuf::from("/work/a"),
                        PathBuf::from("/work/b"),
                        PathBuf::from("/work/unrelated"),
                    ],
                )]),
            };
            s.sidebar.update(cx, |sidebar, cx| {
                sidebar.connect_source(Arc::new(LiveSnapshot(snapshot)), cx);
            });
        });
    });
    cx.run_until_parked();

    cx.update(|_, cx| {
        view.read_with(cx, |s, cx| {
            assert_eq!(
                s.layout.sessions_for_host(&HostId::Local),
                vec![SessionId(41), SessionId(51), SessionId(42), SessionId(52)]
            );
            assert!(
                s.startup_reconcile_panes.is_empty(),
                "settled saved workspaces should retire from startup queue"
            );
            assert_eq!(s.layout.focused(), Some(early_home_pane));
            assert_eq!(
                s.tab_workspace(s.layout.active(), cx).map(|ws| ws.folder),
                Some(PathBuf::from("/Users/home"))
            );
        });
    });
}
