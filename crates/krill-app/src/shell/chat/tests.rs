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
        assert_eq!(this.chat.logs[&local_key(SessionId(9))].items.len(), 2);
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
        let messages = shell.chat.logs[&local_key(SessionId(444))].items.clone();
        assert!(Arc::ptr_eq(
            &messages[0],
            &shell.chat.logs[&local_key(SessionId(444))].items[0]
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
        assert!(shell.chat.poll_tasks.is_empty());
        assert!(!shell.chat.logs.contains_key(&local_key(SessionId(321))));
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
            this.chat.logs[&local_key(SessionId(9241))].items[0].text,
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

#[test]
fn composer_meta_reads_the_statusline_record_from_any_profile_scope() {
    let dir = std::env::temp_dir().join(format!("krill-chat-meta-{}", std::process::id()));
    let scope = dir.join("session-context/profile-scope");
    std::fs::create_dir_all(&scope).unwrap();
    std::fs::write(
        scope.join("sid.json"),
        r#"{"model":"claude-opus-5-5","effort":"high","percent":14.0}"#,
    )
    .unwrap();
    let transcript = dir.join("sid.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"message":{"model":"older","usage":{"input_tokens":900000}}}"#,
            "\n"
        ),
    )
    .unwrap();
    let meta = observe_composer_meta(Some(&dir), "sid", transcript.to_str().unwrap());
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(meta.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(meta.effort.as_deref(), Some("high"));
    assert_eq!(meta.context_percent, Some(14));
}

#[cfg(unix)]
#[gpui::test]
fn closing_chat_pane_drops_its_chat_state(cx: &mut TestAppContext) {
    use crate::shell::tests::{open, state_path};
    let (shell, cx) = open(cx, state_path("chat-close-prune"));
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    shell.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SessionId(445));
        shell.layout.set_chat_view(pane, true);
        shell.apply_chat_poll(
            SessionId(445),
            ChatPollSnapshot {
                bound: true,
                reset: true,
                items: vec![ChatItem {
                    kind: ChatItemKind::Assistant,
                    text: "hello".into(),
                    ..ChatItem::default()
                }],
                ..ChatPollSnapshot::default()
            },
        );
        cx.notify();
    });
    cx.run_until_parked();
    let key = local_key(SessionId(445));
    shell.update(cx, |shell, _| {
        assert!(shell.chat.logs.contains_key(&key));
        assert!(shell.chat.inputs.contains_key(&key));
    });
    shell.update(cx, |shell, cx| shell.request_close_tab(1, cx));
    cx.run_until_parked();
    shell.update(cx, |shell, _| {
        assert_eq!(shell.layout.tabs().len(), 1);
        assert!(
            !shell.chat.logs.contains_key(&key),
            "closed pane keeps chat log"
        );
        assert!(
            !shell.chat.inputs.contains_key(&key),
            "closed pane keeps composer"
        );
    });
}

#[test]
fn work_run_pairs_by_tool_id_falls_back_fifo_and_isolates_orphan_results() {
    let single = work_run::RawSpan::single(5);
    assert_eq!(single.start(), 5);
    assert_eq!(single.end(), 6);
    assert_eq!(single.range(), 5..6);

    let items: Vec<Arc<ChatItem>> = vec![
        ChatItem {
            kind: ChatItemKind::ToolCall,
            tool_id: "call-read".into(),
            tool_name: "Read".into(),
            file_path: "crates/krill-app/src/lib.rs".into(),
            text: "crates/krill-app/src/lib.rs".into(),
            ..ChatItem::default()
        },
        ChatItem {
            kind: ChatItemKind::ToolCall,
            tool_id: "call-bash".into(),
            tool_name: "Bash".into(),
            title: "Run check".into(),
            text: "cargo check --locked".into(),
            is_error: true,
            ..ChatItem::default()
        },
        ChatItem {
            kind: ChatItemKind::ToolCall,
            tool_name: "Grep".into(),
            title: "Search symbols".into(),
            ..ChatItem::default()
        },
        ChatItem {
            kind: ChatItemKind::ToolResult,
            text: "3 matches".into(),
            ..ChatItem::default()
        },
        ChatItem {
            kind: ChatItemKind::ToolResult,
            tool_id: "call-bash".into(),
            text: "exit code 1".into(),
            is_error: true,
            ..ChatItem::default()
        },
        ChatItem {
            kind: ChatItemKind::ToolResult,
            tool_id: "call-read".into(),
            text: "pub mod ui;\n".into(),
            ..ChatItem::default()
        },
        ChatItem {
            kind: ChatItemKind::ToolResult,
            tool_id: "unmatched-id".into(),
            file_path: "crates/krill-app/src/orphan.rs".into(),
            text: "orphan payload".into(),
            ..ChatItem::default()
        },
    ]
    .into_iter()
    .map(Arc::new)
    .collect();

    let feed = work_run::DisplayFeed::project(100, &items);
    assert_eq!(feed.len(), 1);
    assert_eq!(feed.rows()[0].span().range(), 100..107);
    let Some(work_run::DisplayRow::WorkRun(run)) = feed.get(0) else {
        panic!("expected a single WorkRun row");
    };
    assert_eq!(run.counts.tool_calls, 3);
    assert_eq!(run.counts.read, 1);
    assert_eq!(run.counts.command, 1);
    assert_eq!(run.counts.search, 1);
    assert_eq!(run.counts.errors, 1);
    assert_eq!(run.latest_detail, "crates/krill-app/src/orphan.rs");
    assert_eq!(run.steps.len(), 4);

    assert_eq!(run.steps[0].primary_abs_idx(), 100);
    assert_eq!(run.steps[0].file_path(), "crates/krill-app/src/lib.rs");
    assert_eq!(
        run.steps[0].badge(),
        (ui::BadgeTone::Neutral, ctext::STEP_READ.get())
    );
    let read_sections = run.steps[0].body_sections();
    assert_eq!(read_sections.len(), 1);
    assert_eq!(read_sections[0].abs_idx, 105);
    assert_eq!(read_sections[0].label, Some(ctext::STEP_OUTPUT.get()));
    assert_eq!(read_sections[0].text, "pub mod ui;\n");

    assert_eq!(run.steps[1].primary_abs_idx(), 101);
    assert!(run.steps[1].is_error());
    assert_eq!(
        run.steps[1].badge(),
        (ui::BadgeTone::Destructive, ctext::TOOL_ERROR.get())
    );
    let bash_sections = run.steps[1].body_sections();
    assert_eq!(bash_sections.len(), 2);
    assert_eq!(bash_sections[0].label, Some(ctext::STEP_INPUT.get()));
    assert_eq!(bash_sections[1].label, Some(ctext::TOOL_ERROR.get()));

    assert_eq!(run.steps[2].primary_abs_idx(), 102);
    assert_eq!(run.steps[2].headline(), "Search symbols");
    assert_eq!(run.steps[2].copy_text(), "3 matches");

    assert_eq!(run.steps[3].primary_abs_idx(), 106);
    assert!(matches!(
        run.steps[3],
        work_run::WorkStep::Tool(work_run::ToolPair::OrphanResult {
            result_abs_idx: 106,
            ..
        })
    ));
}

