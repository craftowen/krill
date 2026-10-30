//! Regression tests use fake input delivery and in-memory terminal frames only.
#![cfg(unix)]
use super::*;
use crate::shell::tests::{open, state_path};
use gpui::{Entity, TestAppContext, VisualTestContext};
use krill_proto::agent_history::ChatQuestionOption;
use krill_proto::screen::{Op, Revision, Seq, encode_update};
use std::sync::Mutex;

const SESSION: SessionId = SessionId(991);
type Sent = Arc<Mutex<Vec<Vec<u8>>>>;

fn question(multi: bool) -> ChatItem {
    ChatItem {
        kind: ChatItemKind::Question,
        tool_id: "question-1".into(),
        title: "Mode".into(),
        text: "Choose one".into(),
        options: ["Fast", "Safe"]
            .into_iter()
            .map(|label| ChatQuestionOption {
                label: label.into(),
                description: String::new(),
            })
            .collect(),
        multi_select: multi,
        ..Default::default()
    }
}

pub(super) fn permission() -> ChatItem {
    ChatItem {
        kind: ChatItemKind::Permission,
        tool_id: "permission-1".into(),
        text: "deploy production".into(),
        ..Default::default()
    }
}

fn snapshot(items: Vec<ChatItem>) -> ChatPollSnapshot {
    ChatPollSnapshot {
        bound: true,
        session_id: "agent-991".into(),
        transcript_path: "/fake/chat.jsonl".into(),
        items,
        ..Default::default()
    }
}

fn screen(shell: &mut Shell, pane: super::super::PaneId, text: &str, cx: &mut Context<Shell>) {
    let mut ops = vec![Op::Reset {
        cols: 100,
        lines: 10,
    }];
    for (line, text) in text.lines().enumerate() {
        ops.push(Op::Line {
            line: u16::try_from(line).unwrap(),
            cells: text
                .chars()
                .map(|ch| Cell {
                    ch,
                    ..Default::default()
                })
                .collect(),
        });
    }
    for frame in encode_update(Revision(1), Seq(0), ops).unwrap() {
        let (kind, body) = krill_proto::ipc::read_frame(&mut frame.as_slice())
            .unwrap()
            .unwrap();
        shell.on_frame(pane, kind, &body, cx);
    }
}

pub(super) fn fixture<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    item: &ChatItem,
    text: &str,
) -> (
    Entity<Shell>,
    &'a mut VisualTestContext,
    actions::RequestIdentity,
    Sent,
) {
    struct Memory(krill_app::sidebar::Snapshot);
    impl krill_app::sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<krill_app::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }
    let (view, cx) = open(cx, state_path(name));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let captured = sent.clone();
    let request = view.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        // Keep the peer open, without a daemon or IO receiver task.
        shell.attach_test(pane, SESSION, ours, cx);
        shell.live.get_mut(&pane).unwrap().sock = Some(theirs);
        shell.live.get_mut(&pane).unwrap().target = Some(krill_app::hosts::HostTarget::local(
            krill_proto::daemon::Paths {
                socket: "/fake/socket".into(),
                holder: "/fake/holder".into(),
                krilld: "/fake/krilld".into(),
            },
        ));
        shell.paths = None;
        shell.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(Memory(krill_app::sidebar::Snapshot {
                    sessions: vec![krill_app::sidebar::Session {
                        id: SESSION,
                        status: krill_proto::control::AgentStatus {
                            state: krill_proto::control::AgentState::Waiting,
                            agent: "claude".into(),
                            ..krill_proto::control::AgentStatus::UNKNOWN
                        },
                        worktree: None,
                        cwd: None,
                        account: None,
                    }],
                    trees: BTreeMap::new(),
                })),
                cx,
            );
        });

        shell.layout.set_session(pane, SESSION);
        shell.layout.set_chat_view(pane, true);
        shell.chat.send_input = Some(Box::new(move |bytes| {
            captured.lock().unwrap().push(bytes);
            true
        }));
        shell.apply_chat_poll(SESSION, snapshot(vec![item.clone()]));
        screen(shell, pane, text, cx);
        shell.chat_request_identity(pane, SESSION, 0, item)
    });
    cx.run_until_parked();
    (view, cx, request, sent)
}

