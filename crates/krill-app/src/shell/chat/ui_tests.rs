//! Interaction and geometry regressions for the native transcript.
#![cfg(unix)]
use super::*;
use crate::shell::tests::{open, state_path};
use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext, point, px, size};

const SESSION: SessionId = SessionId(771);

fn snapshot(items: Vec<ChatItem>) -> ChatPollSnapshot {
    ChatPollSnapshot {
        agent: "claude".into(),
        bound: true,
        items,
        session_id: "chat-ui-test".into(),
        transcript_path: "/fake/chat-ui.jsonl".into(),
        ..Default::default()
    }
}

fn fixture<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    items: Vec<ChatItem>,
) -> (Entity<Shell>, &'a mut VisualTestContext) {
    let (shell, cx) = open(cx, state_path(name));
    shell.update(cx, |shell, cx| {
        shell.sidebar_collapsed = true;
        shell.panel_collapsed = true;
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SESSION);
        shell.layout.set_chat_view(pane, true);
        shell.apply_chat_poll(SESSION, snapshot(items));
        cx.notify();
    });
    cx.run_until_parked();
    (shell, cx)
}

fn selector(
    shell: &Entity<Shell>,
    cx: &VisualTestContext,
    prefix: &str,
    index: usize,
) -> &'static str {
    let pane = shell.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    Box::leak(format!("{prefix}-{}-{index}", pane.get()).into_boxed_str())
}

fn scroll(cx: &mut VisualTestContext, delta: f32) {
    let feed = cx.debug_bounds("desktop-chat-feed").unwrap();
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: feed.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(delta))),
        modifiers: gpui::Modifiers::default(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    cx.run_until_parked();
}

#[gpui::test]
fn chat_tail_follow_detaches_on_reader_scroll_and_jump_resumes_without_layout_shift(
    cx: &mut TestAppContext,
) {
    let (shell, cx) = fixture(
        cx,
        "chat-tail",
        (0..80)
            .map(|index| ChatItem {
                kind: ChatItemKind::Assistant,
                text: format!("message {index}\n{}", "content\n".repeat(6)),
                ..Default::default()
            })
            .collect(),
    );
    cx.simulate_resize(size(px(640.), px(650.)));
    cx.run_until_parked();
    let first = selector(&shell, cx, "chat-item", 0);
    let tail = selector(&shell, cx, "chat-item", 79);
    assert!(cx.debug_bounds(first).is_none());
    assert!(cx.debug_bounds(tail).is_some());
    assert!(cx.debug_bounds("chat-jump-latest").is_none());
    let composer = cx.debug_bounds("desktop-chat-composer").unwrap();
    // Variable-height virtualization measures older rows as they enter the
    // viewport; use a real sequence of reader wheel gestures.
    for _ in 0..4 {
        scroll(cx, 900.);
    }
    assert!(cx.debug_bounds("chat-jump-latest").is_some());
    let feed = cx.debug_bounds("desktop-chat-feed").unwrap();
    assert!(
        cx.debug_bounds(tail)
            .is_none_or(|bounds| bounds.top() >= feed.bottom() || bounds.bottom() <= feed.top()),
        "tail must be outside the viewport: {:?}, feed {feed:?}",
        cx.debug_bounds(tail)
    );
    shell.update(cx, |shell, cx| {
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![ChatItem {
                kind: ChatItemKind::Assistant,
                text: "new tail".into(),
                ..Default::default()
            }]),
        );
        cx.notify();
    });
    cx.run_until_parked();
    let new_tail = selector(&shell, cx, "chat-item", 80);
    assert!(
        cx.debug_bounds(new_tail)
            .is_none_or(|bounds| bounds.top() >= feed.bottom() || bounds.bottom() <= feed.top()),
        "append must preserve the reader's position: {:?}",
        cx.debug_bounds(new_tail)
    );
    assert_eq!(cx.debug_bounds("desktop-chat-composer").unwrap(), composer);
    let jump = cx.debug_bounds("chat-jump-latest").unwrap();
    cx.simulate_click(jump.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds(new_tail).is_some());
    assert!(cx.debug_bounds("chat-jump-latest").is_none());
    assert_eq!(cx.debug_bounds("desktop-chat-composer").unwrap(), composer);
    shell.update(cx, |shell, cx| {
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![ChatItem {
                kind: ChatItemKind::Assistant,
                text: "following again\n".repeat(8),
                ..Default::default()
            }]),
        );
        cx.notify();
    });
    cx.run_until_parked();
    let following_tail = selector(&shell, cx, "chat-item", 81);
    assert!(cx.debug_bounds(following_tail).is_some());
}