#[test]
fn display_feed_keeps_solo_reasoning_groups_multi_reasoning_and_invalidates_cache() {
    let mut log = SessionChatLog {
        base_index: 50,
        ..SessionChatLog::default()
    };
    log.append_and_trim(
        vec![
            ChatItem {
                kind: ChatItemKind::User,
                text: "question".into(),
                ..ChatItem::default()
            },
            ChatItem {
                kind: ChatItemKind::Reasoning,
                text: "single thought\ndetail".into(),
                ..ChatItem::default()
            },
            ChatItem {
                kind: ChatItemKind::Assistant,
                text: "answer".into(),
                ..ChatItem::default()
            },
        ],
        MAX_CHAT_KEPT,
    );
    let first = log.display_feed();
    let second = log.display_feed();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.len(), 3);
    assert!(matches!(
        first.get(1),
        Some(work_run::DisplayRow::Item { abs_idx: 51, .. })
    ));

    log.append_and_trim(
        vec![
            ChatItem {
                kind: ChatItemKind::Reasoning,
                text: "first step\nmore".into(),
                ..ChatItem::default()
            },
            ChatItem {
                kind: ChatItemKind::Reasoning,
                text: "second step".into(),
                ..ChatItem::default()
            },
        ],
        MAX_CHAT_KEPT,
    );
    let updated = log.display_feed();
    assert!(!Arc::ptr_eq(&first, &updated));
    assert_eq!(updated.len(), 4);
    let Some(work_run::DisplayRow::WorkRun(run)) = updated.get(3) else {
        panic!("two consecutive reasoning items must collapse into a WorkRun");
    };
    assert_eq!(run.span.range(), 53..55);
    assert_eq!(
        run.summary_label(),
        ctext::RUN_REASONING.format(&[("n", "2")])
    );
    assert_eq!(run.latest_detail, "second step");

    log.prepend_earlier(
        vec![ChatItem {
            kind: ChatItemKind::User,
            text: "older".into(),
            ..ChatItem::default()
        }],
        None,
    );
    let after_prepend = log.display_feed();
    assert!(!Arc::ptr_eq(&updated, &after_prepend));
    assert_eq!(
        after_prepend.rows()[0].span(),
        work_run::RawSpan::single(49)
    );

    log.reset_transcript();
    assert!(log.display_feed().is_empty());
}

#[test]
fn work_run_segmentation_folds_short_pre_tool_note_and_resets_at_turn_boundaries() {
    let make = |kind: ChatItemKind, text: &str| {
        Arc::new(ChatItem {
            kind,
            text: text.to_owned(),
            ..Default::default()
        })
    };
    let mut call = ChatItem {
        kind: ChatItemKind::ToolCall,
        tool_id: "c1".into(),
        tool_name: "Read".into(),
        file_path: "Cargo.toml".into(),
        ..Default::default()
    };
    call.title = "Cargo.toml".into();

    let items = vec![
        make(ChatItemKind::Assistant, "Let me inspect."),
        Arc::new(call.clone()),
        make(ChatItemKind::ToolResult, "[package]\nname = \"krill\""),
        make(ChatItemKind::Assistant, "Done."),
    ];
    let feed = work_run::DisplayFeed::project(10, &items);
    assert_eq!(feed.len(), 2);
    let Some(work_run::DisplayRow::WorkRun(run)) = feed.get(0) else {
        panic!("pre-tool note + tool must merge into WorkRun");
    };
    assert_eq!(run.span.range(), 10..13);
    assert!(matches!(
        run.steps[0],
        work_run::WorkStep::Note { abs_idx: 10, .. }
    ));
    assert!(matches!(run.steps[1], work_run::WorkStep::Tool(_)));
    let Some(work_run::DisplayRow::Item { abs_idx: 13, item }) = feed.get(1) else {
        panic!("final short answer must stay standalone Assistant item");
    };
    assert_eq!(item.text, "Done.");

    // Multi-line prose, >160 chars, and prose preceding User turn boundary must not fold.
    let long_prose = "x".repeat(work_run::MAX_PREAMBLE_CHARS + 1);
    let boundary_items = vec![
        make(ChatItemKind::Assistant, "before boundary"),
        make(ChatItemKind::User, "follow up"),
        make(ChatItemKind::Assistant, "line 1\nline 2"),
        Arc::new(call.clone()),
        make(ChatItemKind::Assistant, &long_prose),
        Arc::new(call),
    ];
    let boundary_feed = work_run::DisplayFeed::project(0, &boundary_items);
    assert_eq!(boundary_feed.len(), 6);
    assert!(matches!(
        boundary_feed.get(0),
        Some(work_run::DisplayRow::Item { abs_idx: 0, .. })
    ));
    assert!(matches!(
        boundary_feed.get(2),
        Some(work_run::DisplayRow::Item { abs_idx: 2, .. })
    ));
    assert!(matches!(
        boundary_feed.get(4),
        Some(work_run::DisplayRow::Item { abs_idx: 4, .. })
    ));
}

#[test]
fn work_run_timing_tracks_live_working_run_without_fake_historical_timestamps() {
    use krill_proto::control::AgentState;
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let call = Arc::new(ChatItem {
        kind: ChatItemKind::ToolCall,
        tool_id: "c1".into(),
        tool_name: "Read".into(),
        file_path: "src/lib.rs".into(),
        ..Default::default()
    });
    let result = Arc::new(ChatItem {
        kind: ChatItemKind::ToolResult,
        tool_id: "c1".into(),
        text: "pub fn ok() {}".into(),
        ..Default::default()
    });

    // Historical run first seen while Waiting must have no fabricated elapsed timestamp.
    let settled_feed = work_run::DisplayFeed::project(50, &[Arc::clone(&call), result]);
    let mut timing = work_run::WorkRunTimingState::default();
    timing.sync(&settled_feed, AgentState::Waiting, t0);
    assert_eq!(timing.elapsed_secs(50, t0 + Duration::from_secs(30)), None);

    // Live trailing WorkRun starts counting while Working, latches when Waiting, and survives drain.
    let live_feed = work_run::DisplayFeed::project(60, &[call]);
    timing.sync(&live_feed, AgentState::Working, t0);
    assert_eq!(
        timing.elapsed_secs(60, t0 + Duration::from_secs(65)),
        Some(65)
    );
    timing.sync(
        &live_feed,
        AgentState::Waiting,
        t0 + Duration::from_secs(65),
    );
    assert_eq!(
        timing.elapsed_secs(60, t0 + Duration::from_secs(120)),
        Some(65)
    );
    timing.migrate_head_drain(Some(60), 61);
    assert_eq!(
        timing.elapsed_secs(61, t0 + Duration::from_secs(120)),
        Some(65)
    );

    assert_eq!(work_run::format_elapsed_secs(0), "0s");
    assert_eq!(work_run::format_elapsed_secs(59), "59s");
    assert_eq!(work_run::format_elapsed_secs(60), "1m");
    assert_eq!(work_run::format_elapsed_secs(65), "1m 5s");
    assert_eq!(work_run::format_elapsed_secs(3600), "1h");
    assert_eq!(work_run::format_elapsed_secs(3660), "1h 1m");
}

