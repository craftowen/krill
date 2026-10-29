use super::*;
use gpui::TestAppContext;
use krill_proto::agent_history::ChatQuestionOption;

#[gpui::test]
fn desktop_hybrid_chat_toggle_and_incremental_poll_render(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(None, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_none());

    shell.update(cx, |this, cx| {
        let pane = this.layout.focused().unwrap();
        this.layout.set_session(pane, SessionId(9));
        this.toggle_chat_view(cx);
        this.apply_chat_poll(
            SessionId(9),
            ChatPollSnapshot {
                agent: "claude".into(),
                session_id: "sid-9".into(),
                transcript_path: "/tmp/sid-9.jsonl".into(),
                next_offset: 512,
                reset: false,
                bound: true,
                items: vec![
                    ChatItem {
                        kind: ChatItemKind::User,
                        text: "안녕".into(),
                        ..ChatItem::default()
                    },
                    ChatItem {
                        kind: ChatItemKind::Question,
                        title: "Mode".into(),
                        text: "Choose one".into(),
                        options: vec![ChatQuestionOption {
                            label: "Fast".into(),
                            description: String::new(),
                        }],
                        ..ChatItem::default()
                    },
                ],
            },
        );
        cx.notify();
    });
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds("desktop-chat-overlay")
        .expect("overlay should be rendered");
    assert!(bounds.size.width > gpui::px(200.0));
    assert!(bounds.size.height > gpui::px(100.0));
    assert!(cx.debug_bounds("desktop-chat-send").is_some());

    shell.update(cx, |this, cx| {
        assert!(this.is_active_chat_view());
        assert_eq!(this.chat.logs[&SessionId(9)].items.len(), 2);
        this.toggle_chat_view(cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_none());
}

#[cfg(unix)]
#[gpui::test]
fn long_chat_renders_viewport_rows_and_shares_message_text(cx: &mut TestAppContext) {
    use crate::shell::tests::{open, state_path};
    let (shell, cx) = open(cx, state_path("virtual-chat"));
    let pane = shell.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SessionId(444));
        shell.layout.set_chat_view(pane, true);
        shell.apply_chat_poll(
            SessionId(444),
            ChatPollSnapshot {
                bound: true,
                reset: true,
                items: (0..250)
                    .map(|index| ChatItem {
                        kind: ChatItemKind::Assistant,
                        text: format!("message {index}\n{}", "long answer\n".repeat(16)),
                        ..ChatItem::default()
                    })
                    .collect(),
                ..ChatPollSnapshot::default()
            },
        );
        let messages = shell.chat.logs[&SessionId(444)].items.clone();
        assert!(Arc::ptr_eq(
            &messages[0],
            &shell.chat.logs[&SessionId(444)].items[0]
        ));
        cx.notify();
        pane
    });
    cx.run_until_parked();
    // The GPUI test selector API requires static strings; these two
    // fixture-only selectors live for the test process.
    let first = Box::leak(format!("chat-item-{}-0", pane.get()).into_boxed_str());
    let last = Box::leak(format!("chat-item-{}-249", pane.get()).into_boxed_str());
    assert!(cx.debug_bounds(first).is_none());
    assert!(cx.debug_bounds(last).is_some());
    assert!(cx.debug_bounds("desktop-chat-send").is_some());
}

#[test]
fn base_index_preserves_answer_key_after_250_item_drain() {
    let mut log = SessionChatLog::default();
    let items: Vec<ChatItem> = (0..260)
        .map(|i| ChatItem {
            kind: ChatItemKind::Question,
            tool_id: format!("tool-{i}"),
            ..ChatItem::default()
        })
        .collect();
    log.items.extend(items.into_iter().map(Arc::new));
    let excess = log.items.len() - 250;
    log.items.drain(0..excess);
    log.base_index = log.base_index.saturating_add(excess);

    // Item originally at index 200 is now at relative index 190,
    // and base_index (10) + 190 == 200 preserves the original answer_key.
    let rel_idx = 190;
    let abs_idx = log.base_index + rel_idx;
    assert_eq!(abs_idx, 200);
    assert_eq!(log.items[rel_idx].tool_id, "tool-200");
}

#[test]
fn detect_question_cursor_and_bidirectional_steps() {
    let make_row = |s: &str| -> Vec<Cell> {
        s.chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect()
    };
    let rows = vec![make_row("  Fast"), make_row("❯ Safe"), make_row("  Custom")];
    let options = vec!["Fast".to_owned(), "Safe".to_owned(), "Custom".to_owned()];
    assert_eq!(detect_question_cursor_index(&rows, &options), Some(1));

    let mut cursor = 1;
    let mut steps = Vec::new();
    step_cursor(&mut steps, &mut cursor, 0);
    assert_eq!(cursor, 0);
    assert_eq!(steps, vec![b"\x1b[A".to_vec()]);
}
#[cfg(unix)]
#[gpui::test]
fn slow_chat_poll_is_coalesced_and_rejects_changed_host(cx: &mut gpui::TestAppContext) {
    use crate::shell::tests::{open, state_path};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (view, cx) = open(cx, state_path("async-chat-poll"));
    let calls = Arc::new(AtomicUsize::new(0));
    let (release, gate) = tokio::sync::oneshot::channel();
    view.update(cx, |shell, cx| {
        let pane = shell.layout.active_tab().focused;
        shell.layout.set_session(pane, SessionId(321));
        shell.paths = Some(krill_proto::daemon::Paths {
            socket: "/unused".into(),
            holder: "/unused".into(),
            krilld: "/unused".into(),
        });
        shell.layout.set_chat_view(pane, true);
        let recorded = calls.clone();
        shell.chat.poll_call = Some(Arc::new(move |_| {
            recorded.fetch_add(1, Ordering::SeqCst);
            Ok(Reply::AgentHistory(Response::ChatPolled(
                ChatPollSnapshot {
                    next_offset: 7,
                    bound: true,
                    ..Default::default()
                },
            )))
        }));
        shell.chat.next_poll_io = Some(gate);
        for _ in 0..20 {
            shell.poll_active_chat(cx);
        }
    });
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    view.update(cx, |shell, cx| {
        for _ in 0..20 {
            shell.poll_active_chat(cx);
        }
        let pane = shell.layout.active_tab().focused;
        shell
            .layout
            .set_host(pane, krill_app::hosts::HostId::Remote("changed".into()));
    });
    release.send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    view.read_with(cx, |shell, _| {
        assert!(shell.chat.task.is_none());
        assert!(!shell.chat.logs.contains_key(&SessionId(321)));
    });
}

