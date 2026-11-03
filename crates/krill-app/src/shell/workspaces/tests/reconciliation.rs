use super::*;

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

struct IpcRegistryGuard {
    stop: Arc<std::sync::atomic::AtomicBool>,
    sock_path: PathBuf,
    accept_worker: Option<std::thread::JoinHandle<()>>,
    holder_worker: Option<std::thread::JoinHandle<()>>,
    reader_worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for IpcRegistryGuard {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        std::os::unix::net::UnixStream::connect(&self.sock_path).ok();
        std::fs::remove_file(&self.sock_path).ok();
        if let Some(worker) = self.accept_worker.take() {
            worker.join().ok();
        }
        if let Some(worker) = self.holder_worker.take() {
            worker.join().ok();
        }
        if let Some(worker) = self.reader_worker.take() {
            worker.join().ok();
        }
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "test fixture IPC server setup asserts local socket invariants"
)]
fn spawn_ipc_registry_server(
    state_dir: &Path,
    sock_path: &Path,
) -> (Arc<krilld::registry::Registry>, IpcRegistryGuard) {
    use krill_proto::holder::{FromHolder, ToHolder, VERSION};
    use std::collections::HashMap;
    use std::os::fd::{AsFd as _, OwnedFd};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    std::fs::create_dir_all(state_dir).unwrap();
    std::fs::remove_file(sock_path).ok();
    let (krilld_end, holder_end) = UnixStream::pair().unwrap();
    holder_end
        .set_read_timeout(Some(Duration::from_millis(25)))
        .unwrap();
    let holder_link = krilld::holder::link::Link::new(OwnedFd::from(holder_end));
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        holder_link.send(&m.encode().unwrap(), &[]).unwrap();
    }
    let (holder, link, kept, _) =
        krilld::holder::Holder::connect(OwnedFd::from(krilld_end)).unwrap();
    let reg = Arc::new(krilld::registry::Registry::new(
        Some((holder, kept)),
        Some(state_dir),
    ));
    let reg_reader = Arc::clone(&reg);
    let reader_worker = std::thread::spawn(move || {
        reg_reader.run_holder(link);
    });
    let stop = Arc::new(AtomicBool::new(false));
    let stop_holder = Arc::clone(&stop);
    let holder_worker = std::thread::spawn(move || {
        let mut holder_link = holder_link;
        let mut children: HashMap<SessionId, UnixStream> = HashMap::new();
        while !stop_holder.load(Ordering::SeqCst) {
            let pkt = match holder_link.recv() {
                Ok(Some(pkt)) => pkt,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Ok(None) | Err(_) => break,
            };
            match ToHolder::decode(pkt.frame()).unwrap() {
                ToHolder::Spawn { req, id, .. } => {
                    let (master, child) = UnixStream::pair().unwrap();
                    children.insert(id, child);
                    holder_link
                        .send(
                            &FromHolder::Spawned {
                                req,
                                id,
                                pid: u32::MAX,
                            }
                            .encode()
                            .unwrap(),
                            &[master.as_fd()],
                        )
                        .unwrap();
                }
                ToHolder::Terminate { req, id } | ToHolder::Kill { req, id, .. } => {
                    children.remove(&id);
                    holder_link
                        .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                        .unwrap();
                    holder_link
                        .send(&FromHolder::Exited { id, status: 0 }.encode().unwrap(), &[])
                        .unwrap();
                }
                ToHolder::Release { req, id } => {
                    children.remove(&id);
                    holder_link
                        .send(&FromHolder::Done { req }.encode().unwrap(), &[])
                        .unwrap();
                }
                _ => {}
            }
        }
    });
    let listener = UnixListener::bind(sock_path).unwrap();
    let reg_accept = Arc::clone(&reg);
    let stop_accept = Arc::clone(&stop);
    let accept_worker = std::thread::spawn(move || {
        let hello = krill_proto::ipc::ServerHello {
            proto_min: krill_proto::ipc::PROTO_MIN,
            proto_max: krill_proto::ipc::PROTO_MAX,
            features: krill_proto::ipc::FEATURES,
            build: "test".into(),
            instance_id: 1,
        };
        while let Ok((stream, _)) = listener.accept() {
            if stop_accept.load(Ordering::SeqCst) {
                break;
            }
            let reg = Arc::clone(&reg_accept);
            let hello = hello.clone();
            std::thread::spawn(move || {
                reg.serve(stream, &hello).ok();
            });
        }
    });
    let guard = IpcRegistryGuard {
        stop,
        sock_path: sock_path.to_path_buf(),
        accept_worker: Some(accept_worker),
        holder_worker: Some(holder_worker),
        reader_worker: Some(reader_worker),
    };
    (reg, guard)
}

