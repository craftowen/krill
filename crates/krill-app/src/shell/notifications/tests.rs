use super::*;
use crate::shell::tests::{open, state_path};
use gpui::{Modifiers, TestAppContext};
use krill_proto::SessionId;
use krill_proto::screen::Msg as _;

fn settle(cx: &mut gpui::VisualTestContext) {
    // Let toast measurement, entry/exit and stack geometry each receive frames.
    for _ in 0..8 {
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }
}

#[gpui::test]
fn account_recovery_stays_in_toast_without_resizing_or_releasing_input(cx: &mut TestAppContext) {
    use krill_proto::usage::{AccountsSnapshot, SessionHandoff};

    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("toast-account-recovery"));
    let (ours, _peer) = UnixStream::pair().unwrap();
    let session = SessionId(91);
    let pane = view.read_with(cx, |s, _| s.layout.active_tab().focused);
    view.update(cx, |s, cx| {
        s.attach_test(pane, session, ours, cx);
        s.layout.set_session(pane, session);
        cx.notify();
    });
    cx.run_until_parked();
    let before = cx.debug_bounds("terminal-surface").unwrap();
    view.update(cx, |s, cx| {
        let mut failed = AccountsSnapshot::default();
        failed.details.supported = true;
        failed.details.handoffs.push(SessionHandoff {
            from: session,
            to: None,
            account_id: "selected".into(),
            error: Some("fixture failure".into()),
        });
        s.apply_accounts_snapshot(failed, cx);
        s.notice = None;
    });
    cx.run_until_parked();
    settle(cx);
    assert_eq!(cx.debug_bounds("terminal-surface").unwrap(), before);
    let recovery = cx.debug_bounds("account-input-recovery").unwrap();
    cx.update(|window, cx| {
        assert!(recovery.origin.x > window.viewport_size().width / 2.);
        assert!(recovery.origin.y >= ui::TITLEBAR_HEIGHT);
        assert!(recovery.bottom() < window.viewport_size().height / 2.);
        assert!(view.read(cx).focus.is_focused(window));
    });
    assert!(cx.debug_bounds("account-continue-original").is_some());
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    assert!(cx.debug_bounds("account-continue-original").is_some());
    // Closing the card must neither send held input nor recreate the top strip.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.notifications.notes[&Kind::AccountInput]
                .clone()
                .update(cx, |note, cx| note.dismiss(window, cx));
        });
    });
    settle(cx);
    view.update(cx, |s, cx| {
        s.apply_accounts_snapshot((*s.accounts_snapshot).clone(), cx);
        assert!(s.live[&pane].pane.account_switching());
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("account-input-recovery").is_none());
    assert_eq!(cx.debug_bounds("terminal-surface").unwrap(), before);
}

#[gpui::test]
fn dismissed_account_recovery_stays_dismissed_across_tab_switches_until_new_failure(
    cx: &mut TestAppContext,
) {
    use krill_proto::usage::{AccountsSnapshot, SessionHandoff};

    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("toast-account-tab-switch"));
    let (ours, _peer) = UnixStream::pair().unwrap();
    let session = SessionId(95);
    let pane = view.read_with(cx, |s, _| s.layout.active_tab().focused);
    view.update(cx, |s, cx| {
        s.attach_test(pane, session, ours, cx);
        s.layout.set_session(pane, session);
        s.change(true, cx, |l| {
            l.new_tab();
            l.activate(0);
        });
    });
    cx.run_until_parked();

    let snapshot_for = |account_id: &str, error: &str| {
        let mut snap = AccountsSnapshot::default();
        snap.details.supported = true;
        snap.details.handoffs.push(SessionHandoff {
            from: session,
            to: None,
            account_id: account_id.into(),
            error: Some(error.into()),
        });
        snap
    };

    view.update(cx, |s, cx| {
        s.apply_accounts_snapshot(snapshot_for("selected", "first failure"), cx);
        s.notice = None;
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("account-input-recovery").is_some());

    // Switching away and back without dismissing keeps the recovery card visible.
    view.update(cx, |s, cx| s.change(true, cx, |l| l.activate(1)));
    settle(cx);
    assert!(cx.debug_bounds("account-input-recovery").is_none());
    view.update(cx, |s, cx| s.change(true, cx, |l| l.activate(0)));
    settle(cx);
    assert!(cx.debug_bounds("account-input-recovery").is_some());

    // Explicitly dismissing the recovery card remembers that dismissal across tab switches.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.notifications.notes[&Kind::AccountInput]
                .clone()
                .update(cx, |note, cx| note.dismiss(window, cx));
        });
    });
    settle(cx);
    assert!(cx.debug_bounds("account-input-recovery").is_none());

    view.update(cx, |s, cx| s.change(true, cx, |l| l.activate(1)));
    settle(cx);
    view.update(cx, |s, cx| s.change(true, cx, |l| l.activate(0)));
    settle(cx);
    assert!(
        cx.debug_bounds("account-input-recovery").is_none(),
        "returning to a tab must not resurrect a dismissed recovery toast for the same failure"
    );

    // A new switch failure for a different target still notifies the user.
    view.update(cx, |s, cx| {
        s.apply_accounts_snapshot(snapshot_for("another", "second failure"), cx);
        s.notice = None;
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("account-input-recovery").is_some());
}