#[gpui::test]
fn chat_disclosure_survives_unmount_and_resets_with_transcript_binding(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(
        cx,
        "chat-disclosure",
        vec![ChatItem {
            kind: ChatItemKind::Reasoning,
            text: "short summary\nprivate detail".into(),
            ..Default::default()
        }],
    );
    let body = "chat-message-body";
    assert!(cx.debug_bounds(body).is_none());
    let composer = cx.debug_bounds("desktop-chat-composer").unwrap();
    let disclosure = cx.debug_bounds("chat-disclosure").unwrap();
    cx.simulate_click(disclosure.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds(body).is_some());
    assert_eq!(cx.debug_bounds("desktop-chat-composer").unwrap(), composer);
    let term = cx.debug_bounds("chat-switch-terminal").unwrap();
    cx.simulate_click(term.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_none());
    shell.update(cx, Shell::toggle_chat_view);
    cx.run_until_parked();
    assert!(
        cx.debug_bounds(body).is_some(),
        "disclosure must survive view unmount"
    );
    shell.update(cx, |shell, cx| {
        let mut changed = snapshot(vec![ChatItem {
            kind: ChatItemKind::ToolResult,
            text: "different transcript".into(),
            ..Default::default()
        }]);
        changed.session_id = "new-binding".into();
        shell.apply_chat_poll(SESSION, changed);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds(body).is_none(),
        "a new binding must start folded"
    );
}

#[gpui::test]
fn chat_narrow_code_and_composer_actions_stay_inside_the_pane(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(
        cx,
        "chat-narrow",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            title: "long title ".repeat(10),
            text: format!(
                "설명\n```rust\nlet long_value = \"{}\";\n```",
                "long_token_".repeat(25)
            ),
            file_path: "/a/very/long/path/to/a-source-file.rs".into(),
            ..Default::default()
        }],
    );
    cx.simulate_resize(size(px(320.), px(680.)));
    cx.run_until_parked();
    let overlay = cx.debug_bounds("desktop-chat-overlay").unwrap();
    let code = cx.debug_bounds("chat-code-content").unwrap();
    assert!(code.size.width > px(100.));
    assert!(code.left() >= overlay.left() && code.right() <= overlay.right());
    for target in [
        "desktop-chat-send",
        "chat-switch-terminal",
        "chat-copy-code",
        "chat-copy-item",
    ] {
        let bounds = cx.debug_bounds(target).unwrap();
        assert!(
            bounds.left() >= overlay.left() && bounds.right() <= overlay.right(),
            "{target}: {bounds:?} outside {overlay:?}"
        );
    }
    let copy = cx.debug_bounds("chat-copy-code").unwrap();
    cx.simulate_click(copy.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().unwrap(),
        format!("let long_value = \"{}\";", "long_token_".repeat(25))
    );
    let user = ChatItem {
        kind: ChatItemKind::User,
        text: "사용자 메시지".into(),
        ..Default::default()
    };
    shell.update(cx, |shell, cx| {
        shell.apply_chat_poll(SESSION, snapshot(vec![user]));
        cx.notify();
    });
    cx.run_until_parked();
    let user_bounds = cx
        .debug_bounds(selector(&shell, cx, "chat-item", 1))
        .unwrap();
    assert!(user_bounds.size.width < overlay.size.width);
    assert!(user_bounds.right() <= overlay.right());
}

#[gpui::test]
fn chat_empty_state_send_is_disabled_and_drafts_survive_term_switch(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(cx, "chat-empty-draft", vec![]);
    assert!(cx.debug_bounds("chat-empty").is_some());
    let send = cx.debug_bounds("desktop-chat-send").unwrap();
    cx.simulate_click(send.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-prompt-error").is_none());
    let input = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value("draft\n한글", window, cx));
            input
        })
    });
    let term = cx.debug_bounds("chat-switch-terminal").unwrap();
    cx.simulate_click(term.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.update(cx, Shell::toggle_chat_view);
    cx.run_until_parked();
    assert_eq!(input.read_with(cx, |state, _| state.value()), "draft\n한글");
    cx.update(|window, cx| {
        assert!(input.focus_handle(cx).is_focused(window));
    });
    shell.update(cx, Shell::toggle_chat_view);
    cx.run_until_parked();
    cx.update(|window, cx| {
        assert!(shell.read(cx).focus.is_focused(window));
    });
}

#[gpui::test]
fn chat_wide_reading_column_matches_composer_and_stays_centered(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(
        cx,
        "chat-reading-column",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "## Readable answer\n\n- first step\n- **second step**".into(),
            ..Default::default()
        }],
    );
    cx.simulate_resize(size(px(1600.), px(900.)));
    cx.run_until_parked();
    let message = cx
        .debug_bounds(selector(&shell, cx, "chat-item", 0))
        .unwrap();
    let composer = cx.debug_bounds("desktop-chat-composer").unwrap();
    let overlay = cx.debug_bounds("desktop-chat-overlay").unwrap();
    assert!(message.size.width < overlay.size.width / 2.);
    assert_eq!(message.size.width, composer.size.width);
    assert_eq!(message.left(), composer.left());
    assert!((message.center().x - overlay.center().x).abs() <= px(1.));
}