#[gpui::test]
fn chat_card_copy_and_code_block_copy_write_clean_clipboard(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(None, cx));
    cx.run_until_parked();
    shell.update(cx, |this, cx| {
        let pane = this.layout.focused().unwrap();
        this.layout.set_session(pane, SessionId(42));
        this.toggle_chat_view(cx);
        this.apply_chat_poll(
            SessionId(42),
            ChatPollSnapshot {
                agent: "claude".into(),
                bound: true,
                items: vec![ChatItem {
                    kind: ChatItemKind::Assistant,
                    text: "실행하세요:\n```sh\npython3 -c 'print(123)'\n```".into(),
                    ..ChatItem::default()
                }],
                ..ChatPollSnapshot::default()
            },
        );
        cx.notify();
    });
    cx.run_until_parked();
    let code_btn = cx
        .debug_bounds("chat-copy-code")
        .expect("code copy button should render");
    cx.simulate_click(code_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("python3 -c 'print(123)'")
    );

    let item_btn = cx
        .debug_bounds("chat-copy-item")
        .expect("item copy button should render");
    cx.simulate_click(item_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("실행하세요:\n```sh\npython3 -c 'print(123)'\n```")
    );
}

#[gpui::test]
fn desktop_chat_choice_survives_update_restart(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(None, cx));
    cx.run_until_parked();
    let path = std::env::temp_dir().join(format!("krill-chat-restart-{}.bin", std::process::id()));
    shell.update(cx, |this, cx| {
        let pane = this.layout.focused().unwrap();
        this.layout.set_session(pane, SessionId(9241));
        this.toggle_chat_view(cx);
        assert!(this.is_session_chat_view(SessionId(9241), cx));
        this.layout.save(&path).unwrap();
        // A new app process retains only the persisted layout and the same daemon session.
        this.chat = ChatState::default();
        this.layout = krill_app::layout::Layout::load(&path).unwrap();
        assert_eq!(this.layout.session(pane), Some(SessionId(9241)));
        assert!(
            this.is_session_chat_view(SessionId(9241), cx),
            "update restart replaced the saved Chat view with Terminal"
        );
        this.apply_chat_poll(
            SessionId(9241),
            ChatPollSnapshot {
                agent: "claude".into(),
                session_id: "existing-conversation".into(),
                transcript_path: "fixture.jsonl".into(),
                bound: true,
                items: vec![ChatItem {
                    kind: ChatItemKind::Assistant,
                    text: "existing conversation".into(),
                    ..ChatItem::default()
                }],
                ..ChatPollSnapshot::default()
            },
        );
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_some());
    assert!(
        cx.debug_bounds("desktop-chat-send").is_some(),
        "restart lost the Chat composer"
    );
    shell.update(cx, |this, cx| {
        assert_eq!(
            this.chat.logs[&SessionId(9241)].items[0].text,
            "existing conversation"
        );
        this.toggle_chat_view(cx);
        assert!(!this.is_session_chat_view(SessionId(9241), cx));
        this.layout.save(&path).unwrap();
        this.chat = ChatState::default();
        this.layout = krill_app::layout::Layout::load(&path).unwrap();
        assert!(!this.is_session_chat_view(SessionId(9241), cx));
    });
    std::fs::remove_file(path).unwrap();
}

#[gpui::test]
fn chat_view_choice_does_not_leak_to_same_session_number_on_another_host(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(None, cx));
    cx.run_until_parked();
    shell.update(cx, |this, cx| {
        let local = this.layout.focused().unwrap();
        this.layout.set_session(local, SessionId(9242));
        this.layout.new_tab();
        let remote = this.layout.focused().unwrap();
        this.layout.set_host(
            remote,
            krill_app::hosts::HostId::Remote("windows-pc".into()),
        );
        this.layout.set_session(remote, SessionId(9242));
        this.toggle_chat_view(cx);
        assert!(this.is_pane_chat_view(remote, cx));
        assert!(
            !this.is_pane_chat_view(local, cx),
            "remote Chat choice changed local Terminal"
        );
        this.layout.activate(0);
        assert!(!this.is_session_chat_view(SessionId(9242), cx));
        this.layout.activate(1);
        assert!(this.is_session_chat_view(SessionId(9242), cx));
    });
}
