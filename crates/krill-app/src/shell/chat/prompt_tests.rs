//! Composer input and delayed submission regressions.
#![cfg(unix)]
use super::action_tests::{fixture, permission};
use super::*;
use gpui::TestAppContext;
use krill_proto::term_modes::Modes;
const SESSION: SessionId = SessionId(991);

#[gpui::test]
fn chat_single_line_bracketed_paste_submits_separately_and_keeps_draft_until_enter(
    cx: &mut TestAppContext,
) {
    let (view, cx, _, sent) = fixture(cx, "chat-single-line-paste", &permission(), "ready");
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            shell.on_frame(
                pane,
                krill_proto::term_modes::TERM_MODES,
                &postcard::to_stdvec(&Modes {
                    flags: Modes::BRACKETED_PASTE,
                    kitty: 0,
                })
                .unwrap(),
                cx,
            );
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value("한 줄 prompt", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            assert_eq!(input.read(cx).value(), "한 줄 prompt");
            input
        })
    });
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec!["\x1b[200~한 줄 prompt\x1b[201~".as_bytes().to_vec()]
    );
    cx.executor().advance_clock(Duration::from_millis(249));
    cx.run_until_parked();
    assert_eq!(
        sent.lock().unwrap().len(),
        1,
        "Enter must remain outside the paste suppression window"
    );
    cx.executor().advance_clock(Duration::from_millis(2));
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![
            "\x1b[200~한 줄 prompt\x1b[201~".as_bytes().to_vec(),
            b"\r".to_vec()
        ]
    );
    assert!(input.read_with(cx, |state, _| state.value()).is_empty());
}

#[gpui::test]
fn chat_composer_shift_enter_inserts_newline_enter_sends_one_bracketed_prompt(
    cx: &mut TestAppContext,
) {
    let (view, cx, _, sent) = fixture(
        cx,
        "chat-multiline-keys",
        &permission(),
        "deploy production (y/n)",
    );
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            let modes = krill_proto::term_modes::Modes {
                flags: krill_proto::term_modes::Modes::BRACKETED_PASTE,
                kitty: 0,
            };
            shell.on_frame(
                pane,
                krill_proto::term_modes::TERM_MODES,
                &postcard::to_stdvec(&modes).unwrap(),
                cx,
            );
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.focus(window, cx));
            input
        })
    });
    cx.simulate_input("첫 줄");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("second line");
    cx.run_until_parked();
    assert!(
        sent.lock().unwrap().is_empty(),
        "Shift+Enter must never submit"
    );
    assert_eq!(
        input.read_with(cx, |state, _| state.value()).as_ref(),
        "첫 줄\nsecond line"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec!["\x1b[200~첫 줄\nsecond line\x1b[201~".as_bytes().to_vec()]
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(
        sent.lock().unwrap().len(),
        1,
        "pending submit must be coalesced"
    );
    cx.executor().advance_clock(Duration::from_millis(249));
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().len(), 1);
    cx.executor().advance_clock(Duration::from_millis(2));
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![
            "\x1b[200~첫 줄\nsecond line\x1b[201~".as_bytes().to_vec(),
            b"\r".to_vec()
        ]
    );
    assert!(input.read_with(cx, |state, _| state.value()).is_empty());
    cx.simulate_keystrokes("enter");
    assert_eq!(sent.lock().unwrap().len(), 2, "empty Enter must not submit");
}

#[gpui::test]
fn chat_unbracketed_multiline_and_ime_confirmation_preserve_the_draft(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler as _;
    let (view, cx, _, sent) = fixture(
        cx,
        "chat-multiline-refusal",
        &permission(),
        "deploy production (y/n)",
    );
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value("one\ntwo", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            assert_eq!(input.read(cx).value(), "one\ntwo");
            input
        })
    });
    cx.run_until_parked();
    assert!(sent.lock().unwrap().is_empty());
    assert!(cx.debug_bounds("chat-multiline-error").is_some());
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.replace_and_mark_text_in_range(None, "ㅎ", None, window, cx);
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            assert_eq!(input.read(cx).value(), "ㅎ");
            assert!(sent.lock().unwrap().is_empty());
            input.update(cx, |state, cx| {
                state.replace_text_in_range(None, "한", window, cx);
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    assert_eq!(*sent.lock().unwrap(), vec!["한\r".as_bytes().to_vec()]);
}

#[gpui::test]
fn chat_delayed_submit_keeps_edited_draft_and_never_retargets_a_new_host(cx: &mut TestAppContext) {
    let (view, cx, _, sent) = fixture(
        cx,
        "chat-multiline-origin",
        &permission(),
        "deploy production (y/n)",
    );
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            let modes = Modes {
                flags: Modes::BRACKETED_PASTE,
                kitty: 0,
            };
            shell.on_frame(
                pane,
                krill_proto::term_modes::TERM_MODES,
                &postcard::to_stdvec(&modes).unwrap(),
                cx,
            );
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| {
                state.set_value("first\nmessage", window, cx);
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value("new draft", window, cx));
            input
        })
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(251));
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![b"\x1b[200~first\nmessage\x1b[201~".to_vec(), b"\r".to_vec()]
    );
    assert_eq!(input.read_with(cx, |state, _| state.value()), "new draft");
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            input.update(cx, |state, cx| {
                state.set_value("second\nmessage", window, cx);
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            let pane = shell.layout.focused().unwrap();
            shell
                .layout
                .set_host(pane, krill_app::hosts::HostId::Remote("replacement".into()));
        });
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(251));
    cx.run_until_parked();
    assert_eq!(
        sent.lock().unwrap().len(),
        3,
        "no delayed Enter may target the replacement host"
    );
    assert_eq!(
        input.read_with(cx, |state, _| state.value()),
        "second\nmessage"
    );
    assert!(view.read_with(cx, |shell, _| {
        shell.chat.prompt_review.contains(&local_key(SESSION))
    }));
}