#[gpui::test]
fn chat_narrow_question_options_wrap_descriptions_below_left_aligned_titles(
    cx: &mut TestAppContext,
) {
    let (_shell, cx) = fixture(cx, "chat-question-wrap", vec![ChatItem {
        kind: ChatItemKind::Question, text: "선택해 주세요".into(),
        options: vec![krill_proto::agent_history::ChatQuestionOption {
            label: "긴 제목과 선택지 이름".into(),
            description: "화면이 좁아도 이 설명은 제목 아래에 여러 줄로 표시되어 내용을 끝까지 읽을 수 있어야 합니다.".into(),
        }], ..Default::default()
    }]);
    cx.simulate_resize(size(px(320.), px(680.)));
    cx.run_until_parked();
    let pane = cx.debug_bounds("desktop-chat-overlay").unwrap();
    let option = cx.debug_bounds("chat-question-option").unwrap();
    let description = cx.debug_bounds("chat-option-description").unwrap();
    assert!(option.left() >= pane.left() && option.right() <= pane.right());
    assert!(
        description.size.height > px(20.),
        "description should wrap: {description:?}"
    );
    assert!(description.left() > option.left() && description.right() < option.right());
    assert!(option.size.height >= description.size.height + px(20.));
}

#[gpui::test]
fn chat_reader_anchor_survives_term_and_tab_remount_until_explicit_latest(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(
        cx,
        "chat-reader-remount",
        (0..100)
            .map(|index| ChatItem {
                kind: ChatItemKind::Assistant,
                text: format!("message {index}\n\n{}", "Readable message body. ".repeat(8)),
                ..Default::default()
            })
            .collect(),
    );
    cx.simulate_resize(size(px(640.), px(650.)));
    cx.run_until_parked();
    for _ in 0..4 {
        scroll(cx, 900.);
    }
    let feed = cx.debug_bounds("desktop-chat-feed").unwrap();
    let (anchor, bounds) = (1..99)
        .find_map(|index| {
            let selector = selector(&shell, cx, "chat-item", index);
            let bounds = cx.debug_bounds(selector)?;
            (bounds.top() >= feed.top() + px(12.) && bounds.bottom() <= feed.bottom() - px(12.))
                .then_some((selector, bounds))
        })
        .expect("reader must have a complete middle message in the actual viewport");
    assert!(cx.debug_bounds("chat-jump-latest").is_some());

    let terminal = cx.debug_bounds("chat-switch-terminal").unwrap();
    cx.simulate_click(terminal.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_none());
    // Render an extra frame without Chat, so no frame-local keyed state can
    // accidentally satisfy the remount persistence contract.
    shell.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    shell.update(cx, Shell::toggle_chat_view);
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds(anchor),
        Some(bounds),
        "Term roundtrip must preserve the visible reader anchor exactly"
    );
    assert!(
        cx.debug_bounds("chat-jump-latest").is_some(),
        "Term roundtrip must preserve detached follow intent"
    );

    shell.update(cx, |shell, cx| {
        shell.layout.new_tab();
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SessionId(772));
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_none());
    shell.update(cx, |shell, cx| {
        shell.layout.activate(0);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds(anchor),
        Some(bounds),
        "tab remount must preserve the same viewport anchor"
    );
    assert!(cx.debug_bounds("chat-jump-latest").is_some());

    shell.update(cx, |shell, cx| {
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![ChatItem {
                kind: ChatItemKind::Assistant,
                text: "appended while reading".into(),
                ..Default::default()
            }]),
        );
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        cx.debug_bounds(anchor),
        Some(bounds),
        "append after remount must preserve exact reader bounds"
    );
    assert!(cx.debug_bounds("chat-jump-latest").is_some());
    let jump = cx.debug_bounds("chat-jump-latest").unwrap();
    cx.simulate_click(jump.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-jump-latest").is_none());
    let last = selector(&shell, cx, "chat-item", 100);
    let last_bounds = cx
        .debug_bounds(last)
        .expect("latest output should be rendered after explicit jump");
    assert!(last_bounds.top() < feed.bottom() && last_bounds.bottom() > feed.top());
    shell.update(cx, |shell, cx| {
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![ChatItem {
                kind: ChatItemKind::Assistant,
                text: "follow after explicit latest".into(),
                ..Default::default()
            }]),
        );
        cx.notify();
    });
    cx.run_until_parked();
    let next = selector(&shell, cx, "chat-item", 101);
    let next_bounds = cx
        .debug_bounds(next)
        .expect("following should resume after explicit jump");
    assert!(next_bounds.top() < feed.bottom() && next_bounds.bottom() > feed.top());
    assert!(cx.debug_bounds("chat-jump-latest").is_none());

    // Duplicate views of one session own independent scroll engines. Hidden
    // tabs remain live, whereas closed/reassigned panes release their cache.
    let first_pane = shell.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    let second_pane = shell.update(cx, |shell, cx| {
        shell.layout.split(krill_app::layout::Axis::Horizontal);
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SESSION);
        shell.layout.set_chat_view(pane, true);
        cx.notify();
        pane
    });
    cx.run_until_parked();
    shell.update(cx, |shell, cx| {
        assert_eq!(shell.chat.lists.len(), 2);
        let first = &shell.chat.lists[&(first_pane, SESSION)].state;
        first.set_follow_mode(gpui::FollowMode::Normal);
        assert!(!first.is_following_tail());
        assert!(
            shell.chat.lists[&(second_pane, SESSION)]
                .state
                .is_following_tail(),
            "same-session panes must not share their scroll/follow engine"
        );
        shell.layout.activate(1);
        cx.notify();
    });
    cx.run_until_parked();
    shell.read_with(cx, |shell, _| {
        assert_eq!(
            shell.chat.lists.len(),
            2,
            "hidden tabs must retain both pane caches"
        );
        assert!(shell.chat.lists.contains_key(&(first_pane, SESSION)));
        assert!(shell.chat.lists.contains_key(&(second_pane, SESSION)));
    });
    shell.update(cx, |shell, cx| {
        shell.layout.close_tab(0);
        cx.notify();
    });
    cx.run_until_parked();
    shell.read_with(cx, |shell, _| {
        assert!(shell.chat.lists.is_empty(), "closed panes must be pruned");
    });

    let replacement = shell.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SESSION);
        shell.layout.set_chat_view(pane, true);
        cx.notify();
        pane
    });
    cx.run_until_parked();
    shell.update(cx, |shell, cx| {
        shell.chat.lists[&(replacement, SESSION)]
            .state
            .set_follow_mode(gpui::FollowMode::Normal);
        shell.layout.set_host(
            replacement,
            krill_app::hosts::HostId::Remote("replacement".into()),
        );
        cx.notify();
    });
    cx.run_until_parked();
    shell.update(cx, |shell, cx| {
        assert!(
            !shell.chat.lists.contains_key(&(replacement, SESSION)),
            "the previous host's list and log do not carry over"
        );
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![ChatItem {
                kind: ChatItemKind::Assistant,
                text: "replacement host".into(),
                ..Default::default()
            }]),
        );
        cx.notify();
    });
    cx.run_until_parked();
    shell.update(cx, |shell, cx| {
        let list = &shell.chat.lists[&(replacement, SESSION)];
        assert_eq!(
            list.host,
            krill_app::hosts::HostId::Remote("replacement".into())
        );
        assert!(
            list.state.is_following_tail(),
            "a new host must start at Tail"
        );
        list.state.set_follow_mode(gpui::FollowMode::Normal);
        let mut reset = snapshot(vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "replacement binding".into(),
            ..Default::default()
        }]);
        reset.reset = true;
        reset.session_id = "replacement-binding".into();
        shell.apply_chat_poll(SESSION, reset);
        cx.notify();
    });
    cx.run_until_parked();
    shell.update(cx, |shell, cx| {
        assert!(
            shell.chat.lists[&(replacement, SESSION)]
                .state
                .is_following_tail(),
            "binding/epoch reset must start at Tail"
        );
        shell.layout.set_session(replacement, SessionId(773));
        cx.notify();
    });
    cx.run_until_parked();
    shell.read_with(cx, |shell, _| {
        assert!(
            shell.chat.lists.is_empty(),
            "reassigned panes must be pruned"
        );
    });
}