#[gpui::test]
fn chat_action_uses_its_own_pane_view_when_another_pane_shares_session(cx: &mut TestAppContext) {
    let (view, cx, request, _) = fixture(
        cx,
        "chat-pane-view-binding",
        &permission(),
        "deploy production (y/n)",
    );
    view.update(cx, |shell, cx| {
        let origin = shell.layout.focused().unwrap();
        shell.layout.split(krill_app::layout::Axis::Horizontal);
        let other = shell.layout.focused().unwrap();
        shell.layout.set_session(other, SESSION);
        shell.layout.set_chat_view(other, true);
        shell.layout.set_chat_view(origin, false);
        assert!(
            !shell.chat_request_is_current(&request, cx),
            "a focused Chat pane enabled an action on a Terminal pane"
        );
        shell.layout.set_chat_view(other, false);
        shell.layout.set_chat_view(origin, true);
        assert!(
            shell.chat_request_is_current(&request, cx),
            "a focused Terminal pane disabled another pane's Chat action"
        );
    });
}

#[gpui::test]
fn chat_prompt_failure_preserves_draft_and_success_clears_only_sent_value(cx: &mut TestAppContext) {
    let (view, cx, _, sent) = fixture(cx, "chat-draft", &permission(), "deploy production (y/n)");
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value("draft", window, cx));
            shell.chat.send_input = Some(Box::new(|_| false));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            assert_eq!(input.read(cx).value().as_ref(), "draft");
            assert!(shell.chat.prompt_failed.contains(&local_key(SESSION)));
            input.update(cx, |state, cx| state.set_value("new draft", window, cx));
            Shell::clear_sent_chat_draft(&input, "draft", window, cx);
            assert_eq!(input.read(cx).value().as_ref(), "new draft");
            let recorded = sent.clone();
            shell.chat.send_input = Some(Box::new(move |bytes| {
                recorded.lock().unwrap().push(bytes);
                true
            }));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            assert!(input.read(cx).value().is_empty());
            assert!(!shell.chat.prompt_failed.contains(&local_key(SESSION)));
            // Disconnected Pane::input returns None too; it must not clear a draft.
            let pane = shell.layout.focused().unwrap();
            shell.live.get_mut(&pane).unwrap().pane.unlink();
            input.update(cx, |state, cx| state.set_value("offline draft", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            assert_eq!(input.read(cx).value().as_ref(), "offline draft");
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-prompt-error").is_some());
    assert_eq!(*sent.lock().unwrap(), vec![b"new draft\r".to_vec()]);
}

#[gpui::test]
fn chat_permission_duplicate_clicks_wait_for_actual_result(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-approval",
        &permission(),
        "deploy production (y/n)",
    );
    view.update(cx, |shell, cx| {
        assert!(shell.chat_request_is_current(&request, cx));
        shell.approve_chat_permission(request.clone(), true, cx);
        shell.approve_chat_permission(request.clone(), false, cx);
        assert!(shell.chat.logs[&local_key(SESSION)].answered.is_empty());
    });
    cx.run_until_parked();
    assert_eq!(*sent.lock().unwrap(), vec![b"y\r".to_vec()]);
    assert!(cx.debug_bounds("chat-request-status").is_some());
    view.update(cx, |shell, cx| {
        assert!(matches!(
            shell.chat.logs[&local_key(SESSION)].submissions[&request.key],
            Submission::Awaiting
        ));
        shell.approve_chat_permission(request.clone(), true, cx);
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![ChatItem {
                kind: ChatItemKind::ToolResult,
                tool_id: "permission-1".into(),
                ..Default::default()
            }]),
        );
        assert!(
            shell.chat.logs[&local_key(SESSION)]
                .answered
                .contains(&request.key)
        );
        assert!(matches!(
            shell.chat.logs[&local_key(SESSION)].submissions[&request.key],
            Submission::Confirmed
        ));
    });
    cx.executor().advance_clock(Duration::from_secs(6));
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        assert!(matches!(
            shell.chat.logs[&local_key(SESSION)].submissions[&request.key],
            Submission::Confirmed
        ));
    });
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[gpui::test]
fn chat_question_failure_keeps_choices_and_can_retry_after_reconnect(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-question-retry",
        &question(true),
        "Choose one\n❯ [ ] Fast\n  [ ] Safe",
    );
    view.update(cx, |shell, cx| {
        shell.toggle_chat_multi_option(&request, 0, cx);
        shell.chat.send_input = Some(Box::new(|_| false));
        shell.answer_chat_question(request.clone(), vec![0], true, cx);
    });
    cx.run_until_parked();
    view.update(cx, |shell, cx| {
        let log = &shell.chat.logs[&local_key(SESSION)];
        assert!(log.answered.is_empty());
        assert_eq!(log.multi_selected[&request.key], BTreeSet::from([0]));
        assert!(matches!(log.submissions[&request.key], Submission::Failed));
        let recorded = sent.clone();
        shell.chat.send_input = Some(Box::new(move |bytes| {
            recorded.lock().unwrap().push(bytes);
            true
        }));
        shell.answer_chat_question(request.clone(), vec![0], true, cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(61));
    cx.run_until_parked();
    assert_eq!(*sent.lock().unwrap(), vec![b" ".to_vec(), b"\r".to_vec()]);
    view.read_with(cx, |shell, _| {
        assert!(shell.chat.logs[&local_key(SESSION)].answered.is_empty());
        assert!(matches!(
            shell.chat.logs[&local_key(SESSION)].submissions[&request.key],
            Submission::Awaiting
        ));
    });
}

#[gpui::test]
fn chat_partial_multiselect_stops_and_requires_terminal_review(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-partial",
        &question(true),
        "Choose one\n❯ [ ] Fast\n  [ ] Safe",
    );
    let recorded = sent.clone();
    view.update(cx, |shell, cx| {
        shell.toggle_chat_multi_option(&request, 1, cx);
        shell.chat.send_input = Some(Box::new(move |bytes| {
            let mut sent = recorded.lock().unwrap();
            sent.push(bytes);
            sent.len() == 1
        }));
        shell.answer_chat_question(request.clone(), vec![1], true, cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(61));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(6));
    cx.run_until_parked();
    view.update(cx, |shell, cx| {
        assert!(matches!(
            shell.chat.logs[&local_key(SESSION)].submissions[&request.key],
            Submission::ReviewRequired
        ));
        assert!(shell.chat.logs[&local_key(SESSION)].answered.is_empty());
        assert_eq!(
            shell.chat.logs[&local_key(SESSION)].multi_selected[&request.key],
            BTreeSet::from([1])
        );
        shell.answer_chat_question(request.clone(), vec![1], true, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![b"\x1b[B".to_vec(), b" ".to_vec()]
    );
}

#[gpui::test]
fn chat_replaced_request_stops_remaining_keys_and_accepts_new_request(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-request-replaced",
        &question(false),
        "Choose one\n❯ Fast\n  Safe",
    );
    view.update(cx, |shell, cx| {
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
    });
    cx.run_until_parked();
    view.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.apply_chat_poll(SESSION, snapshot(vec![permission()]));
        screen(shell, pane, "deploy production (y/n)", cx);
    });
    cx.executor().advance_clock(Duration::from_millis(61));
    cx.run_until_parked();
    assert_eq!(*sent.lock().unwrap(), vec![b"\x1b[B".to_vec()]);
    view.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        let new = shell.chat_request_identity(pane, SESSION, 1, &permission());
        assert!(shell.chat_request_is_current(&new, cx));
        shell.approve_chat_permission(new, true, cx);
        shell.approve_chat_permission(request.clone(), true, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![b"\x1b[B".to_vec(), b"y\r".to_vec()]
    );
}