#[gpui::test]
fn chat_multiline_transport_failure_preserves_draft_and_prevents_blind_replay(
    cx: &mut TestAppContext,
) {
    let (view, cx, _, sent) = fixture(
        cx,
        "chat-multiline-failure",
        &permission(),
        "deploy production (y/n)",
    );
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            let modes = Modes {
                flags: Modes::BRACKETED_PASTE,
                kitty: 0,
            };
            shell.on_frame(
                pane,
                krill_proto::term_modes::TERM_MODES,
                &postcard::to_stdvec(&modes).unwrap(),
                cx,
            );
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| {
                state.set_value("retry\ncarefully", window, cx);
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            shell.chat.send_input = Some(Box::new(|_| false));
            input
        })
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(251));
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert_eq!(
        input.read_with(cx, |state, _| state.value()),
        "retry\ncarefully"
    );
    assert!(cx.debug_bounds("chat-prompt-review").is_some());
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    assert_eq!(sent.lock().unwrap().len(), 1);
    let recovery = cx.debug_bounds("chat-prompt-review-done").unwrap();
    cx.simulate_click(recovery.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(!view.read_with(cx, |shell, _| {
        shell.chat.prompt_review.contains(&local_key(SESSION))
    }));
    assert_eq!(
        sent.lock().unwrap().len(),
        1,
        "recovery must not replay the draft"
    );
    assert_eq!(
        input.read_with(cx, |state, _| state.value()),
        "retry\ncarefully"
    );
}

#[gpui::test]
fn chat_paste_waits_for_pty_ack_and_provider_turn_before_clearing(cx: &mut TestAppContext) {
    use krill_proto::input::Delivery;
    let (view, cx, _, sent) = fixture(cx, "chat-receipt-gate", &permission(), "ready");
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            shell.on_frame(
                pane,
                krill_proto::term_modes::TERM_MODES,
                &postcard::to_stdvec(&Modes {
                    flags: Modes::BRACKETED_PASTE,
                    kitty: 0,
                })
                .unwrap(),
                cx,
            );
            shell.chat.prompt_delivery = Some(Delivery::Pending);
            shell.chat.prompt_confirmed = Some(false);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| {
                state.set_value("ack gated prompt", window, cx);
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            input
        })
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(
        sent.lock().unwrap().len(),
        1,
        "a queued paste is not a PTY receipt"
    );
    view.update(cx, |shell, _| {
        shell.chat.prompt_delivery = Some(Delivery::Applied);
    });
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(249));
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().len(), 1);
    cx.executor().advance_clock(Duration::from_millis(2));
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert_eq!(
        input.read_with(cx, |state, _| state.value()),
        "ack gated prompt"
    );
    view.update(cx, |shell, _| shell.chat.prompt_confirmed = Some(true));
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
    assert!(input.read_with(cx, |state, _| state.value()).is_empty());
}

#[gpui::test]
fn chat_uncertain_paste_never_submits_or_replays_and_preserves_draft(cx: &mut TestAppContext) {
    use krill_proto::input::Delivery;
    let (view, cx, _, sent) = fixture(cx, "chat-receipt-lost", &permission(), "ready");
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            shell.on_frame(
                pane,
                krill_proto::term_modes::TERM_MODES,
                &postcard::to_stdvec(&Modes {
                    flags: Modes::BRACKETED_PASTE,
                    kitty: 0,
                })
                .unwrap(),
                cx,
            );
            shell.chat.prompt_delivery = Some(Delivery::Pending);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value("do not replay", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            input
        })
    });
    cx.run_until_parked();
    view.update(cx, |shell, _| {
        shell.chat.prompt_delivery = Some(Delivery::Uncertain);
    });
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert_eq!(
        input.read_with(cx, |state, _| state.value()),
        "do not replay"
    );
    view.read_with(cx, |shell, _| {
        assert!(shell.chat.prompt_review.contains(&local_key(SESSION)));
    });

    view.update(cx, |shell, cx| {
        shell.apply_chat_poll(
            SESSION,
            krill_proto::agent_history::ChatPollSnapshot {
                reset: true,
                bound: false,
                ..Default::default()
            },
        );
        shell.apply_chat_poll(
            SESSION,
            krill_proto::agent_history::ChatPollSnapshot {
                reset: true,
                bound: true,
                items: vec![krill_proto::agent_history::ChatItem {
                    kind: krill_proto::agent_history::ChatItemKind::Assistant,
                    text: "Unrelated recovered history".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        assert!(
            shell.chat.prompt_review.contains(&local_key(SESSION)),
            "polling cannot authorize replay"
        );
        cx.notify();
    });
}
