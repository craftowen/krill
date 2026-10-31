use super::*;
use crate::shell::tests::{open, state_path};
use gpui::TestAppContext;
use krill_proto::input::Input;
use krill_proto::ipc::{ClientHello, ServerHello};
use krill_proto::os::net::UnixListener;
use krill_proto::screen::Msg as _;
use krill_proto::usage::UsageReply;
use std::io::{Read as _, Write as _};

#[gpui::test]
fn changed_forwarding_socket_rechecks_same_session_and_releases_held_enter(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open(cx, state_path("silent-renew-target"));
    let (ours, mut peer) = UnixStream::pair().unwrap();
    view.update(cx, |s, cx| {
        let pane = s.layout.focused().unwrap();
        let host = HostId::Remote("same-registration".into());
        let registration = Arc::new(());
        let target = |name: &str, key| {
            HostTarget::remote_with_connector(
                "same-registration",
                state_path(name),
                krill_app::hosts::RemoteConnector::new(
                    || Err(std::io::ErrorKind::ConnectionRefused.into()),
                    |_, _| {},
                )
                .with_lifetime(key, || true),
            )
        };
        let old = target("old-forwarding", registration.clone());
        let new = target("new-forwarding", registration);
        let different = target("other-registration", Arc::new(()));
        assert!(old.same_session_authority(&new));
        assert!(!old.same_session_authority(&different));
        s.layout.set_host(pane, host.clone());
        s.layout.set_session(pane, SessionId(220));
        s.host_targets.insert(host.clone(), old.clone());
        s.attach_test(pane, SessionId(220), ours, cx);
        s.live.get_mut(&pane).unwrap().target = Some(old.clone());
        let mut failed = AccountsSnapshot::default();
        failed.details.supported = true;
        failed.details.handoffs.push(SessionHandoff {
            from: SessionId(220),
            to: None,
            account_id: "selected".into(),
            error: Some("fixture".into()),
        });
        s.apply_accounts_snapshot_for_host(host.clone(), failed, cx);
        s.live
            .get_mut(&pane)
            .unwrap()
            .pane
            .input(b"abc\r", Now::get());
        let before = s.account_holds[&(host.clone(), SessionId(220))].clone();
        let stale = Cancel {
            hold: before.clone(),
            target: old,
            epoch: s.account_epochs.get(&host).copied().unwrap_or_default(),
        };
        s.host_targets.insert(host.clone(), different.clone());
        s.live.get_mut(&pane).unwrap().target = Some(different);
        s.rebind_account_input_target(pane);
        assert_eq!(
            s.account_holds[&(host.clone(), SessionId(220))].serial,
            before.serial,
            "a different registration must never inherit held commands"
        );
        s.host_targets.insert(host.clone(), new.clone());
        s.live.get_mut(&pane).unwrap().target = Some(new.clone());
        s.rebind_account_input_target(pane);
        let renewed = s.account_holds[&(host.clone(), SessionId(220))].clone();
        assert_ne!(renewed.serial, before.serial);
        let mut receipt = AccountsSnapshot::default();
        receipt.details.supported = true;
        s.finish_account_cancel(&stale, receipt.clone(), cx);
        assert!(s.live[&pane].pane.account_switching());
        let cancel = Cancel {
            hold: renewed,
            target: new,
            epoch: s.account_epochs[&host],
        };
        s.finish_account_cancel(&cancel, receipt, cx);
        assert!(!s.live[&pane].pane.account_switching());
        assert!(!s.account_holds.contains_key(&(host, SessionId(220))));
    });
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let (kind, body) = ipc::read_frame(&mut peer).unwrap().unwrap();
    assert_eq!(Input::decode(kind, &body).unwrap().bytes, b"abc\r");
    peer.set_nonblocking(true).unwrap();
    assert_eq!(
        peer.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[gpui::test]
fn pending_switch_stays_silent_and_recovers_only_after_its_grace_period(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("silent-pending-grace"));
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_session_account_features(HostId::Local, ipc::FEATURES, cx);
        });
        let key = (HostId::Local, SessionId(220));
        s.observe_account_hold(
            &key.0,
            &SessionHandoff {
                from: key.1,
                to: None,
                account_id: "selected".into(),
                error: None,
            },
        );
        assert!(s.account_input_recovers_silently(&s.account_holds[&key], cx));
        assert!(!s.can_recover_account_input(&s.account_holds[&key], cx));
        s.account_holds.get_mut(&key).unwrap().observed_at -= Duration::from_secs(6);
        assert!(s.can_recover_account_input(&s.account_holds[&key], cx));
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_session_account_features(
                HostId::Local,
                ipc::FEATURES & !krill_proto::usage::FEAT_ACCOUNT_INPUT_RECOVERY,
                cx,
            );
        });
        assert!(
            !s.can_recover_account_input(&s.account_holds[&key], cx),
            "older daemons must not receive automatic rollback requests"
        );
    });
}