#[expect(
    clippy::unwrap_used,
    reason = "test fixture control RPC client asserts local socket invariants"
)]
fn daemon_control_rpc(
    sock_path: &Path,
    request: &krill_proto::control::Request,
) -> krill_proto::control::Reply {
    use krill_proto::screen::Msg as _;
    use std::io::Write as _;

    let mut stream = std::os::unix::net::UnixStream::connect(sock_path).unwrap();
    let (kind, body) = krill_proto::ipc::read_frame(&mut stream).unwrap().unwrap();
    krill_proto::ipc::ServerHello::decode(kind, &body).unwrap();
    let hello = krill_proto::daemon::client_hello(krill_proto::ipc::ClientRole::Cli, "test-cli");
    stream.write_all(&hello.encode().unwrap()).unwrap();
    stream.write_all(&request.encode().unwrap()).unwrap();
    let (kind, body) = krill_proto::ipc::read_frame(&mut stream).unwrap().unwrap();
    krill_proto::control::Reply::decode(kind, &body).unwrap()
}

#[gpui::test]
fn externally_closed_background_tabs_are_pruned_via_ipc_sidebar_on_local_and_remote(
    cx: &mut TestAppContext,
) {
    use krill_app::hosts::{HostStatus, HostTarget};
    use krill_proto::control::{Reply, Request};
    use std::os::unix::ffi::OsStrExt as _;
    use std::time::{Duration, Instant};

    let root = PathBuf::from(format!("/tmp/kr-ipc-closed-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    let local_sock = root.join("local.sock");
    let remote_sock = root.join("remote.sock");
    let (_local_reg, local_guard) = spawn_ipc_registry_server(&root.join("local"), &local_sock);
    let (_remote_reg, remote_guard) = spawn_ipc_registry_server(&root.join("remote"), &remote_sock);

    let launch = Create {
        account: None,
        program: Program::Login,
        cwd: repo.as_os_str().as_bytes().to_vec(),
        cols: 80,
        rows: 24,
    };
    let create_on = |sock: &Path| match daemon_control_rpc(sock, &Request::Create(launch.clone())) {
        Reply::Created { session, .. } => session,
        other => panic!("expected Created, got {other:?}"),
    };
    let wait_listed = |sock: &Path, expected: &[SessionId]| {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Reply::Sessions(got) = daemon_control_rpc(sock, &Request::List) {
                let mut ids: Vec<SessionId> = got.into_iter().map(|i| i.session).collect();
                ids.sort_by_key(|s| s.0);
                let mut want = expected.to_vec();
                want.sort_by_key(|s| s.0);
                if ids == want {
                    break;
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    let local_live = create_on(&local_sock);
    let local_closed_offline = create_on(&local_sock);
    let local_closed_online = create_on(&local_sock);
    assert_eq!(
        daemon_control_rpc(
            &local_sock,
            &Request::Close {
                session: local_closed_offline,
            },
        ),
        Reply::Closed
    );
    wait_listed(&local_sock, &[local_live, local_closed_online]);

    let remote_live = create_on(&remote_sock);
    let remote_closed_offline = create_on(&remote_sock);
    let remote_closed_online = create_on(&remote_sock);
    assert_eq!(
        daemon_control_rpc(
            &remote_sock,
            &Request::Close {
                session: remote_closed_offline,
            },
        ),
        Reply::Closed
    );
    wait_listed(&remote_sock, &[remote_live, remote_closed_online]);

    let remote = HostId::Remote("server".into());
    let unclosed_local = SessionId(9901);
    let unclosed_remote = SessionId(9902);
    let (view, cx) = open(cx, state_path("workspace-ipc-closed-reconcile"));
    view.update(cx, |s, cx| {
        s.change(true, cx, |l| {
            let entries = [
                (HostId::Local, local_live),
                (HostId::Local, local_closed_offline),
                (HostId::Local, local_closed_online),
                (HostId::Local, unclosed_local),
                (remote.clone(), remote_live),
                (remote.clone(), remote_closed_offline),
                (remote.clone(), remote_closed_online),
                (remote.clone(), unclosed_remote),
            ];
            for (i, (host, session)) in entries.into_iter().enumerate() {
                if i > 0 {
                    l.new_tab();
                }
                let pane = l.active_tab().focused;
                l.set_host(pane, host);
                l.set_launch(pane, launch.clone());
                l.set_session(pane, session);
            }
            l.activate(0);
        });
        let local_target = HostTarget::local(Paths {
            socket: local_sock.clone(),
            holder: PathBuf::new(),
            krilld: PathBuf::new(),
        });
        let remote_target = HostTarget::remote("server", remote_sock.clone());
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(sidebar::status_source(local_target), cx);
            sidebar.set_remote_host_source(
                "server",
                "Server",
                HostStatus::Connected,
                sidebar::status_source(remote_target),
                cx,
            );
        });
    });
    cx.run_until_parked();

    assert_eq!(
        daemon_control_rpc(
            &local_sock,
            &Request::Close {
                session: local_closed_online,
            },
        ),
        Reply::Closed
    );
    assert_eq!(
        daemon_control_rpc(
            &remote_sock,
            &Request::Close {
                session: remote_closed_online,
            },
        ),
        Reply::Closed
    );
    wait_listed(&local_sock, &[local_live]);
    wait_listed(&remote_sock, &[remote_live]);

    std::thread::sleep(Duration::from_millis(500));
    cx.executor().advance_clock(Duration::from_millis(2500));
    cx.run_until_parked();

    view.read_with(cx, |s, _| {
        assert_eq!(
            s.layout.sessions_for_host(&HostId::Local),
            vec![local_live, unclosed_local]
        );
        assert_eq!(
            s.layout.sessions_for_host(&remote),
            vec![remote_live, unclosed_remote]
        );
    });

    drop(view);
    drop(remote_guard);
    drop(local_guard);
    std::fs::remove_dir_all(root).ok();
}

#[gpui::test]
fn completed_and_unheld_failed_account_handoffs_do_not_pin_closed_tabs(cx: &mut TestAppContext) {
    use krill_proto::control::{AgentState, Source};
    use krill_proto::usage::{AccountsSnapshot, SessionHandoff};

    let (view, cx) = open(cx, state_path("workspace-handoffs-prune"));
    let (pane_100, pane_181, pane_191, pane_195) = view.update(cx, |s, cx| {
        let launch = Create {
            account: None,
            program: Program::Login,
            cwd: b"/work/a".to_vec(),
            cols: 80,
            rows: 24,
        };
        let mut panes = Vec::new();
        s.change(true, cx, |l| {
            for (i, session) in [
                SessionId(100),
                SessionId(181),
                SessionId(191),
                SessionId(195),
            ]
            .into_iter()
            .enumerate()
            {
                if i > 0 {
                    l.new_tab();
                }
                let pane = l.active_tab().focused;
                l.set_launch(pane, launch.clone());
                l.set_session(pane, session);
                panes.push(pane);
            }
            l.activate(0);
        });

        let mut accounts = AccountsSnapshot::default();
        accounts.details.supported = true;
        accounts.details.handoffs = vec![
            SessionHandoff {
                from: SessionId(171),
                to: Some(SessionId(181)),
                account_id: "acct-b".into(),
                error: None,
            },
            SessionHandoff {
                from: SessionId(191),
                to: None,
                account_id: "acct-b".into(),
                error: Some("quota".into()),
            },
            SessionHandoff {
                from: SessionId(195),
                to: None,
                account_id: "acct-b".into(),
                error: None,
            },
        ];
        s.apply_accounts_snapshot(accounts, cx);

        let mut snapshot = live_snapshot(&[100], "/work/a");
        for exited_id in [181, 191, 195] {
            let mut exited = live_session(exited_id, "/work/a");
            exited.status.state = AgentState::Exited;
            exited.status.source = Some(Source::Process);
            snapshot.sessions.push(exited);
        }
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(Arc::new(LiveSnapshot(snapshot)), cx);
        });
        (panes[0], panes[1], panes[2], panes[3])
    });
    cx.run_until_parked();

    view.read_with(cx, |s, _| {
        assert!(!s.layout.pane_exists(pane_181));
        assert!(!s.layout.pane_exists(pane_191));
        assert!(s.layout.pane_exists(pane_195));
        assert!(s.layout.pane_exists(pane_100));
    });
}