/// 세션 ID는 데몬마다 따로 세므로 로컬과 원격의 같은 번호가 로그, 초안, 전송 칸을 공유하면 안 된다.
#[gpui::test]
fn same_session_number_on_two_hosts_keeps_logs_drafts_and_target_apart(cx: &mut TestAppContext) {
    let (shell, cx) = open(cx, state_path("chat-two-hosts"));
    let remote_host = krill_app::hosts::HostId::Remote("windows-pc".into());
    let remote_key = (remote_host.clone(), SESSION);
    let item = |text: &str| ChatItem {
        kind: ChatItemKind::Assistant,
        text: text.into(),
        ..Default::default()
    };
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let local = shell.layout.focused().unwrap();
            shell.layout.set_session(local, SESSION);
            shell.layout.split(krill_app::layout::Axis::Horizontal);
            let remote = shell.layout.focused().unwrap();
            shell.layout.set_host(remote, remote_host.clone());
            shell.layout.set_session(remote, SESSION);

            let local_host = krill_app::hosts::HostId::Local;
            shell.apply_chat_poll_for_host(
                SESSION,
                local_host,
                snapshot(vec![item("local")]),
                None,
            );
            shell.apply_chat_poll_for_host(
                SESSION,
                remote_host,
                snapshot(vec![item("remote")]),
                None,
            );
            let local_log = &shell.chat.logs[&local_key(SESSION)];
            assert_eq!(local_log.items.len(), 1);
            assert_eq!(local_log.items[0].text, "local");
            assert_eq!(shell.chat.logs[&remote_key].items[0].text, "remote");

            let local_input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            let remote_input = shell.ensure_chat_input(remote_key.clone(), window, cx);
            assert_ne!(local_input.entity_id(), remote_input.entity_id());

            shell.layout.focus(local);
            assert_eq!(shell.chat_pane(&remote_key), Some(remote));
            assert_eq!(shell.chat_pane(&local_key(SESSION)), Some(local));
        });
    });
}