#[test]
fn work_run_summary_sentence_progress_and_tool_verbs() {
    let items = vec![
        Arc::new(ChatItem {
            kind: ChatItemKind::ToolCall,
            tool_id: "c1".into(),
            tool_name: "Read".into(),
            file_path: "a.rs".into(),
            ..Default::default()
        }),
        Arc::new(ChatItem {
            kind: ChatItemKind::ToolResult,
            tool_id: "c1".into(),
            text: "fn a() {}".into(),
            ..Default::default()
        }),
        Arc::new(ChatItem {
            kind: ChatItemKind::ToolCall,
            tool_id: "c2".into(),
            tool_name: "Bash".into(),
            title: "cargo test".into(),
            is_error: true,
            ..Default::default()
        }),
        Arc::new(ChatItem {
            kind: ChatItemKind::ToolResult,
            tool_id: "c2".into(),
            is_error: true,
            text: "exit 1".into(),
            ..Default::default()
        }),
        Arc::new(ChatItem {
            kind: ChatItemKind::ToolCall,
            tool_id: "c3".into(),
            tool_name: "Edit".into(),
            file_path: "b.rs".into(),
            ..Default::default()
        }),
    ];
    let feed = work_run::DisplayFeed::project(0, &items);
    let Some(work_run::DisplayRow::WorkRun(run)) = feed.get(0) else {
        panic!("expected WorkRun");
    };
    assert_eq!(run.total_tools(), 3);
    assert_eq!(run.completed_tools(), 2);
    assert_eq!(run.progress_label(true).as_deref(), Some("2/3"));
    assert_eq!(run.progress_label(false), None);
    assert_eq!(
        run.failed_count_label(),
        Some(ctext::RUN_FAILED_N.format(&[("n", "1")]))
    );
    assert_eq!(run.active_detail(), "b.rs");
    assert_eq!(run.steps[0].verb(false), ctext::VERB_READ_DONE.get());
    assert_eq!(run.steps[1].verb(false), ctext::VERB_RUN_FAIL.get());
    assert_eq!(run.steps[2].verb(true), ctext::VERB_EDIT_LIVE.get());
    assert_eq!(run.steps[2].verb(false), ctext::VERB_EDIT_DONE.get());
    assert!(
        run.header_label(true).contains(
            &ctext::RUN_EDIT_LIVE_ONE
                .format(&[("n", "1")])
                .to_ascii_lowercase()
        )
    );
}

#[test]
fn user_prompt_fold_threshold_and_chat_log_wrap_state() {
    assert!(!is_user_prompt_foldable(&"a".repeat(600)));
    assert!(is_user_prompt_foldable(&"a".repeat(601)));
    assert!(!is_user_prompt_foldable("1\n2\n3\n4\n5\n6\n7\n8"));
    assert!(is_user_prompt_foldable("1\n2\n3\n4\n5\n6\n7\n8\n9"));

    let mut log = SessionChatLog::default();
    log.toggle_user_message(5);
    log.toggle_code_wrap(6, 1);
    assert!(log.is_user_message_expanded(5));
    assert!(log.is_code_wrapped(6, 1));

    let items: Vec<ChatItem> = (0..10)
        .map(|i| ChatItem {
            kind: ChatItemKind::User,
            text: format!("msg {i}"),
            ..Default::default()
        })
        .collect();
    log.append_and_trim(items, 4);
    // base_index becomes 6 after trimming 0..6
    assert!(!log.is_user_message_expanded(5));
    assert!(log.is_code_wrapped(6, 1));

    log.reset_transcript();
    assert!(!log.is_code_wrapped(6, 1));
}

#[test]
fn composer_primary_action_respects_sending_working_latch_and_draft_state() {
    use krill_proto::control::AgentState;

    assert_eq!(
        composer_primary_action(ComposerPrimaryInput {
            agent_state: AgentState::Working,
            submission: ComposerSubmissionState::InFlight,
            draft_empty: true,
            interrupt_latched: false,
        }),
        ComposerPrimaryAction::Sending,
    );
    assert_eq!(
        composer_primary_action(ComposerPrimaryInput {
            agent_state: AgentState::Working,
            submission: ComposerSubmissionState::InFlight,
            draft_empty: false,
            interrupt_latched: false,
        }),
        ComposerPrimaryAction::Sending,
    );

    assert_eq!(
        composer_primary_action(ComposerPrimaryInput {
            agent_state: AgentState::Working,
            submission: ComposerSubmissionState::ReviewRequired,
            draft_empty: false,
            interrupt_latched: false,
        }),
        ComposerPrimaryAction::Stop { latched: false },
    );

    // Option A: Working + ready shows Stop even when draft text or attachments exist.
    assert_eq!(
        composer_primary_action(ComposerPrimaryInput {
            agent_state: AgentState::Working,
            submission: ComposerSubmissionState::Ready,
            draft_empty: true,
            interrupt_latched: false,
        }),
        ComposerPrimaryAction::Stop { latched: false },
    );
    assert_eq!(
        composer_primary_action(ComposerPrimaryInput {
            agent_state: AgentState::Working,
            submission: ComposerSubmissionState::Ready,
            draft_empty: false,
            interrupt_latched: false,
        }),
        ComposerPrimaryAction::Stop { latched: false },
    );

    // Latched Working keeps Stop disabled even when text is typed afterward.
    assert_eq!(
        composer_primary_action(ComposerPrimaryInput {
            agent_state: AgentState::Working,
            submission: ComposerSubmissionState::Ready,
            draft_empty: false,
            interrupt_latched: true,
        }),
        ComposerPrimaryAction::Stop { latched: true },
    );

    for state in [
        AgentState::Waiting,
        AgentState::Blocked,
        AgentState::Done,
        AgentState::Exited,
        AgentState::Unknown,
    ] {
        assert_eq!(
            composer_primary_action(ComposerPrimaryInput {
                agent_state: state,
                submission: ComposerSubmissionState::Ready,
                draft_empty: true,
                interrupt_latched: false,
            }),
            ComposerPrimaryAction::Send { enabled: false },
        );
        assert_eq!(
            composer_primary_action(ComposerPrimaryInput {
                agent_state: state,
                submission: ComposerSubmissionState::Ready,
                draft_empty: false,
                interrupt_latched: false,
            }),
            ComposerPrimaryAction::Send { enabled: true },
        );
        assert_eq!(
            composer_primary_action(ComposerPrimaryInput {
                agent_state: state,
                submission: ComposerSubmissionState::ReviewRequired,
                draft_empty: false,
                interrupt_latched: false,
            }),
            ComposerPrimaryAction::Send { enabled: false },
        );
    }
}