#[gpui::test]
fn uncertain_input_is_silent_but_rejected_input_remains_visible(cx: &mut TestAppContext) {
    use krill_proto::input::Fate;

    let (view, cx) = open(cx, state_path("quiet-uncertain-input"));
    view.update(cx, |s, cx| {
        s.show(
            Some(&Notice {
                fate: Fate::Uncertain,
                bytes: b"private input".to_vec(),
            }),
            cx,
        );
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("notice").is_none());
    view.update(cx, |s, cx| {
        for fate in [
            Fate::Expired,
            Fate::NewSession,
            Fate::SessionClosed,
            Fate::Full,
            Fate::Unsupported,
            Fate::AccountSwitchFailed,
        ] {
            let notice = Notice {
                fate,
                bytes: b"rejected input".to_vec(),
            };
            s.show(Some(&notice), cx);
            let important = s.notice.clone();
            assert_eq!(important.as_deref(), Some(notice_text(&notice).as_str()));
            s.show(
                Some(&Notice {
                    fate: Fate::Uncertain,
                    bytes: b"other input".to_vec(),
                }),
                cx,
            );
            assert_eq!(
                s.notice, important,
                "uncertainty must not replace actionable errors"
            );
        }
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("notice").is_some());
}

#[gpui::test]
fn background_account_failure_preserves_existing_notice_but_user_action_reports_error(
    cx: &mut TestAppContext,
) {
    use krill_proto::usage::UsageRequest;

    let path = state_path("quiet-account-refresh");
    let missing_socket = path.with_extension("missing.sock");
    let (view, cx) = open(cx, path);
    view.update(cx, |s, cx| {
        // Bypass connection setup: both requests fail immediately on the same missing socket.
        s.link = Link::Up;
        s.paths = Some(krill_proto::daemon::Paths {
            socket: missing_socket,
            holder: PathBuf::from("/bin/echo"),
            krilld: PathBuf::from("/bin/echo"),
        });
        s.notice = Some("Existing actionable error".into());
        s.refresh_accounts(UsageRequest::ListAccounts, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |s, _| s.notice.clone()).as_deref(),
        Some("Existing actionable error")
    );

    view.update(cx, |s, cx| {
        s.refresh_accounts(UsageRequest::RefreshUsage, cx);
    });
    cx.run_until_parked();
    let notice = view.read_with(cx, |s, _| s.notice.clone());
    assert!(notice.is_some());
    assert_ne!(notice.as_deref(), Some("Existing actionable error"));
}

#[gpui::test]
fn notices_overlay_top_right_without_resize_focus_or_duplicates(cx: &mut TestAppContext) {
    // GPUI's visual animation uses wall time; timers below use the test clock.
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("toast-geometry"));
    let before = cx.debug_bounds("terminal-surface").unwrap();
    view.update(cx, |s, cx| {
        s.notice = Some("선택한 파일을 읽을 수 없습니다. 다른 파일을 선택하거나 다시 시도하세요. Unable to read the selected file. Choose another file or retry after checking the connection.".into());
        cx.notify();
    });
    cx.run_until_parked();
    settle(cx);
    let after = cx.debug_bounds("terminal-surface").unwrap();
    assert_eq!(before, after, "toast must not reduce terminal rows");
    let notice = cx.debug_bounds("notice").unwrap();
    cx.update(|window, cx| {
        assert!(notice.origin.x > window.viewport_size().width / 2.);
        assert!(notice.origin.y >= ui::TITLEBAR_HEIGHT);
        assert!(notice.bottom() < window.viewport_size().height / 2.);
        assert!(notice.right() <= window.viewport_size().width);
        assert!(
            notice.size.height >= ui::TEXT_SM.to_pixels(window.rem_size()) * 2.,
            "long Korean/English notice must wrap into multiple lines"
        );
        assert!(notice.bottom() <= window.viewport_size().height - ui::STATUS_BAR_HEIGHT);
        assert!(view.read(cx).focus.is_focused(window));
    });
    let id = view.update(cx, |s, cx| {
        let list = s.notifications.list.as_ref().unwrap().read(cx);
        assert_eq!(list.notifications().len(), 1);
        s.notifications.notes[&Kind::Notice].entity_id()
    });
    for _ in 0..5 {
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
    }
    view.update(cx, |s, cx| {
        assert_eq!(s.notifications.notes[&Kind::Notice].entity_id(), id);
        assert_eq!(
            s.notifications
                .list
                .as_ref()
                .unwrap()
                .read(cx)
                .notifications()
                .len(),
            1
        );
    });
    // Clicking dismisses before the timer.
    assert!(cx.debug_bounds("notice").is_some());
    let at = cx.debug_bounds("notice").unwrap().center();
    cx.simulate_click(at, Modifiers::default());
    settle(cx);
    assert!(view.read_with(cx, |s, _| s.notice.is_none()));
    assert!(cx.debug_bounds("notice").is_none());
    assert_eq!(cx.debug_bounds("terminal-surface").unwrap(), before);
    // One-shot notices close themselves on the stack's timer.
    view.update(cx, |s, cx| {
        s.notice = Some("Session 7: could not resume with selected account".into());
        cx.notify();
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("notice").is_some());
    cx.executor().advance_clock(Duration::from_secs(6));
    cx.run_until_parked();
    settle(cx);
    assert!(view.read_with(cx, |s, _| s.notice.is_none()));
    assert!(cx.debug_bounds("notice").is_none());
}

#[gpui::test]
fn resolved_link_dismisses_without_hiding_new_errors(cx: &mut TestAppContext) {
    // GPUI's visual animation uses wall time; timers below use the test clock.
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("toast-lifecycle"));
    view.update(cx, |s, cx| {
        s.link = Link::Failed("Connection needs attention".into());
        cx.notify();
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("link-banner").is_some());
    view.update(cx, |s, cx| {
        s.link = Link::Up;
        s.notice = Some("Action failed: retry from the settings page".into());
        cx.notify();
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("link-banner").is_none());
    assert!(cx.debug_bounds("notice").is_some());
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert!(cx.debug_bounds("away-summary").is_none());
}

#[gpui::test]
fn safety_and_hook_actions_remain_available_in_toasts(cx: &mut TestAppContext) {
    // GPUI's visual animation uses wall time; timers below use the test clock.
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("toast-actions"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let pane = view.read_with(cx, |s, _| s.layout.active_tab().focused);
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, SessionId(91), ours, cx);
            s.layout.set_session(pane, SessionId(91));
            s.paste_bytes(b"echo one\necho two", window, cx);
        });
    });
    cx.run_until_parked();
    settle(cx);
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    assert!(cx.debug_bounds("paste-confirm").is_some());
    assert!(cx.update(|window, cx| view.read(cx).focus.is_focused(window)));
    theirs.set_nonblocking(true).unwrap();
    let mut byte = [0];
    assert_eq!(
        std::io::Read::read(&mut theirs, &mut byte)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    let confirm = cx.debug_bounds("paste-confirm").unwrap().center();
    cx.simulate_click(confirm, Modifiers::default());
    settle(cx);
    assert!(view.read_with(cx, |s, _| s.security.pending_paste().is_none()));
    theirs.set_nonblocking(false).unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (kind, body) = ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(
        krill_proto::input::Input::decode(kind, &body)
            .unwrap()
            .bytes,
        b"echo one\recho two"
    );

    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.paste_bytes(b"cancel\nthis", window, cx);
        });
    });
    cx.run_until_parked();
    settle(cx);
    let cancel = cx.debug_bounds("paste-cancel").unwrap().center();
    cx.simulate_mouse_move(cancel, None, Modifiers::default());
    settle(cx);
    let cancel = cx.debug_bounds("paste-cancel").unwrap().center();
    cx.simulate_click(cancel, Modifiers::default());
    settle(cx);
    assert!(view.read_with(cx, |s, _| s.security.pending_paste().is_none()));

    cx.update(|_, cx| {
        cx.set_global(FixNeeds {
            items: vec![FixNeed {
                check_id: "hook.codex".into(),
                message: "Codex hook setup is available".into(),
            }],
        });
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("fix-banner-fix").is_some());
    let fix = cx.debug_bounds("fix-banner").unwrap();
    assert!(fix.origin.y >= ui::TITLEBAR_HEIGHT);
    assert!(fix.bottom() < cx.debug_bounds("terminal-surface").unwrap().size.height / 2.);
    let close = cx.debug_bounds("fix-banner-close").unwrap().center();
    cx.simulate_click(close, Modifiers::default());
    settle(cx);
    assert!(cx.debug_bounds("fix-banner").is_none());
    assert!(cx.update(|_, cx| cx.global::<FixNeeds>().items.is_empty()));
}