#[gpui::test]
fn chat_stale_binding_host_session_and_live_screen_never_inject_keys(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-stale-identity",
        &permission(),
        "deploy production (y/n)",
    );
    view.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell
            .layout
            .set_host(pane, krill_app::hosts::HostId::Remote("other".into()));
        shell.approve_chat_permission(request.clone(), true, cx);
        shell.layout.set_host(pane, krill_app::hosts::HostId::Local);
        shell.layout.set_session(pane, SessionId(992));
        shell.approve_chat_permission(request.clone(), true, cx);
        shell.layout.set_session(pane, SESSION);
        shell.layout.set_chat_view(pane, true);
        screen(shell, pane, "shell$", cx);
        shell.approve_chat_permission(request.clone(), true, cx);
        screen(shell, pane, "deploy production (y/n)", cx);
        let mut reset = snapshot(vec![permission()]);
        reset.reset = true;
        shell.apply_chat_poll(SESSION, reset);
        shell.approve_chat_permission(request.clone(), true, cx);
        let fresh = shell.chat_request_identity(pane, SESSION, 0, &permission());
        assert!(shell.chat_request_is_current(&fresh, cx));
        let mut changed_binding = snapshot(vec![permission()]);
        changed_binding.session_id = "different-agent".into();
        shell.apply_chat_poll(SESSION, changed_binding);
        shell.approve_chat_permission(fresh, true, cx);
        let latest = shell.chat_request_identity(pane, SESSION, 0, &permission());
        shell.live.get_mut(&pane).unwrap().pane.unlink();
        shell.approve_chat_permission(latest, true, cx);
    });
    cx.run_until_parked();
    assert!(sent.lock().unwrap().is_empty());
}