#[test]
fn work_run_unified_diff_summary_and_unread_divider_projection() {
    assert!(work_run::is_unified_diff(
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n"
    ));
    assert!(!work_run::is_unified_diff("just plain explanation text"));

    let items = vec![
        Arc::new(ChatItem {
            kind: ChatItemKind::User,
            text: "fix lib".into(),
            ..Default::default()
        }),
        Arc::new(ChatItem {
            kind: ChatItemKind::ToolCall,
            tool_name: "Edit".into(),
            file_path: "src/lib.rs".into(),
            text: "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n".into(),
            ..Default::default()
        }),
        Arc::new(ChatItem {
            kind: ChatItemKind::Assistant,
            text: "Updated src/lib.rs".into(),
            ..Default::default()
        }),
    ];

    let feed = work_run::DisplayFeed::project_with_unread(10, &items, Some(11));
    assert_eq!(feed.rows().len(), 4);
    assert!(matches!(
        feed.rows().get(1),
        Some(work_run::DisplayRow::UnreadDivider { first_abs_idx: 11 })
    ));
    let Some(work_run::DisplayRow::WorkRun(run)) = feed.rows().get(2) else {
        panic!("expected work run row after unread divider");
    };
    assert_eq!(run.changed_files, 1);
    assert_eq!(run.diff_added, 1);
    assert_eq!(run.diff_deleted, 1);
    let summary = run
        .diff_summary_label()
        .expect("diff summary should be formatted");
    assert!(summary.contains('1'));
    assert!(summary.contains("+1"));
    assert!(summary.contains("-1"));
}

/// Bot chats: a launch preamble turn is not drawn, and item changes sit between the turns sent
/// around them (Orca's bot item note rows).
#[test]
fn bot_chat_hides_preamble_turn_and_interleaves_item_notes_by_turn_time() {
    use krill_proto::bots::BotItemStatus;
    let user = |text: &str| {
        Arc::new(ChatItem {
            kind: ChatItemKind::User,
            text: text.into(),
            ..Default::default()
        })
    };
    let reply = |text: &str| {
        Arc::new(ChatItem {
            kind: ChatItemKind::Assistant,
            text: text.into(),
            ..Default::default()
        })
    };
    let preamble = "You are Shop, a bot that runs in Krill. Your home is the current folder.\n\n- Coordinate with other bots using `krill bot send --to <name>` when a task belongs to another bot or needs input from one.";
    let items = vec![
        user(preamble),
        reply("ready"),
        user("first"),
        reply("one"),
        user("second"),
        reply("two"),
    ];
    let note = |at_ms: u64, title: &str| {
        Arc::new(work_run::ItemNote {
            at_ms,
            actor: work_run::NoteActor::Bot,
            action: work_run::NoteAction::Status(BotItemStatus::Working),
            title: title.into(),
            status: BotItemStatus::Working,
            detail: String::new(),
        })
    };
    // Turns: "first" at 100, "second" at 200 (newest first).
    let notes = [note(150, "between"), note(250, "after"), note(50, "before")];
    let feed =
        work_run::DisplayFeed::project_with_notes(0, &items, None, &notes, &[200, 100], None);
    let shape: Vec<String> = feed
        .rows()
        .iter()
        .map(|row| match row {
            work_run::DisplayRow::Item { item, .. } => item.text.clone(),
            work_run::DisplayRow::ItemNote { note, .. } => format!("note:{}", note.title),
            _ => "other".into(),
        })
        .collect();
    assert_eq!(
        shape,
        [
            "ready",
            "note:before",
            "first",
            "one",
            "note:between",
            "second",
            "two",
            "note:after"
        ]
    );
}

