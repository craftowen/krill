use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use gpui::TestAppContext;
use krill_proto::input::Input;
use krill_proto::screen::Msg as _;

use crate::shell::tests::{open, state_path};

#[gpui::test]
fn slow_pastes_keep_keystrokes_in_submission_order(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("async-paste-order"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    let (first, second) = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let first = shell.begin_clipboard_paste(window, cx).unwrap();
            shell.send(
                &krill_term::ime::Output {
                    pty: b"x".to_vec(),
                    discard_ime: false,
                },
                Some(window),
                cx,
            );
            let second = shell.begin_clipboard_paste(window, cx).unwrap();
            shell.send(
                &krill_term::ime::Output {
                    pty: b"y".to_vec(),
                    discard_ime: false,
                },
                Some(window),
                cx,
            );
            (first, second)
        })
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(second, Some(b"B".to_vec()), window, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("panel-column").is_some(),
        "window keeps drawing while clipboard work is pending"
    );
    theirs.set_nonblocking(true).unwrap();
    let mut byte = [0];
    assert_eq!(
        theirs.read(&mut byte).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    theirs.set_nonblocking(false).unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(first, Some(b"A".to_vec()), window, cx);
        });
    });
    let received = (0..4)
        .map(|_| {
            let (kind, body) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
            Input::decode(kind, &body).unwrap().bytes
        })
        .collect::<Vec<_>>();
    assert_eq!(
        received,
        [b"A".to_vec(), b"x".to_vec(), b"B".to_vec(), b"y".to_vec()]
    );
}

#[gpui::test]
fn paste_result_never_targets_a_later_tab(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("async-paste-stale"));
    let (ours, _theirs) = UnixStream::pair().unwrap();
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    let pending = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.begin_clipboard_paste(window, cx).unwrap()
        })
    });
    cx.simulate_keystrokes("cmd-t");
    let later = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    view.update(cx, |shell, cx| {
        shell.attach_test(later, krill_proto::SessionId(43), ours, cx);
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(pending, Some(b"stale".to_vec()), window, cx);
        });
    });
    cx.run_until_parked();
    theirs.set_nonblocking(true).unwrap();
    assert_eq!(
        theirs.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[gpui::test]
fn paste_deadline_releases_typing_and_rejects_late_image_result(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("async-paste-deadline"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    let request = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let request = shell.begin_clipboard_paste(window, cx).unwrap();
            shell.send(
                &krill_term::ime::Output {
                    pty: b"typed".to_vec(),
                    discard_ime: false,
                },
                Some(window),
                cx,
            );
            request
        })
    });
    cx.run_until_parked();
    cx.executor()
        .advance_clock(super::PASTE_PREPARATION_TIMEOUT);
    cx.run_until_parked();
    let (kind, body) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(Input::decode(kind, &body).unwrap().bytes, b"typed");
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(request, Some(b"late image".to_vec()), window, cx);
        });
    });
    cx.run_until_parked();
    theirs.set_nonblocking(true).unwrap();
    assert_eq!(
        theirs.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[gpui::test]
fn pending_paste_in_another_pane_does_not_block_ready_paste(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("async-paste-independent-panes"));
    let (ours, _theirs) = UnixStream::pair().unwrap();
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.begin_clipboard_paste(window, cx).unwrap();
        });
    });
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let request = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            // Keep the earlier pane's request pending to exercise independent queues.
            shell.layout.new_tab();
            let pane = shell.layout.active_tab().focused;
            shell.attach_test(pane, krill_proto::SessionId(43), ours, cx);
            shell.begin_clipboard_paste(window, cx).unwrap()
        })
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(request, Some(b"ready".to_vec()), window, cx);
        });
    });
    let (kind, body) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(Input::decode(kind, &body).unwrap().bytes, b"ready");
}

#[test]
fn preparation_admission_is_held_until_backend_guard_finishes() {
    let active = std::sync::atomic::AtomicUsize::new(0);
    let first = super::PreparationPermit::acquire(&active).unwrap();
    let second = super::PreparationPermit::acquire(&active).unwrap();
    assert!(super::PreparationPermit::acquire(&active).is_none());
    // Releasing a UI request does not release these backend-owned permits.
    assert_eq!(active.load(std::sync::atomic::Ordering::Acquire), 2);
    drop(first);
    assert!(super::PreparationPermit::acquire(&active).is_some());
    drop(second);
    assert_eq!(active.load(std::sync::atomic::Ordering::Acquire), 0);
}