#[gpui::test]
fn chat_unconfirmed_approval_leaves_waiting_state_and_new_binding_recovers(
    cx: &mut TestAppContext,
) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-confirm-timeout",
        &permission(),
        "deploy production (y/n)",
    );
    view.update(cx, |shell, cx| {
        shell.approve_chat_permission(request.clone(), true, cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(6));
    cx.run_until_parked();
    view.update(cx, |shell, cx| {
        assert!(matches!(
            shell.chat.logs[&local_key(SESSION)].submissions[&request.key],
            Submission::ReviewRequired
        ));
        assert!(shell.chat.logs[&local_key(SESSION)].answered.is_empty());
        shell.approve_chat_permission(request.clone(), true, cx);
        let mut reset = snapshot(vec![permission()]);
        reset.reset = true;
        shell.apply_chat_poll(SESSION, reset);
        let pane = shell.layout.focused().unwrap();
        let fresh = shell.chat_request_identity(pane, SESSION, 0, &permission());
        shell.approve_chat_permission(fresh, false, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![b"y\r".to_vec(), b"n\r".to_vec()]
    );
}

#[gpui::test]
fn chat_permission_delivery_failure_allows_explicit_retry(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-approval-failed",
        &permission(),
        "deploy production (y/n)",
    );
    view.update(cx, |shell, cx| {
        shell.chat.send_input = Some(Box::new(|_| false));
        shell.approve_chat_permission(request.clone(), true, cx);
    });
    cx.run_until_parked();
    view.update(cx, |shell, cx| {
        assert!(matches!(
            shell.chat.logs[&local_key(SESSION)].submissions[&request.key],
            Submission::Failed
        ));
        let recorded = sent.clone();
        shell.chat.send_input = Some(Box::new(move |bytes| {
            recorded.lock().unwrap().push(bytes);
            true
        }));
        shell.approve_chat_permission(request.clone(), false, cx);
        shell.approve_chat_permission(request.clone(), true, cx);
    });
    cx.run_until_parked();
    assert_eq!(*sent.lock().unwrap(), vec![b"n\r".to_vec()]);
}

#[gpui::test]
fn chat_host_session_changes_and_binding_reset_cancel_remaining_question_keys(
    cx: &mut TestAppContext,
) {
    let (view, cx, mut request, sent) = fixture(
        cx,
        "chat-mid-sequence-identity",
        &question(false),
        "Choose one\n❯ Fast\n  Safe",
    );
    view.update(cx, |shell, cx| {
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
    });
    cx.run_until_parked();
    view.update(cx, |shell, _| {
        let pane = shell.layout.focused().unwrap();
        shell
            .layout
            .set_host(pane, krill_app::hosts::HostId::Remote("changed".into()));
    });
    cx.executor().advance_clock(Duration::from_millis(61));
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().len(), 1);
    view.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_host(pane, krill_app::hosts::HostId::Local);
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
        assert!(matches!(
            shell.chat.logs[&local_key(SESSION)].submissions[&request.key],
            Submission::ReviewRequired
        ));
        let mut reset = snapshot(vec![question(false)]);
        reset.reset = true;
        shell.apply_chat_poll(SESSION, reset);
        request = shell.chat_request_identity(pane, SESSION, 0, &question(false));
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
    });
    cx.run_until_parked();
    view.update(cx, |shell, _| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SessionId(992));
    });
    cx.executor().advance_clock(Duration::from_millis(61));
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().len(), 2);
    view.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SESSION);
        shell.layout.set_chat_view(pane, true);
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
        assert!(matches!(
            shell.chat.logs[&local_key(SESSION)].submissions[&request.key],
            Submission::ReviewRequired
        ));
        let mut reset = snapshot(vec![question(false)]);
        reset.reset = true;
        shell.apply_chat_poll(SESSION, reset);
        request = shell.chat_request_identity(pane, SESSION, 0, &question(false));
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
    });
    cx.run_until_parked();
    view.update(cx, |shell, _| {
        let mut reset = snapshot(vec![question(false)]);
        reset.reset = true;
        shell.apply_chat_poll(SESSION, reset);
    });
    cx.executor().advance_clock(Duration::from_millis(61));
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().len(), 3);
    view.update(cx, |shell, cx| {
        assert!(shell.chat.logs[&local_key(SESSION)].answered.is_empty());
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
        let pane = shell.layout.focused().unwrap();
        let fresh = shell.chat_request_identity(pane, SESSION, 0, &question(false));
        shell.answer_chat_question(fresh, vec![0], false, cx);
    });
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().last(), Some(&b"\r".to_vec()));
    assert_eq!(sent.lock().unwrap().len(), 4);
}