#[test]
fn bot_item_notes_do_not_jump_or_vanish_when_prompt_submitted() {
    use krill_proto::bots::BotItemStatus;
    let user = |text: &str, at: &str| {
        Arc::new(ChatItem {
            kind: ChatItemKind::User,
            text: text.into(),
            at: at.into(),
            ..Default::default()
        })
    };
    let reply = |text: &str, at: &str| {
        Arc::new(ChatItem {
            kind: ChatItemKind::Assistant,
            text: text.into(),
            at: at.into(),
            ..Default::default()
        })
    };
    let note = |at_ms: u64, title: &str| {
        Arc::new(work_run::ItemNote {
            at_ms,
            actor: work_run::NoteActor::Bot,
            action: work_run::NoteAction::Status(BotItemStatus::Done),
            title: title.into(),
            status: BotItemStatus::Done,
            detail: String::new(),
        })
    };
    let shape_of = |feed: &work_run::DisplayFeed| -> Vec<String> {
        feed.rows()
            .iter()
            .map(|row| match row {
                work_run::DisplayRow::Item { item, .. } => item.text.clone(),
                work_run::DisplayRow::ItemNote { note, .. } => format!("note:{}", note.title),
                _ => "other".into(),
            })
            .collect()
    };

    // Real timestamps mirroring bot-5: Turn 1 (12:52:50Z), Turn 2 (14:42:54Z),
    // Item notes at 20:04:48.458Z (05:04 KST), Turn 3 submitted at 22:51:49Z.
    let t1_ms = 1_795_438_370_876_u64;
    let t2_ms = 1_795_444_974_785_u64;
    let note_ms = 1_795_464_288_458_u64;
    let t3_ms = 1_795_474_309_176_u64;

    let notes = [note(note_ms, "done")];
    let stamp = |at_ms: u64, text: &str| work_run::TurnStamp {
        at_ms,
        text: text.into(),
    };
    let empty_obs = std::collections::BTreeMap::new();

    // Case 1: SessionPromptHistory ALREADY recorded Turn 3 before ChatPoll loaded User #3.
    // Even without item.at, text-matching prevents note from disappearing above Turn 2!
    let items_before_u3 = vec![
        user("swarm sweep", ""),
        reply("working", ""),
        user("status report", ""),
        reply("all done", ""),
    ];
    let turns_with_t3 = [
        stamp(t3_ms, "check screenshot"),
        stamp(t2_ms, "status report"),
        stamp(t1_ms, "swarm sweep"),
    ];
    let feed_ahead = work_run::DisplayFeed::project_with_turn_stamps(
        0,
        &items_before_u3,
        None,
        &notes,
        &turns_with_t3,
        &empty_obs,
        None,
    );
    assert_eq!(
        shape_of(&feed_ahead),
        [
            "swarm sweep",
            "working",
            "status report",
            "all done",
            "note:done"
        ],
        "ahead-of-transcript prompt history must not pull Turn 2's notes up above Turn 2"
    );

    // Case 2: ChatPoll loaded incremental User #3 (empty item.at, observed at t3_ms) BEFORE
    // SessionPromptHistory reloaded Turn 3 (still only [t2, t1]).
    // Note must stay right above User #3, NEVER jump below User #3!
    let items_with_u3 = vec![
        user("swarm sweep", "2026-11-23T12:52:50Z"),
        reply("working", "2026-11-23T12:53:00Z"),
        user("status report", "2026-11-23T14:42:54Z"),
        reply("all done", "2026-11-23T20:04:40Z"),
        user("check screenshot", ""),
    ];
    let mut obs_u3 = std::collections::BTreeMap::new();
    obs_u3.insert(4, t3_ms);
    let turns_stale = [stamp(t2_ms, "status report"), stamp(t1_ms, "swarm sweep")];
    let feed_behind = work_run::DisplayFeed::project_with_turn_stamps(
        0,
        &items_with_u3,
        None,
        &notes,
        &turns_stale,
        &obs_u3,
        None,
    );
    let expected_with_u3 = [
        "swarm sweep",
        "working",
        "status report",
        "all done",
        "note:done",
        "check screenshot",
    ];
    assert_eq!(
        shape_of(&feed_behind),
        expected_with_u3,
        "stale prompt history must not push Turn 2's notes below newly polled User #3"
    );

    // Case 3: Both transcript and prompt history synced -> identical placement.
    let feed_synced = work_run::DisplayFeed::project_with_turn_stamps(
        0,
        &items_with_u3,
        None,
        &notes,
        &turns_with_t3,
        &obs_u3,
        None,
    );
    assert_eq!(shape_of(&feed_synced), expected_with_u3);

    // Case 4: Prompt history empty (e.g. >5 turns or post-account-switch) -> item.at anchors notes.
    let items_all_timed = vec![
        user("swarm sweep", "2026-11-23T12:52:50Z"),
        reply("working", "2026-11-23T12:53:00Z"),
        user("status report", "2026-11-23T14:42:54Z"),
        reply("all done", "2026-11-23T20:04:40Z"),
        user("check screenshot", "2026-11-23T22:51:48Z"),
    ];
    let feed_at_only = work_run::DisplayFeed::project_with_turn_stamps(
        0,
        &items_all_timed,
        None,
        &notes,
        &[],
        &empty_obs,
        None,
    );
    assert_eq!(shape_of(&feed_at_only), expected_with_u3);

    // Case 5: Image-attached prompt where `take_leading_image_paths` lifted the path into
    // `item.file_path` (including Windows/non-POSIX paths not stripped by `clean_user_prompt`
    // and truncated at `MAX_PROMPT_DISPLAY_CHARS` with `…`) still matches its turn stamp cleanly.
    let img_path =
        r"C:\Users\owen\AppData\Local\Temp\krill-paste\file-cdd938d5975cf5c9ba612ce775b382c0.png";
    let long_korean = "첨부한 이미지 봐봐라. 빨간박스 부분에 전환 누르면 해당 채팅 탭 사라진다. 어디로 가는지 모르겠어. 전환 눌러도 현재 페이지에서 계정만 전환되어야하는데 사라지면 어떻게 다시 찾냐.. UX고려해서 수정해라. 모든 프로바이더 다 점검해라. 프로필 전환이 그냥 같은 창 안에서 전환되면 좋겠는데. 그게 어려운 일이야? 추가로 계정이 바뀌어도 현재 채팅 내용이 유지되어야 한다.";
    let raw_img_prompt = format!("'{img_path}' {long_korean}");
    let truncated_stamp_text = krill_proto::git::clean_prompt_display(
        &raw_img_prompt,
        krill_proto::git::MAX_PROMPT_DISPLAY_CHARS,
    )
    .expect("clean prompt display");
    assert!(truncated_stamp_text.ends_with('…'));
    let img_user = Arc::new(ChatItem {
        kind: ChatItemKind::User,
        text: long_korean.into(),
        file_path: img_path.into(),
        ..Default::default()
    });
    let items_with_img = vec![user("status report", ""), reply("all done", ""), img_user];
    let turns_with_img = [
        stamp(t3_ms, &truncated_stamp_text),
        stamp(t2_ms, "status report"),
    ];
    let feed_img = work_run::DisplayFeed::project_with_turn_stamps(
        0,
        &items_with_img,
        None,
        &notes,
        &turns_with_img,
        &empty_obs,
        None,
    );
    assert_eq!(
        shape_of(&feed_img),
        ["status report", "all done", "note:done", long_korean],
        "image-attached prompt whose leading path was moved into file_path must still match its turn stamp"
    );

    // Case 6: Identical repeated prompt text ("진행해" submitted twice) while the second submit
    // is already in SessionPromptHistory ahead of ChatPoll must NOT bind the earlier turn to the
    // newer timestamp and yank Turn 1's note above Turn 1.
    let items_dup_before = vec![user("진행해", ""), reply("done turn 1", "")];
    let mut obs_dup = std::collections::BTreeMap::new();
    obs_dup.insert(0, t2_ms);
    let turns_dup_ahead = [stamp(t3_ms, "진행해"), stamp(t2_ms, "진행해")];
    let feed_dup = work_run::DisplayFeed::project_with_turn_stamps(
        0,
        &items_dup_before,
        None,
        &notes,
        &turns_dup_ahead,
        &obs_dup,
        None,
    );
    assert_eq!(
        shape_of(&feed_dup),
        ["진행해", "done turn 1", "note:done"],
        "ahead-of-transcript duplicate-text prompt must not steal the earlier turn's timestamp"
    );

    // Case 7: When incremental ChatPoll arrives slightly after a bot note written during the new
    // turn (observed_now_ms > turn_note_ms > submit_ms), matching SessionPromptHistory submit time
    // anchors User #3 BEFORE its own turn's note instead of keeping the late poll arrival time.
    let turn3_note_ms = t3_ms + 300;
    let late_poll_ms = t3_ms + 800;
    let mut obs_late = std::collections::BTreeMap::new();
    obs_late.insert(4, late_poll_ms);
    let notes_both = [note(note_ms, "done"), note(turn3_note_ms, "turn3-started")];
    let feed_late_poll = work_run::DisplayFeed::project_with_turn_stamps(
        0,
        &items_with_u3,
        None,
        &notes_both,
        &turns_with_t3,
        &obs_late,
        None,
    );
    assert_eq!(
        shape_of(&feed_late_poll),
        [
            "swarm sweep",
            "working",
            "status report",
            "all done",
            "note:done",
            "check screenshot",
            "note:turn3-started"
        ],
        "exact prompt submit timestamp must win over late incremental poll arrival time"
    );
}

