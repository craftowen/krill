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
fn notices_overlay_bottom_right_without_resize_focus_or_duplicates(cx: &mut TestAppContext) {
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
        assert!(notice.origin.y > window.viewport_size().height / 2.);
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
    // Background snapshot updates do not make warnings expire.
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    assert!(cx.debug_bounds("notice").is_some());
    let at = cx.debug_bounds("notice").unwrap().center();
    cx.simulate_click(at, Modifiers::default());
    settle(cx);
    assert!(view.read_with(cx, |s, _| s.notice.is_none()));
    assert!(cx.debug_bounds("notice").is_none());
    assert_eq!(cx.debug_bounds("terminal-surface").unwrap(), before);
}

#[gpui::test]
fn summaries_expire_and_resolved_link_dismisses_without_hiding_new_errors(cx: &mut TestAppContext) {
    // GPUI's visual animation uses wall time; timers below use the test clock.
    cx.update(|cx| cx.set_reduce_motion(true));
    let (view, cx) = open(cx, state_path("toast-lifecycle"));
    view.update(cx, |s, cx| {
        s.link = Link::Down;
        s.away = Some("3 sessions completed while away".into());
        cx.notify();
    });
    cx.run_until_parked();
    settle(cx);
    assert!(cx.debug_bounds("away-summary").is_some());
    cx.executor().advance_clock(Duration::from_secs(6));
    cx.run_until_parked();
    settle(cx);
    assert!(view.read_with(cx, |s, _| s.away.is_none()));
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
    assert!(fix.origin.y > cx.debug_bounds("terminal-surface").unwrap().size.height / 2.);
    let close = cx.debug_bounds("fix-banner-close").unwrap().center();
    cx.simulate_click(close, Modifiers::default());
    settle(cx);
    assert!(cx.debug_bounds("fix-banner").is_none());
    assert!(cx.update(|_, cx| cx.global::<FixNeeds>().items.is_empty()));
}