#[gpui::test]
fn chat_unbound_and_unresolved_history_cards_are_read_only(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-historical-card",
        &permission(),
        "deploy production (y/n)",
    );
    view.update(cx, |shell, cx| {
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![ChatItem {
                kind: ChatItemKind::Assistant,
                text: "Finished".into(),
                ..Default::default()
            }]),
        );
        shell.approve_chat_permission(request.clone(), true, cx);
        let pane = shell.layout.focused().unwrap();
        let mut unbound = snapshot(vec![permission()]);
        unbound.bound = false;
        shell.apply_chat_poll(SESSION, unbound);
        let latest = shell.chat_request_identity(pane, SESSION, 0, &permission());
        shell.approve_chat_permission(latest, true, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-request-status").is_some());
    let terminal = cx.debug_bounds("chat-switch-terminal").unwrap().center();
    cx.simulate_click(terminal, gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_none());
    assert!(sent.lock().unwrap().is_empty());
}

#[gpui::test]
fn chat_permission_button_clicks_submit_once_and_working_session_is_read_only(
    cx: &mut TestAppContext,
) {
    struct Working;
    impl krill_app::sidebar::Source for Working {
        fn fetch(&self) -> std::io::Result<krill_app::sidebar::Snapshot> {
            Ok(krill_app::sidebar::Snapshot {
                sessions: vec![krill_app::sidebar::Session {
                    id: SESSION,
                    status: krill_proto::control::AgentStatus {
                        state: krill_proto::control::AgentState::Working,
                        ..krill_proto::control::AgentStatus::UNKNOWN
                    },
                    worktree: None,
                    cwd: None,
                    account: None,
                }],
                trees: BTreeMap::new(),
            })
        }
    }
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-approval-button",
        &permission(),
        "deploy production (y/n)",
    );
    let button = cx.debug_bounds("chat-permission-approve").unwrap().center();
    cx.simulate_click(button, gpui::Modifiers::default());
    cx.simulate_click(button, gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(*sent.lock().unwrap(), vec![b"y\r".to_vec()]);
    view.update(cx, |shell, cx| {
        shell.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(Arc::new(Working), cx);
        });
    });
    cx.run_until_parked();
    view.update(cx, |shell, cx| {
        assert!(!shell.chat_request_is_current(&request, cx));
        let mut reset = snapshot(vec![permission()]);
        reset.reset = true;
        shell.apply_chat_poll(SESSION, reset);
        let pane = shell.layout.focused().unwrap();
        let fresh = shell.chat_request_identity(pane, SESSION, 0, &permission());
        shell.approve_chat_permission(fresh, true, cx);
    });
    cx.run_until_parked();
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[gpui::test]
fn chat_pending_paste_preserves_draft_and_does_not_queue_card_keys(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-paste-queue",
        &permission(),
        "deploy production (y/n)",
    );
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| {
                state.set_value("draft after paste", window, cx);
            });
            shell.paste_clipboard(window, cx);
            assert!(shell.paste_queue.has_pending_for(pane));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            shell.approve_chat_permission(request.clone(), true, cx);
            assert_eq!(input.read(cx).value().as_ref(), "draft after paste");
            assert!(sent.lock().unwrap().is_empty());
            shell.cancel_pending_paste_for(pane, cx);
            assert!(!shell.paste_queue.has_pending_for(pane));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
            shell.approve_chat_permission(request.clone(), false, cx);
            assert!(input.read(cx).value().is_empty());
        });
    });
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![b"draft after paste\r".to_vec(), b"n\r".to_vec()]
    );
}