#[test]
fn chat_page_parses_timestamps_and_remote_autocomplete_items_validate_paths() {
    let page_json = serde_json::json!({
        "ok": true,
        "data": {
            "agent": "claude",
            "session_id": "sid-ts",
            "transcript_path": "/tmp/ts.jsonl",
            "next_offset": 256,
            "reset": false,
            "bound": true,
            "page_start": 0,
            "has_earlier": false,
            "items": [{
                "kind": "assistant",
                "text": "hello",
                "at": "2026-11-21T12:34:56+09:00"
            }]
        }
    });
    let page = parse_chat_page(&page_json.to_string()).expect("chat page should parse");
    assert_eq!(page.snapshot.items.len(), 1);
    assert_eq!(page.snapshot.items[0].at, "2026-11-21T12:34:56+09:00");
    // The message time reads the offset (Orca formats it in the viewer's zone): 03:34:56Z.
    assert_eq!(
        krill_proto::time::parse_reset_epoch(&page.snapshot.items[0].at),
        Some(1_795_232_096)
    );

    let slash_json = serde_json::json!({
        "ok": true,
        "data": {
            "items": [
                {"name": "review", "description": "Code review", "kind": "skill"},
                {"name": "/compact", "description": "Compact context", "kind": "command", "scope": "Built-in"}
            ]
        }
    });
    let slash =
        catalog::parse_remote_slash_items(&slash_json.to_string(), "rev").expect("slash items");
    assert_eq!(slash.len(), 1);
    assert_eq!(slash[0].label, "/review");
    assert_eq!(slash[0].source, catalog::AutocompleteSource::Skill);

    let mention_json = serde_json::json!({
        "ok": true,
        "data": {
            "items": [
                {"path": "src/main.rs"},
                {"path": "docs/read me.md"},
                {"path": "../secret.txt"},
                {"path": "/etc/passwd"},
                {"path": "win\\bad.rs"}
            ]
        }
    });
    let mentions =
        catalog::parse_remote_mention_items(&mention_json.to_string(), "").expect("mention items");
    assert_eq!(mentions.len(), 2);
    assert_eq!(mentions[0].insert_text, "@src/main.rs ");
    assert_eq!(mentions[1].insert_text, "@\"docs/read me.md\" ");
}