#[gpui::test]
fn hook_setup_hint_expires_without_reappearing_on_render(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("toast-hook-timeout"));
    cx.update(|_, cx| {
        cx.set_global(FixNeeds {
            items: vec![FixNeed {
                check_id: "hook.codex".into(),
                message: "Codex hook setup is available".into(),
            }],
        });
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("fix-banner-fix").is_some());
    cx.executor().advance_clock(Duration::from_secs(6));
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("fix-banner").is_none());
    assert!(cx.update(|_, cx| cx.global::<FixNeeds>().items.is_empty()));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("fix-banner").is_none());
    assert!(cx.update(|window, cx| view.read(cx).focus.is_focused(window)));
}

#[gpui::test]
fn sidebar_poll_and_notice_alerts_render_as_toasts_without_sidebar_layout_shift(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("toast-sidebar-alerts"));
    let sidebar_header_before = cx.debug_bounds("sidebar-header").unwrap();
    let sidebar_viewport_before = cx.debug_bounds("sidebar-viewport").unwrap();
    let terminal_before = cx.debug_bounds("terminal-surface").unwrap();

    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sb, cx| {
            sb.set_poll_error(Some("daemon slow".into()), cx);
        });
    });
    cx.run_until_parked();
    settle(cx);

    assert_eq!(
        cx.debug_bounds("sidebar-header").unwrap(),
        sidebar_header_before
    );
    assert_eq!(
        cx.debug_bounds("sidebar-viewport").unwrap(),
        sidebar_viewport_before
    );
    assert_eq!(
        cx.debug_bounds("terminal-surface").unwrap(),
        terminal_before
    );
    let poll_toast = cx.debug_bounds("sidebar-poll-error").unwrap();
    cx.update(|window, _| {
        assert!(poll_toast.origin.x > window.viewport_size().width / 2.);
        assert!(poll_toast.origin.y >= ui::TITLEBAR_HEIGHT);
        assert!(poll_toast.bottom() < window.viewport_size().height / 2.);
    });

    // 폴링이 복구되면 토스트가 자동으로 사라진다.
    view.update(cx, |s, cx| {
        s.sidebar.update(cx, |sb, cx| {
            sb.clear_poll_error(cx);
        });
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("sidebar-poll-error").is_none());
}

#[gpui::test]
fn automatic_reconnect_is_quiet_but_actionable_failure_is_visible(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("quiet-reconnect"));
    view.update(cx, |s, cx| s.set_link(Link::Down, cx));
    settle(cx);
    assert!(cx.debug_bounds("link-banner").is_none());
    view.update(cx, |s, cx| {
        s.set_link(Link::Failed("permission denied".into()), cx);
    });
    settle(cx);
    assert!(cx.debug_bounds("link-banner").is_some());
    view.update(cx, |s, cx| s.set_link(Link::Up, cx));
    settle(cx);
    assert!(cx.debug_bounds("link-banner").is_none());
}
