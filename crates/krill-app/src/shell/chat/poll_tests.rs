//! Polling must not depend on terminal redraws or hide transport failures.
#![cfg(unix)]
use super::*;
use crate::shell::tests::{open, state_path};
use std::sync::atomic::{AtomicUsize, Ordering};

#[gpui::test]
fn chat_retries_without_terminal_output_and_recovers_from_poll_failure(
    cx: &mut gpui::TestAppContext,
) {
    let (view, cx) = open(cx, state_path("chat-idle-retry"));
    let calls = Arc::new(AtomicUsize::new(0));
    view.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SessionId(992));
        shell.paths = Some(krill_proto::daemon::Paths {
            socket: "/unused".into(),
            holder: "/unused".into(),
            krilld: "/unused".into(),
        });
        let recorded = calls.clone();
        shell.chat.poll_call = Some(Arc::new(move |request| {
            let Request::AgentHistory(Action::ChatPoll { offset, .. }) = request else {
                panic!("expected ChatPoll");
            };
            let call = recorded.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                return Err(std::io::ErrorKind::ConnectionReset.into());
            }
            // The first retry still has no hook binding. A later transcript flush
            // becomes visible with no PTY frames, user clicks, or tab changes.
            Ok(Reply::AgentHistory(Response::ChatPolled(
                ChatPollSnapshot {
                    agent: "codex".into(),
                    session_id: "idle-conversation".into(),
                    transcript_path: "/external/rollout.jsonl".into(),
                    bound: call >= 2,
                    next_offset: if call >= 2 { 64 } else { 0 },
                    items: if call == 2 {
                        assert_eq!(*offset, 0);
                        vec![ChatItem {
                            kind: ChatItemKind::Assistant,
                            text: "Loaded after flush".into(),
                            ..Default::default()
                        }]
                    } else {
                        Vec::new()
                    },
                    ..Default::default()
                },
            )))
        }));
        shell.toggle_chat_view(cx);
    });
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(cx.debug_bounds("chat-poll-error").is_some());
    for _ in 0..2 {
        cx.executor().advance_clock(Duration::from_millis(800));
        cx.run_until_parked();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(cx.debug_bounds("chat-poll-error").is_none());
    view.read_with(cx, |shell, _| {
        let log = &shell.chat.logs[&SessionId(992)];
        assert!(log.bound);
        assert_eq!(log.offset, 64);
        assert_eq!(log.items[0].text, "Loaded after flush");
    });
    view.update(cx, super::super::Shell::toggle_chat_view);
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    view.read_with(cx, |shell, _| assert!(shell.chat.poll_timer.is_none()));
}

#[gpui::test]
fn chat_switch_moves_focus_and_feed_click_keeps_input_out_of_terminal(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::Focusable as _;
    let (view, cx) = open(cx, state_path("chat-toggle-focus"));
    view.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SessionId(994));
        cx.notify();
    });
    cx.run_until_parked();
    let toggle = cx.debug_bounds("toggle-chat-view").unwrap().center();
    cx.simulate_click(toggle, gpui::Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.read_with(cx, |shell, cx| {
            assert!(shell.is_active_chat_view());
            assert!(
                shell.chat.inputs[&SessionId(994)]
                    .0
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
        });
    });
    let overlay = cx.debug_bounds("desktop-chat-overlay").unwrap();
    let feed = overlay.origin + gpui::point(gpui::px(40.0), gpui::px(80.0));
    cx.simulate_click(feed, gpui::Modifiers::default());
    cx.simulate_keystrokes("h e l l o");
    cx.run_until_parked();
    view.read_with(cx, |shell, cx| {
        assert_eq!(
            shell.chat.inputs[&SessionId(994)]
                .0
                .read(cx)
                .value()
                .as_ref(),
            "hello"
        );
    });
    let term = cx.debug_bounds("chat-switch-terminal").unwrap().center();
    cx.simulate_click(term, gpui::Modifiers::default());
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.read_with(cx, |shell, _| {
            assert!(!shell.is_active_chat_view());
            assert!(shell.focus.is_focused(window));
        });
    });
    cx.simulate_click(toggle, gpui::Modifiers::default());
    cx.run_until_parked();
    cx.simulate_keystrokes("space a g a i n");
    view.read_with(cx, |shell, cx| {
        assert_eq!(
            shell.chat.inputs[&SessionId(994)]
                .0
                .read(cx)
                .value()
                .as_ref(),
            "hello again"
        );
    });
}