#[gpui::test]
fn chat_orca_composer_controls_and_quick_menu_work_and_fit_narrow_pane(cx: &mut TestAppContext) {
    let items = vec![
        ChatItem {
            kind: ChatItemKind::User,
            text: "Inspect composer".into(),
            ..Default::default()
        },
        ChatItem {
            kind: ChatItemKind::ToolCall,
            tool_name: "Read".into(),
            title: "Read composer.rs".into(),
            text: "crates/krill-app/src/shell/chat/composer.rs".into(),
            ..Default::default()
        },
        ChatItem {
            kind: ChatItemKind::Assistant,
            text: "Composer is ready.".into(),
            ..Default::default()
        },
    ];
    let (shell, cx) = fixture(cx, "chat-orca-composer", items);
    cx.simulate_resize(size(px(320.), px(680.)));
    let workflow_requests: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
        std::sync::Arc::default();
    let recorded = std::sync::Arc::clone(&workflow_requests);
    shell.update(cx, |shell, cx| {
        shell.chat.workflow_call = Some(std::sync::Arc::new(move |raw| {
            let req: serde_json::Value = serde_json::from_str(raw).unwrap();
            recorded.lock().unwrap().push(req.clone());
            let action = req.get("action").and_then(serde_json::Value::as_str).unwrap_or("");
            let resp = match action {
                "chat_composer" => serde_json::json!({
                    "ok": true,
                    "data": {
                        "agent": "claude",
                        "model": "claude-sonnet-4-6",
                        "model_label": "Sonnet 4.6",
                        "effort": "high",
                        "context": { "used": 84_000, "limit": 200_000, "percent": 42.0 },
                        "supported": true,
                        "effort_supported": true,
                        "efforts": ["low", "medium", "high", "xhigh", "max"],
                        "models": [
                            { "id": "default", "label": "Default (recommended)", "efforts": ["low", "medium", "high", "xhigh", "max"] },
                            { "id": "opus", "label": "Opus", "efforts": ["low", "medium", "high", "xhigh", "max"] },
                            { "id": "sonnet", "label": "Sonnet", "efforts": ["low", "medium", "high", "xhigh", "max"] }
                        ]
                    }
                }),
                "composer_models" => serde_json::json!({
                    "ok": true,
                    "data": {
                        "agent": "claude",
                        "supported": true,
                        "current": "sonnet",
                        "items": [
                            { "id": "default", "label": "Default (recommended)", "efforts": ["low", "medium", "high", "xhigh", "max"] },
                            { "id": "opus", "label": "Opus", "efforts": ["low", "medium", "high", "xhigh", "max"] },
                            { "id": "sonnet", "label": "Sonnet", "efforts": ["low", "medium", "high", "xhigh", "max"] }
                        ]
                    }
                }),
                "set_model" => {
                    let model = req.get("model").and_then(serde_json::Value::as_str).unwrap_or("opus");
                    serde_json::json!({
                        "ok": true,
                        "data": {
                            "applied": true,
                            "supported": true,
                            "model": model,
                            "label": "Opus 4.7"
                        }
                    })
                }
                "set_effort" => {
                    let effort = req.get("effort").and_then(serde_json::Value::as_str).unwrap_or("max");
                    serde_json::json!({
                        "ok": true,
                        "data": {
                            "applied": true,
                            "supported": true,
                            "effort": effort
                        }
                    })
                }
                _ => serde_json::json!({ "ok": false, "error": "unknown" }),
            };
            Ok(resp.to_string())
        }));
        shell.refresh_chat_composer(&local_key(SESSION), false, cx);
        cx.notify();
    });
    cx.run_until_parked();

    let overlay = cx.debug_bounds("desktop-chat-overlay").unwrap();
    for selector in [
        "desktop-chat-composer",
        "chat-composer-plus",
        "chat-composer-model",
        "chat-composer-context",
        "desktop-chat-send",
    ] {
        let target = cx.debug_bounds(selector).expect(selector);
        assert!(
            target.left() >= overlay.left() && target.right() <= overlay.right(),
            "{selector} overflowed pane horizontally: {target:?} outside {overlay:?}"
        );
    }

    assert!(cx.debug_bounds("chat-quick-model").is_none());
    let plus = cx.debug_bounds("chat-composer-plus").unwrap();
    cx.simulate_click(plus.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-quick-model").is_some());
    let compact = cx.debug_bounds("chat-quick-compact").unwrap();
    cx.simulate_click(compact.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-quick-model").is_none());
    shell.read_with(cx, |shell, cx| {
        let (input, _) = &shell.chat.inputs[&local_key(SESSION)];
        assert_eq!(input.read(cx).value().as_ref(), "/compact");
    });

    assert!(cx.debug_bounds("chat-model-picker").is_none());
    let model_btn = cx.debug_bounds("chat-composer-model").unwrap();
    cx.simulate_click(model_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let picker = cx.debug_bounds("chat-model-picker").unwrap();
    assert!(
        picker.left() >= overlay.left() && picker.right() <= overlay.right(),
        "chat-model-picker overflowed pane horizontally: {picker:?} outside {overlay:?}"
    );
    let opus_opt = cx.debug_bounds("chat-model-option-opus").unwrap();
    cx.simulate_click(opus_opt.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-model-picker").is_none());
    shell.read_with(cx, |shell, _| {
        assert_eq!(
            shell.chat.logs[&local_key(SESSION)]
                .meta
                .current_model_id
                .as_deref(),
            Some("opus")
        );
    });

    let model_btn = cx.debug_bounds("chat-composer-model").unwrap();
    cx.simulate_click(model_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let max_opt = cx.debug_bounds("chat-effort-option-max").unwrap();
    cx.simulate_click(max_opt.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-model-picker").is_none());
    let actions: Vec<String> = workflow_requests
        .lock()
        .unwrap()
        .iter()
        .filter_map(|v| {
            v.get("action")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert!(actions.contains(&"chat_composer".to_owned()));
    assert!(actions.contains(&"composer_models".to_owned()));
    assert!(actions.contains(&"set_model".to_owned()));
    assert!(actions.contains(&"set_effort".to_owned()));

    let copy_btn = cx.debug_bounds("chat-copy-item").unwrap();
    cx.simulate_click(copy_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |shell, _| {
        assert!(shell.chat.copied_item.is_some());
    });
}

#[test]
fn composer_meta_observation_extracts_codex_and_claude_records() {
    let mut codex_meta = ComposerMeta::default();
    let turn_ctx = serde_json::json!({
        "type": "turn_context",
        "payload": {
            "model": "gpt-5.4",
            "reasoning_effort": "high"
        }
    });
    let token_cnt = serde_json::json!({
        "payload": {
            "info": {
                "last_token_usage": { "total_tokens": 64_000 },
                "model_context_window": 128_000
            }
        }
    });
    parse_transcript_record(&turn_ctx, &mut codex_meta);
    parse_transcript_record(&token_cnt, &mut codex_meta);
    assert_eq!(codex_meta.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(codex_meta.effort.as_deref(), Some("high"));
    assert_eq!(codex_meta.context_percent, Some(50));

    let mut claude_meta = ComposerMeta::default();
    let claude_msg = serde_json::json!({
        "message": {
            "model": "claude-opus-4-7",
            "usage": {
                "input_tokens": 30_000,
                "output_tokens": 10_000,
                "cache_read_input_tokens": 20_000
            }
        }
    });
    parse_transcript_record(&claude_msg, &mut claude_meta);
    assert_eq!(claude_meta.model.as_deref(), Some("claude-opus-4-7"));
    assert_eq!(
        claude_meta.context_percent, None,
        "the context window is unknown here; a 1M model must not read as full"
    );
}

#[gpui::test]
fn chat_work_run_collapses_by_default_expands_steps_without_moving_composer_and_survives_remount(
    cx: &mut TestAppContext,
) {
    let (shell, cx) = fixture(
        cx,
        "chat-work-run-disclosure",
        vec![
            ChatItem {
                kind: ChatItemKind::Reasoning,
                text: "Running verification\nChecking warnings".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::ToolCall,
                tool_id: "call-bash".into(),
                tool_name: "Bash".into(),
                title: "Run cargo check".into(),
                text: "cargo check -p krill-app --locked".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::ToolResult,
                tool_id: "call-bash".into(),
                text: "Finished check".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::Reasoning,
                text: "Inspecting crate root\nChecking exports".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::ToolCall,
                tool_id: "call-read".into(),
                tool_name: "Read".into(),
                file_path: "crates/krill-app/src/lib.rs".into(),
                text: "crates/krill-app/src/lib.rs".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::ToolResult,
                tool_id: "call-read".into(),
                text: "pub mod ui;\n".into(),
                ..Default::default()
            },
        ],
    );

    let run_row = selector(&shell, cx, "chat-item", 0);
    assert!(cx.debug_bounds(run_row).is_some());
    assert!(
        cx.debug_bounds(selector(&shell, cx, "chat-item", 1))
            .is_none()
    );
    assert!(cx.debug_bounds("chat-work-run-toggle").is_some());
    assert!(cx.debug_bounds("chat-disclosure").is_none());
    assert!(cx.debug_bounds("chat-message-body").is_none());

    let composer = cx.debug_bounds("desktop-chat-composer").unwrap();
    let toggle = cx.debug_bounds("chat-work-run-toggle").unwrap();
    cx.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert_eq!(cx.debug_bounds("desktop-chat-composer").unwrap(), composer);
    assert!(cx.debug_bounds("chat-disclosure").is_some());
    assert!(cx.debug_bounds("chat-message-body").is_none());

    let step_toggle = cx.debug_bounds("chat-disclosure").unwrap();
    cx.simulate_click(step_toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-message-body").is_some());
    assert_eq!(cx.debug_bounds("desktop-chat-composer").unwrap(), composer);

    let copy_btn = cx.debug_bounds("chat-copy-item").unwrap();
    cx.simulate_click(copy_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("pub mod ui;\n")
    );
    assert!(
        cx.debug_bounds("chat-message-body").is_some(),
        "copy button inside step header must not collapse the step"
    );

    let term = cx.debug_bounds("chat-switch-terminal").unwrap();
    cx.simulate_click(term.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_none());

    shell.update(cx, Shell::toggle_chat_view);
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-disclosure").is_some());
    assert!(cx.debug_bounds("chat-message-body").is_some());
}

#[gpui::test]
fn chat_solo_reasoning_promotes_cleanly_and_requests_keep_raw_abs_idx_after_work_run(
    cx: &mut TestAppContext,
) {
    struct Memory(krill_app::sidebar::Snapshot);
    impl krill_app::sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<krill_app::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }

    let (shell, cx) = fixture(
        cx,
        "chat-solo-promote-and-requests",
        vec![ChatItem {
            kind: ChatItemKind::Reasoning,
            text: "Planning safe rollout\nChecking preconditions".into(),
            ..Default::default()
        }],
    );

    assert!(cx.debug_bounds("chat-work-run-toggle").is_none());
    assert!(cx.debug_bounds("chat-message-body").is_none());
    let solo_disclosure = cx.debug_bounds("chat-disclosure").unwrap();
    cx.simulate_click(solo_disclosure.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-message-body").is_some());

    shell.update(cx, |shell, cx| {
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![
                ChatItem {
                    kind: ChatItemKind::ToolCall,
                    tool_id: "call-1".into(),
                    tool_name: "Read".into(),
                    file_path: "deploy.sh".into(),
                    ..Default::default()
                },
                ChatItem {
                    kind: ChatItemKind::ToolResult,
                    tool_id: "call-1".into(),
                    text: "#!/bin/sh\n".into(),
                    ..Default::default()
                },
            ]),
        );
        cx.notify();
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("chat-work-run-toggle").is_some());
    assert!(cx.debug_bounds("chat-message-body").is_none());
    let run_toggle = cx.debug_bounds("chat-work-run-toggle").unwrap();
    cx.simulate_click(run_toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("chat-message-body").is_some(),
        "step expanded while solo Reasoning must stay expanded when promoted into WorkRun"
    );

    let sent: Arc<std::sync::Mutex<Vec<Vec<u8>>>> = Arc::default();
    let captured = Arc::clone(&sent);
    shell.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
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
        shell.chat.send_input = Some(Box::new(move |bytes| {
            captured.lock().unwrap().push(bytes);
            true
        }));
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![
                ChatItem {
                    kind: ChatItemKind::Question,
                    tool_id: "q-1".into(),
                    title: "Target".into(),
                    text: "Select environment".into(),
                    options: vec![krill_proto::agent_history::ChatQuestionOption {
                        label: "Staging".into(),
                        description: String::new(),
                    }],
                    ..Default::default()
                },
                ChatItem {
                    kind: ChatItemKind::Permission,
                    tool_id: "perm-1".into(),
                    text: "deploy production".into(),
                    ..Default::default()
                },
            ]),
        );
        let ops = vec![
            krill_proto::screen::Op::Reset {
                cols: 100,
                lines: 6,
            },
            krill_proto::screen::Op::Line {
                line: 0,
                cells: "Allow deploy production? (y/n)"
                    .chars()
                    .map(|ch| Cell {
                        ch,
                        ..Default::default()
                    })
                    .collect(),
            },
        ];
        for frame in krill_proto::screen::encode_update(
            krill_proto::screen::Revision(1),
            krill_proto::screen::Seq(0),
            ops,
        )
        .unwrap()
        {
            let (kind, body) = krill_proto::ipc::read_frame(&mut frame.as_slice())
                .unwrap()
                .unwrap();
            shell.on_frame(pane, kind, &body, cx);
        }
        cx.notify();
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds(selector(&shell, cx, "chat-item", 3))
            .is_some()
    );
    assert!(
        cx.debug_bounds(selector(&shell, cx, "chat-item", 4))
            .is_some()
    );

    let approve = cx
        .debug_bounds("chat-permission-approve")
        .expect("permission approve button must render outside WorkRun");
    cx.simulate_click(approve.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(*sent.lock().unwrap(), vec![b"y\r".to_vec()]);
}

#[gpui::test]
fn chat_narrow_320px_work_run_stays_inside_pane_collapsed_and_expanded(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(
        cx,
        "chat-work-run-narrow-320",
        vec![
            ChatItem {
                kind: ChatItemKind::ToolCall,
                tool_id: "c1".into(),
                tool_name: "Edit".into(),
                title: "Update deeply nested layout component with long description".into(),
                file_path: "/very/long/workspace/path/crates/krill-app/src/shell/chat/view.rs"
                    .into(),
                text: "diff --git a/view.rs b/view.rs".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::ToolResult,
                tool_id: "c1".into(),
                is_error: true,
                text: format!(
                    "error summary\n```rust\nlet wide = \"{}\";\n```",
                    "very_long_identifier_segment_".repeat(16)
                ),
                ..Default::default()
            },
        ],
    );
    cx.simulate_resize(size(px(320.), px(680.)));
    cx.run_until_parked();

    let overlay = cx.debug_bounds("desktop-chat-overlay").unwrap();
    let run_card = cx
        .debug_bounds(selector(&shell, cx, "chat-item", 0))
        .unwrap();
    let run_toggle = cx.debug_bounds("chat-work-run-toggle").unwrap();
    assert!(run_card.left() >= overlay.left() && run_card.right() <= overlay.right());
    assert!(run_toggle.left() >= overlay.left() && run_toggle.right() <= overlay.right());
    assert!(
        run_toggle.size.height < px(56.),
        "collapsed 320px header must truncate on one line instead of vertical-wrapping: {run_toggle:?}"
    );

    cx.simulate_click(run_toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    let step_disclosure = cx.debug_bounds("chat-disclosure").unwrap();
    let open_file = cx.debug_bounds("chat-open-file").unwrap();
    let copy_item = cx.debug_bounds("chat-copy-item").unwrap();
    for (name, bounds) in [
        ("step_disclosure", step_disclosure),
        ("open_file", open_file),
        ("copy_item", copy_item),
    ] {
        assert!(
            bounds.left() >= overlay.left() && bounds.right() <= overlay.right(),
            "{name} overflowed 320px pane: {bounds:?} outside {overlay:?}"
        );
    }

    cx.simulate_click(step_disclosure.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    let expanded_card = cx
        .debug_bounds(selector(&shell, cx, "chat-item", 0))
        .unwrap();
    let code = cx.debug_bounds("chat-code-content").unwrap();
    let composer = cx.debug_bounds("desktop-chat-composer").unwrap();
    for (name, bounds) in [
        ("expanded_card", expanded_card),
        ("code", code),
        ("composer", composer),
    ] {
        assert!(
            bounds.left() >= overlay.left() && bounds.right() <= overlay.right(),
            "{name} overflowed 320px pane when expanded: {bounds:?} outside {overlay:?}"
        );
    }
}

#[gpui::test]
fn chat_composer_popovers_close_on_second_trigger_click_and_escape_and_expose_plus_a11y(
    cx: &mut TestAppContext,
) {
    use gpui::EntityInputHandler as _;

    let (shell, cx) = fixture(
        cx,
        "chat-composer-popover-toggle",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "Ready.".into(),
            ..Default::default()
        }],
    );
    cx.simulate_resize(size(px(640.), px(680.)));
    cx.run_until_parked();

    let plus = cx.debug_bounds("chat-composer-plus").unwrap();
    cx.simulate_mouse_move(plus.center(), None, gpui::Modifiers::default());
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("tooltip:chat-composer-plus").is_some(),
        "closed quick-commands button must show hover tooltip"
    );

    // Open quick menu via +, confirm tooltip hides while open, and confirm 2nd click closes it.
    cx.simulate_click(plus.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-quick-model").is_some());
    cx.simulate_mouse_move(plus.center(), None, gpui::Modifiers::default());
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("tooltip:chat-composer-plus").is_none(),
        "open quick menu must suppress trigger tooltip"
    );

    let plus = cx.debug_bounds("chat-composer-plus").unwrap();
    cx.simulate_click(plus.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("chat-quick-model").is_none(),
        "clicking + while quick menu is open must close it without reopening"
    );

    // Reopen quick menu: IME Escape keeps it open, unmarked Escape closes it.
    let plus = cx.debug_bounds("chat-composer-plus").unwrap();
    cx.simulate_click(plus.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-quick-model").is_some());
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let (input, _) = &shell.chat.inputs[&local_key(SESSION)];
            input.update(cx, |state, cx| {
                state.focus(window, cx);
                state.replace_and_mark_text_in_range(None, "ㅎ", None, window, cx);
            });
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("chat-quick-model").is_some(),
        "IME Escape cancellation must not dismiss quick menu"
    );
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let (input, _) = &shell.chat.inputs[&local_key(SESSION)];
            input.update(cx, |state, cx| state.unmark_text(window, cx));
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-quick-model").is_none());

    // Model picker: 1st click opens, 2nd click on trigger closes, Escape closes.
    let model_btn = cx.debug_bounds("chat-composer-model").unwrap();
    cx.simulate_click(model_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-model-picker").is_some());

    let model_btn = cx.debug_bounds("chat-composer-model").unwrap();
    cx.simulate_click(model_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("chat-model-picker").is_none(),
        "clicking model button while picker is open must close it without reopening"
    );

    let model_btn = cx.debug_bounds("chat-composer-model").unwrap();
    cx.simulate_click(model_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-model-picker").is_some());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-model-picker").is_none());
}
