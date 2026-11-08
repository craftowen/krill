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
        let log = &shell.chat.logs[&local_key(SessionId(992))];
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
                shell.chat.inputs[&local_key(SessionId(994))]
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
            shell.chat.inputs[&local_key(SessionId(994))]
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
            shell.chat.inputs[&local_key(SessionId(994))]
                .0
                .read(cx)
                .value()
                .as_ref(),
            "hello again"
        );
    });
}

#[gpui::test]
fn chat_opens_on_the_newest_rows_and_adds_older_ones_above(cx: &mut gpui::TestAppContext) {
    let (view, cx) = open(cx, state_path("chat-tail-first"));
    let page = |texts: &[&str], start: u64, has_earlier: bool, next_offset: u64| {
        // Same shape as the daemon's `chat_page_value`, including its per-item `at`.
        let mut data = serde_json::to_value(ChatPollSnapshot {
            agent: "claude".into(),
            session_id: "tail".into(),
            transcript_path: "/external/tail.jsonl".into(),
            bound: true,
            next_offset,
            items: texts
                .iter()
                .map(|text| ChatItem {
                    kind: ChatItemKind::Assistant,
                    text: (*text).to_owned(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
        .unwrap();
        for item in data["items"].as_array_mut().unwrap() {
            item["at"] = "2026-11-01T00:00:00Z".into();
        }
        data["page_start"] = start.into();
        data["has_earlier"] = has_earlier.into();
        serde_json::json!({"ok":true,"data":data}).to_string()
    };
    let polled_from = Arc::new(std::sync::Mutex::new(Vec::new()));
    view.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SessionId(993));
        shell.paths = Some(krill_proto::daemon::Paths {
            socket: "/unused".into(),
            holder: "/unused".into(),
            krilld: "/unused".into(),
        });
        let recorded = polled_from.clone();
        shell.chat.poll_call = Some(Arc::new(move |request| {
            let Request::AgentHistory(Action::ChatPoll { offset, .. }) = request else {
                panic!("expected ChatPoll");
            };
            recorded.lock().unwrap().push(*offset);
            Ok(Reply::AgentHistory(Response::ChatPolled(
                ChatPollSnapshot {
                    agent: "claude".into(),
                    session_id: "tail".into(),
                    transcript_path: "/external/tail.jsonl".into(),
                    bound: true,
                    next_offset: *offset,
                    ..Default::default()
                },
            )))
        }));
        shell.chat.workflow_call = Some(Arc::new(move |raw| {
            let request: serde_json::Value = serde_json::from_str(raw).unwrap();
            if request["action"] != "chat_poll" {
                return Ok(r#"{"ok":true,"data":{}}"#.to_owned());
            }
            assert_eq!(request["latest"], true);
            Ok(match request["end_offset"].as_u64() {
                None => page(&["newest"], 900, true, 1_000),
                Some(900) => page(&["older"], 0, false, 900),
                other => panic!("unexpected boundary {other:?}"),
            })
        }));
        shell.toggle_chat_view(cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        let log = &shell.chat.logs[&local_key(SessionId(993))];
        let texts: Vec<_> = log.items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, ["older", "newest"]);
        assert_eq!(
            log.offset, 1_000,
            "the live poll resumes after the tail page"
        );
        assert_eq!(log.earlier, None);
    });
    cx.executor().advance_clock(Duration::from_millis(800));
    cx.run_until_parked();
    assert_eq!(
        *polled_from.lock().unwrap(),
        [1_000],
        "never read from byte 0"
    );
}

#[gpui::test]
fn prepended_rows_keep_the_rows_on_screen_in_place(cx: &mut gpui::TestAppContext) {
    let make_items = |n: usize| -> Vec<Arc<ChatItem>> {
        (0..n)
            .map(|i| {
                Arc::new(ChatItem {
                    kind: ChatItemKind::Assistant,
                    text: format!("msg {i}"),
                    ..Default::default()
                })
            })
            .collect()
    };
    let initial = Arc::new(work_run::DisplayFeed::project(1_000, &make_items(100)));
    let state = gpui::ListState::new(initial.len(), gpui::ListAlignment::Bottom, gpui::px(0.));
    state.scroll_to(gpui::ListOffset {
        item_ix: 40,
        offset_in_item: gpui::px(0.),
    });
    let mut list = ChatList {
        host: local_key(SessionId(1)).0,
        feed: initial,
        revision: 0,
        epoch: 1,
        state: state.clone(),
    };
    let prepended = Arc::new(work_run::DisplayFeed::project(990, &make_items(110)));
    cx.update(|_| {
        list.sync(&prepended, 1, 1);
    });
    assert_eq!(state.logical_scroll_top().item_ix, 50);
    assert_eq!(state.item_count(), 110);
}

#[gpui::test]
fn chat_poll_reads_composer_meta_from_the_transcript(cx: &mut gpui::TestAppContext) {
    let dir = std::env::temp_dir().join(format!("krill-chat-meta-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join("session.jsonl");
    std::fs::write(
        &transcript,
        "{\"message\":{\"model\":\"claude-meta-test\",\"usage\":{\"input_tokens\":1}}}\n",
    )
    .unwrap();
    let transcript = transcript.to_string_lossy().into_owned();
    let (view, cx) = open(cx, state_path("chat-poll-meta"));
    view.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SessionId(993));
        shell.paths = Some(krill_proto::daemon::Paths {
            socket: "/unused".into(),
            holder: "/unused".into(),
            krilld: "/unused".into(),
        });
        shell.chat.poll_call = Some(Arc::new(move |_| {
            Ok(Reply::AgentHistory(Response::ChatPolled(
                ChatPollSnapshot {
                    agent: "claude".into(),
                    session_id: "meta-session".into(),
                    transcript_path: transcript.clone(),
                    bound: true,
                    next_offset: 64,
                    items: vec![ChatItem {
                        kind: ChatItemKind::Assistant,
                        text: "hi".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )))
        }));
        shell.toggle_chat_view(cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        let log = &shell.chat.logs[&local_key(SessionId(993))];
        assert_eq!(log.meta.model.as_deref(), Some("claude-meta-test"));
    });
    std::fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn work_run_list_sync_preserves_anchor_across_boundary_prepend_head_drain_and_tail_growth(
    cx: &mut gpui::TestAppContext,
) {
    let item = |kind: ChatItemKind, text: &str| ChatItem {
        kind,
        text: text.to_owned(),
        ..Default::default()
    };
    let mut log = SessionChatLog {
        base_index: 1_000,
        epoch: 1,
        items: vec![
            Arc::new(item(ChatItemKind::ToolCall, "read a.rs")),
            Arc::new(item(ChatItemKind::ToolResult, "contents a")),
            Arc::new(item(ChatItemKind::Assistant, "answer 1")),
            Arc::new(item(ChatItemKind::User, "anchor user message")),
            Arc::new(item(ChatItemKind::Assistant, "answer 2")),
            Arc::new(item(ChatItemKind::ToolCall, "read b.rs")),
        ],
        ..Default::default()
    };
    let feed = log.display_feed();
    assert_eq!(feed.len(), 5);
    log.toggle_run(feed.rows()[0].span());
    assert!(log.is_run_expanded(feed.rows()[0].span()));
    log.toggle_run(feed.rows()[4].span());
    assert!(log.is_run_expanded(feed.rows()[4].span()));

    let state = gpui::ListState::new(feed.len(), gpui::ListAlignment::Bottom, gpui::px(0.));
    state.scroll_to(gpui::ListOffset {
        item_ix: 2,
        offset_in_item: gpui::px(0.),
    });
    let mut list = ChatList {
        host: local_key(SessionId(1)).0,
        feed,
        revision: log.revision,
        epoch: log.epoch,
        state: state.clone(),
    };
    assert!(list.near_top(log.base_index, 4));
    assert!(!list.near_top(log.base_index, 3));

    log.append_and_trim(
        vec![
            item(ChatItemKind::ToolResult, "contents b"),
            item(ChatItemKind::Reasoning, "checking"),
            item(ChatItemKind::ToolCall, "read c.rs"),
        ],
        1_000,
    );
    log.revision = log.revision.wrapping_add(1);
    let tail_grown = log.display_feed();
    assert_eq!(tail_grown.len(), 5);
    cx.update(|_| {
        list.sync(&tail_grown, log.revision, log.epoch);
    });
    assert_eq!(state.item_count(), 5);
    assert_eq!(state.logical_scroll_top().item_ix, 2);

    log.prepend_earlier(
        vec![
            item(ChatItemKind::User, "older question"),
            item(ChatItemKind::ToolCall, "earlier tool merged into front run"),
        ],
        None,
    );
    let merged = log.display_feed();
    assert_eq!(merged.len(), 6);
    assert_eq!(merged.rows()[1].span().range(), 999..1_002);
    assert!(log.is_run_expanded(merged.rows()[1].span()));
    cx.update(|_| {
        list.sync(&merged, log.revision, log.epoch);
    });
    assert_eq!(state.item_count(), 6);
    assert_eq!(state.logical_scroll_top().item_ix, 3);

    log.append_and_trim(vec![item(ChatItemKind::Assistant, "final answer")], 9);
    log.revision = log.revision.wrapping_add(1);
    let drained = log.display_feed();
    assert_eq!(log.base_index, 1_001);
    assert_eq!(drained.rows()[0].span().range(), 1_001..1_002);
    assert!(
        log.is_run_expanded(drained.rows()[0].span()),
        "partially drained front WorkRun must keep its expanded state"
    );
    cx.update(|_| {
        list.sync(&drained, log.revision, log.epoch);
    });
    assert_eq!(state.logical_scroll_top().item_ix, 2);
    let Some(work_run::DisplayRow::Item {
        abs_idx,
        item: row_item,
    }) = drained.get(state.logical_scroll_top().item_ix)
    else {
        panic!("expected anchor row to remain DisplayRow::Item");
    };
    assert_eq!(*abs_idx, 1_003);
    assert_eq!(row_item.text, "anchor user message");

    log.append_and_trim(vec![item(ChatItemKind::Assistant, "later answer")], 4);
    let tail_drained = log.display_feed();
    assert_eq!(log.base_index, 1_007);
    assert_eq!(tail_drained.rows()[0].span().range(), 1_007..1_009);
    assert!(
        log.is_run_expanded(tail_drained.rows()[0].span()),
        "run expanded before tail growth keeps its open state after draining past its initial span"
    );
}
