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
            assert!(input.read(cx).value().is_empty());
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
    assert!(input.read_with(cx, |state, _| state.value().is_empty()));
    view.update(cx, |shell, _| {
        assert_eq!(shell.chat.pending_prompts[&local_key(SESSION)].len(), 1);
        shell.apply_chat_poll(
            SESSION,
            krill_proto::agent_history::ChatPollSnapshot {
                bound: true,
                session_id: "agent-991".into(),
                transcript_path: "/fake/chat.jsonl".into(),
                items: vec![krill_proto::agent_history::ChatItem {
                    kind: krill_proto::agent_history::ChatItemKind::User,
                    text: "ack gated prompt".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        shell.chat.prompt_confirmed = None;
    });
    cx.executor().advance_clock(Duration::from_millis(20));
    cx.run_until_parked();
    assert!(input.read_with(cx, |state, _| state.value()).is_empty());
    view.read_with(cx, |shell, _| {
        assert!(
            shell
                .chat
                .pending_prompts
                .get(&local_key(SESSION))
                .is_none_or(Vec::is_empty)
        );
    });
}

#[gpui::test]
fn chat_first_turn_unbound_to_bound_poll_confirms_and_clears_draft_without_review_warning(
    cx: &mut TestAppContext,
) {
    use krill_proto::input::Delivery;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (view, cx, _, sent) = fixture(cx, "chat-first-turn-bind", &permission(), "ready");
    let polls = Arc::new(AtomicUsize::new(0));
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
            shell.paths = Some(krill_proto::daemon::Paths {
                socket: "/unused".into(),
                holder: "/unused".into(),
                krilld: "/unused".into(),
            });
            // Start from a fresh unbound session (before Turn 1 creates a transcript).
            shell.chat.logs.remove(&local_key(SESSION));
            shell.apply_chat_poll(
                SESSION,
                krill_proto::agent_history::ChatPollSnapshot {
                    agent: "agy".into(),
                    bound: false,
                    ..Default::default()
                },
            );
            let recorded_polls = polls.clone();
            shell.chat.poll_call = Some(Arc::new(move |_| {
                let n = recorded_polls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Ok(Reply::AgentHistory(Response::ChatPolled(
                        krill_proto::agent_history::ChatPollSnapshot {
                            agent: "agy".into(),
                            bound: false,
                            ..Default::default()
                        },
                    )))
                } else {
                    Ok(Reply::AgentHistory(Response::ChatPolled(
                        krill_proto::agent_history::ChatPollSnapshot {
                            agent: "agy".into(),
                            session_id: "d9f46ca3-075d-4a88-8788-954ef91d444b".into(),
                            transcript_path:
                                "/fake/brain/d9f46ca3/.system_generated/logs/transcript_full.jsonl"
                                    .into(),
                            bound: true,
                            next_offset: 512,
                            items: vec![krill_proto::agent_history::ChatItem {
                                kind: krill_proto::agent_history::ChatItemKind::User,
                                text: "/tmp/image.png\n\n테마 시안좀 5개 더 뽑아줘 다크테마.\n\n대충하지말고 유명 디자인 테마들 참조해서 완성도 있게 해줘".into(),
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                    )))
                }
            }));
            shell.chat.prompt_delivery = Some(Delivery::Applied);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| {
                state.set_value(
                    "/tmp/image.png\n\n테마 시안좀 5개 더 뽑아줘 다크테마.\n\n대충하지말고 유명 디자인 테마들 참조해서 완성도 있게 해줘",
                    window,
                    cx,
                );
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            // An unbound poll lands while PASTE_SUBMIT_DELAY is still waiting.
            shell.apply_chat_poll(
                SESSION,
                krill_proto::agent_history::ChatPollSnapshot {
                    agent: "agy".into(),
                    bound: false,
                    ..Default::default()
                },
            );
            input
        })
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(260));
    cx.run_until_parked();
    for _ in 0..10 {
        cx.executor().advance_clock(Duration::from_millis(25));
        cx.run_until_parked();
    }
    assert_eq!(sent.lock().unwrap().len(), 2);
    assert!(
        input.read_with(cx, |state, _| state.value()).is_empty(),
        "draft must be cleared once Turn 1 binds and confirms the user prompt"
    );
    view.read_with(cx, |shell, _| {
        assert!(
            !shell.chat.prompt_review.contains(&local_key(SESSION)),
            "initial transcript binding must not trigger PROMPT_REVIEW"
        );
    });
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

#[gpui::test]
fn tui_opening_slash_command_defers_terminal_switch_until_delivery_applied(
    cx: &mut TestAppContext,
) {
    use krill_proto::input::Delivery;

    let (view, cx, _, _) = fixture(cx, "chat-tui-slash-bracketed", &permission(), "ready");
    let pane = view.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
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
            input.update(cx, |state, cx| state.set_value("/config", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });

    // Bracketed delivery is still Pending after 2ms -> stay in Chat View!
    cx.executor().advance_clock(Duration::from_millis(2));
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |shell, app_cx| shell.is_pane_chat_view(pane, app_cx)),
        "must not flip to Terminal View while slash prompt delivery is Pending"
    );

    // Next receipt tick observes Delivery::Applied -> switches to Terminal View once delay + poll settle.
    view.update(cx, |shell, _| {
        shell.chat.prompt_delivery = Some(Delivery::Applied);
    });
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    assert!(
        !view.read_with(cx, |shell, app_cx| shell.is_pane_chat_view(pane, app_cx)),
        "must switch to Terminal View once TUI-opening slash command is Applied"
    );

    // Uncertain delivery must NOT switch to Terminal View.
    let (view2, cx2, _, _) = fixture(cx, "chat-tui-slash-uncertain", &permission(), "ready");
    let pane2 = view2.read_with(cx2, |shell, _| shell.layout.focused().unwrap());
    cx2.update(|window, cx| {
        view2.update(cx, |shell, cx| {
            shell.chat.prompt_delivery = Some(Delivery::Uncertain);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value("/resume", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx2.executor().advance_clock(Duration::from_millis(2));
    cx2.run_until_parked();
    assert!(
        view2.read_with(cx2, |shell, app_cx| shell.is_pane_chat_view(pane2, app_cx)),
        "Uncertain slash delivery must keep Chat View open"
    );

    // Unbracketed Pending -> Applied transitions to Terminal View once Applied.
    let (view3, cx3, _, _) = fixture(cx, "chat-tui-slash-unbracketed", &permission(), "ready");
    let pane3 = view3.read_with(cx3, |shell, _| shell.layout.focused().unwrap());
    cx3.update(|window, cx| {
        view3.update(cx, |shell, cx| {
            shell.chat.prompt_delivery = Some(Delivery::Pending);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value("/config", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx3.executor().advance_clock(Duration::from_millis(2));
    cx3.run_until_parked();
    assert!(
        view3.read_with(cx3, |shell, app_cx| shell.is_pane_chat_view(pane3, app_cx)),
        "unbracketed Pending delivery must stay in Chat View"
    );
    view3.update(cx3, |shell, _| {
        shell.chat.prompt_delivery = Some(Delivery::Applied);
    });
    cx3.executor().advance_clock(Duration::from_millis(30));
    cx3.run_until_parked();
    assert!(
        !view3.read_with(cx3, |shell, app_cx| shell.is_pane_chat_view(pane3, app_cx)),
        "unbracketed slash command switches to Terminal View after Applied settles"
    );
}

#[gpui::test]
fn non_echoed_local_slash_command_confirms_without_prompt_review_and_attachments_clear_review(
    cx: &mut gpui::TestAppContext,
) {
    use krill_proto::input::Delivery;

    let (view, cx, _, sent) = fixture(cx, "chat-local-slash", &permission(), "ready");
    let pane = view.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
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
            shell.chat.poll_call = Some(Arc::new(|_| {
                Ok(krill_proto::control::Reply::AgentHistory(
                    krill_proto::agent_history::Response::ChatPolled(
                        krill_proto::agent_history::ChatPollSnapshot {
                            agent: "claude".into(),
                            bound: true,
                            ..Default::default()
                        },
                    ),
                ))
            }));
            shell.chat.prompt_delivery = Some(Delivery::Applied);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value("/compact", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx.executor()
        .advance_clock(krill_proto::input::PASTE_SUBMIT_DELAY + Duration::from_millis(60));
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().len(), 2);
    view.read_with(cx, |shell, app_cx| {
        assert!(
            shell.is_pane_chat_view(pane, app_cx),
            "local non-echoed slash stays in Chat view"
        );
        assert!(
            !shell.chat.prompt_review.contains(&local_key(SESSION)),
            "local non-echoed slash must confirm without PROMPT_REVIEW"
        );
        assert!(
            !shell.chat.pending_prompts.contains_key(&local_key(SESSION)),
            "local non-echoed slash pending prompt must clear after Enter ACK"
        );
    });

    // Staging an attachment clears a prior prompt_review lock.
    let dir = std::env::temp_dir().join(format!("krill-chat-att-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sample = dir.join("note.txt");
    std::fs::write(&sample, b"hello").unwrap();
    let att = crate::shell::chat::DraftAttachment::from_path(sample).unwrap();
    view.update(cx, |shell, cx| {
        shell.chat.prompt_review.insert(local_key(SESSION));
        shell.stage_chat_attachments(&local_key(SESSION), vec![att], cx);
        assert!(!shell.chat.prompt_review.contains(&local_key(SESSION)));
    });
    std::fs::remove_dir_all(&dir).unwrap();
}

fn push_agy_screen(
    shell: &mut Shell,
    pane: super::super::PaneId,
    rev: u64,
    lines: &[&str],
    cx: &mut Context<Shell>,
) {
    use krill_proto::screen::{Cell, Flags, Op, Revision, Seq, encode_update};
    let mut ops = vec![Op::Reset {
        cols: 120,
        lines: u16::try_from(lines.len().max(6)).unwrap(),
    }];
    for (line, text) in lines.iter().enumerate() {
        let mut cells = Vec::new();
        for ch in text.chars() {
            let wide = !ch.is_ascii() && ch != '─' && ch != '▸' && ch != '│' && ch != '…';
            let mut flags = Flags::default();
            if wide {
                flags |= Flags::WIDE;
            }
            cells.push(Cell {
                ch,
                flags,
                ..Cell::default()
            });
            if wide {
                cells.push(Cell {
                    ch: ' ',
                    flags: Flags::SPACER,
                    ..Cell::default()
                });
            }
        }
        ops.push(Op::Line {
            line: u16::try_from(line).unwrap(),
            cells,
        });
    }
    for frame in encode_update(Revision(rev), Seq(rev), ops).unwrap() {
        let (kind, body) = krill_proto::ipc::read_frame(&mut frame.as_slice())
            .unwrap()
            .unwrap();
        shell.on_frame(pane, kind, &body, cx);
    }
}

#[gpui::test]
fn agy_mid_turn_steer_confirms_from_queued_screen_sends_alt_m_and_avoids_prompt_review(
    cx: &mut TestAppContext,
) {
    use krill_proto::input::Delivery;
    const BORDER: &str = "────────────────────────────────────────────────────────────────";
    const PROMPT: &str = "다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라.";

    let (view, cx, _, sent) = fixture(cx, "chat-agy-midturn-steer", &permission(), "ready");
    let pane = view.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
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
            push_agy_screen(
                shell,
                pane,
                2,
                &[
                    "⣽  Running command...",
                    BORDER,
                    "> ",
                    BORDER,
                    "Gemini 4 Neon (XHigh) (1.0M) │ 144.2k/1.0M",
                ],
                cx,
            );
            shell.apply_chat_poll(
                SESSION,
                krill_proto::agent_history::ChatPollSnapshot {
                    agent: "agy".into(),
                    session_id: "agy-conv-1".into(),
                    transcript_path: "/fake/brain/agy-conv-1/.system_generated/logs/transcript_full.jsonl".into(),
                    bound: true,
                    ..Default::default()
                },
            );
            // Simulate a long-running mid-turn tool: transcript poll returns no new user row yet.
            shell.chat.poll_call = Some(Arc::new(|_| {
                Ok(krill_proto::control::Reply::AgentHistory(
                    krill_proto::agent_history::Response::ChatPolled(
                        krill_proto::agent_history::ChatPollSnapshot {
                            agent: "agy".into(),
                            session_id: "agy-conv-1".into(),
                            transcript_path: "/fake/brain/agy-conv-1/.system_generated/logs/transcript_full.jsonl".into(),
                            bound: true,
                            ..Default::default()
                        },
                    ),
                ))
            }));
            shell.chat.prompt_delivery = Some(Delivery::Applied);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value(PROMPT, window, cx));
            shell.steer_chat_prompt(&local_key(SESSION), window, cx);
            input
        })
    });
    cx.run_until_parked();
    cx.executor()
        .advance_clock(krill_proto::input::PASTE_SUBMIT_DELAY + Duration::from_millis(20));
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![
            format!("\x1b[200~{PROMPT}\x1b[201~").into_bytes(),
            b"\r".to_vec()
        ]
    );

    // Antigravity clears its composer and displays the queued prompt above the border.
    cx.update(|_, cx| {
        view.update(cx, |shell, cx| {
            push_agy_screen(
                shell,
                pane,
                3,
                &[
                    "⢿  Running command...",
                    "▸ 다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라. (alt+m to send immediately)",
                    BORDER,
                    "> ",
                    BORDER,
                    "Gemini 4 Neon (XHigh) (1.0M) │ 144.2k/1.0M",
                ],
                cx,
            );
        });
    });
    cx.executor().advance_clock(Duration::from_millis(40));
    cx.run_until_parked();

    assert_eq!(
        *sent.lock().unwrap(),
        vec![
            format!("\x1b[200~{PROMPT}\x1b[201~").into_bytes(),
            b"\r".to_vec(),
            b"\x1bm".to_vec(),
        ],
        "steer on agy must promote queued prompt via alt+m"
    );
    assert!(
        input.read_with(cx, |state, _| state.value()).is_empty(),
        "draft must stay cleared once agy queues the prompt"
    );
    view.read_with(cx, |shell, _| {
        assert!(
            !shell.chat.prompt_review.contains(&local_key(SESSION)),
            "queued mid-turn agy prompt must not trigger PROMPT_REVIEW"
        );
        assert!(
            !shell.chat.prompt_tasks.contains_key(&local_key(SESSION)),
            "composer must unlock immediately once queued"
        );
        let pending = &shell.chat.pending_prompts[&local_key(SESSION)];
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, super::PendingStatus::Queued);
    });

    // Even after exceeding the 15s turn poll timeout, no false PROMPT_REVIEW fires.
    cx.executor().advance_clock(Duration::from_secs(20));
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        assert!(!shell.chat.prompt_review.contains(&local_key(SESSION)));
    });

    // Later, when agy dequeues the message and writes USER_EXPLICIT to transcript_full.jsonl,
    // the Queued optimistic bubble reconciles and clears automatically.
    view.update(cx, |shell, _| {
        shell.apply_chat_poll(
            SESSION,
            krill_proto::agent_history::ChatPollSnapshot {
                agent: "agy".into(),
                session_id: "agy-conv-1".into(),
                transcript_path:
                    "/fake/brain/agy-conv-1/.system_generated/logs/transcript_full.jsonl".into(),
                bound: true,
                items: vec![krill_proto::agent_history::ChatItem {
                    kind: krill_proto::agent_history::ChatItemKind::User,
                    text: PROMPT.into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
    });
    view.read_with(cx, |shell, _| {
        assert!(
            shell
                .chat
                .pending_prompts
                .get(&local_key(SESSION))
                .is_none_or(Vec::is_empty),
            "queued pending card must reconcile when transcript records the user turn"
        );
    });
}

#[gpui::test]
fn agy_mid_turn_unsubmitted_draft_in_composer_still_triggers_prompt_review(
    cx: &mut TestAppContext,
) {
    use krill_proto::input::Delivery;
    const BORDER: &str = "────────────────────────────────────────────────────────────────";
    const PROMPT: &str = "다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라.";

    let (view, cx, _, sent) = fixture(cx, "chat-agy-unsubmitted-review", &permission(), "ready");
    let pane = view.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
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
            push_agy_screen(
                shell,
                pane,
                2,
                &[
                    "⣽  Running command...",
                    BORDER,
                    "> 다 만들고 크릴 아티팩트로 한 눈에 보이게 만들어봐라.",
                    BORDER,
                ],
                cx,
            );
            shell.apply_chat_poll(
                SESSION,
                krill_proto::agent_history::ChatPollSnapshot {
                    agent: "agy".into(),
                    session_id: "agy-conv-2".into(),
                    transcript_path: "/fake/brain/agy-conv-2/.system_generated/logs/transcript_full.jsonl".into(),
                    bound: true,
                    ..Default::default()
                },
            );
            shell.chat.poll_call = Some(Arc::new(|_| {
                Ok(krill_proto::control::Reply::AgentHistory(
                    krill_proto::agent_history::Response::ChatPolled(
                        krill_proto::agent_history::ChatPollSnapshot {
                            agent: "agy".into(),
                            session_id: "agy-conv-2".into(),
                            transcript_path: "/fake/brain/agy-conv-2/.system_generated/logs/transcript_full.jsonl".into(),
                            bound: true,
                            ..Default::default()
                        },
                    ),
                ))
            }));
            shell.chat.prompt_delivery = Some(Delivery::Applied);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value(PROMPT, window, cx));
            shell.steer_chat_prompt(&local_key(SESSION), window, cx);
            input
        })
    });
    cx.run_until_parked();
    cx.executor()
        .advance_clock(krill_proto::input::PASTE_SUBMIT_DELAY + Duration::from_secs(16));
    cx.run_until_parked();

    assert_eq!(
        sent.lock().unwrap().len(),
        2,
        "alt+m must never be sent when the draft remains unsubmitted in composer"
    );
    assert_eq!(input.read_with(cx, |state, _| state.value()), PROMPT);
    view.read_with(cx, |shell, _| {
        assert!(
            shell.chat.prompt_review.contains(&local_key(SESSION)),
            "unsubmitted draft stuck in composer must still require PROMPT_REVIEW"
        );
    });
}

#[gpui::test]
fn agy_normal_submit_while_busy_confirms_queued_without_sending_alt_m(cx: &mut TestAppContext) {
    use krill_proto::input::Delivery;
    const BORDER: &str = "────────────────────────────────────────────────────────────────";
    const PROMPT: &str = "후속 작업 부탁해";

    let (view, cx, _, sent) = fixture(cx, "chat-agy-normal-queued", &permission(), "ready");
    let pane = view.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
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
            push_agy_screen(shell, pane, 2, &[BORDER, "> ", BORDER], cx);
            shell.apply_chat_poll(
                SESSION,
                krill_proto::agent_history::ChatPollSnapshot {
                    agent: "agy".into(),
                    session_id: "agy-conv-3".into(),
                    transcript_path: "/fake/brain/agy-conv-3/.system_generated/logs/transcript_full.jsonl".into(),
                    bound: true,
                    ..Default::default()
                },
            );
            shell.chat.poll_call = Some(Arc::new(|_| {
                Ok(krill_proto::control::Reply::AgentHistory(
                    krill_proto::agent_history::Response::ChatPolled(
                        krill_proto::agent_history::ChatPollSnapshot {
                            agent: "agy".into(),
                            session_id: "agy-conv-3".into(),
                            transcript_path: "/fake/brain/agy-conv-3/.system_generated/logs/transcript_full.jsonl".into(),
                            bound: true,
                            ..Default::default()
                        },
                    ),
                ))
            }));
            shell.chat.prompt_delivery = Some(Delivery::Applied);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value(PROMPT, window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();
    cx.executor()
        .advance_clock(krill_proto::input::PASTE_SUBMIT_DELAY + Duration::from_millis(20));
    cx.run_until_parked();
    cx.update(|_, cx| {
        view.update(cx, |shell, cx| {
            push_agy_screen(
                shell,
                pane,
                3,
                &[
                    "▸ 후속 작업 부탁해 (alt+m to send immediately)",
                    BORDER,
                    "> ",
                    BORDER,
                ],
                cx,
            );
        });
    });
    cx.executor().advance_clock(Duration::from_millis(40));
    cx.run_until_parked();

    assert_eq!(
        *sent.lock().unwrap(),
        vec![
            format!("\x1b[200~{PROMPT}\x1b[201~").into_bytes(),
            b"\r".to_vec(),
        ],
        "non-steer submit must not send alt+m"
    );
    view.read_with(cx, |shell, _| {
        assert!(!shell.chat.prompt_review.contains(&local_key(SESSION)));
        assert_eq!(
            shell.chat.pending_prompts[&local_key(SESSION)][0].status,
            super::PendingStatus::Queued
        );
    });
}

#[gpui::test]
fn working_state_transition_confirms_prompt_without_transcript_echo(cx: &mut TestAppContext) {
    use krill_proto::control::{AgentState, AgentStatus};
    use krill_proto::input::Delivery;

    struct StateSource(krill_app::sidebar::Snapshot);
    impl krill_app::sidebar::Source for StateSource {
        fn fetch(&self) -> std::io::Result<krill_app::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }

    let snap = |state: AgentState, since_unix_ns: u64| krill_app::sidebar::Snapshot {
        sessions: vec![krill_app::sidebar::Session {
            id: SESSION,
            status: AgentStatus {
                state,
                since_unix_ns,
                agent: "agy".into(),
                ..AgentStatus::UNKNOWN
            },
            worktree: None,
            cwd: None,
            account: None,
        }],
        trees: BTreeMap::new(),
    };

    let (view, cx, _, sent) = fixture(cx, "chat-working-confirm", &permission(), "ready");
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
            shell.sidebar.update(cx, |sb, cx| {
                sb.connect_source(Arc::new(StateSource(snap(AgentState::Done, 100))), cx);
            });
            // Poll returns no user echo at all.
            shell.chat.poll_call = Some(Arc::new(|_| {
                Ok(Reply::AgentHistory(Response::ChatPolled(
                    krill_proto::agent_history::ChatPollSnapshot {
                        agent: "agy".into(),
                        bound: true,
                        ..Default::default()
                    },
                )))
            }));
            shell.chat.prompt_delivery = Some(Delivery::Applied);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| {
                state.set_value("turn started without echo", window, cx);
            });
            input
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx.executor()
        .advance_clock(krill_proto::input::PASTE_SUBMIT_DELAY + Duration::from_millis(40));
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().len(), 2);

    // Transition session state to Working; prompt must confirm without transcript echo.
    view.update(cx, |shell, cx| {
        shell.sidebar.update(cx, |sb, cx| {
            sb.connect_source(Arc::new(StateSource(snap(AgentState::Working, 200))), cx);
        });
    });
    cx.executor().advance_clock(Duration::from_millis(40));
    cx.run_until_parked();
    assert!(input.read_with(cx, |state, _| state.value().is_empty()));
    view.read_with(cx, |shell, _| {
        assert!(!shell.chat.prompt_review.contains(&local_key(SESSION)));
        assert!(
            shell
                .chat
                .pending_prompts
                .get(&local_key(SESSION))
                .is_none_or(Vec::is_empty)
        );
    });
}

#[gpui::test]
fn switching_to_terminal_or_tab_after_delivery_does_not_trigger_prompt_review(
    cx: &mut TestAppContext,
) {
    use krill_proto::input::Delivery;

    let (view, cx, _, _) = fixture(cx, "chat-view-switch-no-review", &permission(), "ready");
    let pane = view.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
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
            shell.chat.poll_call = Some(Arc::new(|_| {
                Ok(Reply::AgentHistory(Response::ChatPolled(
                    krill_proto::agent_history::ChatPollSnapshot {
                        agent: "agy".into(),
                        bound: true,
                        ..Default::default()
                    },
                )))
            }));
            shell.chat.prompt_delivery = Some(Delivery::Applied);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| {
                state.set_value("sent before switching to term", window, cx);
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            input
        })
    });
    cx.executor()
        .advance_clock(krill_proto::input::PASTE_SUBMIT_DELAY + Duration::from_millis(40));
    cx.run_until_parked();
    // User switches to Term while provider has not echoed yet.
    view.update(cx, |shell, cx| {
        shell.set_pane_chat_view(pane, false, cx);
    });
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    assert!(
        input.read_with(cx, |state, _| state.value().is_empty()),
        "switching to Term must not restore already-sent draft"
    );
    view.read_with(cx, |shell, _| {
        assert!(
            !shell.chat.prompt_review.contains(&local_key(SESSION)),
            "switching to Term must not trigger PROMPT_REVIEW"
        );
        assert!(
            !shell.chat.prompt_tasks.contains_key(&local_key(SESSION)),
            "switching to Term must not leave prompt send locked"
        );
    });
}

