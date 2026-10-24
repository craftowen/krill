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