fn question_with_labels(multi: bool, labels: &[&str]) -> ChatItem {
    ChatItem {
        options: labels
            .iter()
            .map(|label| ChatQuestionOption {
                label: (*label).into(),
                description: String::new(),
            })
            .collect(),
        ..question(multi)
    }
}

fn rows(text: &str) -> Vec<Vec<Cell>> {
    text.lines()
        .map(|line| {
            line.chars()
                .map(|ch| Cell {
                    ch,
                    ..Cell::default()
                })
                .collect()
        })
        .collect()
}

#[test]
fn question_cursor_uses_unique_whole_labels_and_rejects_ambiguity() {
    for (labels, screen, expected) in [
        (["Run", "Run tests"], "> Run tests", Some(1)),
        (["Run tests", "Run"], "> Run tests\n  Run", Some(0)),
        (["Run", "Run tests"], "> rerun Run tests", None),
        (["Run", "Run tests"], "> Run\n  Run", None),
        (["Run", "Run tests"], "> Run\n> Run tests", None),
        (["Run", "Run"], "> Run", None),
        (["Run", " Run "], "> Run", None),
        (["Run", ""], "> Run", None),
        (["Run", "Run — tests"], "> Run — tests", None),
        (["Run", "Run (Recommended)"], "> Run (Recommended)", None),
        (["Run", "1. Run"], "> 1. Run", None),
    ] {
        let labels = labels.map(str::to_owned);
        assert_eq!(
            detect_question_cursor_index(&rows(screen), &labels),
            expected,
            "{screen:?}, {labels:?}"
        );
    }
}

#[test]
fn question_checkboxes_share_label_matching_and_use_only_prefix_state() {
    let labels = ["Run tests".to_owned(), "Run".to_owned()];
    for screen in [
        "> [ ] Run tests\n  [x] Run",
        "> 1. [ ] Run tests — uses [x] in description\n  2) [✓] Run (Recommended)",
        "→ ☐ 1. Run tests: execute suite\n  ☑ 2. Run",
    ] {
        let rows = rows(screen);
        assert_eq!(
            detect_question_cursor_index(&rows, &labels),
            Some(0),
            "{screen}"
        );
        assert_eq!(
            actions::checked_options(&rows, &labels),
            Some(BTreeSet::from([1])),
            "{screen}"
        );
    }
    for screen in [
        "> [ ] Run tests\n  [x] Run\n  [ ] Run",
        "> [ ] Run tests\n  Run",
        "> [ ] Run tests\n  [ ] Run — extra\n  [x] Run",
    ] {
        assert_eq!(
            actions::checked_options(&rows(screen), &labels),
            None,
            "{screen}"
        );
    }
    let duplicates = ["Run".to_owned(), "Run".to_owned()];
    assert_eq!(
        actions::checked_options(&rows("> [ ] Run\n  [x] Run"), &duplicates),
        None
    );
}