#[gpui::test]
fn flush_queued_prompt_preserves_failures_and_handles_bracketed_and_slash(cx: &mut TestAppContext) {
    use krill_proto::control::{AgentState, AgentStatus};
    use krill_proto::input::Delivery;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StaticState(AgentState);
    impl krill_app::sidebar::Source for StaticState {
        fn fetch(&self) -> std::io::Result<krill_app::sidebar::Snapshot> {
            Ok(krill_app::sidebar::Snapshot {
                sessions: vec![krill_app::sidebar::Session {
                    id: SESSION,
                    status: AgentStatus {
                        state: self.0,
                        agent: "claude".into(),
                        ..AgentStatus::UNKNOWN
                    },
                    worktree: None,
                    cwd: None,
                    account: None,
                }],
                trees: std::collections::BTreeMap::new(),
            })
        }
    }
    let set_state =
        |view: &gpui::Entity<Shell>, vcx: &mut gpui::VisualTestContext, state: AgentState| {
            view.update(vcx, |shell, cx| {
                shell.sidebar.update(cx, |sidebar, cx| {
                    sidebar.connect_source(Arc::new(StaticState(state)), cx);
                });
            });
            vcx.run_until_parked();
        };

    // 1. Immediate send failure preserves draft and marks prompt_failed instead of dropping.
    let (view, cx, _, _) = fixture(cx, "chat-queue-fail-first", &permission(), "ready");
    set_state(&view, cx, AgentState::Done);
    let input = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            shell
                .chat
                .logs
                .entry(local_key(SESSION))
                .or_default()
                .queued_prompts
                .push_back(QueuedChatPrompt {
                    id: 77,
                    display_text: "queued failure draft".into(),
                    submitted_payload: "queued failure draft".into(),
                    attachments: Vec::new(),
                });
            shell.chat.send_input = Some(Box::new(|_| false));
            shell.flush_queued_chat_prompt_if_ready(&local_key(SESSION), cx);
            input
        })
    });
    cx.run_until_parked();
    view.read_with(cx, |shell, cx| {
        assert!(shell.chat.prompt_failed.contains(&local_key(SESSION)));
        let preserved_in_pending = shell
            .chat
            .pending_prompts
            .get(&local_key(SESSION))
            .is_some_and(|list| {
                list.iter()
                    .any(|p| p.id == 77 && p.status == PendingStatus::ReviewRequired)
            });
        let preserved_in_input = input.read(cx).value().as_ref() == "queued failure draft";
        assert!(
            preserved_in_pending || preserved_in_input,
            "failed queued send must preserve prompt for review instead of dropping it"
        );
    });

    // 2. Bracketed `\r` failure transitions out of PendingStatus::Sending (never wedges).
    let (view2, cx2, _, _) = fixture(cx, "chat-queue-fail-enter", &permission(), "ready");
    set_state(&view2, cx2, AgentState::Done);
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&calls);
    view2.update(cx2, |shell, cx| {
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
        shell.chat.prompt_delivery = Some(Delivery::Applied);
        shell.chat.send_input = Some(Box::new(move |_| {
            calls_ref.fetch_add(1, Ordering::SeqCst) == 0
        }));
        shell
            .chat
            .logs
            .entry(local_key(SESSION))
            .or_default()
            .queued_prompts
            .push_back(QueuedChatPrompt {
                id: 88,
                display_text: "queued\nmultiline".into(),
                submitted_payload: "queued\nmultiline".into(),
                attachments: Vec::new(),
            });
        shell.flush_queued_chat_prompt_if_ready(&local_key(SESSION), cx);
    });
    cx2.executor()
        .advance_clock(krill_proto::input::PASTE_SUBMIT_DELAY + Duration::from_millis(60));
    cx2.run_until_parked();
    view2.read_with(cx2, |shell, _| {
        assert!(
            !shell.chat.prompt_tasks.contains_key(&local_key(SESSION)),
            "bracketed enter failure must clear prompt_tasks"
        );
        let stuck_sending = shell
            .chat
            .pending_prompts
            .get(&local_key(SESSION))
            .is_some_and(|list| list.iter().any(|p| p.status == PendingStatus::Sending));
        assert!(
            !stuck_sending,
            "bracketed enter failure must not wedge queued prompt in Sending"
        );
    });

    // 3. Queued TUI-opening slash command switches pane to terminal.
    let (view3, cx3, _, _) = fixture(cx, "chat-queue-slash-tui", &permission(), "ready");
    set_state(&view3, cx3, AgentState::Done);
    let pane3 = view3.read_with(cx3, |shell, _| shell.layout.focused().unwrap());
    view3.update(cx3, |shell, cx| {
        shell.chat.prompt_delivery = Some(Delivery::Applied);
        shell
            .chat
            .logs
            .entry(local_key(SESSION))
            .or_default()
            .queued_prompts
            .push_back(QueuedChatPrompt {
                id: 99,
                display_text: "/config".into(),
                submitted_payload: "/config".into(),
                attachments: Vec::new(),
            });
        shell.flush_queued_chat_prompt_if_ready(&local_key(SESSION), cx);
    });
    cx3.run_until_parked();
    assert!(
        !view3.read_with(cx3, |shell, app_cx| shell.is_pane_chat_view(pane3, app_cx)),
        "queued TUI-opening slash command must switch pane to terminal"
    );
}