#[test]
fn composer_meta_cache_and_composer_status_json_parse_rate_limit_and_live_prompt() {
    let dir = std::env::temp_dir().join(format!(
        "krill-composer-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let ctx_dir = dir.join("session-context/default");
    std::fs::create_dir_all(&ctx_dir).unwrap();
    let transcript = dir.join("chat.jsonl");
    std::fs::write(
        &transcript,
        r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":1000}}}"#,
    )
    .unwrap();
    let path_str = transcript.to_str().unwrap();

    let first = observe_composer_meta(Some(&dir), "sid-cache", path_str);
    let second = observe_composer_meta(Some(&dir), "sid-cache", path_str);
    assert_eq!(first.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(second.model, first.model);

    // Statusline session-context file overrides cached transcript values.
    std::fs::write(
        ctx_dir.join("sid-cache.json"),
        r#"{"model":"claude-opus-5-5","effort":"high","percent":42.5}"#,
    )
    .unwrap();
    let overridden = observe_composer_meta(Some(&dir), "sid-cache", path_str);
    assert_eq!(overridden.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(overridden.effort.as_deref(), Some("high"));
    assert_eq!(overridden.context_percent, Some(43));

    std::fs::remove_dir_all(&dir).ok();

    let mut meta = ComposerMeta::default();
    let status_json = serde_json::json!({
        "ok": true,
        "data": {
            "agent": "claude",
            "model": "claude-opus-5-5",
            "rate_limited": true,
            "live_prompt": {
                "kind": "yes_no",
                "prompt": "Continue deploy?"
            }
        }
    });
    let agent = apply_composer_status_json(&status_json.to_string(), &mut meta);
    assert_eq!(agent.as_deref(), Some("claude"));
    assert!(meta.rate_limited);
    assert!(matches!(
        meta.live_prompt,
        Some(krill_proto::screen::prompt::PromptKind::YesNo { ref prompt })
            if prompt == "Continue deploy?"
    ));
}

#[test]
fn long_clipboard_text_paste_saves_private_txt_attachment_and_quotes_cleanly() {
    let long = "line\n".repeat(7000);
    assert!(long.len() >= 32 * 1024);
    let item = gpui::ClipboardItem::new_string(long);
    let classified = classify_chat_clipboard_item(&item);
    let ChatClipboardContent::Attachments(atts) = classified else {
        panic!("expected long paste >= 32 KiB to become a file attachment");
    };
    assert_eq!(atts.len(), 1);
    let path = &atts[0].path;
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("txt"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    std::fs::remove_file(path).ok();

    assert_eq!(
        format_chat_quote("hello\n\nworld\x1b[31m!").as_deref(),
        Some("> hello\n>\n> world!\n\n")
    );
    assert_eq!(format_chat_quote("   "), None);
}

/// Every key of a typed sentence reaches the composer: none is taken by a chat shortcut
/// (a PR screenshot once showed "Who. re. ou?", which turned out to be the synthetic typing tool
/// under the Korean input method, not the composer).
#[cfg(unix)]
#[gpui::test]
fn composer_keeps_every_typed_key(cx: &mut TestAppContext) {
    use crate::shell::tests::{open, state_path};
    let (shell, cx) = open(cx, state_path("chat-typed-keys"));
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    shell.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SessionId(446));
        shell.layout.set_chat_view(pane, true);
        shell.apply_chat_poll(
            SessionId(446),
            ChatPollSnapshot {
                bound: true,
                reset: true,
                ..ChatPollSnapshot::default()
            },
        );
        cx.notify();
    });
    cx.run_until_parked();
    let key = local_key(SessionId(446));
    let input = shell.update(cx, |shell, _| {
        shell.chat.inputs.get(&key).unwrap().0.clone()
    });
    cx.update(|window, cx| input.update(cx, |state, cx| state.focus(window, cx)));
    cx.run_until_parked();
    let sentence = "Hi! Who are you? Answer in one short sentence.";
    cx.simulate_input(sentence);
    cx.run_until_parked();
    cx.update(|_, cx| assert_eq!(input.read(cx).value().as_ref(), sentence));
}

#[test]
fn delivered_mail_renders_as_sender_notice_not_owner_bubble() {
    use work_run::{DisplayFeed, DisplayRow, NoticeKind, parse_inbound_notice};
    let pointer = "[Krill orchestration] Unread mail. From: 'worker-a' (worker_done), 'it'\\''s' (question). Read with: krill orchestration check --to 'coordinator' --run 'r1' --unread --json";
    let notice = parse_inbound_notice(pointer).unwrap();
    assert_eq!(notice.kind, NoticeKind::Message);
    assert_eq!(notice.from, ["worker-a", "it's"]);
    let legacy = parse_inbound_notice("[Krill orchestration] Unread mail. Read with: krill orchestration check --to 'c' --unread --json").unwrap();
    assert_eq!((legacy.kind, legacy.from.len()), (NoticeKind::Update, 1));

    let mail =
        parse_inbound_notice("[Krill bot mail from Scout] Subject: Daily digest\nThree new PRs.")
            .unwrap();
    assert_eq!(mail.from, ["Scout"]);
    assert_eq!(mail.detail, "Daily digest\nThree new PRs.");

    let ext = parse_inbound_notice("[Krill external message via slack | channel: C1 | from: Owen (U1)]\n<<<\nhi >> there\n>>>\nAttachments:\n- /tmp/a.png\n").unwrap();
    assert_eq!(ext.from, ["Owen (slack)"]);
    assert_eq!(ext.detail, "hi >> there\nAttachments:\n- /tmp/a.png");
    assert!(parse_inbound_notice("please check [Krill orchestration] Unread mail.").is_none());

    let items: Vec<Arc<ChatItem>> = [pointer, "real question"]
        .into_iter()
        .map(|text| {
            Arc::new(ChatItem {
                kind: ChatItemKind::User,
                text: text.into(),
                ..ChatItem::default()
            })
        })
        .collect();
    let feed = DisplayFeed::project(0, &items);
    assert!(matches!(
        feed.get(0),
        Some(DisplayRow::Notice { abs_idx: 0, .. })
    ));
    assert!(matches!(
        feed.get(1),
        Some(DisplayRow::Item { abs_idx: 1, .. })
    ));
}

/// Bot chats fold each settled turn behind its answer under a "Worked for" line (Orca's turn
/// fold); the running turn and an opened one keep their work in view.
#[test]
fn bot_chat_folds_settled_turns_behind_their_answer() {
    let at = |kind: ChatItemKind, text: &str, at: &str| {
        Arc::new(ChatItem {
            kind,
            text: text.into(),
            at: at.into(),
            ..Default::default()
        })
    };
    let items = vec![
        at(ChatItemKind::User, "fix it", "2026-11-22T01:00:00Z"),
        at(ChatItemKind::Assistant, "looking", "2026-11-22T01:00:02Z"),
        at(ChatItemKind::ToolCall, "Read a.rs", "2026-11-22T01:00:05Z"),
        at(ChatItemKind::ToolResult, "ok", "2026-11-22T01:00:06Z"),
        at(ChatItemKind::Assistant, "fixed", "2026-11-22T01:00:16Z"),
        at(ChatItemKind::User, "thanks", "2026-11-22T01:01:00Z"),
        at(ChatItemKind::ToolCall, "Read b.rs", "2026-11-22T01:01:01Z"),
    ];
    let shape = |fold: &work_run::TurnFold| -> Vec<String> {
        work_run::DisplayFeed::project_with_notes(0, &items, None, &[], &[], Some(fold))
            .rows()
            .iter()
            .map(|row| match row {
                work_run::DisplayRow::Item { item, .. } => item.text.clone(),
                work_run::DisplayRow::WorkRun(_) => "run".into(),
                work_run::DisplayRow::TurnBar {
                    worked_secs,
                    folds,
                    expanded,
                    ..
                } => format!("bar:{worked_secs}:{folds}:{expanded}"),
                _ => "other".into(),
            })
            .collect()
    };
    let live = work_run::TurnFold {
        live: true,
        expanded: std::collections::BTreeSet::default(),
    };
    assert_eq!(
        shape(&live),
        ["fix it", "bar:16:true:false", "fixed", "thanks", "run"]
    );
    let opened = work_run::TurnFold {
        live: false,
        expanded: [0].into_iter().collect(),
    };
    assert_eq!(
        shape(&opened),
        [
            "fix it",
            "bar:16:true:true",
            // "looking" is the run's own preamble line.
            "run",
            "fixed",
            "thanks",
            "bar:1:true:false"
        ]
    );
    assert_eq!(work_run::format_turn_duration(16), "16s");
    assert_eq!(work_run::format_turn_duration(125), "2m 5s");
    assert_eq!(work_run::format_turn_duration(3_725), "1h 2m 5s");
}

/// Orca's memory note summary: counts while one or two kinds changed, a total past that.
#[test]
fn memory_receipt_summary_reads_like_orca() {
    let receipt = |added: usize, removed: usize, other_changes: bool| work_run::MemoryReceipt {
        added: vec!["a".into(); added],
        removed: vec!["r".into(); removed],
        other_changes,
    };
    assert_eq!(
        receipt(1, 0, false).summary(),
        "메모리 업데이트됨 · 1개 추가"
    );
    assert_eq!(
        receipt(1, 2, false).summary(),
        "메모리 업데이트됨 · 1개 추가, 2개 제거"
    );
    assert_eq!(
        receipt(1, 1, true).summary(),
        "메모리 업데이트됨 · 3개 변경"
    );
    assert_eq!(receipt(0, 0, false).summary(), "메모리 업데이트됨");
}

#[test]
fn strip_image_token_spans_markers_and_payload_whitespace_separation() {
    use gpui_component::input::{InlineToken, InputContent};

    let image_att = |path: &str| DraftAttachment {
        path: std::path::PathBuf::from(path),
        preview_path: None,
        display_name: std::path::Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        byte_len: 42,
        is_image: true,
    };
    let img1 = image_att("/tmp/a.png");
    let img2 = image_att("/tmp/b.png");
    let spaced = image_att("/Users/owen/my shot.png");
    let tok =
        |path: &str, label: &str| InlineToken::new(format!("{IMAGE_TOKEN_PREFIX}{path}"), label);

    // Leading token directly before text (`[Image #1]이거 봐`).
    let c = InputContent::new("[Image #1]이거 봐")
        .with_token(0..10, tok("/tmp/a.png", "[Image #1]"))
        .unwrap();
    assert_eq!(strip_image_token_spans(c.text(), c.tokens()), "이거 봐");
    assert_eq!(
        build_chat_payload_with_tokens(c.text(), c.tokens(), std::slice::from_ref(&img1))
            .as_deref(),
        Some("/tmp/a.png 이거 봐")
    );

    // Leading token with space (`[Image #1] 이거 봐`) does not double spaces.
    let c = InputContent::new("[Image #1] 이거 봐")
        .with_token(0..10, tok("/tmp/a.png", "[Image #1]"))
        .unwrap();
    assert_eq!(strip_image_token_spans(c.text(), c.tokens()), "이거 봐");
    assert_eq!(
        build_chat_payload_with_tokens(c.text(), c.tokens(), std::slice::from_ref(&img1))
            .as_deref(),
        Some("/tmp/a.png 이거 봐")
    );

    // Quoted path with spaces still gets trailing space before adjacent text.
    let c = InputContent::new("[Image #1]이거 봐")
        .with_token(0..10, tok("/Users/owen/my shot.png", "[Image #1]"))
        .unwrap();
    assert_eq!(
        build_chat_payload_with_tokens(c.text(), c.tokens(), std::slice::from_ref(&spaced))
            .as_deref(),
        Some("'/Users/owen/my shot.png' 이거 봐")
    );

    // Adjacent inline tokens (`[Image #1][Image #2]비교`).
    let c = InputContent::new("[Image #1][Image #2]비교")
        .with_token(0..10, tok("/tmp/a.png", "[Image #1]"))
        .unwrap()
        .with_token(10..20, tok("/tmp/b.png", "[Image #2]"))
        .unwrap();
    assert_eq!(strip_image_token_spans(c.text(), c.tokens()), "비교");
    assert_eq!(
        build_chat_payload_with_tokens(c.text(), c.tokens(), &[img1.clone(), img2.clone()])
            .as_deref(),
        Some("/tmp/a.png /tmp/b.png 비교")
    );

    // Mid-text inline token with and without spaces (`앞 [Image #1] 뒤`, `앞[Image #1]뒤`).
    let prefix_len = "앞 ".len();
    let c = InputContent::new("앞 [Image #1] 뒤")
        .with_token(prefix_len..prefix_len + 10, tok("/tmp/a.png", "[Image #1]"))
        .unwrap();
    assert_eq!(strip_image_token_spans(c.text(), c.tokens()), "앞 뒤");
    assert_eq!(
        build_chat_payload_with_tokens(c.text(), c.tokens(), std::slice::from_ref(&img1))
            .as_deref(),
        Some("앞 /tmp/a.png 뒤")
    );

    let tight_len = "앞".len();
    let c = InputContent::new("앞[Image #1]뒤")
        .with_token(tight_len..tight_len + 10, tok("/tmp/a.png", "[Image #1]"))
        .unwrap();
    assert_eq!(strip_image_token_spans(c.text(), c.tokens()), "앞 뒤");
    assert_eq!(
        build_chat_payload_with_tokens(c.text(), c.tokens(), std::slice::from_ref(&img1))
            .as_deref(),
        Some("앞 /tmp/a.png 뒤")
    );

    // Image-only prompt (`[Image #1]`).
    let c = InputContent::new("[Image #1]")
        .with_token(0..10, tok("/tmp/a.png", "[Image #1]"))
        .unwrap();
    assert_eq!(strip_image_token_spans(c.text(), c.tokens()), "");
    assert_eq!(
        build_chat_payload_with_tokens(c.text(), c.tokens(), std::slice::from_ref(&img1))
            .as_deref(),
        Some("/tmp/a.png")
    );

    // Removed image token produces neither a path nor extra spaces.
    assert_eq!(
        build_chat_payload_with_tokens(c.text(), c.tokens(), &[]),
        None
    );
    let c = InputContent::new("앞[Image #1]뒤")
        .with_token(tight_len..tight_len + 10, tok("/tmp/a.png", "[Image #1]"))
        .unwrap();
    assert_eq!(
        build_chat_payload_with_tokens(c.text(), c.tokens(), &[]).as_deref(),
        Some("앞뒤")
    );
    let c = InputContent::new("[Image #1][Image #2]이거 봐")
        .with_token(0..10, tok("/tmp/a.png", "[Image #1]"))
        .unwrap()
        .with_token(10..20, tok("/tmp/b.png", "[Image #2]"))
        .unwrap();
    assert_eq!(
        build_chat_payload_with_tokens(c.text(), c.tokens(), std::slice::from_ref(&img1))
            .as_deref(),
        Some("/tmp/a.png 이거 봐")
    );

    // Literal non-token `[Image #1]` and non-image tokens are preserved by `strip_image_token_spans`.
    assert_eq!(
        strip_image_token_spans("[Image #1] 이거 봐", &[]),
        "[Image #1] 이거 봐"
    );
    let mention = InlineToken::new("mention:src/main.rs", "@src/main.rs");
    let c = InputContent::new("@src/main.rs [Image #1] 봐")
        .with_token(0..12, mention)
        .unwrap()
        .with_token(13..23, tok("/tmp/a.png", "[Image #1]"))
        .unwrap();
    assert_eq!(
        strip_image_token_spans(c.text(), c.tokens()),
        "@src/main.rs 봐"
    );

    // `strip_chat_image_markers` removes case-insensitive `[Image #<digits>]` anywhere.
    assert_eq!(strip_chat_image_markers("[Image #1] 이거 봐"), "이거 봐");
    assert_eq!(strip_chat_image_markers("[Image #1]이거 봐"), "이거 봐");
    assert_eq!(strip_chat_image_markers("앞 [image #12] 뒤"), "앞 뒤");
    assert_eq!(strip_chat_image_markers("앞[IMAGE #3]뒤"), "앞 뒤");
    assert_eq!(strip_chat_image_markers("[Image #1][Image #2]"), "");
    assert_eq!(
        strip_chat_image_markers("keep [Image #] [Image #1a] [Image #1"),
        "keep [Image #] [Image #1a] [Image #1"
    );
}

#[test]
fn upload_chat_attachments_with_maps_remote_path_and_registers_preview() {
    let img = DraftAttachment {
        path: std::path::PathBuf::from("/local/shot.png"),
        preview_path: None,
        display_name: "shot.png".into(),
        byte_len: 128,
        is_image: true,
    };
    let doc = DraftAttachment {
        path: std::path::PathBuf::from("/local/notes.txt"),
        preview_path: None,
        display_name: "notes.txt".into(),
        byte_len: 64,
        is_image: false,
    };
    let uploaded = upload_chat_attachments_with(vec![img.clone(), doc.clone()], |path| {
        let name = path.file_name()?.to_str()?;
        Some(format!("/var/folders/remote/krill-paste.test/{name}"))
    })
    .expect("all uploads succeeded");
    assert_eq!(uploaded.len(), 2);
    assert_eq!(
        uploaded[0].path,
        std::path::PathBuf::from("/var/folders/remote/krill-paste.test/shot.png")
    );
    assert_eq!(
        uploaded[0].preview_path.as_deref(),
        Some(std::path::Path::new("/local/shot.png"))
    );
    assert_eq!(
        uploaded[0].preview_path(),
        std::path::Path::new("/local/shot.png")
    );
    assert_eq!(
        uploaded[1].path,
        std::path::PathBuf::from("/var/folders/remote/krill-paste.test/notes.txt")
    );
    assert_eq!(uploaded[1].preview_path, None);

    // Partial upload failure aborts the whole batch without leaking local paths.
    let failed = upload_chat_attachments_with(vec![img.clone(), doc], |path| {
        if path.ends_with("shot.png") {
            Some("/tmp/remote/shot.png".to_owned())
        } else {
            None
        }
    });
    assert!(failed.is_none());

    // Control characters in remote path are rejected.
    let invalid =
        upload_chat_attachments_with(vec![img], |_| Some("/tmp/bad\npath.png".to_owned()));
    assert!(invalid.is_none());

    // ChatImages remote preview registry resolves exact and macOS `/private/` aliases.
    let mut images = images::ChatImages::default();
    images.register_remote_preview(
        std::path::PathBuf::from("/var/folders/remote/krill-paste.test/shot.png"),
        std::path::PathBuf::from("/local/shot.png"),
    );
    assert_eq!(
        images.resolve_remote_preview(std::path::Path::new(
            "/var/folders/remote/krill-paste.test/shot.png"
        )),
        Some(std::path::Path::new("/local/shot.png"))
    );
    assert_eq!(
        images.resolve_remote_preview(std::path::Path::new(
            "/private/var/folders/remote/krill-paste.test/shot.png"
        )),
        Some(std::path::Path::new("/local/shot.png"))
    );
    assert_eq!(
        images.resolve_remote_preview(std::path::Path::new("/remote/unknown.png")),
        None
    );
}