#[gpui::test]
fn late_automatic_receipt_cannot_release_input_after_a_new_account_mutation(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open(cx, state_path("silent-late-receipt"));
    let (ours, mut peer) = UnixStream::pair().unwrap();
    view.update(cx, |s, cx| {
        let pane = s.layout.focused().unwrap();
        let paths = Paths {
            socket: state_path("silent-late-socket"),
            holder: state_path("silent-late-holder"),
            krilld: state_path("silent-late-daemon"),
        };
        let target = HostTarget::local(paths.clone());
        s.paths = Some(paths);
        s.link = Link::Up;
        s.attach_test(pane, SessionId(220), ours, cx);
        s.layout.set_session(pane, SessionId(220));
        s.live.get_mut(&pane).unwrap().target = Some(target.clone());
        let mut failed = AccountsSnapshot::default();
        failed.details.supported = true;
        failed.details.handoffs.push(SessionHandoff {
            from: SessionId(220),
            to: None,
            account_id: "selected".into(),
            error: Some("fixture".into()),
        });
        s.apply_accounts_snapshot(failed, cx);
        let cancel = Cancel {
            hold: s.account_holds[&(HostId::Local, SessionId(220))].clone(),
            target,
            epoch: s
                .account_epochs
                .get(&HostId::Local)
                .copied()
                .unwrap_or_default(),
        };
        s.account_cancel = Some(cancel.clone());
        s.account_recovery.automatic = true;
        s.send_inner(
            &Output {
                pty: b"abc\r".to_vec(),
                ..Output::default()
            },
            false,
            None,
            cx,
        );
        s.begin_account_mutation(&HostId::Local);
        let mut receipt = AccountsSnapshot::default();
        receipt.details.supported = true;
        s.settle_account_recovery(&cancel, Ok(Some(receipt)), cx);
        assert!(s.live[&pane].pane.account_switching());
        assert!(
            s.account_holds
                .contains_key(&(HostId::Local, SessionId(220)))
        );
    });
    peer.set_nonblocking(true).unwrap();
    assert_eq!(
        peer.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[gpui::test]
fn automatic_recovery_keeps_typing_and_enter_then_delivers_once_without_a_toast(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open(cx, state_path("silent-account-input"));
    let root = state_path("silent-account-daemon")
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(&root).unwrap();
    let paths = Paths {
        socket: root.join("daemon.sock"),
        holder: root.join("unused-holder"),
        krilld: root.join("unused-daemon"),
    };
    let listener = UnixListener::bind(&paths.socket).unwrap();
    let (release, blocked) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let hello = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: ipc::FEATURES & !krill_proto::daemon::FEAT_HOLDER_WATCH,
            build: "recovery fixture".into(),
            instance_id: 1,
        };
        let (mut watch, _) = listener.accept().unwrap();
        watch.write_all(&hello.encode().unwrap()).unwrap();
        ipc::read_frame(&mut watch).unwrap().unwrap();
        drop(watch);
        let (mut connection, _) = listener.accept().unwrap();
        connection
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        connection.write_all(&hello.encode().unwrap()).unwrap();
        let (kind, body) = ipc::read_frame(&mut connection).unwrap().unwrap();
        let client = ClientHello::decode(kind, &body).unwrap();
        let (kind, body) = ipc::read_frame(&mut connection).unwrap().unwrap();
        assert_eq!(
            UsageRequest::decode(kind, &body).unwrap(),
            UsageRequest::RecoverAccountInput {
                session: SessionId(220),
                account_id: "selected".into(),
            }
        );
        blocked.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut recovered = AccountsSnapshot::default();
        recovered.details.supported = true;
        connection
            .write_all(
                &UsageReply::Accounts(recovered)
                    .for_features(client.features)
                    .encode()
                    .unwrap(),
            )
            .unwrap();
    });
    let (ours, mut peer) = UnixStream::pair().unwrap();
    let pane = view.read_with(cx, |s, _| s.layout.focused().unwrap());
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, SessionId(220), ours, cx);
            s.paths = Some(paths.clone());
            s.link = Link::Up;
            s.layout.set_session(pane, SessionId(220));
            s.live.get_mut(&pane).unwrap().target = Some(HostTarget::local(paths));
            s.sidebar.update(cx, |sidebar, cx| {
                sidebar.set_session_account_features(HostId::Local, ipc::FEATURES, cx);
            });
            let mut failed = AccountsSnapshot::default();
            failed.details.supported = true;
            failed.details.handoffs.push(SessionHandoff {
                from: SessionId(220),
                to: None,
                account_id: "selected".into(),
                error: Some("fixture failure".into()),
            });
            s.apply_accounts_snapshot(failed, cx);
            assert!(s.account_cancel.is_some());
            assert!(s.account_recovery.automatic);
            // Deliver both handlers before the asynchronous recovery receipt can apply.
            s.replace_text_in_range(None, "abc", window, cx);
            s.on_key_down(
                &gpui::KeyDownEvent {
                    keystroke: gpui::Keystroke::parse("enter").unwrap(),
                    is_held: false,
                    prefer_character_input: false,
                },
                window,
                cx,
            );
            assert!(s.live[&pane].pane.account_switching());
            assert!(s.notice.is_none());
        });
    });
    peer.set_nonblocking(true).unwrap();
    assert_eq!(
        peer.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        cx.run_until_parked();
        if view.read_with(cx, |s, _| !s.live[&pane].pane.account_switching()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "automatic rollback did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    worker.join().unwrap();
    assert!(cx.debug_bounds("account-input-recovery").is_none());
    assert!(cx.debug_bounds("notice").is_none());
    peer.set_nonblocking(false).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut bytes = Vec::new();
    while bytes.len() < 4 {
        let (kind, body) = ipc::read_frame(&mut peer).unwrap().unwrap();
        bytes.extend(Input::decode(kind, &body).unwrap().bytes);
    }
    assert_eq!(bytes, b"abc\r");
    peer.set_nonblocking(true).unwrap();
    assert_eq!(
        peer.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    cx.update(|window, cx| {
        let s = view.read(cx);
        assert!(s.focus.is_focused(window));
        assert!(s.notice.is_none());
        assert!(s.account_holds.is_empty());
        assert!(s.closed_sessions.is_empty());
    });
}