#[gpui::test]
fn unbracketed_turn_watcher_stops_when_transport_becomes_stale(cx: &mut TestAppContext) {
    let (view, cx, _, _) = fixture(
        cx,
        "chat-unbracketed-stale-transport",
        &permission(),
        "ready",
    );
    let pane = view.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.chat.prompt_confirmed = Some(false);
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| {
                state.set_value("hello unbracketed", window, cx);
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        assert!(!shell.chat.turn_tasks.is_empty());
        assert_eq!(
            shell.chat.pending_prompts[&local_key(SESSION)][0].status,
            PendingStatus::Sending
        );
    });
    // Keep a draft in the composer so failure keeps the pending card in ReviewRequired
    // instead of restoring into an empty composer, then drop the pane's live transport.
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| {
                state.set_value("next draft", window, cx);
            });
            shell.live.remove(&pane);
        });
    });
    cx.executor().advance_clock(Duration::from_millis(40));
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        assert!(
            shell.chat.turn_tasks.is_empty(),
            "stale transport must stop unbracketed turn watcher promptly"
        );
        assert_eq!(
            shell.chat.pending_prompts[&local_key(SESSION)][0].status,
            PendingStatus::ReviewRequired,
            "stale transport must transition pending prompt to ReviewRequired"
        );
    });
}