#[gpui::test]
fn chat_contained_single_label_submits_from_actual_cursor(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-contained-single",
        &question_with_labels(false, &["Run", "Run tests"]),
        "Choose one\n  Run\n> Run tests",
    );
    view.update(cx, |shell, cx| {
        assert!(shell.chat_request_is_current(&request, cx));
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
    });
    cx.run_until_parked();
    assert_eq!(*sent.lock().unwrap(), vec![b"\r".to_vec()]);
}

#[gpui::test]
fn chat_contained_multi_label_preserves_existing_checked_option(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-contained-multi",
        &question_with_labels(true, &["Run tests", "Run"]),
        "Choose one\n> [ ] Run tests\n  [x] Run",
    );
    view.update(cx, |shell, cx| {
        assert!(shell.chat_request_is_current(&request, cx));
        shell.answer_chat_question(request.clone(), vec![1], true, cx);
    });
    cx.run_until_parked();
    assert_eq!(*sent.lock().unwrap(), vec![b"\r".to_vec()]);
}

#[gpui::test]
fn chat_duplicate_labels_reject_card_input_and_keep_terminal_fallback(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-duplicate-labels",
        &question_with_labels(true, &["Run", "Run"]),
        "Choose one\n> [ ] Run\n  [x] Run",
    );
    view.update(cx, |shell, cx| {
        assert!(!shell.chat_request_is_current(&request, cx));
        shell.answer_chat_question(request.clone(), vec![0], true, cx);
    });
    cx.run_until_parked();
    assert!(sent.lock().unwrap().is_empty());
    let terminal = cx.debug_bounds("chat-switch-terminal").unwrap().center();
    cx.simulate_click(terminal, gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_none());
}

#[gpui::test]
fn chat_ambiguous_label_descriptions_reject_card_input(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-ambiguous-labels",
        &question_with_labels(false, &["Run", "Run — tests"]),
        "Choose one\n> Run — tests\n  Run",
    );
    view.update(cx, |shell, cx| {
        assert!(!shell.chat_request_is_current(&request, cx));
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
    });
    cx.run_until_parked();
    assert!(sent.lock().unwrap().is_empty());
}

#[gpui::test]
fn chat_partial_single_select_stale_frame_cannot_repeat_cursor_steps(cx: &mut TestAppContext) {
    let (view, cx, request, sent) = fixture(
        cx,
        "chat-partial-single",
        &question_with_labels(false, &["Run", "Run tests", "Skip"]),
        "Choose one\n> Run\n  Run tests\n  Skip",
    );
    let recorded = sent.clone();
    view.update(cx, |shell, cx| {
        shell.chat.send_input = Some(Box::new(move |bytes| {
            let mut sent = recorded.lock().unwrap();
            sent.push(bytes);
            sent.len() == 1
        }));
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(61));
    cx.run_until_parked();
    view.update(cx, |shell, cx| {
        assert!(matches!(
            shell.chat.logs[&local_key(SESSION)].submissions[&request.key],
            Submission::ReviewRequired
        ));
        // The live frame still claims cursor 0 after the delivered Down key.
        // Even after input delivery recovers, replaying Down could select Skip.
        let recorded = sent.clone();
        shell.chat.send_input = Some(Box::new(move |bytes| {
            recorded.lock().unwrap().push(bytes);
            true
        }));
        shell.answer_chat_question(request.clone(), vec![1], false, cx);
        assert!(shell.chat.logs[&local_key(SESSION)].answered.is_empty());
    });
    cx.executor().advance_clock(Duration::from_millis(61));
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![b"\x1b[B".to_vec(), b"\r".to_vec()]
    );
}
