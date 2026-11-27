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
        shell.layout.split(krill_app_core::layout::Axis::Horizontal);
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
            krill_app_core::hosts::HostId::Remote("replacement".into()),
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
            krill_app_core::hosts::HostId::Remote("replacement".into())
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
    let remote_host = krill_app_core::hosts::HostId::Remote("windows-pc".into());
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
            shell.layout.split(krill_app_core::layout::Axis::Horizontal);
            let remote = shell.layout.focused().unwrap();
            shell.layout.set_host(remote, remote_host.clone());
            shell.layout.set_session(remote, SESSION);

            let local_host = krill_app_core::hosts::HostId::Local;
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
    struct Memory(krill_app_core::sidebar::Snapshot);
    impl krill_app_core::sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<krill_app_core::sidebar::Snapshot> {
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
        shell.live.get_mut(&pane).unwrap().target = Some(krill_app_core::hosts::HostTarget::local(
            krill_proto::daemon::Paths {
                socket: "/fake/socket".into(),
                holder: "/fake/holder".into(),
                krilld: "/fake/krilld".into(),
            },
        ));
        shell.paths = None;
        shell.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(Memory(krill_app_core::sidebar::Snapshot {
                    sessions: vec![krill_app_core::sidebar::Session {
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

#[gpui::test]
fn chat_code_block_copy_feedback_conditional_header_and_line_wrap(cx: &mut TestAppContext) {
    let code_body = format!("let value = \"{}\";", "segment_token_".repeat(12));
    let (shell, cx) = fixture(
        cx,
        "chat-code-header-wrap",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: format!("Here:\n```rs\n{code_body}\n```"),
            ..Default::default()
        }],
    );
    cx.simulate_resize(size(px(320.), px(680.)));
    cx.run_until_parked();

    let overlay = cx.debug_bounds("desktop-chat-overlay").unwrap();
    let header = cx.debug_bounds("chat-code-header").unwrap();
    assert!(header.size.height <= px(34.));
    assert!(header.left() >= overlay.left() && header.right() <= overlay.right());

    let copy_before = cx.debug_bounds("chat-copy-code").unwrap();
    let wrap_btn = cx.debug_bounds("chat-wrap-code").unwrap();
    assert!(wrap_btn.left() >= overlay.left() && wrap_btn.right() <= overlay.right());

    cx.simulate_click(copy_before.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(shell.read_with(cx, |s, _| s.chat.copied_code.is_some()));
    assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), code_body);
    let copy_during = cx.debug_bounds("chat-copy-code").unwrap();
    assert_eq!(
        copy_during.size.width, copy_before.size.width,
        "reserved copy/copied width must prevent layout shift"
    );

    cx.executor().advance_clock(Duration::from_millis(1600));
    cx.run_until_parked();
    assert!(
        shell.read_with(cx, |s, _| s.chat.copied_code.is_none()),
        "1.5s timer must reset Copied feedback"
    );

    let wrap_btn = cx.debug_bounds("chat-wrap-code").unwrap();
    cx.simulate_click(wrap_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(shell.read_with(cx, |s, _| {
        s.chat.logs[&local_key(SESSION)].is_code_wrapped(0, 0)
    }));
    let content = cx.debug_bounds("chat-code-content").unwrap();
    assert!(content.left() >= overlay.left() && content.right() <= overlay.right());

    // Language-less fence omits header bar and keeps corner actions inside bounds.
    shell.update(cx, |shell, cx| {
        let mut next = snapshot(vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: format!("```\n{code_body}\n```"),
            ..Default::default()
        }]);
        next.reset = true;
        shell.apply_chat_poll(SESSION, next);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("chat-code-header").is_none(),
        "unfenced/no-lang block must omit top header bar"
    );
    assert!(
        shell.read_with(cx, |s, _| {
            !s.chat.logs[&local_key(SESSION)].is_code_wrapped(0, 0)
        }),
        "wrap state must clear on transcript reset"
    );
    for target in ["chat-wrap-code", "chat-copy-code", "chat-code-content"] {
        let b = cx.debug_bounds(target).unwrap();
        assert!(b.left() >= overlay.left() && b.right() <= overlay.right());
    }
}

#[gpui::test]
fn chat_user_bubble_intrinsic_width_long_fold_and_assistant_footer(cx: &mut TestAppContext) {
    let short_user = ChatItem {
        kind: ChatItemKind::User,
        text: "안녕".into(),
        ..Default::default()
    };
    let (shell, cx) = fixture(cx, "chat-user-bubble", vec![short_user]);
    cx.simulate_resize(size(px(800.), px(680.)));
    cx.run_until_parked();

    let composer = cx.debug_bounds("desktop-chat-composer").unwrap();
    let bubble = cx
        .debug_bounds(selector(&shell, cx, "chat-item", 0))
        .unwrap();
    assert!(
        bubble.size.width < composer.size.width * 0.5,
        "short user bubble must size to content rather than reading column: bubble={bubble:?}, composer={composer:?}"
    );
    assert!(
        bubble.right() >= composer.right() - px(4.),
        "short user bubble must right-align inside reading column: bubble={bubble:?}, composer={composer:?}"
    );

    // Long prompt folds to <= 176px height + toggle, and expands on click.
    let long_prompt = (1..=14)
        .map(|i| format!("Prompt detail line {i} explaining requirements clearly"))
        .collect::<Vec<_>>()
        .join("\n");
    shell.update(cx, |shell, cx| {
        let mut next = snapshot(vec![ChatItem {
            kind: ChatItemKind::User,
            text: long_prompt,
            ..Default::default()
        }]);
        next.reset = true;
        shell.apply_chat_poll(SESSION, next);
        cx.notify();
    });
    cx.run_until_parked();

    let folded_bubble = cx
        .debug_bounds(selector(&shell, cx, "chat-item", 0))
        .unwrap();
    let fold_btn = cx.debug_bounds("chat-user-fold").unwrap();
    cx.simulate_click(fold_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let expanded_bubble = cx
        .debug_bounds(selector(&shell, cx, "chat-item", 0))
        .unwrap();
    assert!(
        expanded_bubble.size.height > folded_bubble.size.height,
        "expanding folded user prompt must reveal full content: before={folded_bubble:?}, after={expanded_bubble:?}"
    );

    // Borderless assistant message renders footer copy + file + scroll-to-top without hover.
    shell.update(cx, |shell, cx| {
        let mut next = snapshot(vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "First paragraph.\n\nSecond paragraph.".into(),
            file_path: "crates/krill-app/src/shell/chat/view.rs".into(),
            ..Default::default()
        }]);
        next.reset = true;
        shell.apply_chat_poll(SESSION, next);
        cx.notify();
    });
    cx.run_until_parked();
    let assistant_row = cx
        .debug_bounds(selector(&shell, cx, "chat-item", 0))
        .unwrap();
    assert_eq!(assistant_row.left(), composer.left());
    assert_eq!(assistant_row.size.width, composer.size.width);
    assert!(cx.debug_bounds("chat-copy-item").is_some());
    assert!(cx.debug_bounds("chat-open-file").is_some());
    assert!(cx.debug_bounds("chat-scroll-top").is_some());
}

#[gpui::test]
fn chat_borderless_work_run_guide_line_and_preamble_note(cx: &mut TestAppContext) {
    let (_shell, cx) = fixture(
        cx,
        "chat-work-run-orca",
        vec![
            ChatItem {
                kind: ChatItemKind::Assistant,
                text: "Let me check Cargo.toml first.".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::ToolCall,
                tool_id: "r1".into(),
                tool_name: "Read".into(),
                file_path: "Cargo.toml".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::ToolResult,
                tool_id: "r1".into(),
                text: "[workspace]".into(),
                ..Default::default()
            },
        ],
    );
    cx.simulate_resize(size(px(320.), px(680.)));
    cx.run_until_parked();

    let overlay = cx.debug_bounds("desktop-chat-overlay").unwrap();
    let toggle = cx.debug_bounds("chat-work-run-toggle").unwrap();
    assert!(toggle.size.height < px(56.));
    assert!(toggle.left() >= overlay.left() && toggle.right() <= overlay.right());
    assert!(cx.debug_bounds("chat-work-note").is_none());

    cx.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    for target in [
        "chat-work-note",
        "chat-disclosure",
        "chat-open-file",
        "chat-copy-item",
    ] {
        let b = cx.debug_bounds(target).unwrap();
        assert!(
            b.left() >= overlay.left() && b.right() <= overlay.right(),
            "{target} out of 320px bounds: {b:?}"
        );
    }
}

#[gpui::test]
fn chat_top_right_term_pill_context_menu_turn_status_and_empty_state(cx: &mut TestAppContext) {
    struct Memory(krill_app_core::sidebar::Snapshot);
    impl krill_app_core::sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<krill_app_core::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }

    let (shell, cx) = open(cx, state_path("chat-chrome-overhaul"));
    shell.update(cx, |shell, cx| {
        shell.sidebar_collapsed = true;
        shell.panel_collapsed = true;
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SESSION);
        shell.layout.set_chat_view(pane, true);
        shell.apply_chat_poll(
            SESSION,
            ChatPollSnapshot {
                bound: false,
                ..Default::default()
            },
        );
        cx.notify();
    });
    cx.simulate_resize(size(px(320.), px(680.)));
    cx.run_until_parked();

    let overlay = cx.debug_bounds("desktop-chat-overlay").unwrap();
    let switch_pill = cx.debug_bounds("chat-switch-terminal").unwrap();
    assert!(switch_pill.left() >= overlay.left() && switch_pill.right() <= overlay.right());
    assert!(
        switch_pill.top() < overlay.center().y && switch_pill.right() > overlay.center().x,
        "Term pill must float at top-right: {switch_pill:?} vs {overlay:?}"
    );
    assert!(cx.debug_bounds("chat-empty").is_some());
    assert!(
        cx.debug_bounds("chat-turn-status").is_none(),
        "live turn status line must stay hidden when idle and no live status text exists"
    );

    // Working sidebar session shows live turn status line directly above composer.
    shell.update(cx, |shell, cx| {
        shell.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                Arc::new(Memory(krill_app_core::sidebar::Snapshot {
                    sessions: vec![krill_app_core::sidebar::Session {
                        id: SESSION,
                        status: krill_proto::control::AgentStatus {
                            state: krill_proto::control::AgentState::Working,
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
        cx.notify();
    });
    cx.run_until_parked();
    let status_line = cx.debug_bounds("chat-turn-status").unwrap();
    let composer = cx.debug_bounds("desktop-chat-composer").unwrap();
    assert!(status_line.bottom() <= composer.top() + px(1.));

    // Right-click on chat overlay opens context menu with Continue with Terminal Agent.
    let feed = cx.debug_bounds("desktop-chat-feed").unwrap();
    cx.simulate_mouse_down(
        feed.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(
        feed.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    let menu_switch = cx.debug_bounds("chat-context-switch-terminal").unwrap();
    cx.simulate_click(menu_switch.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("desktop-chat-overlay").is_none(),
        "context menu item must switch pane back to Terminal"
    );

    // Top-right Term pill also switches back to Terminal.
    shell.update(cx, Shell::toggle_chat_view);
    cx.run_until_parked();
    let switch_pill = cx.debug_bounds("chat-switch-terminal").unwrap();
    cx.simulate_click(switch_pill.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_none());
}

#[gpui::test]
fn chat_overlay_split_right_button_splits_and_closes_pane(cx: &mut TestAppContext) {
    let (shell, cx) = open(cx, state_path("chat-overlay-split"));
    let pane = shell.update(cx, |shell, cx| {
        shell.sidebar_collapsed = true;
        shell.panel_collapsed = true;
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_session(pane, SESSION);
        shell.layout.set_chat_view(pane, true);
        shell.apply_chat_poll(
            SESSION,
            ChatPollSnapshot {
                bound: false,
                ..Default::default()
            },
        );
        cx.notify();
        pane
    });
    cx.run_until_parked();

    let overlay = cx.debug_bounds("desktop-chat-overlay").unwrap();
    let split_sel: &'static str =
        Box::leak(format!("chat-split-right-{}", pane.get()).into_boxed_str());
    let close_sel: &'static str =
        Box::leak(format!("chat-pane-close-{}", pane.get()).into_boxed_str());
    let split_btn = cx.debug_bounds(split_sel).expect("chat split-right button");
    assert!(
        split_btn.left() >= overlay.left() && split_btn.right() <= overlay.right(),
        "chat split button must sit inside overlay: {split_btn:?} vs {overlay:?}"
    );
    assert!(
        cx.debug_bounds(close_sel).is_none(),
        "single chat pane hides close button"
    );

    cx.simulate_click(split_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let visible_count = shell.read_with(cx, |shell, _| shell.layout.visible().len());
    assert_eq!(visible_count, 2);
    assert!(
        cx.debug_bounds(close_sel).is_none(),
        "unfocused split chat pane auto-hides close button until hovered"
    );
    let top_sel: &'static str =
        Box::leak(format!("chat-top-controls-{}", pane.get()).into_boxed_str());
    let top_bounds = cx
        .debug_bounds(top_sel)
        .expect("split chat pane mounts top controls");
    cx.simulate_mouse_move(
        point(top_bounds.right() - px(8.), top_bounds.center().y),
        None,
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    let down_sel: &'static str =
        Box::leak(format!("chat-split-down-{}", pane.get()).into_boxed_str());
    assert!(
        cx.debug_bounds(down_sel).is_some(),
        "hovering split chat top controls reveals split-down"
    );
    let close_btn = cx
        .debug_bounds(close_sel)
        .expect("hovering split chat top controls reveals close button");
    cx.simulate_click(close_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let visible_count = shell.read_with(cx, |shell, _| shell.layout.visible().len());
    assert_eq!(visible_count, 1);
}

fn attach_live_test_pane(
    shell: &mut Shell,
    pane: super::super::PaneId,
    session: SessionId,
    cx: &mut Context<Shell>,
) {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    shell.attach_test(pane, session, ours, cx);
    let live = shell.live.get_mut(&pane).unwrap();
    live.sock = Some(theirs);
    live.target = Some(krill_app_core::hosts::HostTarget::local(
        krill_proto::daemon::Paths {
            socket: "/fake/socket".into(),
            holder: "/fake/holder".into(),
            krilld: "/fake/krilld".into(),
        },
    ));
}

struct TestTempDir(std::path::PathBuf);

impl TestTempDir {
    fn new(prefix: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("krill-{prefix}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[gpui::test]
fn chat_pending_card_renders_orca_intrinsic_bubble_confirms_fifo_and_rebases_across_chat_tail(
    cx: &mut gpui::TestAppContext,
) {
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let (shell, cx) = fixture(
        cx,
        "chat-pending-orca-fifo",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "How can I help?".into(),
            ..Default::default()
        }],
    );
    cx.simulate_resize(size(px(800.), px(680.)));
    cx.run_until_parked();

    // Orca `NativeChatComposerField`: circular 28x28 Send button (`size-7`).
    let send_btn = cx.debug_bounds("desktop-chat-send").unwrap();
    assert!((send_btn.size.width - px(28.)).abs() <= px(1.));
    assert!((send_btn.size.height - px(28.)).abs() <= px(1.));

    let composer = cx.debug_bounds("desktop-chat-composer").unwrap();
    let input = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            attach_live_test_pane(shell, pane, SESSION, cx);
            let recorded = sent.clone();
            shell.chat.send_input = Some(Box::new(move |bytes| {
                recorded.lock().unwrap().push(bytes);
                true
            }));
            shell.ensure_chat_input(local_key(SESSION), window, cx)
        })
    });
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            input.update(cx, |state, cx| state.set_value("hello", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();

    assert!(input.read_with(cx, |s, _| s.value().is_empty()));
    let pending_bounds = cx
        .debug_bounds("chat-pending-card")
        .expect("optimistic prompt bubble must render on submit");
    assert!(
        pending_bounds.size.width < composer.size.width * 0.5,
        "Orca pending bubble must wrap short prompts to intrinsic width, got {:?} vs composer {:?}",
        pending_bounds.size.width,
        composer.size.width
    );
    assert!(
        pending_bounds.right() >= composer.right() - px(4.),
        "pending bubble must align flush with the right edge of the composer"
    );

    // Queue a second optimistic prompt and confirm FIFO with a transcript poll + CHAT_BASE_ORIGIN rebase.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            input.update(cx, |state, cx| state.set_value("second", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |s, _| s.chat.pending_prompts[&local_key(SESSION)].len()),
        2
    );

    shell.update(cx, |shell, _| {
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![ChatItem {
                kind: ChatItemKind::User,
                text: "hello".into(),
                ..Default::default()
            }]),
        );
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        let pending = &s.chat.pending_prompts[&local_key(SESSION)];
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].display_text, "second");
        assert!(pending[0].origin_item < super::CHAT_BASE_ORIGIN);
    });

    // Confirm remaining "second", then verify CHAT_BASE_ORIGIN rebase across apply_chat_tail on bind.
    shell.update(cx, |shell, _| {
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![ChatItem {
                kind: ChatItemKind::User,
                text: "second".into(),
                ..Default::default()
            }]),
        );
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert!(
            s.chat
                .pending_prompts
                .get(&local_key(SESSION))
                .is_none_or(Vec::is_empty)
        );
    });

    shell.update(cx, |shell, _| {
        shell.chat.logs.remove(&local_key(SESSION));
    });
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            input.update(cx, |state, cx| state.set_value("rebased", window, cx));
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(
            s.chat.pending_prompts[&local_key(SESSION)][0].origin_item,
            0
        );
    });

    shell.update(cx, |shell, _| {
        shell.apply_chat_tail(
            SESSION,
            local_key(SESSION).0,
            super::ChatPage {
                snapshot: krill_proto::agent_history::ChatPollSnapshot {
                    agent: "claude".into(),
                    bound: true,
                    reset: false,
                    session_id: "chat-ui-tail".into(),
                    transcript_path: "/fake/chat-ui-tail.jsonl".into(),
                    items: vec![ChatItem {
                        kind: ChatItemKind::Assistant,
                        text: "older".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                start: 20,
                has_earlier: true,
            },
            None,
            krill_proto::control::AgentState::Done,
            std::time::Instant::now(),
        );
    });
    shell.read_with(cx, |s, _| {
        let pending = &s.chat.pending_prompts[&local_key(SESSION)];
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].origin_item, super::CHAT_BASE_ORIGIN);
    });
    shell.update(cx, |shell, cx| {
        shell.apply_chat_poll(
            SESSION,
            krill_proto::agent_history::ChatPollSnapshot {
                agent: "claude".into(),
                bound: true,
                reset: false,
                session_id: "chat-ui-tail".into(),
                transcript_path: "/fake/chat-ui-tail.jsonl".into(),
                items: vec![ChatItem {
                    kind: ChatItemKind::User,
                    text: "rebased".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        cx.notify();
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert!(
            s.chat
                .pending_prompts
                .get(&local_key(SESSION))
                .is_none_or(Vec::is_empty)
        );
    });
    assert!(cx.debug_bounds("chat-pending-card").is_none());
}

#[gpui::test]
fn chat_unconfirmed_prompt_restores_empty_composer_or_keeps_review_card_when_new_draft_exists(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, cx) = fixture(
        cx,
        "chat-pending-restore-or-review",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "Ready.".into(),
            ..Default::default()
        }],
    );
    let send_ok = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));

    let input = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            attach_live_test_pane(shell, pane, SESSION, cx);
            let ok = send_ok.clone();
            let recorded = sent.clone();
            shell.chat.send_input = Some(Box::new(move |bytes| {
                if ok.load(std::sync::atomic::Ordering::SeqCst) {
                    recorded.lock().unwrap().push(bytes);
                    true
                } else {
                    false
                }
            }));
            shell.ensure_chat_input(local_key(SESSION), window, cx)
        })
    });

    // Case 1: failed send with empty composer restores prompt text back into composer.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            input.update(cx, |state, cx| {
                state.set_value("restore me", window, cx);
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(input.read_with(cx, |s, _| s.value()), "restore me");
    assert!(cx.debug_bounds("chat-pending-card").is_none());

    // Case 2: failed send while user has already typed a new draft enters ReviewRequired card.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let pending_id = 42;
            shell.chat.pending_prompts.insert(
                local_key(SESSION),
                vec![super::PendingChatPrompt {
                    id: pending_id,
                    origin_item: 0,
                    submitted_payload: "failed prompt".into(),
                    display_text: "failed prompt".into(),
                    attachments: Vec::new(),
                    status: super::PendingStatus::Sending,
                    submitted_at_ms: 0,
                }],
            );
            input.update(cx, |state, cx| state.set_value("still typing", window, cx));
            shell.transition_pending_to_review_required(
                super::PendingReview {
                    key: &local_key(SESSION),
                    pending_id,
                    immediate_send_failure: true,
                },
                Some(window),
                cx,
            );
        });
    });
    cx.run_until_parked();

    assert_eq!(input.read_with(cx, |s, _| s.value()), "still typing");
    assert!(cx.debug_bounds("chat-pending-card").is_some());
    let copy_btn = cx.debug_bounds("chat-pending-copy").unwrap();
    let retry_btn = cx.debug_bounds("chat-pending-retry").unwrap();

    cx.simulate_click(copy_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some("failed prompt")
    );

    send_ok.store(true, std::sync::atomic::Ordering::SeqCst);
    cx.simulate_click(retry_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(input.read_with(cx, |s, _| s.value()), "still typing");
    assert!(
        !sent.lock().unwrap().is_empty(),
        "clicking Retry must re-dispatch failed pending prompt"
    );
}

#[gpui::test]
fn chat_autocomplete_supports_file_and_slash_selection_ime_guard_working_priority_and_320px_bounds(
    cx: &mut gpui::TestAppContext,
) {
    struct MetaMemory(krill_app_core::sidebar::Snapshot);
    impl krill_app_core::sidebar::Source for MetaMemory {
        fn fetch(&self) -> std::io::Result<krill_app_core::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }

    let workspace = TestTempDir::new("chat-ac-ws");
    std::fs::create_dir_all(workspace.path().join("src")).unwrap();
    std::fs::create_dir_all(workspace.path().join(".git")).unwrap();
    std::fs::create_dir_all(workspace.path().join("target/debug")).unwrap();
    std::fs::create_dir_all(workspace.path().join("node_modules/pkg")).unwrap();
    std::fs::write(workspace.path().join("README.md"), "# krill\n").unwrap();
    std::fs::write(workspace.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(workspace.path().join(".env"), "SECRET=1\n").unwrap();
    std::fs::write(workspace.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(workspace.path().join("target/debug/krill"), "").unwrap();
    std::fs::write(workspace.path().join("node_modules/pkg/index.js"), "").unwrap();

    let (shell, cx) = fixture(
        cx,
        "chat-ac-and-composer",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "Ready.".into(),
            ..Default::default()
        }],
    );
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let ws_path = workspace.path().to_path_buf();
    let input = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            attach_live_test_pane(shell, pane, SESSION, cx);
            let recorded = sent.clone();
            shell.chat.send_input = Some(Box::new(move |bytes| {
                recorded.lock().unwrap().push(bytes);
                true
            }));
            let log = shell.chat.logs.entry(local_key(SESSION)).or_default();
            log.agent = "codex".into();
            log.meta = ComposerMeta {
                model: Some("gpt-5.4".into()),
                current_model_id: Some("gpt-5.4".into()),
                models: vec![ComposerModelItem {
                    id: "gpt-5.4".into(),
                    label: "GPT-5.4".into(),
                    efforts: vec![
                        "max".into(),
                        "high".into(),
                        "low".into(),
                        "xhigh".into(),
                        "medium".into(),
                    ],
                    default_effort: Some("medium".into()),
                }],
                effort: Some("medium".into()),
                supported: true,
                effort_supported: true,
                ..Default::default()
            };
            shell.sidebar.update(cx, |sidebar, cx| {
                sidebar.connect_source(
                    std::sync::Arc::new(MetaMemory(krill_app_core::sidebar::Snapshot {
                        sessions: vec![krill_app_core::sidebar::Session {
                            id: SESSION,
                            status: krill_proto::control::AgentStatus {
                                state: krill_proto::control::AgentState::Done,
                                agent: "codex".into(),
                                ..krill_proto::control::AgentStatus::UNKNOWN
                            },
                            worktree: Some(ws_path.clone()),
                            cwd: Some(ws_path),
                            account: None,
                        }],
                        trees: std::collections::BTreeMap::new(),
                    })),
                    cx,
                );
            });
            shell.ensure_chat_input(local_key(SESSION), window, cx)
        })
    });
    cx.run_until_parked();

    let send_btn = cx.debug_bounds("desktop-chat-send").unwrap();

    // Effort picker must sort choices lowest-to-highest regardless of backend order.
    let model_btn = cx.debug_bounds("chat-composer-model").unwrap();
    cx.simulate_click(model_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let opt_low = cx.debug_bounds("chat-effort-option-low").unwrap();
    let opt_med = cx.debug_bounds("chat-effort-option-medium").unwrap();
    let opt_high = cx.debug_bounds("chat-effort-option-high").unwrap();
    let opt_xhigh = cx.debug_bounds("chat-effort-option-xhigh").unwrap();
    let opt_max = cx.debug_bounds("chat-effort-option-max").unwrap();
    assert!(opt_low.left() < opt_med.left());
    assert!(opt_med.left() < opt_high.left());
    assert!(opt_high.left() < opt_xhigh.left());
    assert!(opt_xhigh.left() < opt_max.left());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // Narrow 320px window: autocomplete `@` popover opens, excludes hidden/build dirs, and fits within bounds.
    cx.simulate_resize(size(px(320.), px(680.)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            input.update(cx, |state, cx| {
                state.focus(window, cx);
                state.set_value("", window, cx);
                gpui::EntityInputHandler::replace_text_in_range(state, None, "@", window, cx);
            });
            shell.sync_chat_autocomplete(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();

    let popover = cx
        .debug_bounds("chat-autocomplete-popover")
        .expect("@ query must open autocomplete popover");
    let overlay = cx.debug_bounds("desktop-chat-overlay").unwrap();
    let composer = cx.debug_bounds("desktop-chat-composer").unwrap();
    assert!(popover.left() >= overlay.left());
    assert!(popover.right() <= overlay.right());
    assert!(popover.bottom() <= composer.top() + px(2.));
    shell.read_with(cx, |s, _| {
        let items = &s.chat.autocomplete[&local_key(SESSION)].items;
        assert!(items.iter().any(|it| it.insert_text == "@README.md "));
        assert!(items.iter().any(|it| it.insert_text == "@src/main.rs "));
        assert!(!items.iter().any(|it| it.insert_text.contains(".git")
            || it.insert_text.contains(".env")
            || it.insert_text.contains("target/")
            || it.insert_text.contains("node_modules/")));
    });

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(input.read_with(cx, |s, _| s.value().starts_with('@')));
    assert!(
        sent.lock().unwrap().is_empty(),
        "Enter accepting autocomplete must not send prompt"
    );

    // IME composition must keep autocomplete closed.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                gpui::EntityInputHandler::replace_and_mark_text_in_range(
                    state, None, "@ㅎ", None, window, cx,
                );
            });
            shell.sync_chat_autocomplete(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-autocomplete-popover").is_none());

    // Slash command discovery opens on `/`.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                gpui::EntityInputHandler::replace_text_in_range(state, None, "/comp", window, cx);
            });
            shell.sync_chat_autocomplete(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-autocomplete-popover").is_some());
    assert!(cx.debug_bounds("chat-autocomplete-option-0").is_some());

    // Option A Stop: Working switches Send -> Stop at identical 32px circular dimensions even with draft.
    shell.update(cx, |shell, cx| {
        shell.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(
                std::sync::Arc::new(MetaMemory(krill_app_core::sidebar::Snapshot {
                    sessions: vec![krill_app_core::sidebar::Session {
                        id: SESSION,
                        status: krill_proto::control::AgentStatus {
                            state: krill_proto::control::AgentState::Working,
                            agent: "codex".into(),
                            since_unix_ns: 999,
                            ..krill_proto::control::AgentStatus::UNKNOWN
                        },
                        worktree: None,
                        cwd: None,
                        account: None,
                    }],
                    trees: std::collections::BTreeMap::new(),
                })),
                cx,
            );
        });
    });
    cx.run_until_parked();

    let stop_btn = cx
        .debug_bounds("desktop-chat-stop")
        .expect("Working session with draft text must show Stop under Option A");
    assert!(cx.debug_bounds("desktop-chat-send").is_none());
    assert_eq!(stop_btn.size, send_btn.size);
    // Orca `NativeChatComposerField`: circular 28x28 Stop button (`size-7`).
    assert!((stop_btn.size.width - px(28.)).abs() <= px(1.));
    cx.simulate_click(stop_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(*sent.lock().unwrap(), vec![b"\x03".to_vec()]);
    assert_eq!(input.read_with(cx, |s, _| s.value()), "/comp");
}

#[gpui::test]
fn chat_attachments_stage_from_drop_and_paste_remove_individually_and_submit_shell_escaped_payload(
    cx: &mut gpui::TestAppContext,
) {
    let dir = TestTempDir::new("chat-attach");
    let first = dir.path().join("hello world.txt");
    let second = dir.path().join("spec'1.md");
    std::fs::write(&first, "alpha").unwrap();
    std::fs::write(&second, "beta").unwrap();

    let (shell, cx) = fixture(
        cx,
        "chat-attachments-flow",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "Ready.".into(),
            ..Default::default()
        }],
    );
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let input = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            attach_live_test_pane(shell, pane, SESSION, cx);
            let recorded = sent.clone();
            shell.chat.send_input = Some(Box::new(move |bytes| {
                recorded.lock().unwrap().push(bytes);
                true
            }));
            shell.ensure_chat_input(local_key(SESSION), window, cx)
        })
    });

    // `+` popup exposes `chat-quick-attach`.
    let plus = cx.debug_bounds("chat-composer-plus").unwrap();
    cx.simulate_click(plus.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-quick-attach").is_some());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // Stage one file via overlay drop and another via clipboard image paste.
    let drop_paths = gpui::ExternalPaths(vec![first.clone()].into());
    // Minimal 1x1 PNG bytes for clipboard image paste staging test.
    let png = gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ],
    );
    cx.write_to_clipboard(gpui::ClipboardItem::new_image(&png));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_dropped_paths_to_chat(&local_key(SESSION), &drop_paths, window, cx);
            shell.intercept_chat_clipboard_paste(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();

    // Files stay chips; the pasted image becomes a numbered thumbnail with its `Image #1` pill.
    assert!(cx.debug_bounds("chat-attachment-chips").is_some());
    assert!(cx.debug_bounds("chat-attachment-chip-0").is_some());
    assert!(cx.debug_bounds("chat-attachment-chip-1").is_none());
    assert!(cx.debug_bounds("chat-attachment-image-0").is_some());
    assert!(cx.debug_bounds("chat-attachment-image-pill-1").is_some());

    // Remove the dropped file (slot 0), then stage `spec'1.md` and remove the image (now slot 0).
    let remove0 = cx.debug_bounds("chat-attachment-remove-0").unwrap();
    cx.simulate_click(remove0.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-attachment-chips").is_none());
    assert!(cx.debug_bounds("chat-attachment-image-0").is_some());

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_dropped_paths_to_chat(
                &local_key(SESSION),
                &gpui::ExternalPaths(vec![second.clone()].into()),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    let remove_img = cx.debug_bounds("chat-attachment-remove-0").unwrap();
    cx.simulate_click(remove_img.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-attachment-image-0").is_none());
    assert!(cx.debug_bounds("chat-attachment-image-pills").is_none());
    assert!(cx.debug_bounds("chat-attachment-chip-0").is_some());
    assert!(cx.debug_bounds("chat-attachment-chip-1").is_none());

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            input.update(cx, |state, cx| {
                state.set_value("please inspect", window, cx);
            });
            shell.submit_chat_prompt(&local_key(SESSION), window, cx);
        });
    });
    cx.run_until_parked();

    let payload = String::from_utf8_lossy(sent.lock().unwrap().last().unwrap()).into_owned();
    assert!(
        payload.contains("'\\''"),
        "shell-escaped attachment payload must quote single quotes safely, got {payload:?}"
    );
    assert!(payload.contains("please inspect"));
}

#[gpui::test]
fn chat_images_show_as_numbered_thumbnails_and_open_in_a_viewer_that_esc_closes(
    cx: &mut gpui::TestAppContext,
) {
    let dir = TestTempDir::new("chat-images");
    let write_png = |name: &str, w: u32, h: u32| {
        let path = dir.path().join(name);
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 40, 40, 255]))
            .save(&path)
            .unwrap();
        path
    };
    let sent_a = write_png("sent-a.png", 400, 200);
    let sent_b = write_png("sent-b.png", 30, 90);
    let draft = write_png("draft.png", 64, 64);

    let (shell, cx) = fixture(
        cx,
        "chat-images-flow",
        vec![
            ChatItem {
                kind: ChatItemKind::User,
                text: "[Image #3] [Image #4] 이거 봐".into(),
                file_path: format!("{}\n{}", sent_a.display(), sent_b.display()),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::Assistant,
                text: "봤습니다.".into(),
                ..Default::default()
            },
        ],
    );
    cx.run_until_parked();

    // The sent prompt draws both images as 80 px tiles numbered like its `[Image #n]` text,
    // and no file link for them.
    let tiles = [
        cx.debug_bounds("chat-message-image-0").unwrap(),
        cx.debug_bounds("chat-message-image-1").unwrap(),
    ];
    assert!(cx.debug_bounds("chat-message-image-2").is_none());
    assert!((tiles[0].size.width - px(80.)).abs() <= px(1.));
    assert!(cx.debug_bounds("chat-open-file").is_none());
    shell.read_with(cx, |shell, _| {
        assert!(matches!(
            shell.chat.images.thumbs_for_test(&sent_a),
            Some(super::images::Loaded::Ready(_))
        ));
    });

    cx.simulate_click(tiles[1].center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-image-viewer").is_some());
    assert!(cx.debug_bounds("chat-image-viewer-picture").is_some());
    shell.read_with(cx, |shell, _| {
        let viewer = shell.chat.images.viewer.as_ref().unwrap();
        assert_eq!(viewer.index, 1);
        assert_eq!(viewer.numbers, vec![3, 4]);
    });
    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    shell.read_with(cx, |shell, _| {
        assert_eq!(shell.chat.images.viewer.as_ref().unwrap().index, 0);
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-image-viewer").is_none());

    // A drafted image: a 56 px thumbnail with its `#1` badge and remove button, an `Image #1`
    // pill, and a click opens the viewer; the close button shuts it.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_dropped_paths_to_chat(
                &local_key(SESSION),
                &gpui::ExternalPaths(vec![draft.clone()].into()),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    let thumb = cx.debug_bounds("chat-attachment-image-0").unwrap();
    assert!((thumb.size.width - px(56.)).abs() <= px(1.));
    assert!(cx.debug_bounds("chat-attachment-image-pill-1").is_some());
    assert!(cx.debug_bounds("chat-attachment-remove-0").is_some());
    cx.simulate_click(thumb.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-image-viewer").is_some());
    assert!(cx.debug_bounds("chat-image-viewer-next").is_none());
    let close = cx.debug_bounds("chat-image-viewer-close").unwrap();
    cx.simulate_click(close.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-image-viewer").is_none());
    shell.read_with(cx, |shell, _| assert!(shell.chat.images.viewer.is_none()));
}

#[test]
fn image_numbers_follow_the_prompt_markers_or_count_from_one() {
    use super::images::image_numbers;
    assert_eq!(image_numbers("[Image #3] [Image #4] 봐", 2), vec![3, 4]);
    assert_eq!(image_numbers("[Image #2] 하나만", 2), vec![1, 2]);
    assert_eq!(image_numbers("표기 없음", 3), vec![1, 2, 3]);
    assert_eq!(image_numbers("[Image #1] [Image #1]", 1), vec![1]);
}

#[gpui::test]
fn chat_selection_copy_find_shortcuts_reasoning_copy_effort_and_discoverability(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, cx) = open(cx, state_path("chat-ux-phase1a"));
    let pane = shell.read_with(cx, |s, _| s.layout.focused().unwrap());
    shell.update(cx, |shell, cx| {
        shell.sidebar_collapsed = true;
        shell.panel_collapsed = true;
        shell.layout.set_session(pane, SESSION);
        shell.layout.set_chat_view(pane, true);
        shell.apply_chat_poll(
            SESSION,
            ChatPollSnapshot {
                agent: "claude".into(),
                bound: true,
                items: vec![
                    ChatItem {
                        kind: ChatItemKind::User,
                        text: "alpha user question".into(),
                        ..Default::default()
                    },
                    ChatItem {
                        kind: ChatItemKind::Reasoning,
                        text: "thinking about alpha".into(),
                        ..Default::default()
                    },
                    ChatItem {
                        kind: ChatItemKind::Assistant,
                        text:
                            "First rust block:\n```rs\nfn hello() {}\n```\nFindable Needle phrase"
                                .into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
        let log = shell.chat.logs.get_mut(&local_key(SESSION)).unwrap();
        log.meta = ComposerMeta {
            model: Some("haiku".into()),
            current_model_id: Some("haiku".into()),
            supported: true,
            effort_supported: false,
            ..Default::default()
        };
        cx.notify();
    });
    cx.run_until_parked();

    // 1. effort_supported: false hides effort options in model picker.
    let model_btn = cx.debug_bounds("chat-composer-model").unwrap();
    cx.simulate_click(model_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-model-picker").is_some());
    assert!(
        cx.debug_bounds("chat-effort-option-low").is_none(),
        "effort options must be hidden when effort_supported is false"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // 2. Expand Solo Reasoning card -> Copy button writes reasoning text to clipboard.
    let disclosure = cx.debug_bounds("chat-disclosure").unwrap();
    cx.simulate_click(disclosure.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let copy_btn = cx.debug_bounds("chat-reasoning-copy").unwrap();
    cx.simulate_click(copy_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert!(
        copied.contains("thinking about alpha"),
        "expected reasoning copy on clipboard, got {copied:?}"
    );

    // 3. Cmd+F opens Chat Find Bar, matches row, and Esc dismisses.
    let feed = cx.debug_bounds("desktop-chat-feed").unwrap();
    cx.simulate_click(feed.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-f");
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-find-bar").is_some());
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let find = &shell.chat.finds[&local_key(SESSION)];
            find.input.update(cx, |state, cx| {
                state.focus(window, cx);
                gpui::EntityInputHandler::replace_text_in_range(state, None, "Needle", window, cx);
            });
        });
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        let find = &s.chat.finds[&local_key(SESSION)];
        assert_eq!(find.matches.len(), 1);
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-find-bar").is_none());

    // 4. SelectAll (Cmd+A) + Copy (Cmd+C) copies chat text, Cmd+K is safe, Cmd+Shift+J toggles view.
    cx.simulate_keystrokes("cmd-a cmd-c");
    cx.run_until_parked();
    let selected_clip = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert!(
        selected_clip.contains("Needle") || selected_clip.contains("alpha"),
        "Cmd+A + Cmd+C in Chat View must copy selected chat text, got {selected_clip:?}"
    );
    cx.simulate_keystrokes("cmd-k");
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(s.chat.logs[&local_key(SESSION)].items.len(), 3);
    });
    cx.simulate_keystrokes("cmd-shift-j");
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_none());
    cx.simulate_keystrokes("cmd-shift-j");
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-chat-overlay").is_some());
}

fn set_phase2b_session_state(
    view: &Entity<Shell>,
    cx: &mut VisualTestContext,
    state: krill_proto::control::AgentState,
    since_unix_ns: u64,
) {
    struct StateSource(krill_app_core::sidebar::Snapshot);
    impl krill_app_core::sidebar::Source for StateSource {
        fn fetch(&self) -> std::io::Result<krill_app_core::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }

    let snap = krill_app_core::sidebar::Snapshot {
        sessions: vec![krill_app_core::sidebar::Session {
            id: SESSION,
            status: krill_proto::control::AgentStatus {
                state,
                agent: "claude".into(),
                since_unix_ns,
                ..krill_proto::control::AgentStatus::UNKNOWN
            },
            worktree: None,
            cwd: None,
            account: None,
        }],
        trees: std::collections::BTreeMap::new(),
    };
    view.update(cx, |shell, cx| {
        shell.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(std::sync::Arc::new(StateSource(snap)), cx);
        });
    });
    cx.run_until_parked();
}

fn inject_phase2b_screen(
    shell: &mut Shell,
    pane: super::super::PaneId,
    text: &str,
    cx: &mut gpui::Context<Shell>,
) {
    let mut ops = vec![krill_proto::screen::Op::Reset {
        cols: 100,
        lines: 12,
    }];
    for (line, row_text) in text.lines().enumerate() {
        ops.push(krill_proto::screen::Op::Line {
            line: u16::try_from(line).unwrap(),
            cells: row_text
                .chars()
                .map(|ch| krill_proto::screen::Cell {
                    ch,
                    ..Default::default()
                })
                .collect(),
        });
    }
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
}

#[gpui::test]
fn chat_phase2b_queue_steer_stop_cooldown_and_banners_work_end_to_end(cx: &mut TestAppContext) {
    use krill_proto::control::AgentState;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let (shell, cx) = open(cx, state_path("chat-phase2b-queue-stop"));
    let sent: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = sent.clone();
    let input = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.sidebar_collapsed = true;
            shell.panel_collapsed = true;
            let pane = shell.layout.focused().unwrap();
            attach_live_test_pane(shell, pane, SESSION, cx);
            shell.layout.set_session(pane, SESSION);
            shell.layout.set_chat_view(pane, true);
            shell.chat.send_input = Some(Box::new(move |bytes| {
                recorded.lock().unwrap().push(bytes);
                true
            }));
            shell.apply_chat_poll(SESSION, snapshot(Vec::new()));
            let input = shell.ensure_chat_input(local_key(SESSION), window, cx);
            input.update(cx, |state, cx| state.set_value("queued task", window, cx));
            input
        })
    });
    cx.run_until_parked();

    set_phase2b_session_state(&shell, cx, AgentState::Working, 100);
    assert!(cx.debug_bounds("desktop-chat-stop").is_some());
    assert!(cx.debug_bounds("desktop-chat-send").is_none());
    let queue_btn = cx
        .debug_bounds("chat-composer-queue")
        .expect("Queue button appears when Working and draft is non-empty");
    assert!(cx.debug_bounds("chat-composer-steer").is_some());

    // Clicking Queue stages the prompt locally without interrupting the running turn.
    cx.simulate_click(queue_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(input.read_with(cx, |s, _| s.value().is_empty()));
    assert!(cx.debug_bounds("chat-queued-strip").is_some());
    assert!(cx.debug_bounds("chat-queued-item").is_some());
    assert!(sent.lock().unwrap().is_empty());

    // Stop button sets 2s cooldown that clears on GPUI clock advance.
    let stop_btn = cx.debug_bounds("desktop-chat-stop").unwrap();
    cx.simulate_click(stop_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |s, cx| {
        assert!(s.is_chat_stop_cooldown(&local_key(SESSION), cx));
    });
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    shell.read_with(cx, |s, cx| {
        assert!(!s.is_chat_stop_cooldown(&local_key(SESSION), cx));
    });

    // Returning to Done flushes the queued prompt.
    set_phase2b_session_state(&shell, cx, AgentState::Done, 200);
    shell.update(cx, |s, cx| {
        s.flush_queued_chat_prompt_if_ready(&local_key(SESSION), cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-queued-strip").is_none());
    let dispatched = String::from_utf8_lossy(
        sent.lock()
            .unwrap()
            .last()
            .expect("queued prompt should flush on Idle"),
    )
    .into_owned();
    assert!(dispatched.contains("queued task"));

    // While Working again, Steer sends immediately without queuing into chat-queued-strip.
    sent.lock().unwrap().clear();
    set_phase2b_session_state(&shell, cx, AgentState::Working, 300);
    cx.update(|window, cx| {
        input.update(cx, |state, cx| state.set_value("urgent steer", window, cx));
    });
    cx.run_until_parked();
    let steer_btn = cx.debug_bounds("chat-composer-steer").unwrap();
    cx.simulate_click(steer_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(input.read_with(cx, |s, _| s.value().is_empty()));
    assert!(cx.debug_bounds("chat-queued-strip").is_none());
    let all_sent = sent.lock().unwrap();
    assert!(!all_sent.is_empty());
    assert!(
        all_sent
            .iter()
            .any(|bytes| String::from_utf8_lossy(bytes).contains("urgent steer"))
    );
    drop(all_sent);

    // Rate-limit banner opens Settings -> Accounts on click.
    shell.update(cx, |s, cx| {
        if let Some(log) = s.chat.logs.get_mut(&local_key(SESSION)) {
            log.meta.rate_limited = true;
        }
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-rate-limit-banner").is_some());
    let switch_btn = cx.debug_bounds("chat-switch-account").unwrap();
    cx.simulate_click(switch_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).settings().is_some()));
    assert!(cx.debug_bounds("admin-center").is_some());
}

#[gpui::test]
fn chat_phase2b_diffs_timestamps_unread_divider_and_prompt_cards(cx: &mut TestAppContext) {
    use krill_proto::agent_history::ChatQuestionOption;
    use std::sync::{Arc, Mutex};

    let (shell, cx) = fixture(
        cx,
        "chat-phase2b-cards",
        (0..40)
            .map(|i| ChatItem {
                kind: ChatItemKind::Assistant,
                text: format!("older message {i}\n{}", "content\n".repeat(6)),
                at: "2026-11-21T09:15:00+09:00".into(),
                ..Default::default()
            })
            .collect(),
    );
    cx.simulate_resize(size(px(640.), px(650.)));
    cx.run_until_parked();
    let sent: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = sent.clone();
    set_phase2b_session_state(&shell, cx, krill_proto::control::AgentState::Waiting, 100);
    let pane = shell.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        attach_live_test_pane(shell, pane, SESSION, cx);
        shell.chat.send_input = Some(Box::new(move |bytes| {
            recorded.lock().unwrap().push(bytes);
            true
        }));
        inject_phase2b_screen(
            shell,
            pane,
            "Would you like to proceed?\n❯ 1. Yes, bypass\n  2. No, keep planning",
            cx,
        );
        pane
    });
    cx.run_until_parked();

    // Scroll up away from tail so incoming messages produce an inline UnreadDivider.
    for _ in 0..3 {
        scroll(cx, 700.);
    }
    assert!(cx.debug_bounds("chat-jump-latest").is_some());
    shell.update(cx, |shell, cx| {
        let mut snap = snapshot(vec![
            ChatItem {
                kind: ChatItemKind::ToolCall,
                tool_name: "Edit".into(),
                file_path: "src/app.rs".into(),
                text: "--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::Permission,
                tool_id: "perm-plan-1".into(),
                tool_name: "ExitPlanMode".into(),
                text: "Would you like to proceed?".into(),
                at: "2026-11-21T09:16:00+09:00".into(),
                ..Default::default()
            },
        ]);
        snap.next_offset = 512;
        shell.apply_chat_poll(SESSION, snap);
        cx.notify();
    });
    cx.run_until_parked();

    for _ in 0..4 {
        scroll(cx, -700.);
    }
    assert!(cx.debug_bounds("chat-unread-divider").is_some());
    assert!(cx.debug_bounds("chat-work-run-diff-summary").is_some());
    assert!(cx.debug_bounds("chat-message-time").is_some());
    assert!(cx.debug_bounds("chat-plan-approval-badge").is_some());
    assert!(cx.debug_bounds("chat-permission-choice-0").is_some());
    let reject_btn = cx
        .debug_bounds("chat-permission-choice-1")
        .expect("numbered permission option 2 should render");
    cx.simulate_click(reject_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(!sent.lock().unwrap().is_empty());

    // Question card with an "Other" option mounts the custom text input + submit button.
    set_phase2b_session_state(&shell, cx, krill_proto::control::AgentState::Waiting, 101);
    shell.update(cx, |shell, cx| {
        inject_phase2b_screen(
            shell,
            pane,
            "Pick an approach\n❯ 1. Fast\n  2. Other\nType something.",
            cx,
        );
        shell.apply_chat_poll(
            SESSION,
            snapshot(vec![ChatItem {
                kind: ChatItemKind::Question,
                tool_id: "q-other-1".into(),
                title: "Approach".into(),
                text: "Pick an approach".into(),
                options: vec![
                    ChatQuestionOption {
                        label: "Fast".into(),
                        description: String::new(),
                    },
                    ChatQuestionOption {
                        label: "Other".into(),
                        description: String::new(),
                    },
                ],
                ..Default::default()
            }]),
        );
        cx.notify();
    });
    cx.run_until_parked();
    let other_submit = cx
        .debug_bounds("chat-question-other-submit")
        .expect("Question with Other option should render custom Other input and submit button");
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            if let Some(other_input) = s.chat.question_other_inputs.values().next().cloned() {
                other_input.update(cx, |state, cx| {
                    state.set_value("custom plan", window, cx);
                });
            }
        });
    });
    cx.run_until_parked();
    sent.lock().unwrap().clear();
    cx.simulate_click(other_submit.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(!sent.lock().unwrap().is_empty());
}

#[gpui::test]
fn phase3b_minimap_quote_history_long_paste_reuse_and_conditional_fork(
    cx: &mut gpui::TestAppContext,
) {
    use krill_proto::control::AgentState;

    let (shell, cx) = open(cx, state_path("chat-phase3b"));
    let key = local_key(SESSION);
    let pane = shell.update(cx, |s, cx| {
        s.sidebar_collapsed = true;
        s.panel_collapsed = true;
        let pane = s.layout.focused().unwrap();
        attach_live_test_pane(s, pane, SESSION, cx);
        if let Some(live) = s.live.get_mut(&pane) {
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
            live.pane.link(
                SESSION,
                1,
                krill_app_core::pane::Writer::spawn(ours).unwrap(),
                krill_proto::input::FEAT_INPUT | krill_proto::control::FEAT_SESSION_FORK,
                krill_proto::input::Now::get(),
            );
            live.sock = Some(theirs);
        }
        s.layout.set_session(pane, SESSION);
        s.layout.set_chat_view(pane, true);
        pane
    });

    let make_user = |text: &str| ChatItem {
        kind: ChatItemKind::User,
        text: text.into(),
        ..Default::default()
    };

    // 3 user turns: minimap remains hidden; fork hidden when agent is not claude/codex.
    let mut snap = ChatPollSnapshot {
        agent: "opencode".into(),
        session_id: "s-3b".into(),
        transcript_path: "/tmp/3b.jsonl".into(),
        bound: true,
        next_offset: 30,
        items: vec![
            make_user("turn one"),
            make_user("turn two"),
            make_user("turn three"),
        ],
        ..Default::default()
    };
    shell.update(cx, |s, cx| {
        s.apply_chat_poll(SESSION, snap.clone());
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-turn-minimap").is_none());
    assert!(cx.debug_bounds("chat-fork-item").is_none());
    assert!(cx.debug_bounds("chat-quote-item").is_some());
    assert!(cx.debug_bounds("chat-reuse-item").is_some());

    // 4th user turn: minimap rail appears; claude agent reveals Fork on its latest response
    // (Orca's "Fork from this response").
    snap.agent = "claude".into();
    snap.next_offset = 40;
    snap.items = vec![
        make_user("turn four prompt preview"),
        ChatItem {
            kind: ChatItemKind::Assistant,
            text: "four".into(),
            ..Default::default()
        },
    ];
    set_phase2b_session_state(&shell, cx, AgentState::Done, 400);
    shell.update(cx, |s, cx| {
        let log = s.chat.logs.entry(key.clone()).or_default();
        log.agent = "claude".into();
        s.apply_chat_poll(SESSION, snap);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-turn-minimap").is_some());
    assert!(cx.debug_bounds("chat-minimap-turn-4").is_some());
    assert!(cx.debug_bounds("chat-fork-item").is_some());
    let reuse_bounds = cx.debug_bounds("chat-reuse-item").unwrap();
    cx.simulate_click(reuse_bounds.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |s, cx| {
        let (input, _) = s.chat.inputs.get(&key).expect("reuse creates input");
        assert!(!input.read(cx).value().trim().is_empty());
    });

    // Quote inserts `> `-prefixed lines and focuses composer.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let input = s.chat.inputs.get(&key).unwrap().0.clone();
            input.update(cx, |st, cx| st.set_value(String::new(), window, cx));
            s.quote_text_into_chat_composer(&key, "line A\nline B", window, cx);
            let value = s.chat.inputs[&key].0.read(cx).value().to_string();
            assert_eq!(value, "> line A\n> line B\n\n");

            // Prompt history cycling preserves WIP draft on Down past newest.
            s.record_chat_prompt_history(&key, "first sent");
            s.record_chat_prompt_history(&key, "second sent");
            s.chat.inputs[&key]
                .0
                .update(cx, |st, cx| st.set_value("draft wip", window, cx));
            assert!(s.step_chat_prompt_history(&key, -1, window, cx));
            assert_eq!(
                s.chat.inputs[&key].0.read(cx).value().as_ref(),
                "second sent"
            );
            assert!(s.step_chat_prompt_history(&key, -1, window, cx));
            assert_eq!(
                s.chat.inputs[&key].0.read(cx).value().as_ref(),
                "first sent"
            );
            assert!(s.step_chat_prompt_history(&key, 1, window, cx));
            assert_eq!(
                s.chat.inputs[&key].0.read(cx).value().as_ref(),
                "second sent"
            );
            assert!(s.step_chat_prompt_history(&key, 1, window, cx));
            assert_eq!(s.chat.inputs[&key].0.read(cx).value().as_ref(), "draft wip");
        });
    });

    // >= 32 KiB text paste creates a .txt attachment instead of pasting inline.
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("A".repeat(33 * 1024)));
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.intercept_chat_clipboard_paste(&key, window, cx);
        });
    });
    cx.run_until_parked();
    let attached_paths: Vec<std::path::PathBuf> = shell.read_with(cx, |s, _| {
        s.chat
            .draft_attachments
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.path)
            .collect()
    });
    assert_eq!(attached_paths.len(), 1);
    assert_eq!(
        attached_paths[0].extension().and_then(|e| e.to_str()),
        Some("txt")
    );
    for p in attached_paths {
        std::fs::remove_file(p).ok();
    }
    let _ = pane;
}

type SentBytes = std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>;

const RL_NOW: u64 = 1_900_000_000;
const RL_RESET: u64 = RL_NOW + 60;

/// A rate-limited chat on account `main` whose weekly window resets in a minute.
fn rate_limited_fixture<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    with_alternative: bool,
    auto_continue_setting: bool,
) -> (Entity<Shell>, &'a mut VisualTestContext, SentBytes) {
    rate_limited_fixture_at(
        cx,
        state_path(name),
        with_alternative,
        auto_continue_setting,
    )
}

fn rate_limited_fixture_at(
    cx: &mut TestAppContext,
    layout_path: std::path::PathBuf,
    with_alternative: bool,
    auto_continue_setting: bool,
) -> (Entity<Shell>, &mut VisualTestContext, SentBytes) {
    use krill_proto::usage::{
        AccountInfo, AccountsSnapshot, FEAT_PROVIDER_DETAILS, FEAT_SESSION_ACCOUNT_SWITCH,
        FEAT_USAGE, ProviderKind, Snapshot, Window,
    };
    struct StateSource(krill_app_core::sidebar::Snapshot);
    impl krill_app_core::sidebar::Source for StateSource {
        fn fetch(&self) -> std::io::Result<krill_app_core::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }
    let (shell, cx) = open(cx, layout_path);
    // Settings default is now `true`; explicitly apply `auto_continue_setting` so fixtures test both.
    cx.update(|_, cx| {
        krill_app_core::settings::edit(cx, move |s| {
            s.chat_auto_continue_after_rate_limit = auto_continue_setting;
        });
    });
    cx.run_until_parked();
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let recorded = sent.clone();
    let account = |id: &str, seven_day: Option<Window>| AccountInfo {
        id: id.into(),
        provider: ProviderKind::Claude,
        label: id.into(),
        email: None,
        profile_dir: format!("/synthetic/{id}"),
        active: id == "main",
        host_default: false,
        stale: false,
        usage: Snapshot {
            five_hour: None,
            seven_day,
        },
    };
    let mut accounts = vec![account(
        "main",
        Some(Window {
            percent: 100,
            resets_at: Some(RL_RESET.to_string()),
        }),
    )];
    if with_alternative {
        accounts.push(account("other", None));
    }
    let snap = AccountsSnapshot {
        accounts,
        ..AccountsSnapshot::default()
    };
    let sessions = krill_app_core::sidebar::Snapshot {
        sessions: vec![krill_app_core::sidebar::Session {
            id: SESSION,
            status: krill_proto::control::AgentStatus {
                state: krill_proto::control::AgentState::Done,
                agent: "claude".into(),
                since_unix_ns: 1,
                ..krill_proto::control::AgentStatus::UNKNOWN
            },
            worktree: None,
            cwd: None,
            account: Some(krill_proto::control::SessionAccount {
                session: SESSION,
                account_id: "main".into(),
                label: "main".into(),
            }),
        }],
        trees: std::collections::BTreeMap::new(),
    };
    shell.update(cx, |shell, cx| {
        shell.sidebar_collapsed = true;
        shell.panel_collapsed = true;
        let pane = shell.layout.focused().unwrap();
        attach_live_test_pane(shell, pane, SESSION, cx);
        shell.layout.set_session(pane, SESSION);
        shell.layout.set_chat_view(pane, true);
        shell.chat.now_override = Some(RL_NOW);
        shell.chat.send_input = Some(Box::new(move |bytes| {
            recorded.lock().unwrap().push(bytes);
            true
        }));
        shell.apply_chat_poll(SESSION, snapshot(Vec::new()));
        shell.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(std::sync::Arc::new(StateSource(sessions)), cx);
        });
    });
    cx.run_until_parked();
    shell.update(cx, |shell, cx| {
        shell.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_session_account_features(
                krill_app_core::hosts::HostId::Local,
                FEAT_USAGE | FEAT_SESSION_ACCOUNT_SWITCH | FEAT_PROVIDER_DETAILS,
                cx,
            );
        });
        shell.apply_accounts_snapshot_for_host(krill_app_core::hosts::HostId::Local, snap, cx);
        if let Some(log) = shell.chat.logs.get_mut(&local_key(SESSION)) {
            log.meta.rate_limited = true;
        }
        cx.notify();
    });
    cx.run_until_parked();
    (shell, cx, sent)
}

fn sent_text(sent: &std::sync::Mutex<Vec<Vec<u8>>>) -> String {
    sent.lock()
        .unwrap()
        .iter()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .collect()
}

#[gpui::test]
fn rate_limit_banner_names_weekly_limit_and_switches_inline_without_settings(
    cx: &mut TestAppContext,
) {
    use std::cell::RefCell;
    use std::rc::Rc;
    let (shell, cx, _) = rate_limited_fixture(cx, "chat-rate-limit-inline", true, false);
    let clock = super::rate_limit::reset_clock(RL_RESET).unwrap();
    shell.read_with(cx, |s, cx| {
        let info = s.chat_limit_info(&local_key(SESSION), cx);
        assert_eq!(
            super::rate_limit::banner_words(info, "claude", false),
            (
                ctext::LIMIT_TITLE_WEEKLY.format(&[("agent", "Claude")]),
                ctext::LIMIT_CAN_CONTINUE_AT.format(&[("time", &clock)])
            )
        );
    });
    assert!(cx.debug_bounds("chat-rate-limit-text").is_some());
    assert!(cx.debug_bounds("chat-auto-continue-arm").is_some());

    let switched = Rc::new(RefCell::new(Vec::new()));
    cx.update(|_, cx| {
        let sidebar = shell.read(cx).sidebar.clone();
        let switched = switched.clone();
        cx.subscribe(
            &sidebar,
            move |_, e: &krill_app_core::sidebar::SwitchSessionAccount, _| {
                switched.borrow_mut().push(e.clone());
            },
        )
        .detach();
    });
    let switch_btn = cx.debug_bounds("chat-switch-account").unwrap();
    cx.simulate_click(switch_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).settings().is_none()));
    assert_eq!(
        *switched.borrow(),
        vec![krill_app_core::sidebar::SwitchSessionAccount {
            host: krill_app_core::hosts::HostId::Local,
            session: SESSION,
            provider: krill_proto::usage::ProviderKind::Claude,
            account_id: "other".into(),
        }]
    );
}

#[gpui::test]
fn rate_limit_banner_without_alternatives_opens_account_settings(cx: &mut TestAppContext) {
    let (shell, cx, _) = rate_limited_fixture(cx, "chat-rate-limit-add-account", false, false);
    let switch_btn = cx.debug_bounds("chat-switch-account").unwrap();
    cx.simulate_click(switch_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).settings().is_some()));
    assert!(cx.debug_bounds("admin-center").is_some());
}

#[gpui::test]
fn auto_continue_arms_from_setting_cancels_and_sends_once_after_reset(cx: &mut TestAppContext) {
    use std::time::Duration;
    let (shell, cx, sent) = rate_limited_fixture(cx, "chat-auto-continue", false, true);
    let key = local_key(SESSION);
    // Setting on: the next composer sync schedules it and the banner offers Cancel.
    shell.update(cx, |s, cx| s.sync_auto_continue(&key, cx));
    cx.run_until_parked();
    assert!(shell.read_with(cx, |s, _| s.chat.auto_continue.contains_key(&key)));
    assert!(cx.debug_bounds("chat-auto-continue-scheduled").is_some());
    let cancel = cx.debug_bounds("chat-auto-continue-cancel").unwrap();
    assert!(cx.debug_bounds("chat-auto-continue-arm").is_none());

    // Cancel drops it and the same cycle is not re-armed automatically.
    cx.simulate_click(cancel.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.update(cx, |s, cx| s.sync_auto_continue(&key, cx));
    assert!(shell.read_with(cx, |s, _| !s.chat.auto_continue.contains_key(&key)));
    assert!(cx.debug_bounds("chat-auto-continue-arm").is_some());

    // A hand-sent prompt cancels a manual schedule too.
    let arm = cx.debug_bounds("chat-auto-continue-arm").unwrap();
    cx.simulate_click(arm.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(shell.read_with(cx, |s, _| s.chat.auto_continue.contains_key(&key)));
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let input = s.ensure_chat_input(key.clone(), window, cx);
            input.update(cx, |state, cx| state.set_value("by hand", window, cx));
            s.submit_chat_prompt(&key, window, cx);
        });
    });
    cx.run_until_parked();
    assert!(shell.read_with(cx, |s, _| !s.chat.auto_continue.contains_key(&key)));
    assert!(sent_text(&sent).contains("by hand"));

    // Re-armed by hand, the reset passes and the continue prompt goes out exactly once.
    sent.lock().unwrap().clear();
    let arm = cx.debug_bounds("chat-auto-continue-arm").unwrap();
    cx.simulate_click(arm.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.update(cx, |s, _| s.chat.now_override = Some(RL_RESET + 5));
    cx.executor().advance_clock(Duration::from_secs(65));
    cx.run_until_parked();
    let prompt = ctext::AUTO_CONTINUE_PROMPT.get();
    assert_eq!(sent_text(&sent).matches(prompt).count(), 1);
    assert!(shell.read_with(cx, |s, _| !s.chat.auto_continue.contains_key(&key)));
    cx.executor().advance_clock(Duration::from_secs(120));
    cx.run_until_parked();
    shell.update(cx, |s, cx| s.sync_auto_continue(&key, cx));
    assert_eq!(sent_text(&sent).matches(prompt).count(), 1);
}

#[gpui::test]
fn auto_continue_continues_early_on_account_switch_with_fresh_headroom(cx: &mut TestAppContext) {
    use krill_proto::usage::{AccountInfo, AccountsSnapshot, ProviderKind, Snapshot, Window};
    struct StateSource(krill_app_core::sidebar::Snapshot);
    impl krill_app_core::sidebar::Source for StateSource {
        fn fetch(&self) -> std::io::Result<krill_app_core::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }

    let (shell, cx, sent) = rate_limited_fixture(cx, "chat-auto-continue-switch", true, true);
    let key = local_key(SESSION);
    // Initial rate limit on account `main` arms the timer for RL_RESET (60s away).
    shell.update(cx, |s, cx| s.sync_auto_continue(&key, cx));
    cx.run_until_parked();
    assert!(shell.read_with(cx, |s, _| s.chat.auto_continue.contains_key(&key)));
    assert!(sent_text(&sent).is_empty());

    // Simulate switching the session to `other` (which has fresh headroom) BEFORE RL_RESET!
    let switched_sessions = krill_app_core::sidebar::Snapshot {
        sessions: vec![krill_app_core::sidebar::Session {
            id: SESSION,
            status: krill_proto::control::AgentStatus {
                state: krill_proto::control::AgentState::Done,
                agent: "claude".into(),
                since_unix_ns: 2,
                ..krill_proto::control::AgentStatus::UNKNOWN
            },
            worktree: None,
            cwd: None,
            account: Some(krill_proto::control::SessionAccount {
                session: SESSION,
                account_id: "other".into(),
                label: "other".into(),
            }),
        }],
        trees: std::collections::BTreeMap::new(),
    };
    let fresh_snap = AccountsSnapshot {
        accounts: vec![
            AccountInfo {
                id: "main".into(),
                provider: ProviderKind::Claude,
                label: "main".into(),
                email: None,
                profile_dir: "/synthetic/main".into(),
                active: false,
                host_default: false,
                stale: false,
                usage: Snapshot {
                    five_hour: None,
                    seven_day: Some(Window {
                        percent: 100,
                        resets_at: Some(RL_RESET.to_string()),
                    }),
                },
            },
            AccountInfo {
                id: "other".into(),
                provider: ProviderKind::Claude,
                label: "other".into(),
                email: None,
                profile_dir: "/synthetic/other".into(),
                active: true,
                host_default: false,
                stale: false,
                usage: Snapshot {
                    five_hour: Some(Window {
                        percent: 15,
                        resets_at: Some((RL_RESET + 3_600).to_string()),
                    }),
                    seven_day: None,
                },
            },
        ],
        ..AccountsSnapshot::default()
    };
    shell.update(cx, |s, cx| {
        s.on_chat_account_switch_requested(&key, "other", cx);
        s.sidebar.update(cx, |sidebar, cx| {
            sidebar.connect_source(std::sync::Arc::new(StateSource(switched_sessions)), cx);
        });
    });
    cx.run_until_parked();
    shell.update(cx, |s, cx| {
        s.apply_accounts_snapshot_for_host(krill_app_core::hosts::HostId::Local, fresh_snap, cx);
    });
    cx.run_until_parked();

    // Immediately continued early without waiting for RL_RESET, and only once.
    let prompt = ctext::AUTO_CONTINUE_PROMPT.get();
    assert_eq!(sent_text(&sent).matches(prompt).count(), 1);
    assert!(shell.read_with(cx, |s, _| !s.chat.auto_continue.contains_key(&key)));

    // Re-syncing does not double-send.
    shell.update(cx, |s, cx| s.sync_auto_continue(&key, cx));
    cx.run_until_parked();
    assert_eq!(sent_text(&sent).matches(prompt).count(), 1);
}

#[gpui::test]
fn auto_continue_survives_restart_once_and_respects_explicit_false(cx: &mut TestAppContext) {
    let layout_path = state_path("chat-auto-continue-restart");
    let state_dir = layout_path.parent().expect("state dir").to_path_buf();
    // First run: record a pending cut on `main` resetting at RL_RESET.
    {
        let (shell, cx, sent) = rate_limited_fixture_at(cx, layout_path.clone(), false, true);
        let key = local_key(SESSION);
        shell.update(cx, |s, cx| s.sync_auto_continue(&key, cx));
        cx.run_until_parked();
        assert!(sent_text(&sent).is_empty());
        assert!(state_dir.join("chat-rate-limit-recovery.json").is_file());
    }

    // Second run with setting explicitly OFF (`false`): even after reset elapsed, must NOT send.
    {
        let (shell, cx, sent) = rate_limited_fixture_at(cx, layout_path.clone(), false, false);
        let key = local_key(SESSION);
        shell.update(cx, |s, cx| {
            s.chat.now_override = Some(RL_RESET + 10);
            s.sync_auto_continue(&key, cx);
        });
        cx.run_until_parked();
        assert!(sent_text(&sent).is_empty());
    }

    // Third run with setting ON (`true`) after reset elapsed: continues ONCE and persists `Continued`.
    {
        let (shell, cx, sent) = rate_limited_fixture_at(cx, layout_path.clone(), false, true);
        let key = local_key(SESSION);
        shell.update(cx, |s, cx| {
            s.chat.now_override = Some(RL_RESET + 10);
            s.sync_auto_continue(&key, cx);
        });
        cx.run_until_parked();
        let prompt = ctext::AUTO_CONTINUE_PROMPT.get();
        assert_eq!(sent_text(&sent).matches(prompt).count(), 1);
    }

    // Fourth run (another restart): already persisted as `Continued`, must NEVER double-send!
    {
        let (shell, cx, sent) = rate_limited_fixture_at(cx, layout_path, false, true);
        let key = local_key(SESSION);
        shell.update(cx, |s, cx| {
            s.chat.now_override = Some(RL_RESET + 30);
            s.sync_auto_continue(&key, cx);
        });
        cx.run_until_parked();
        assert!(sent_text(&sent).is_empty());
    }
}

fn bot_for(session: SessionId, paused: bool) -> krill_bots_ui::bots::SidebarBot {
    let id = krill_proto::bots::BotId::new("orca-like").unwrap();
    let mut cfg =
        krill_proto::bots::BotConfig::new(id.clone(), "Shop", "claude", 1_700_000_000_000).unwrap();
    cfg.paused = paused;
    cfg.schedules = vec![krill_proto::bots::BotScheduleConfig {
        id: "s1".into(),
        name: "Morning triage".into(),
        cron: "0 9 * * *".into(),
        prompt: "Triage new issues.".into(),
        enabled: true,
        timezone: None,
    }];
    let summary = krill_proto::bots::BotSessionSummary {
        session_id: session,
        bot_id: id,
        link_id: None,
        pid: 1,
        live: true,
        working: false,
        label: String::new(),
        created_at_ms: 0,
    };
    let mut row = krill_bots_ui::bots::rpc::sidebar_snapshot(
        &[cfg],
        &[],
        &[],
        std::slice::from_ref(&summary),
    )
    .bots
    .remove(0);
    row.sessions = vec![summary];
    row
}

/// Orca's bot chat rows: the Home pill, the paused banner and its Resume, a settled turn folded
/// behind "Worked for" that opens on press, a connector sender tag over the words alone, a
/// schedule's turn as "Scheduled · name" with its instructions, and a memory receipt.
#[gpui::test]
fn bot_chat_draws_orca_rows_and_they_open_on_press(cx: &mut TestAppContext) {
    let at = |kind: ChatItemKind, text: &str, at: &str| ChatItem {
        kind,
        text: text.into(),
        at: at.into(),
        ..Default::default()
    };
    let slack = format!(
        "[Krill external message via slack | channel: C1 | from: Sam Lee (U1)]\n{}",
        krill_proto::bots::fence_quoted_text("배포 언제?")
    );
    let (shell, cx) = fixture(
        cx,
        "chat-bot-orca-rows",
        vec![
            at(ChatItemKind::User, "fix it", "2026-11-22T01:00:00Z"),
            at(ChatItemKind::Assistant, "looking", "2026-11-22T01:00:02Z"),
            at(ChatItemKind::ToolCall, "Read a.rs", "2026-11-22T01:00:05Z"),
            at(ChatItemKind::ToolResult, "ok", "2026-11-22T01:00:06Z"),
            at(ChatItemKind::Assistant, "fixed", "2026-11-22T01:00:16Z"),
            at(ChatItemKind::User, &slack, "2026-11-22T01:01:00Z"),
            at(ChatItemKind::Assistant, "tonight", "2026-11-22T01:01:04Z"),
            at(
                ChatItemKind::User,
                "Triage new issues.",
                "2026-11-22T02:00:00Z",
            ),
            at(
                ChatItemKind::Assistant,
                "nothing urgent",
                "2026-11-22T02:00:09Z",
            ),
        ],
    );
    shell.update(cx, |s, cx| {
        s.sidebar_bots = vec![bot_for(SESSION, true)];
        let bot = s.sidebar_bots[0].id.clone();
        s.chat.bot_notes.insert(
            (krill_app_core::hosts::HostId::Local, bot),
            (
                std::time::Instant::now(),
                memory_notes(&[krill_proto::bots::BotMemoryHistoryEntry {
                    entry_id: format!("turn-s{}-1-1", SESSION.0),
                    bot_id: krill_proto::bots::BotId::new("orca-like").unwrap(),
                    timestamp_ms: 1_795_314_062_000,
                    actor: "bot:orca-like".into(),
                    action: "turn".into(),
                    added: vec!["Ships at 6pm".into()],
                    removed: Vec::new(),
                    before_content: String::new(),
                    after_content: String::new(),
                    undoable: true,
                    undone: false,
                }]),
            ),
        );
        cx.notify();
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("chat-bot-home-banner").is_some());
    assert!(cx.debug_bounds("chat-bot-paused-banner").is_some());
    assert!(cx.debug_bounds("chat-bot-paused-resume").is_some());
    // The first turn folds its work behind "16s 작업 완료"; its answer stays.
    assert!(cx.debug_bounds("chat-work-run-toggle").is_none());
    let bar = cx.debug_bounds("chat-turn-bar-0").unwrap();
    cx.simulate_click(bar.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-work-run-toggle").is_some());
    // A connector's message is #189's source card, not an owner bubble.
    assert!(cx.debug_bounds("chat-asker-tag").is_none());
    // The schedule's turn, then its instructions on press.
    let run = cx.debug_bounds("chat-scheduled-run").unwrap();
    assert!(cx.debug_bounds("chat-scheduled-copy").is_none());
    cx.simulate_click(run.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-scheduled-copy").is_some());
    // The memory receipt opens to its Open memory link.
    let memory = cx.debug_bounds("chat-memory-note").unwrap();
    assert!(cx.debug_bounds("chat-memory-open").is_none());
    cx.simulate_click(memory.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-memory-open").is_some());
    // Orca's message rows: Copy and the time, as icons.
    assert!(cx.debug_bounds("chat-copy-item").is_some());
    assert!(cx.debug_bounds("chat-message-time").is_some());
}

/// The image viewer zooms in and out, fits again, and copies the picture as PNG.
#[gpui::test]
fn chat_image_viewer_zooms_fits_and_copies_png(cx: &mut TestAppContext) {
    let dir = state_path("chat-viewer-zoom");
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("pic.png");
    image::RgbaImage::from_pixel(40, 30, image::Rgba([10, 20, 30, 255]))
        .save(&png)
        .unwrap();
    let (shell, cx) = fixture(cx, "chat-viewer-zoom-ui", vec![]);
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_chat_image_viewer(vec![png.clone()], vec![1], 0, window, cx);
        });
    });
    cx.run_until_parked();
    let fit = cx.debug_bounds("chat-image-viewer-fit").unwrap();
    let zoom_in = cx.debug_bounds("chat-image-viewer-zoom-in").unwrap();
    cx.simulate_click(zoom_in.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let zoom = shell.read_with(cx, |s, _| s.chat.images.viewer.as_ref().unwrap().zoom);
    assert!((zoom - 1.25).abs() < 0.001, "{zoom}");
    cx.simulate_click(fit.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let zoom = shell.read_with(cx, |s, _| s.chat.images.viewer.as_ref().unwrap().zoom);
    assert!((zoom - 1.).abs() < f32::EPSILON);
    let copy = cx.debug_bounds("chat-image-viewer-copy").unwrap();
    cx.simulate_click(copy.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let copied = cx.update(|_, cx| cx.read_from_clipboard());
    assert!(
        copied.is_some_and(|item| item.entries().iter().any(|e| matches!(
            e,
            gpui::ClipboardEntry::Image(img) if img.bytes.starts_with(b"\x89PNG")
        ))),
        "the picture is on the clipboard as PNG"
    );
}

/// Orca's create offer card: "New bot", its name and purpose, Not now and Create bot while open;
/// answered, only its status (and Open once created).
#[gpui::test]
fn bot_create_offer_card_offers_create_and_not_now_until_answered(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(
        cx,
        "chat-bot-offer",
        vec![ChatItem {
            kind: ChatItemKind::User,
            text: "help".into(),
            ..Default::default()
        }],
    );
    let offer = |dismissed: bool| krill_proto::bots::BotOffer {
        offer_id: "offer-1".into(),
        bot_id: krill_proto::bots::BotId::new("orca-like").unwrap(),
        session_id: Some(SESSION.0),
        name: "Ledger".into(),
        purpose: "Keeps the spend sheet.".into(),
        offered_at_ms: 1,
        created: None,
        dismissed,
    };
    let show = |shell: &Entity<Shell>, cx: &mut VisualTestContext, dismissed: bool| {
        shell.update(cx, |s, cx| {
            s.sidebar_bots = vec![bot_for(SESSION, false)];
            let bot = s.sidebar_bots[0].id.clone();
            s.chat.bot_notes.insert(
                (krill_app_core::hosts::HostId::Local, bot),
                (
                    std::time::Instant::now(),
                    offer_notes(vec![offer(dismissed)]),
                ),
            );
            cx.notify();
        });
        cx.run_until_parked();
    };
    show(&shell, cx, false);
    assert!(cx.debug_bounds("chat-create-offer").is_some());
    assert!(cx.debug_bounds("chat-offer-create").is_some());
    assert!(cx.debug_bounds("chat-offer-not-now").is_some());
    show(&shell, cx, true);
    assert!(cx.debug_bounds("chat-create-offer").is_some());
    assert!(cx.debug_bounds("chat-offer-create").is_none());
}

#[gpui::test]
fn memory_note_toggle_bumps_log_revision_for_list_remeasure(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(
        cx,
        "chat-memory-revision",
        vec![ChatItem {
            kind: ChatItemKind::User,
            text: "remember".into(),
            at: "2026-11-22T01:00:00Z".into(),
            ..Default::default()
        }],
    );
    shell.update(cx, |s, cx| {
        s.sidebar_bots = vec![bot_for(SESSION, false)];
        let bot = s.sidebar_bots[0].id.clone();
        s.chat.bot_notes.insert(
            (krill_app_core::hosts::HostId::Local, bot),
            (
                std::time::Instant::now(),
                memory_notes(&[krill_proto::bots::BotMemoryHistoryEntry {
                    entry_id: format!("turn-s{}-1-1", SESSION.0),
                    bot_id: krill_proto::bots::BotId::new("orca-like").unwrap(),
                    timestamp_ms: 1_795_314_062_000,
                    actor: "bot:orca-like".into(),
                    action: "turn".into(),
                    added: vec!["Prefers dark mode".into()],
                    removed: Vec::new(),
                    before_content: String::new(),
                    after_content: String::new(),
                    undoable: true,
                    undone: false,
                }]),
            ),
        );
        cx.notify();
    });
    cx.run_until_parked();
    let before = shell.read_with(cx, |s, _| s.chat.logs[&local_key(SESSION)].revision);
    let memory = cx.debug_bounds("chat-memory-note").unwrap();
    cx.simulate_click(memory.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let after = shell.read_with(cx, |s, _| s.chat.logs[&local_key(SESSION)].revision);
    assert_ne!(
        before, after,
        "toggling open_memory_notes must increment log.revision so ChatList::sync remeasures"
    );
}

#[gpui::test]
fn image_viewer_blocks_workspace_shortcuts_and_cmd_w_closes_viewer_first(cx: &mut TestAppContext) {
    let dir = state_path("chat-viewer-cmd-w");
    std::fs::create_dir_all(&dir).unwrap();
    let png_a = dir.join("a.png");
    let png_b = dir.join("b.png");
    image::RgbaImage::from_pixel(20, 20, image::Rgba([10, 20, 30, 255]))
        .save(&png_a)
        .unwrap();
    image::RgbaImage::from_pixel(20, 20, image::Rgba([40, 50, 60, 255]))
        .save(&png_b)
        .unwrap();
    let (shell, cx) = fixture(cx, "chat-viewer-overlay", vec![]);
    let pane = shell.read_with(cx, |s, _| s.layout.focused().unwrap());
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.open_chat_image_viewer(
                vec![png_a.clone(), png_b.clone()],
                vec![1, 2],
                0,
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert!(shell.read_with(cx, |s, _| s.has_blocking_overlay()));

    // Copy PNG sets copied_at and clears after 1500ms; stepping clears immediately.
    let copy_btn = cx.debug_bounds("chat-image-viewer-copy").unwrap();
    cx.simulate_click(copy_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(1600));
    cx.run_until_parked();

    // SplitRight and ToggleChatView are blocked while the viewer is open.
    cx.dispatch_action(crate::shell::SplitRight);
    cx.dispatch_action(crate::shell::ToggleChatView);
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |s, _| s.layout.visible().len()), 1);
    assert!(shell.read_with(cx, |s, cx| s.is_pane_chat_view(pane, cx)));

    // ClosePane (Cmd+W) closes the image viewer instead of closing the pane behind it.
    cx.dispatch_action(crate::shell::ClosePane);
    cx.run_until_parked();
    assert!(shell.read_with(cx, |s, _| !s.has_chat_image_viewer()));
    assert!(shell.read_with(cx, |s, _| s.layout.pane_exists(pane)));
    std::fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn bot_offer_create_guard_and_host_switch_preserve_open_bot_chat(cx: &mut TestAppContext) {
    use krill_proto::bots::{BotConfig, BotId, BotOffer, BotRpcRequest, BotRpcResponse};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (shell, cx) = fixture(
        cx,
        "chat-bot-offer-host-guard",
        vec![ChatItem {
            kind: ChatItemKind::User,
            text: "hi".into(),
            ..Default::default()
        }],
    );
    let create_calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = Arc::clone(&create_calls);
    let rpc: krill_bots_ui::bots::BotRpcCall = Arc::new(move |req| match req {
        BotRpcRequest::CreateBot(params) => {
            calls_ref.fetch_add(1, Ordering::SeqCst);
            let id = BotId::new("ledger").unwrap();
            let cfg = BotConfig::new(id, &params.name, &params.agent_id, 1).unwrap();
            Ok(BotRpcResponse::Bot(Box::new(cfg)))
        }
        _ => Ok(BotRpcResponse::Ok),
    });
    let offer = BotOffer {
        offer_id: "offer-dup".into(),
        bot_id: BotId::new("orca-like").unwrap(),
        session_id: Some(SESSION.0),
        name: "Ledger".into(),
        purpose: "Tracks budget.".into(),
        offered_at_ms: 1,
        created: None,
        dismissed: false,
    };
    shell.update(cx, |s, cx| {
        s.bot_rpc_override = Some(rpc);
        s.sidebar_bots = vec![bot_for(SESSION, false)];
        let bot_id = s.sidebar_bots[0].id.clone();
        s.chat.bot_souls.insert(
            (krill_app_core::hosts::HostId::Local, bot_id),
            "Local soul".into(),
        );
        s.sync_bot_item_notes(&local_key(SESSION), cx);
        // Double click create_offered_bot synchronously must fire only once.
        s.create_offered_bot(&local_key(SESSION), &offer, cx);
        s.create_offered_bot(&local_key(SESSION), &offer, cx);
        assert!(s.chat.creating_offers.contains("offer-dup"));
        // Switching launch_host to remote keeps the open local bot chat metadata intact.
        s.launch_host = krill_app_core::hosts::HostId::Remote("other".into());
        s.sidebar_bots.clear();
        assert!(s.chat_bot(&local_key(SESSION)).is_some());
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(
        create_calls.load(Ordering::SeqCst),
        1,
        "creating_offers must block duplicate CreateBot RPCs"
    );
    shell.read_with(cx, |s, _| {
        assert!(!s.chat.creating_offers.contains("offer-dup"));
    });
}

#[gpui::test]
fn chat_error_banners_dismiss_on_click_and_input_change(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(cx, "chat-error-dismiss", vec![]);
    let key = local_key(SESSION);
    let input = cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let input = s.ensure_chat_input(key.clone(), window, cx);
            s.chat.prompt_failed.insert(key.clone());
            s.chat.multiline_blocked.insert(key.clone());
            cx.notify();
            input
        })
    });
    cx.run_until_parked();
    let dismiss_prompt = cx.debug_bounds("chat-prompt-error-dismiss").unwrap();
    let dismiss_multiline = cx.debug_bounds("chat-multiline-error-dismiss").unwrap();
    cx.simulate_click(dismiss_prompt.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(shell.read_with(cx, |s, _| !s.chat.prompt_failed.contains(&key)));
    cx.simulate_click(dismiss_multiline.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(shell.read_with(cx, |s, _| !s.chat.multiline_blocked.contains(&key)));

    // Typing (InputEvent::Change) also clears both flags.
    cx.update(|window, cx| {
        shell.update(cx, |s, _cx| {
            s.chat.prompt_failed.insert(key.clone());
            s.chat.multiline_blocked.insert(key.clone());
        });
        input.update(cx, |st, cx| st.focus(window, cx));
    });
    cx.simulate_input("edited");
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert!(!s.chat.prompt_failed.contains(&key));
        assert!(!s.chat.multiline_blocked.contains(&key));
    });
}

#[gpui::test]
fn prune_chat_sessions_drops_cooldown_history_tokens_and_other_inputs(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(cx, "chat-prune-leaks", vec![]);
    let stale_key = local_key(SessionId(9999));
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.chat.stop_cooldown_since.insert(stale_key.clone(), 42);
            s.chat
                .sent_history
                .insert(stale_key.clone(), vec!["old".into()]);
            s.chat.history_cursor.insert(stale_key.clone(), 0);
            s.chat
                .history_saved_draft
                .insert(stale_key.clone(), "saved".into());
            s.chat
                .image_tokens
                .entry(stale_key.clone())
                .or_default()
                .insert(std::path::PathBuf::from("/tmp/a.png"));
            s.ensure_question_other_input(&stale_key, "tool:1", window, cx);
            s.prune_chat_sessions();
            assert!(!s.chat.stop_cooldown_since.contains_key(&stale_key));
            assert!(!s.chat.sent_history.contains_key(&stale_key));
            assert!(!s.chat.history_cursor.contains_key(&stale_key));
            assert!(!s.chat.history_saved_draft.contains_key(&stale_key));
            assert!(!s.chat.image_tokens.contains_key(&stale_key));
            assert!(
                !s.chat
                    .question_other_inputs
                    .contains_key(&(stale_key.clone(), "tool:1".into()))
            );
        });
    });
}

#[gpui::test]
fn remote_bot_without_workspace_footer_does_not_overwrite_clipboard_and_efforts_localize(
    cx: &mut TestAppContext,
) {
    assert_eq!(
        composer::effort_text("minimal"),
        ctext::EFFORT_MINIMAL.get()
    );
    assert_eq!(composer::effort_text("ultra"), ctext::EFFORT_ULTRA.get());
    assert_eq!(
        composer::effort_text("very_high"),
        ctext::EFFORT_XHIGH.get()
    );
    assert_eq!(
        composer::effort_text("very-high"),
        ctext::EFFORT_XHIGH.get()
    );
    assert_eq!(composer::effort_text("none"), ctext::EFFORT_NONE.get());
    assert_eq!(composer::effort_text("off"), ctext::EFFORT_OFF.get());

    let (shell, cx) = fixture(cx, "chat-remote-no-workspace", vec![]);
    let remote = krill_app_core::hosts::HostId::Remote("box".into());
    shell.update(cx, |s, cx| {
        let pane = s.layout.focused().unwrap();
        s.launch_host = remote.clone();
        s.layout.set_host(pane, remote.clone());
        let mut bot = bot_for(SESSION, false);
        bot.host = remote.clone();
        bot.workspace = None;
        s.sidebar_bots = vec![bot];
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("keep-me".into()));
        cx.notify();
    });
    cx.run_until_parked();
    let folder = cx.debug_bounds("chat-composer-folder").unwrap();
    cx.simulate_click(folder.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let clip = cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(
        clip.as_deref(),
        Some("keep-me"),
        "clicking static folder label for remote bot without workspace must not overwrite clipboard"
    );
}

/// Commits 1–4 (`6edf9f05`, `c9e22e23`, `dcde358a`, `028100ad`):
/// - Consecutive activity rows (agent messages, item notes) get even 4px spacing while leaving
///   16px before regular messages.
/// - Item note chips use full available width beyond 256px before truncating, and stay readable
///   (>= 96px) even with a long comment/detail on the same row.
/// - Agent message rows show a plain first-line preview and an always-visible chevron that opens
///   the full details box on click.
#[gpui::test]
fn chat_activity_rows_even_spacing_wide_chips_and_expandable_agent_message(
    cx: &mut TestAppContext,
) {
    let long_title = "Investigate intermittent webhook delivery retries across regional queues";
    let (shell, cx) = fixture(
        cx,
        "chat-activity-sync-2b",
        vec![
            ChatItem {
                kind: ChatItemKind::User,
                text: "[Krill bot mail from Release Bot] Subject: Nightly check\nAll **tests** passed on `src/**/*.rs`.\nDetails on line two.".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::Assistant,
                text: "Acknowledged, continuing the deploy.".into(),
                ..Default::default()
            },
        ],
    );
    shell.update(cx, |s, cx| {
        s.sidebar_bots = vec![bot_for(SESSION, false)];
        let bot_id = s.sidebar_bots[0].id.clone();
        let started = BotNote {
            session: Some(SESSION),
            note: Arc::new(work_run::ItemNote {
                at_ms: 1_000,
                actor: work_run::NoteActor::Bot,
                action: work_run::NoteAction::Status(krill_proto::bots::BotItemStatus::Working),
                status: krill_proto::bots::BotItemStatus::Working,
                title: long_title.into(),
                detail: String::new(),
            }),
        };
        let commented = BotNote {
            session: Some(SESSION),
            note: Arc::new(work_run::ItemNote {
                at_ms: 2_000,
                actor: work_run::NoteActor::Bot,
                action: work_run::NoteAction::Commented,
                status: krill_proto::bots::BotItemStatus::Working,
                title: "Refactor authentication middleware pipeline".into(),
                detail: "Long comment detail that should yield space first before crushing the item chip".into(),
            }),
        };
        s.chat.bot_notes.insert(
            (krill_app_core::hosts::HostId::Local, bot_id),
            (std::time::Instant::now(), vec![started, commented]),
        );
        cx.notify();
    });
    cx.run_until_parked();

    // Commit 3 (dcde358a): agent message row shows sender, first-line preview, always-visible
    // chevron, and opens its details box on click.
    assert!(cx.debug_bounds("chat-notice").is_some());
    assert!(cx.debug_bounds("chat-notice-sender").is_some());
    assert!(cx.debug_bounds("chat-notice-preview").is_some());
    let chevron = cx
        .debug_bounds("chat-notice-chevron")
        .expect("always-visible chevron on multi-line agent message");
    assert!(chevron.size.width > gpui::px(0.));
    assert!(cx.debug_bounds("chat-notice-details").is_none());

    let toggle = cx
        .debug_bounds("chat-notice-toggle")
        .expect("notice toggle row");
    cx.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("chat-notice-details").is_some(),
        "clicking agent message row expands details box"
    );

    // Commits 2 & 4 (c9e22e23, 028100ad): item chip uses width beyond 256px when room permits,
    // and stays >= 96px readable even with a long comment next to it.
    let chip = cx
        .debug_bounds("chat-item-note-chip")
        .expect("item note chip rendered");
    assert!(
        chip.size.width >= gpui::px(96.),
        "item pill must stay readable (>= 96px), got {:?}",
        chip.size.width
    );
}

#[gpui::test]
fn chat_inline_image_token_submit_reconciles_without_duplicate_or_timeout(cx: &mut TestAppContext) {
    use krill_proto::input::Delivery;
    use krill_proto::term_modes::Modes;

    let dir = TestTempDir::new("chat-inline-image-reconcile");
    let png = dir.path().join("shot.png");
    image::RgbaImage::from_pixel(16, 16, image::Rgba([20, 120, 220, 255]))
        .save(&png)
        .unwrap();

    let (shell, cx) = fixture(
        cx,
        "chat-inline-image-reconcile",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "Ready.".into(),
            ..Default::default()
        }],
    );
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let recorded = sent.clone();
    let pane = shell.read_with(cx, |s, _| s.layout.focused().unwrap());
    let key = local_key(SESSION);
    let polled_items = std::sync::Arc::new(std::sync::Mutex::new(Vec::<ChatItem>::new()));
    let poll_source = polled_items.clone();

    let input = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            attach_live_test_pane(shell, pane, SESSION, cx);
            shell.paths = Some(krill_proto::daemon::Paths {
                socket: "/fake/socket".into(),
                holder: "/fake/holder".into(),
                krilld: "/fake/krilld".into(),
            });
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
            shell.chat.send_input = Some(Box::new(move |bytes| {
                recorded.lock().unwrap().push(bytes);
                true
            }));
            shell.chat.prompt_delivery = Some(Delivery::Applied);
            shell.chat.poll_call = Some(std::sync::Arc::new(move |_| {
                let items = std::mem::take(&mut *poll_source.lock().unwrap());
                Ok(krill_proto::control::Reply::AgentHistory(
                    krill_proto::agent_history::Response::ChatPolled(ChatPollSnapshot {
                        agent: "claude".into(),
                        bound: true,
                        items,
                        session_id: "chat-ui-test".into(),
                        transcript_path: "/fake/chat-ui.jsonl".into(),
                        ..Default::default()
                    }),
                ))
            }));
            shell.attach_dropped_paths_to_chat(
                &key,
                &gpui::ExternalPaths(vec![png.clone()].into()),
                window,
                cx,
            );
            shell.ensure_chat_input(key.clone(), window, cx)
        })
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("chat-attachment-image-pill-1").is_some());
    assert_eq!(input.read_with(cx, |s, _| s.tokens().len()), 1);

    // Type directly after `[Image #1]` with no space (`[Image #1]이거 봐`) and submit.
    cx.update(|window, cx| {
        input.update(cx, |state, cx| {
            gpui::EntityInputHandler::replace_text_in_range(state, None, "이거 봐", window, cx);
        });
        shell.update(cx, |shell, cx| {
            shell.submit_chat_prompt(&key, window, cx);
        });
    });
    cx.run_until_parked();

    let first_payload = sent_text(&sent);
    assert!(
        first_payload.contains(&format!("{} 이거 봐", png.display()))
            || first_payload.contains(&format!("'{}' 이거 봐", png.display())),
        "submitted payload must separate image path from text, got {first_payload:?}"
    );
    shell.read_with(cx, |s, _| {
        let pending = &s.chat.pending_prompts[&key];
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].display_text, "이거 봐",
            "pending display_text must strip inline image token span"
        );
    });
    assert!(cx.debug_bounds("chat-pending-card").is_some());

    // Poll back the krilld-stripped user item (`item.text = "이거 봐"`, `item.file_path = png`).
    polled_items.lock().unwrap().push(ChatItem {
        kind: ChatItemKind::User,
        text: "이거 봐".into(),
        file_path: png.display().to_string(),
        ..Default::default()
    });
    cx.executor()
        .advance_clock(krill_proto::input::PASTE_SUBMIT_DELAY + Duration::from_secs(16));
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("chat-pending-card").is_none(),
        "reconciled prompt must not leave a duplicate pending card"
    );
    shell.read_with(cx, |s, cx| {
        assert!(s.chat.pending_prompts.get(&key).is_none_or(Vec::is_empty));
        assert!(
            !s.chat.prompt_review.contains(&key),
            "reconciled inline image prompt must not time out into PROMPT_REVIEW"
        );
        assert!(
            s.chat.draft_attachments.get(&key).is_none_or(Vec::is_empty),
            "reconciled prompt must not restore draft attachments"
        );
        assert!(
            input.read(cx).value().is_empty(),
            "composer must stay empty without false draft restore, got {:?}",
            input.read(cx).value()
        );
        assert!(
            s.chat.logs[&key]
                .items
                .iter()
                .any(|item| item.kind == ChatItemKind::User && item.text == "이거 봐"),
            "polled user item must be present in transcript"
        );
    });

    // Image-only submit (`[Image #1]`) reconciles when krilld strips the path into `file_path`.
    sent.lock().unwrap().clear();
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.attach_dropped_paths_to_chat(
                &key,
                &gpui::ExternalPaths(vec![png.clone()].into()),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.submit_chat_prompt(&key, window, cx);
        });
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(s.chat.pending_prompts[&key][0].display_text, "");
    });
    polled_items.lock().unwrap().push(ChatItem {
        kind: ChatItemKind::User,
        text: String::new(),
        file_path: png.display().to_string(),
        ..Default::default()
    });
    cx.executor()
        .advance_clock(krill_proto::input::PASTE_SUBMIT_DELAY + Duration::from_secs(16));
    cx.run_until_parked();
    shell.read_with(cx, |s, cx| {
        assert!(s.chat.pending_prompts.get(&key).is_none_or(Vec::is_empty));
        assert!(!s.chat.prompt_review.contains(&key));
        assert!(s.chat.draft_attachments.get(&key).is_none_or(Vec::is_empty));
        assert!(input.read(cx).value().is_empty());
    });
}

#[gpui::test]
fn chat_failed_submit_with_image_restores_draft_without_duplicate_image_marker(
    cx: &mut TestAppContext,
) {
    let dir = TestTempDir::new("chat-failed-image-restore");
    let png = dir.path().join("shot.png");
    image::RgbaImage::from_pixel(16, 16, image::Rgba([40, 180, 90, 255]))
        .save(&png)
        .unwrap();

    let (shell, cx) = fixture(
        cx,
        "chat-failed-image-restore",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "Ready.".into(),
            ..Default::default()
        }],
    );
    let pane = shell.read_with(cx, |s, _| s.layout.focused().unwrap());
    let key = local_key(SESSION);
    let input = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            attach_live_test_pane(shell, pane, SESSION, cx);
            shell.chat.send_input = Some(Box::new(|_| false));
            shell.attach_dropped_paths_to_chat(
                &key,
                &gpui::ExternalPaths(vec![png.clone()].into()),
                window,
                cx,
            );
            shell.ensure_chat_input(key.clone(), window, cx)
        })
    });
    cx.run_until_parked();

    cx.update(|window, cx| {
        input.update(cx, |state, cx| {
            gpui::EntityInputHandler::replace_text_in_range(state, None, "이거 봐", window, cx);
        });
        shell.update(cx, |shell, cx| {
            shell.submit_chat_prompt(&key, window, cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("chat-attachment-image-pill-1").is_some());
    assert!(cx.debug_bounds("chat-pending-card").is_none());
    shell.read_with(cx, |s, cx| {
        assert_eq!(s.chat.draft_attachments[&key].len(), 1);
        let state = input.read(cx);
        assert_eq!(state.tokens().len(), 1);
        assert_eq!(
            state.value().matches("[Image #1]").count(),
            1,
            "restored draft must not duplicate raw [Image #1] text, got {:?}",
            state.value()
        );
        assert_eq!(
            strip_image_token_spans(&state.value(), state.tokens()),
            "이거 봐"
        );
    });
}

#[gpui::test]
fn remote_chat_attachments_refuse_local_fallback_and_render_uploaded_preview(
    cx: &mut TestAppContext,
) {
    let dir = TestTempDir::new("chat-remote-attach");
    let local_png = dir.path().join("remote-shot.png");
    image::RgbaImage::from_pixel(24, 24, image::Rgba([220, 60, 40, 255]))
        .save(&local_png)
        .unwrap();
    let png_bytes = std::fs::read(&local_png).unwrap();

    let (shell, cx) = open(cx, state_path("chat-remote-attach-ui"));
    let remote_host = krill_app_core::hosts::HostId::Remote("mac-mini".into());
    let remote_session = SessionId(901);
    let remote_key = (remote_host.clone(), remote_session);
    let pane = shell.read_with(cx, |s, _| s.layout.focused().unwrap());

    shell.update(cx, |shell, cx| {
        shell.sidebar_collapsed = true;
        shell.panel_collapsed = true;
        shell.layout.set_host(pane, remote_host.clone());
        shell.layout.set_session(pane, remote_session);
        shell.layout.set_chat_view(pane, true);
        shell.apply_chat_poll_for_host(
            remote_session,
            remote_host.clone(),
            snapshot(vec![ChatItem {
                kind: ChatItemKind::Assistant,
                text: "Remote ready.".into(),
                ..Default::default()
            }]),
            None,
        );
        cx.notify();
    });
    cx.run_until_parked();

    // 1. Disconnected remote refuses clipboard image paste without staging local paths.
    let clip_img = gpui::Image::from_bytes(gpui::ImageFormat::Png, png_bytes);
    cx.write_to_clipboard(gpui::ClipboardItem::new_image(&clip_img));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.intercept_chat_clipboard_paste(&remote_key, window, cx);
        });
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert!(
            s.chat
                .draft_attachments
                .get(&remote_key)
                .is_none_or(Vec::is_empty),
            "disconnected remote must never stage local clipboard image paths"
        );
        assert_eq!(
            s.notice.as_deref(),
            Some(ctext::ATTACH_REMOTE_UPLOAD_FAILED.get())
        );
    });

    // 2. Disconnected remote refuses dropped files without staging local paths.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.notice = None;
            shell.attach_dropped_paths_to_chat(
                &remote_key,
                &gpui::ExternalPaths(vec![local_png.clone()].into()),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert!(
            s.chat
                .draft_attachments
                .get(&remote_key)
                .is_none_or(Vec::is_empty),
            "disconnected remote must never stage local dropped file paths"
        );
        assert_eq!(
            s.notice.as_deref(),
            Some(ctext::ATTACH_REMOTE_UPLOAD_FAILED.get())
        );
    });

    // 3. Plain text paste still works normally on a disconnected remote pane.
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("hello remote".into()));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.notice = None;
            shell.intercept_chat_clipboard_paste(&remote_key, window, cx);
        });
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, cx| {
        assert_eq!(s.notice, None);
        assert_eq!(
            s.chat.inputs[&remote_key].0.read(cx).value().as_ref(),
            "hello remote"
        );
    });

    // 4. Successful remote upload keeps remote path for payload, local preview for thumbnails/viewer,
    //    and renders settled remote user bubbles with local preview while unknown remote paths stay icons.
    let remote_uploaded_path = "/var/folders/remote/krill-paste.test/remote-shot.png";
    let local_att = DraftAttachment::from_path(local_png.clone()).unwrap();
    let uploaded =
        upload_chat_attachments_with(vec![local_att], |_| Some(remote_uploaded_path.to_owned()))
            .unwrap();
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let recorded = sent.clone();
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            attach_live_test_pane(shell, pane, remote_session, cx);
            if let Some(live) = shell.live.get_mut(&pane) {
                live.target = Some(krill_app_core::hosts::HostTarget::remote(
                    "mac-mini",
                    std::path::PathBuf::from("/fake/remote.sock"),
                ));
            }
            shell.chat.send_input = Some(Box::new(move |bytes| {
                recorded.lock().unwrap().push(bytes);
                true
            }));
            shell.chat.inputs[&remote_key]
                .0
                .update(cx, |state, cx| state.set_value(String::new(), window, cx));
            shell.stage_chat_attachments(&remote_key, uploaded, cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("chat-attachment-image-pill-1").is_some());
    let composer_thumb = cx.debug_bounds("chat-attachment-image-0").unwrap();
    shell.read_with(cx, |s, _| {
        assert_eq!(
            s.chat
                .images
                .resolve_remote_preview(std::path::Path::new(remote_uploaded_path)),
            Some(local_png.as_path())
        );
        assert!(matches!(
            s.chat.images.thumbs_for_test(&local_png),
            Some(super::images::Loaded::Ready(_))
        ));
    });
    cx.simulate_click(composer_thumb.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        let viewer = s.chat.images.viewer.as_ref().unwrap();
        assert_eq!(viewer.paths, vec![local_png.clone()]);
    });
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // Submit with text: payload uses the REMOTE path, never the local path.
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let input = shell.chat.inputs[&remote_key].0.clone();
            input.update(cx, |state, cx| {
                gpui::EntityInputHandler::replace_text_in_range(
                    state,
                    None,
                    "원격 확인",
                    window,
                    cx,
                );
            });
            shell.submit_chat_prompt(&remote_key, window, cx);
        });
    });
    cx.run_until_parked();
    let payload = sent_text(&sent);
    assert!(
        payload.contains(&format!("{remote_uploaded_path} 원격 확인")),
        "remote submit must send uploaded remote path, got {payload:?}"
    );
    assert!(
        !payload.contains(&local_png.display().to_string()),
        "remote submit must not leak local path, got {payload:?}"
    );

    // Poll settled remote user bubble with 1 uploaded image + 1 unknown remote-only image.
    shell.update(cx, |shell, cx| {
        shell.apply_chat_poll_for_host(
            remote_session,
            remote_host.clone(),
            snapshot(vec![ChatItem {
                kind: ChatItemKind::User,
                text: "원격 확인".into(),
                file_path: format!("{remote_uploaded_path}\n/remote/only/unknown.png"),
                ..Default::default()
            }]),
            None,
        );
        cx.notify();
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("chat-pending-card").is_none());
    let uploaded_tile = cx.debug_bounds("chat-message-image-0").unwrap();
    let unknown_tile = cx.debug_bounds("chat-message-image-1").unwrap();

    // Unknown remote image stays non-readable and does not open the viewer.
    cx.simulate_click(unknown_tile.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-image-viewer").is_none());

    // Locally-uploaded remote image opens the viewer using its local preview path.
    cx.simulate_click(uploaded_tile.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-image-viewer").is_some());
    shell.read_with(cx, |s, _| {
        let viewer = s.chat.images.viewer.as_ref().unwrap();
        assert_eq!(viewer.paths, vec![local_png.clone()]);
        assert_eq!(viewer.numbers, vec![1]);
    });
}

#[gpui::test]
fn wave2b_bot_chat_flush_stage_banner_autohide_side_chat_and_resume_search(
    cx: &mut TestAppContext,
) {
    let (shell, cx) = open(cx, state_path("wave2b-bot-stage"));
    let bot_id = krill_proto::bots::BotId::new("krillbot").unwrap();
    let key = local_key(SESSION);
    set_phase2b_session_state(&shell, cx, krill_proto::control::AgentState::Working, 100);
    let pane = shell.update(cx, |s, cx| {
        s.sidebar_collapsed = true;
        s.panel_collapsed = true;
        let pane = s.layout.focused().unwrap();
        attach_live_test_pane(s, pane, SESSION, cx);
        s.layout.set_session(pane, SESSION);
        s.layout.set_chat_view(pane, true);
        let cfg =
            krill_proto::bots::BotConfig::new(bot_id.clone(), "크릴봇", "agy", 1_700_000_000_000)
                .unwrap();
        let summary = krill_proto::bots::BotSessionSummary {
            session_id: SESSION,
            bot_id: bot_id.clone(),
            link_id: None,
            pid: 1,
            live: true,
            working: false,
            label: String::new(),
            created_at_ms: 0,
        };
        let mut row = krill_bots_ui::bots::rpc::sidebar_snapshot(
            &[cfg],
            &[],
            &[],
            std::slice::from_ref(&summary),
        )
        .bots
        .remove(0);
        row.sessions = vec![summary];
        s.sidebar_bots = vec![row];
        s.chat.bot_souls.insert(
            (krill_app_core::hosts::HostId::Local, bot_id.clone()),
            "CI 모니터링과 이슈 분류를 담당하는 크릴 봇입니다.".into(),
        );
        s.chat.agy_version = Some("1.5.16".into());
        s.chat.agy_version_probe_started = true;
        s.apply_chat_poll(
            SESSION,
            ChatPollSnapshot {
                bound: false,
                reset: true,
                agent: "agy".into(),
                items: Vec::new(),
                ..Default::default()
            },
        );
        cx.notify();
        pane
    });
    cx.run_until_parked();

    // Flush stage removes outer pane-card-stage border on bot chats.
    assert!(cx.debug_bounds("pane-card-stage").is_none());
    // Unbound first-run persona shows BotFirstScreen steps card and bot home banner.
    assert!(cx.debug_bounds("chat-bot-first-screen").is_some());
    assert!(cx.debug_bounds("chat-bot-home-banner").is_some());
    assert!(cx.debug_bounds("chat-composer-agy-auth").is_some());
    assert!(cx.debug_bounds("chat-composer-permission").is_some());

    // Bound empty persona transitions to BotHowToAsk.
    shell.update(cx, |s, cx| {
        s.apply_chat_poll(
            SESSION,
            ChatPollSnapshot {
                bound: true,
                reset: true,
                agent: "agy".into(),
                items: Vec::new(),
                ..Default::default()
            },
        );
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-bot-how-to-ask").is_some());

    // Narrow width <= 264px auto-hides the bot home banner (Orca bot-home-banner.css).
    shell.update(cx, |s, cx| {
        s.chat.pane_widths.insert(pane, px(240.0));
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-bot-home-banner").is_none());

    // Restore wide width and verify ContextCleared divider renders.
    shell.update(cx, |s, cx| {
        s.chat.pane_widths.insert(pane, px(820.0));
        s.apply_chat_poll(
            SESSION,
            ChatPollSnapshot {
                bound: true,
                reset: true,
                agent: "agy".into(),
                items: vec![ChatItem {
                    kind: ChatItemKind::User,
                    text: "/clear".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-bot-home-banner").is_some());
    assert!(cx.debug_bounds("chat-context-cleared-divider").is_some());

    // `#42129`: typing `/btw` displays the Side chat pill in the composer.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let input = s.ensure_chat_input(key.clone(), window, cx);
            input.update(cx, |st, cx| st.set_value("/btw quick question", window, cx));
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-side-chat-pill").is_some());

    // `#47815`: `/resume <query>` opens immediate session search popover.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let input = s.ensure_chat_input(key.clone(), window, cx);
            input.update(cx, |st, cx| st.set_value("/resume triage", window, cx));
            s.submit_chat_prompt(&key, window, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-resume-search-popover").is_some());
}

#[gpui::test]
fn wave2b_question_batch_worker_dock_auto_close_and_rate_limit_switch(cx: &mut TestAppContext) {
    let (shell, cx) = open(cx, state_path("wave2b-interactive"));
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let recorded = sent.clone();
    let key = local_key(SESSION);

    set_phase2b_session_state(&shell, cx, krill_proto::control::AgentState::Waiting, 101);
    shell.update(cx, |s, cx| {
        s.sidebar_collapsed = true;
        s.panel_collapsed = true;
        let pane = s.layout.focused().unwrap();
        attach_live_test_pane(s, pane, SESSION, cx);
        s.layout.set_session(pane, SESSION);
        s.layout.set_chat_view(pane, true);
        inject_phase2b_screen(s, pane, "Which runtime?\n❯ 1. Node\n  2. Rust", cx);
        s.chat.send_input = Some(Box::new(move |bytes| {
            recorded.lock().unwrap().push(bytes);
            true
        }));

        // Inject orchestration snapshot with 1 Active + 1 Stopped worker for this chat,
        // and also mark SESSION as a worker dispatch for `#43521` auto-close banner test.
        let now_secs = 1_800_000_000u64;
        let now_ms = now_secs * 1_000;
        s.chat.now_override = Some(now_secs);
        let mut orch = krill_proto::orchestration::OrchestrationStore::default();
        orch.runs.push(krill_proto::orchestration::Run {
            id: "run-1".into(),
            title: "Wave 2B".into(),
            coordinator_session_id: Some(SESSION.0),
            status: krill_proto::orchestration::RunStatus::Active,
            created_at_ms: now_ms - 60_000,
            updated_at_ms: now_ms,
        });
        orch.dispatches.push(krill_proto::orchestration::Dispatch {
            id: "disp-active".into(),
            run_id: "run-1".into(),
            task_id: None,
            worker_name: "researcher".into(),
            agent: "agy".into(),
            session_id: None,
            depth: 1,
            cwd: "/tmp".into(),
            prompt: "Investigate long background task description wrapping cleanly across lines"
                .into(),
            status: krill_proto::orchestration::DispatchStatus::Active,
            summary: None,
            archived_output: None,
            started_at_ms: now_ms - 30_000,
            ended_at_ms: None,
            release_state: krill_proto::orchestration::DispatchReleaseState::Active,
            session_origin: krill_proto::orchestration::DispatchSessionOrigin::Created,
            model: None,
            effort: None,
            retry_of: None,
            request_id: None,
        });
        orch.dispatches.push(krill_proto::orchestration::Dispatch {
            id: "disp-stopped".into(),
            run_id: "run-1".into(),
            task_id: None,
            worker_name: "linter".into(),
            agent: "codex".into(),
            session_id: Some(902),
            depth: 1,
            cwd: "/tmp".into(),
            prompt: "Run linter".into(),
            status: krill_proto::orchestration::DispatchStatus::Stopped,
            summary: Some("Stopped by user".into()),
            archived_output: None,
            started_at_ms: now_ms - 30_000,
            ended_at_ms: Some(now_ms - 5_000),
            release_state: krill_proto::orchestration::DispatchReleaseState::Active,
            session_origin: krill_proto::orchestration::DispatchSessionOrigin::Created,
            model: None,
            effort: None,
            retry_of: None,
            request_id: None,
        });
        orch.dispatches.push(krill_proto::orchestration::Dispatch {
            id: "disp-self".into(),
            run_id: "run-parent".into(),
            task_id: None,
            worker_name: "self-worker".into(),
            agent: "agy".into(),
            session_id: Some(SESSION.0),
            depth: 1,
            cwd: "/tmp".into(),
            prompt: "Self temp worker".into(),
            status: krill_proto::orchestration::DispatchStatus::Active,
            summary: None,
            archived_output: None,
            started_at_ms: now_ms - 30_000,
            ended_at_ms: None,
            release_state: krill_proto::orchestration::DispatchReleaseState::Active,
            session_origin: krill_proto::orchestration::DispatchSessionOrigin::Created,
            model: None,
            effort: None,
            retry_of: None,
            request_id: None,
        });
        s.orch_snapshot = std::sync::Arc::new(orch);

        // Multi-question batch (2 consecutive questions).
        s.apply_chat_poll(
            SESSION,
            ChatPollSnapshot {
                bound: true,
                reset: true,
                agent: "agy".into(),
                session_id: "s-w2b".into(),
                transcript_path: "/tmp/w2b.jsonl".into(),
                items: vec![
                    ChatItem {
                        kind: ChatItemKind::Question,
                        tool_id: "q-1".into(),
                        title: "Runtime".into(),
                        text: "Which runtime?".into(),
                        options: vec![
                            krill_proto::agent_history::ChatQuestionOption {
                                label: "Node".into(),
                                description: String::new(),
                            },
                            krill_proto::agent_history::ChatQuestionOption {
                                label: "Rust".into(),
                                description: String::new(),
                            },
                        ],
                        ..Default::default()
                    },
                    ChatItem {
                        kind: ChatItemKind::Question,
                        tool_id: "q-2".into(),
                        title: "Target".into(),
                        text: "Deploy target?".into(),
                        options: vec![
                            krill_proto::agent_history::ChatQuestionOption {
                                label: "Local".into(),
                                description: String::new(),
                            },
                            krill_proto::agent_history::ChatQuestionOption {
                                label: "Cloud".into(),
                                description: String::new(),
                            },
                        ],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
        cx.notify();
    });
    cx.run_until_parked();

    // `#43521` worker auto-close banner visible and dismisses on Keep open.
    let keep_btn = cx
        .debug_bounds("chat-worker-keep-open")
        .expect("worker auto-close banner visible");
    cx.simulate_click(keep_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-worker-auto-close-banner").is_none());

    // Combined worker dock renders, expands on click, and Clear all hides settled workers.
    let dock_toggle = cx
        .debug_bounds("chat-worker-dock-toggle")
        .expect("worker dock visible");
    cx.simulate_click(dock_toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-worker-dock-list").is_some());
    assert!(cx.debug_bounds("chat-worker-dock-row-0").is_some());
    assert!(cx.debug_bounds("chat-worker-dock-row-1").is_some());
    let clear_btn = cx
        .debug_bounds("chat-worker-dock-clear")
        .expect("clear all shown for settled worker");
    cx.simulate_click(clear_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-worker-dock-row-1").is_none());

    // `#46963`/`#47959`/`#42789`: 1-at-a-time question batch + Skip sends answered steps.
    assert!(cx.debug_bounds("chat-question-batch").is_some());
    let opt0 = cx
        .debug_bounds("chat-question-batch-option-0")
        .expect("step 1 option 0");
    cx.simulate_click(opt0.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let skip_btn = cx
        .debug_bounds("chat-question-batch-skip")
        .expect("skip button on step 2");
    cx.simulate_click(skip_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![b"\r".to_vec()],
        "Skip must submit already-answered step 1 via terminal confirm"
    );

    // `#47079`: rate-limited chat with unknown reset time shows Continue after account switch.
    shell.update(cx, |s, cx| {
        let log = s.chat.logs.entry(key.clone()).or_default();
        log.meta.rate_limited = true;
        log.rate_limit_account_switched = true;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-rate-limit-banner").is_some());
    assert!(cx.debug_bounds("chat-rate-limit-continue").is_some());
}

#[gpui::test]
fn s2_16_18_19_20_wave2b_permission_host_cards_steer_queue_and_lifecycle(cx: &mut TestAppContext) {
    use krill_app_core::hosts::HostId;
    use krill_proto::agent::Permission;
    use krill_proto::bots::{BotConfig, BotId, BotMcpToolConfig, BotRpcRequest, BotRpcResponse};

    let (shell, cx) = open(cx, state_path("wave2b-swarm-fixes"));
    let remote = HostId::Remote("remote-box".into());
    let bot_id = BotId::new("ops-bot").unwrap();

    // S2-16: set_bot_permission sends UpdateBot to the pane's host (not launch_host) & notifies on error.
    let local_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let remote_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let local_hits_c = local_hits.clone();
    let remote_hits_c = remote_hits.clone();
    let local_rpc: krill_bots_ui::bots::BotRpcCall = std::sync::Arc::new(move |_| {
        local_hits_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(BotRpcResponse::Ok)
    });
    let remote_err_rpc: krill_bots_ui::bots::BotRpcCall = std::sync::Arc::new(move |req| {
        if matches!(req, BotRpcRequest::UpdateBot(_)) {
            remote_hits_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Err("remote permission denied".into());
        }
        Ok(BotRpcResponse::Ok)
    });
    shell.update(cx, |s, cx| {
        s.launch_host = HostId::Local;
        s.bot_rpc_overrides.insert(HostId::Local, local_rpc);
        s.bot_rpc_overrides.insert(remote.clone(), remote_err_rpc);
        s.set_bot_permission(remote.clone(), bot_id.clone(), Permission::Yolo, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        local_hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "S2-16: UpdateBot must not be routed to Local launch_host"
    );
    assert_eq!(
        remote_hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "S2-16: UpdateBot must be routed to the pane's remote host"
    );
    assert_eq!(
        shell.read_with(cx, |s, _| s.notice.clone()).as_deref(),
        Some("remote permission denied")
    );

    // S2-18: Interactive cards scope resolution by (ChatKey, abs_idx), Connect/ImportPlan do not
    // fake resolution, and ToolAdded Remove calls GetBot + UpdateBot via real RPC.
    let removed_tools = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let removed_tools_c = removed_tools.clone();
    let tool_rpc: krill_bots_ui::bots::BotRpcCall = std::sync::Arc::new({
        let bot_id = bot_id.clone();
        move |req| match req {
            BotRpcRequest::GetBot { .. } => {
                let mut cfg = BotConfig::new(bot_id.clone(), "Ops", "agy", 1).unwrap();
                cfg.mcp_tools.push(BotMcpToolConfig {
                    id: "linear_query".into(),
                    name: "linear_query".into(),
                    endpoint_url: None,
                    command: Vec::new(),
                    auth_kind: krill_proto::bots::BotMcpAuthKind::None,
                    header_names: Vec::new(),
                    secret_ref: None,
                    enabled: true,
                });
                Ok(BotRpcResponse::Bot(Box::new(cfg)))
            }
            BotRpcRequest::UpdateBot(params) => {
                if let Some(tools) = &params.mcp_tools {
                    *removed_tools_c.lock().unwrap() = tools.iter().map(|t| t.id.clone()).collect();
                }
                let cfg = BotConfig::new(bot_id.clone(), "Ops", "agy", 2).unwrap();
                Ok(BotRpcResponse::Bot(Box::new(cfg)))
            }
            _ => Ok(BotRpcResponse::Ok),
        }
    });
    let key = local_key(SESSION);
    shell.update(cx, |s, cx| {
        s.sidebar_collapsed = true;
        s.panel_collapsed = true;
        s.bot_rpc_overrides.clear();
        s.bot_rpc_override = Some(tool_rpc);
        let pane = s.layout.focused().unwrap();
        attach_live_test_pane(s, pane, SESSION, cx);
        s.layout.set_session(pane, SESSION);
        s.layout.set_chat_view(pane, true);
        let cfg = BotConfig::new(bot_id.clone(), "Ops", "agy", 1).unwrap();
        let summary = krill_proto::bots::BotSessionSummary {
            session_id: SESSION,
            bot_id: bot_id.clone(),
            link_id: None,
            pid: 1,
            live: true,
            working: false,
            label: String::new(),
            created_at_ms: 0,
        };
        let mut row = krill_bots_ui::bots::rpc::sidebar_snapshot(
            &[cfg],
            &[],
            &[],
            std::slice::from_ref(&summary),
        )
        .bots
        .remove(0);
        row.sessions = vec![summary];
        s.sidebar_bots = vec![row];
        s.apply_chat_poll(
            SESSION,
            ChatPollSnapshot {
                bound: true,
                reset: true,
                agent: "agy".into(),
                items: vec![
                    ChatItem {
                        kind: ChatItemKind::Assistant,
                        text: "[Krill card: connector-offer | same-id | Slack | Connect Slack | Read channels]"
                            .into(),
                        ..Default::default()
                    },
                    ChatItem {
                        kind: ChatItemKind::Assistant,
                        text: "[Krill card: connector-offer | same-id | Slack | Connect Slack 2 | Read channels]"
                            .into(),
                        ..Default::default()
                    },
                    ChatItem {
                        kind: ChatItemKind::Assistant,
                        text: "[Krill card: tool-added | t1 | linear_query | Added to tools]".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
        cx.notify();
    });
    cx.run_until_parked();

    // Clicking Connect opens Connectors tab WITHOUT writing a fake Connected resolution.
    let conn_btn = cx
        .debug_bounds("chat-connector-offer-connect")
        .expect("connector offer connect button");
    cx.simulate_click(conn_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert!(!s.panel_collapsed);
        assert_eq!(
            s.bot_ui.panel.tab,
            crate::shell::bot_panel::BotPanelTab::Connectors
        );
        assert!(
            s.chat.resolved_bot_cards.is_empty(),
            "Connect must not mark card Connected before OAuth finishes"
        );
    });

    // Dismissing card at abs_idx 0 must NOT dismiss card at abs_idx 1 even with identical card_id.
    shell.update(cx, |s, cx| {
        s.chat
            .resolved_bot_cards
            .insert((key.clone(), 0), super::BotCardResolution::Dismissed);
        cx.notify();
    });
    assert!(
        !shell.read_with(cx, |s, _| s
            .chat
            .resolved_bot_cards
            .contains_key(&(key.clone(), 1))),
        "same static card_id at different abs_idx must remain independent"
    );

    // Clicking Remove on ToolAdded invokes GetBot + UpdateBot and only then marks (key, 2) Removed.
    shell.update(cx, |s, cx| {
        s.remove_bot_tool_from_card(key.clone(), 2, bot_id.clone(), "linear_query", cx);
    });
    cx.run_until_parked();
    assert!(removed_tools.lock().unwrap().is_empty());
    assert_eq!(
        shell.read_with(cx, |s, _| s
            .chat
            .resolved_bot_cards
            .get(&(key.clone(), 2))
            .copied()),
        Some(super::BotCardResolution::Removed)
    );

    // S2-19: steer_queued_chat_prompt preserves queued message & attachments when multiline paste blocks.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let log = s.chat.logs.entry(key.clone()).or_default();
            log.queued_prompts.push_back(super::QueuedChatPrompt {
                id: 77,
                display_text: "line1\nline2".into(),
                submitted_payload: "line1\nline2".into(),
                attachments: vec![super::DraftAttachment {
                    display_name: "spec.txt".into(),
                    path: std::path::PathBuf::from("/tmp/spec.txt"),
                    preview_path: None,
                    byte_len: 12,
                    is_image: false,
                }],
            });
            s.steer_queued_chat_prompt(&key, 77, window, cx);
            assert!(
                s.chat.multiline_blocked.contains(&key),
                "multiline prompt without bracketed paste must trigger paste confirmation"
            );
            let q = s
                .chat
                .logs
                .get(&key)
                .and_then(|l| l.queued_prompts.front())
                .expect("S2-19: queued prompt must NOT be deleted when multiline paste blocks");
            assert_eq!(q.id, 77);
            assert_eq!(q.attachments.len(), 1);
        });
    });

    // S2-20: rebind_chat_session migrates all Wave 2B maps & resets rate-limit flags; prune drops closed state.
    let rebind_to = SessionId(555);
    let stale_key = local_key(SessionId(8888));
    cx.update(|_, cx| {
        shell.update(cx, |s, cx| {
            let pane = s.layout.focused().unwrap();
            s.layout.new_tab_after(0);
            let stale_pane = s.layout.focused().unwrap();
            s.layout.close_tab(1);
            s.layout.focus(pane);
            let log = s.chat.logs.entry(key.clone()).or_default();
            log.rate_limited_account = Some("acct-old".into());
            log.rate_limit_account_switched = true;
            s.chat.permission_picker.insert(key.clone());
            s.chat.agy_auth_picker.insert(key.clone());
            s.chat.worker_dock_expanded.insert(key.clone());
            s.chat.worker_keep_open.insert(SESSION);
            s.chat.question_batch_step.insert((key.clone(), 0), 1);
            s.chat.question_batch_answers.insert(
                (key.clone(), 0),
                std::collections::BTreeMap::from([(0, vec![1])]),
            );
            s.chat
                .approval_focus
                .insert((key.clone(), "req:1".into()), cx.focus_handle());
            s.chat
                .approval_auto_focused
                .insert((key.clone(), "req:1".into()));
            s.chat.resume_toast = Some((key.clone(), "ready".into()));
            s.chat.resume_search.insert(key.clone(), "triage".into());
            s.chat
                .side_chat_mode
                .insert(key.clone(), super::side_chat::SideChatPrefix::btw());
            s.chat.editing_queued.insert(key.clone());
            s.chat.queued_focused.insert(key.clone(), 42);
            s.chat.selection_quote = Some(super::selection_quote::ActiveSelectionQuote {
                key: key.clone(),
                text: "quoted".into(),
                _anchor: None,
            });

            s.rebind_chat_session(&HostId::Local, SESSION, rebind_to, &[pane]);
            let new_key = local_key(rebind_to);
            let rebound_log = s.chat.logs.get(&new_key).unwrap();
            assert!(rebound_log.rate_limited_account.is_none());
            assert!(!rebound_log.rate_limit_account_switched);
            assert!(s.chat.permission_picker.contains(&new_key));
            assert!(s.chat.agy_auth_picker.contains(&new_key));
            assert!(s.chat.worker_dock_expanded.contains(&new_key));
            assert!(s.chat.worker_keep_open.contains(&rebind_to));
            assert_eq!(
                s.chat.side_chat_mode.get(&new_key),
                Some(&super::side_chat::SideChatPrefix::btw())
            );
            assert!(s.chat.editing_queued.contains(&new_key));
            assert_eq!(s.chat.queued_focused.get(&new_key), Some(&42));
            assert_eq!(
                s.chat.selection_quote.as_ref().map(|sq| &sq.key),
                Some(&new_key)
            );
            assert_eq!(
                s.chat.question_batch_step.get(&(new_key.clone(), 0)),
                Some(&1)
            );
            assert!(
                s.chat
                    .question_batch_answers
                    .contains_key(&(new_key.clone(), 0))
            );
            assert!(
                s.chat
                    .approval_focus
                    .contains_key(&(new_key.clone(), "req:1".into()))
            );
            assert!(
                s.chat
                    .approval_auto_focused
                    .contains(&(new_key.clone(), "req:1".into()))
            );
            assert_eq!(s.chat.resume_toast.as_ref().map(|(k, _)| k), Some(&new_key));
            assert_eq!(
                s.chat.resume_search.get(&new_key).map(String::as_str),
                Some("triage")
            );
            assert_eq!(
                s.chat
                    .resolved_bot_cards
                    .get(&(new_key.clone(), 2))
                    .copied(),
                Some(super::BotCardResolution::Removed)
            );

            // Seed stale entries and verify prune_chat_sessions cleans all of them.
            s.chat.pane_widths.insert(stale_pane, px(300.0));
            s.chat.permission_picker.insert(stale_key.clone());
            s.chat.agy_auth_picker.insert(stale_key.clone());
            s.chat.worker_dock_expanded.insert(stale_key.clone());
            s.chat.worker_dock_cleared.insert("gone-dispatch".into());
            s.chat
                .worker_dock_owner_stopped
                .insert("gone-owner-stop".into());
            s.chat.worker_keep_open.insert(SessionId(8888));
            s.chat.worker_banner_kept.insert("gone-banner".into());
            s.chat
                .side_chat_mode
                .insert(stale_key.clone(), super::side_chat::SideChatPrefix::btw());
            s.chat.editing_queued.insert(stale_key.clone());
            s.chat.queued_focused.insert(stale_key.clone(), 99);
            s.chat.selection_quote = Some(super::selection_quote::ActiveSelectionQuote {
                key: stale_key.clone(),
                text: "stale".into(),
                _anchor: None,
            });
            s.chat.selection_quote_dismissed = Some("stale".into());
            s.chat.question_batch_step.insert((stale_key.clone(), 0), 1);
            s.chat
                .question_batch_answers
                .insert((stale_key.clone(), 0), std::collections::BTreeMap::new());
            s.chat
                .approval_focus
                .insert((stale_key.clone(), "r".into()), cx.focus_handle());
            s.chat
                .approval_auto_focused
                .insert((stale_key.clone(), "r".into()));
            s.chat.resume_toast = Some((stale_key.clone(), "stale".into()));
            s.chat.resume_search.insert(stale_key.clone(), "old".into());
            s.chat
                .resolved_bot_cards
                .insert((stale_key.clone(), 0), super::BotCardResolution::Dismissed);

            s.prune_chat_sessions();
            assert!(!s.chat.pane_widths.contains_key(&stale_pane));
            assert!(!s.chat.permission_picker.contains(&stale_key));
            assert!(!s.chat.agy_auth_picker.contains(&stale_key));
            assert!(!s.chat.worker_dock_expanded.contains(&stale_key));
            assert!(!s.chat.worker_dock_cleared.contains("gone-dispatch"));
            assert!(!s.chat.worker_dock_owner_stopped.contains("gone-owner-stop"));
            assert!(!s.chat.worker_keep_open.contains(&SessionId(8888)));
            assert!(!s.chat.worker_banner_kept.contains("gone-banner"));
            assert!(!s.chat.side_chat_mode.contains_key(&stale_key));
            assert!(!s.chat.editing_queued.contains(&stale_key));
            assert!(!s.chat.queued_focused.contains_key(&stale_key));
            assert!(s.chat.selection_quote.is_none());
            assert!(s.chat.selection_quote_dismissed.is_none());
            assert!(
                !s.chat
                    .question_batch_step
                    .contains_key(&(stale_key.clone(), 0))
            );
            assert!(
                !s.chat
                    .question_batch_answers
                    .contains_key(&(stale_key.clone(), 0))
            );
            assert!(
                !s.chat
                    .approval_focus
                    .contains_key(&(stale_key.clone(), "r".into()))
            );
            assert!(
                !s.chat
                    .approval_auto_focused
                    .contains(&(stale_key.clone(), "r".into()))
            );
            assert!(s.chat.resume_toast.is_none());
            assert!(!s.chat.resume_search.contains_key(&stale_key));
            assert!(
                !s.chat
                    .resolved_bot_cards
                    .contains_key(&(stale_key.clone(), 0))
            );
        });
    });
}

#[gpui::test]
fn s2_17_question_batch_cleans_empty_multi_submits_all_steps_and_guards_stale(
    cx: &mut TestAppContext,
) {
    let (shell, cx) = open(cx, state_path("wave2b-qbatch-s2-17"));
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let recorded = sent.clone();
    let key = local_key(SESSION);

    set_phase2b_session_state(&shell, cx, krill_proto::control::AgentState::Waiting, 102);
    let pane = shell.update(cx, |s, cx| {
        s.sidebar_collapsed = true;
        s.panel_collapsed = true;
        let pane = s.layout.focused().unwrap();
        attach_live_test_pane(s, pane, SESSION, cx);
        s.layout.set_session(pane, SESSION);
        s.layout.set_chat_view(pane, true);
        inject_phase2b_screen(s, pane, "Features?\n❯ [ ] Lint\n  [ ] Test", cx);
        s.chat.send_input = Some(Box::new(move |bytes| {
            recorded.lock().unwrap().push(bytes);
            true
        }));
        s.apply_chat_poll(
            SESSION,
            ChatPollSnapshot {
                bound: true,
                reset: true,
                agent: "agy".into(),
                session_id: "s-q17".into(),
                transcript_path: "/tmp/q17.jsonl".into(),
                items: vec![
                    ChatItem {
                        kind: ChatItemKind::Question,
                        tool_id: "q-1".into(),
                        title: "Features".into(),
                        text: "Features?".into(),
                        multi_select: true,
                        options: vec![
                            krill_proto::agent_history::ChatQuestionOption {
                                label: "Lint".into(),
                                description: String::new(),
                            },
                            krill_proto::agent_history::ChatQuestionOption {
                                label: "Test".into(),
                                description: String::new(),
                            },
                        ],
                        ..Default::default()
                    },
                    ChatItem {
                        kind: ChatItemKind::Question,
                        tool_id: "q-2".into(),
                        title: "Target".into(),
                        text: "Deploy target?".into(),
                        options: vec![
                            krill_proto::agent_history::ChatQuestionOption {
                                label: "Local".into(),
                                description: String::new(),
                            },
                            krill_proto::agent_history::ChatQuestionOption {
                                label: "Cloud".into(),
                                description: String::new(),
                            },
                        ],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
        cx.notify();
        pane
    });
    cx.run_until_parked();

    // Toggling a multi-select option on then off must remove the empty entry cleanly.
    let opt0 = cx
        .debug_bounds("chat-question-batch-option-0")
        .expect("step 1 multi option 0");
    cx.simulate_click(opt0.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        shell.read_with(cx, |s, _| s
            .chat
            .question_batch_answers
            .get(&(key.clone(), 0))
            .and_then(|m| m.get(&0))
            .cloned()),
        Some(vec![0])
    );
    cx.simulate_click(opt0.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        shell.read_with(cx, |s, _| !s
            .chat
            .question_batch_answers
            .contains_key(&(key.clone(), 0))),
        "unchecking last multi-select option must clean up empty vector"
    );

    // Select step 1 option 0, advance to step 2, select option 1, and submit batch ->
    // step 1 sends first, then after screen transitions to step 2, step 2 sends as well.
    cx.simulate_click(opt0.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let next_btn = cx
        .debug_bounds("chat-question-batch-confirm")
        .expect("next button");
    cx.simulate_click(next_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let step2_opt1 = cx
        .debug_bounds("chat-question-batch-option-1")
        .expect("step 2 option 1");
    cx.simulate_click(step2_opt1.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let submit_btn = cx
        .debug_bounds("chat-question-batch-confirm")
        .expect("submit button on step 2");
    cx.simulate_click(submit_btn.center(), gpui::Modifiers::default());
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(120));
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![b" ".to_vec(), b"\r".to_vec()],
        "step 1 must be submitted first while step 1 prompt is on screen"
    );
    // Advance TUI screen to step 2; batch task must deliver step 2's answer too.
    shell.update(cx, |s, cx| {
        inject_phase2b_screen(s, pane, "Deploy target?\n❯ 1. Local\n  2. Cloud", cx);
        cx.notify();
    });
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![
            b" ".to_vec(),
            b"\r".to_vec(),
            b"\x1b[B".to_vec(),
            b"\r".to_vec(),
        ],
        "S2-17: batch submission must sequentially deliver step 2 after screen advances"
    );

    // Non-current (historical) batch must ignore option clicks.
    set_phase2b_session_state(&shell, cx, krill_proto::control::AgentState::Done, 103);
    shell.update(cx, |s, cx| {
        s.chat.question_batch_answers.clear();
        s.chat.question_batch_step.clear();
        cx.notify();
    });
    cx.run_until_parked();
    let stale_opt = cx
        .debug_bounds("chat-question-batch-option-0")
        .expect("historical option renders");
    cx.simulate_click(stale_opt.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(
        shell.read_with(cx, |s, _| s.chat.question_batch_answers.is_empty()),
        "S2-17: non-current question batch must not accept option clicks"
    );
}

#[gpui::test]
fn chat_worker_dock_renders_above_composer_counts_active_only_and_clears_failures(
    cx: &mut TestAppContext,
) {
    use krill_proto::control::AgentState;
    use krill_proto::orchestration::{
        Dispatch, DispatchReleaseState, DispatchSessionOrigin, DispatchStatus, OrchestrationStore,
        Run, RunStatus,
    };

    struct MultiSessionSource(krill_app_core::sidebar::Snapshot);
    impl krill_app_core::sidebar::Source for MultiSessionSource {
        fn fetch(&self) -> std::io::Result<krill_app_core::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }

    let (shell, cx) = fixture(cx, "chat-worker-dock-ui", vec![]);
    let now_secs = 1_800_000_000u64;
    let now_ms = now_secs * 1000;
    let worker_working = SessionId(901);
    let worker_waiting = SessionId(902);
    let worker_exited = SessionId(903);

    shell.update(cx, |s, cx| {
        s.chat.now_override = Some(now_secs);
        s.sidebar_bots = vec![bot_for(SESSION, false)];
        let snap = krill_app_core::sidebar::Snapshot {
            sessions: vec![
                krill_app_core::sidebar::Session {
                    id: SESSION,
                    status: krill_proto::control::AgentStatus {
                        state: AgentState::Done,
                        agent: "claude".into(),
                        since_unix_ns: now_secs * 1_000_000_000,
                        ..krill_proto::control::AgentStatus::UNKNOWN
                    },
                    worktree: None,
                    cwd: None,
                    account: None,
                },
                krill_app_core::sidebar::Session {
                    id: worker_working,
                    status: krill_proto::control::AgentStatus {
                        state: AgentState::Working,
                        agent: "claude".into(),
                        since_unix_ns: now_secs * 1_000_000_000,
                        ..krill_proto::control::AgentStatus::UNKNOWN
                    },
                    worktree: None,
                    cwd: None,
                    account: None,
                },
                krill_app_core::sidebar::Session {
                    id: worker_waiting,
                    status: krill_proto::control::AgentStatus {
                        state: AgentState::Waiting,
                        agent: "codex".into(),
                        since_unix_ns: now_secs * 1_000_000_000,
                        ..krill_proto::control::AgentStatus::UNKNOWN
                    },
                    worktree: None,
                    cwd: None,
                    account: None,
                },
                krill_app_core::sidebar::Session {
                    id: worker_exited,
                    status: krill_proto::control::AgentStatus {
                        state: AgentState::Exited,
                        agent: "claude".into(),
                        since_unix_ns: now_secs * 1_000_000_000,
                        ..krill_proto::control::AgentStatus::UNKNOWN
                    },
                    worktree: None,
                    cwd: None,
                    account: None,
                },
            ],
            trees: std::collections::BTreeMap::new(),
        };
        s.sidebar.update(cx, |sb, cx| {
            sb.connect_source(Arc::new(MultiSessionSource(snap)), cx);
        });
        let mk = |id: &str, name: &str, sid: Option<u64>, status: DispatchStatus| Dispatch {
            id: id.into(),
            run_id: "run-dock".into(),
            task_id: None,
            worker_name: name.into(),
            agent: "claude".into(),
            session_id: sid,
            depth: 1,
            cwd: "/tmp/repo".into(),
            prompt: "task".into(),
            status,
            summary: None,
            archived_output: None,
            started_at_ms: now_ms - 60_000,
            ended_at_ms: (status != DispatchStatus::Active).then_some(now_ms - 5_000),
            release_state: DispatchReleaseState::Active,
            session_origin: DispatchSessionOrigin::Created,
            model: None,
            effort: None,
            retry_of: None,
            request_id: None,
        };
        s.orch_snapshot = Arc::new(OrchestrationStore {
            runs: vec![Run {
                id: "run-dock".into(),
                title: "Dock".into(),
                coordinator_session_id: Some(SESSION.0),
                status: RunStatus::Active,
                created_at_ms: now_ms - 120_000,
                updated_at_ms: now_ms,
            }],
            dispatches: vec![
                mk(
                    "d-w1",
                    "worker-1",
                    Some(worker_working.0),
                    DispatchStatus::Active,
                ),
                mk(
                    "d-w2",
                    "worker-2",
                    Some(worker_waiting.0),
                    DispatchStatus::Active,
                ),
                mk(
                    "d-ex",
                    "worker-cut",
                    Some(worker_exited.0),
                    DispatchStatus::Active,
                ),
                mk("d-f1", "worker-f1", None, DispatchStatus::Failed),
                mk("d-f2", "worker-f2", None, DispatchStatus::Failed),
            ],
            ..OrchestrationStore::default()
        });
        cx.notify();
    });
    cx.run_until_parked();

    let dock = cx
        .debug_bounds("chat-worker-dock")
        .expect("worker dock renders above composer");
    let composer = cx.debug_bounds("desktop-chat-composer").unwrap();
    assert!(
        dock.bottom() <= composer.top(),
        "worker dock {dock:?} must sit above composer {composer:?}"
    );
    assert!(cx.debug_bounds("chat-worker-dock-list").is_none());

    // Active-only count is 1 (only worker-1 is actively Working).
    shell.read_with(cx, |s, cx| {
        let snap = s.worker_dock_snapshot(&local_key(SESSION), cx);
        assert_eq!(snap.active_count, 1);
        assert_eq!(snap.needs_you_count, 1);
        assert_eq!(snap.failed_count, 3);
    });

    // Expand dock and verify rows, per-row Clear, and Clear all.
    let toggle = cx.debug_bounds("chat-worker-dock-toggle").unwrap();
    cx.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-worker-dock-list").is_some());
    assert!(cx.debug_bounds("chat-worker-dock-clear").is_some());
    assert!(cx.debug_bounds("chat-worker-dock-row-d-w1").is_some());
    assert!(cx.debug_bounds("chat-worker-dock-row-d-ex").is_some());

    let clear_f1 = cx.debug_bounds("chat-worker-dock-clear-d-f1").unwrap();
    cx.simulate_click(clear_f1.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-worker-dock-row-d-f1").is_none());
    assert!(cx.debug_bounds("chat-worker-dock-row-d-f2").is_some());

    let clear_all = cx.debug_bounds("chat-worker-dock-clear").unwrap();
    cx.simulate_click(clear_all.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-worker-dock-row-d-f2").is_none());
    assert!(cx.debug_bounds("chat-worker-dock-row-d-ex").is_none());
    assert!(cx.debug_bounds("chat-worker-dock-clear").is_none());
    assert!(cx.debug_bounds("chat-worker-dock-row-d-w1").is_some());

    // Clicking a bot-spawned worker opens it and defaults to Chat view.
    let row_w1 = cx.debug_bounds("chat-worker-dock-row-d-w1").unwrap();
    cx.simulate_click(row_w1.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |s, cx| {
        let pane = s.layout.focused().unwrap();
        assert_eq!(s.layout.session(pane), Some(worker_working));
        assert_eq!(s.layout.chat_view(pane), None);
        assert!(
            s.is_pane_chat_view(pane, cx),
            "bot-spawned worker defaults to Chat view"
        );
    });

    // Explicitly switching to Terminal overrides the bot-spawned worker Chat default.
    let term_btn = cx.debug_bounds("chat-switch-terminal").unwrap();
    cx.simulate_click(term_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |s, cx| {
        let pane = s.layout.focused().unwrap();
        assert_eq!(s.layout.chat_view(pane), Some(false));
        assert!(!s.is_pane_chat_view(pane, cx));
    });
}

#[gpui::test]
fn temporary_chat_worker_banner_renders_countdown_and_keep_open_removes_it(
    cx: &mut TestAppContext,
) {
    use krill_proto::orchestration::{
        Dispatch, DispatchReleaseState, DispatchSessionOrigin, DispatchStatus, OrchestrationStore,
        Run, RunStatus,
    };

    let (shell, cx) = fixture(cx, "chat-temp-worker-banner-ui", vec![]);
    let now_secs = 1_800_000_000u64;
    let now_ms = now_secs * 1000;
    let make_store = |status: DispatchStatus, ended_at_ms: Option<u64>| {
        Arc::new(OrchestrationStore {
            runs: vec![Run {
                id: "run-temp".into(),
                title: "Temp".into(),
                coordinator_session_id: Some(500),
                status: RunStatus::Active,
                created_at_ms: now_ms - 60_000,
                updated_at_ms: now_ms,
            }],
            dispatches: vec![Dispatch {
                id: "disp-temp".into(),
                run_id: "run-temp".into(),
                task_id: None,
                worker_name: "worker-alpha".into(),
                agent: "claude".into(),
                session_id: Some(SESSION.0),
                depth: 1,
                cwd: "/tmp/repo".into(),
                prompt: "work".into(),
                status,
                summary: None,
                archived_output: None,
                started_at_ms: now_ms - 60_000,
                ended_at_ms,
                release_state: DispatchReleaseState::Active,
                session_origin: DispatchSessionOrigin::Created,
                model: None,
                effort: None,
                retry_of: None,
                request_id: None,
            }],
            ..OrchestrationStore::default()
        })
    };

    // Active temporary worker renders banner above feed.
    shell.update(cx, |s, cx| {
        s.chat.now_override = Some(now_secs);
        s.orch_snapshot = make_store(DispatchStatus::Active, None);
        cx.notify();
    });
    cx.run_until_parked();
    let banner = cx
        .debug_bounds("chat-worker-auto-close-banner")
        .expect("running temp worker banner visible");
    let feed = cx.debug_bounds("desktop-chat-feed").unwrap();
    assert!(banner.bottom() <= feed.top());
    assert!(cx.debug_bounds("chat-worker-keep-open").is_some());

    // Settled worker shows live 2-minute countdown banner.
    shell.update(cx, |s, cx| {
        s.orch_snapshot = make_store(DispatchStatus::Succeeded, Some(now_ms - 30_000));
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-worker-auto-close-banner").is_some());
    shell.read_with(cx, |s, _| {
        let state = worker_dock::temporary_worker_banner_for_session(
            &s.orch_snapshot,
            SESSION,
            &s.chat.worker_banner_kept,
            s.chat_now_epoch_ms(),
        )
        .unwrap();
        assert_eq!(
            state.phase,
            worker_dock::TemporaryWorkerPhase::Settled {
                remaining_ms: 90_000
            }
        );
    });

    // Clicking `Keep open` hides the banner immediately and retains the dispatch.
    let keep = cx.debug_bounds("chat-worker-keep-open").unwrap();
    cx.simulate_click(keep.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-worker-auto-close-banner").is_none());
    shell.read_with(cx, |s, _| {
        assert!(s.chat.worker_keep_open.contains(&SESSION));
        assert!(s.chat.worker_banner_kept.contains("disp-temp"));
        assert_eq!(
            s.orch_snapshot.dispatches[0].release_state,
            DispatchReleaseState::Retained
        );
    });
}

#[gpui::test]
fn side_chat_pill_activates_dismisses_and_preserves_command_on_submit(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(cx, "chat-side-pill-ui", vec![]);
    let key = local_key(SESSION);
    let sent: SentBytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sent_clone = sent.clone();

    let input = cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let pane = s.layout.focused().unwrap();
            attach_live_test_pane(s, pane, SESSION, cx);
            s.chat.send_input = Some(Box::new(move |bytes| {
                sent_clone.lock().unwrap().push(bytes);
                true
            }));
            s.ensure_chat_input(key.clone(), window, cx)
        })
    });
    cx.run_until_parked();

    // Typing `/btw ` via text input activates the muted Side chat pill and empties input.
    cx.update(|window, cx| {
        input.update(cx, |state, cx| {
            state.focus(window, cx);
            gpui::EntityInputHandler::replace_text_in_range(state, None, "/btw ", window, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-side-chat-pill").is_some());
    assert_eq!(input.read_with(cx, |s, _| s.value().to_string()), "");

    // Pressing Backspace at offset 0 turns the pill back into `/btw`.
    cx.simulate_keystrokes("backspace");
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-side-chat-pill").is_none());
    assert_eq!(input.read_with(cx, |s, _| s.value().to_string()), "/btw");

    // Typing `/side keep this` activates the pill; clicking it dismisses back to normal draft.
    cx.update(|window, cx| {
        input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            gpui::EntityInputHandler::replace_text_in_range(
                state,
                None,
                "/side keep this",
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-side-chat-pill").is_some());
    assert_eq!(
        input.read_with(cx, |s, _| s.value().to_string()),
        "keep this"
    );
    let pill = cx.debug_bounds("chat-side-chat-pill").unwrap();
    cx.simulate_click(pill.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-side-chat-pill").is_none());
    assert_eq!(
        input.read_with(cx, |s, _| s.value().to_string()),
        "keep this"
    );

    // Submitting with active `/btw ` pill preserves `/btw <question>` in the wire payload.
    cx.update(|window, cx| {
        input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            gpui::EntityInputHandler::replace_text_in_range(
                state,
                None,
                "/btw what does the fork keep?",
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-side-chat-pill").is_some());
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.submit_chat_prompt(&key, window, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-side-chat-pill").is_none());
    let payload = sent
        .lock()
        .unwrap()
        .iter()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .collect::<String>();
    assert!(
        payload.contains("/btw what does the fork keep?"),
        "expected /btw prefix preserved in sent payload, got {payload:?}"
    );
}

#[gpui::test]
fn queued_messages_keep_steer_visible_and_support_edit_delete_and_steer(cx: &mut TestAppContext) {
    use krill_proto::control::AgentState;

    let (shell, cx) = fixture(cx, "chat-queued-actions-ui", vec![]);
    let key = local_key(SESSION);
    let sent: SentBytes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sent_clone = sent.clone();

    let input = cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let pane = s.layout.focused().unwrap();
            attach_live_test_pane(s, pane, SESSION, cx);
            s.chat.send_input = Some(Box::new(move |bytes| {
                sent_clone.lock().unwrap().push(bytes);
                true
            }));
            s.ensure_chat_input(key.clone(), window, cx)
        })
    });
    set_phase2b_session_state(&shell, cx, AgentState::Working, 100);

    // Queue two editable prompts + one attachment-only prompt while Working.
    for text in ["first queued task", "second queued task"] {
        cx.update(|window, cx| {
            input.update(cx, |state, cx| state.set_value(text, window, cx));
            shell.update(cx, |s, cx| s.submit_chat_prompt(&key, window, cx));
        });
        cx.run_until_parked();
    }
    shell.update(cx, |s, cx| {
        s.chat
            .logs
            .entry(key.clone())
            .or_default()
            .queued_prompts
            .push_back(QueuedChatPrompt {
                id: 9999,
                display_text: String::new(),
                submitted_payload: "/tmp/only.png".into(),
                attachments: vec![DraftAttachment {
                    path: std::path::PathBuf::from("/tmp/only.png"),
                    preview_path: None,
                    display_name: "only.png".into(),
                    byte_len: 12,
                    is_image: true,
                }],
            });
        s.chat.pending_revision = s.chat.pending_revision.wrapping_add(1);
        cx.notify();
    });
    cx.run_until_parked();

    let (id1, id2, id3) = shell.read_with(cx, |s, _| {
        let q = &s.chat.logs[&key].queued_prompts;
        assert_eq!(q.len(), 3);
        (q[0].id, q[1].id, q[2].id)
    });

    // Steer is visible on queued rows at rest; focusing row 2 does not shift Steer bounds.
    let steer_before = cx
        .debug_bounds("chat-queued-steer")
        .expect("Steer visible at rest");
    shell.update(cx, |s, cx| s.focus_queued_chat_prompt(&key, id2, cx));
    cx.run_until_parked();
    let steer_after = cx.debug_bounds("chat-queued-steer").unwrap();
    assert_eq!(
        steer_before, steer_after,
        "revealing Edit/Delete on focus must not shift Steer"
    );

    // Edit loads the second queued prompt back into the composer and removes it from the queue.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| s.edit_queued_chat_prompt(&key, id2, window, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        input.read_with(cx, |s, _| s.value().to_string()),
        "second queued task"
    );
    shell.read_with(cx, |s, _| {
        assert_eq!(s.chat.logs[&key].queued_prompts.len(), 2);
        assert_eq!(s.chat.logs[&key].queued_prompts[0].id, id1);
        assert_eq!(s.chat.logs[&key].queued_prompts[1].id, id3);
    });

    // Attachment-only queued prompt cannot be edited into text, and Delete removes it cleanly.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| s.edit_queued_chat_prompt(&key, id3, window, cx));
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(s.chat.logs[&key].queued_prompts.len(), 2);
    });
    shell.update(cx, |s, cx| s.remove_queued_chat_prompt(&key, id3, cx));
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(s.chat.logs[&key].queued_prompts.len(), 1);
    });

    // Steer on the remaining queued row sends it immediately and clears the queue.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.steer_queued_chat_prompt(&key, id1, window, cx);
        });
    });
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert!(s.chat.logs[&key].queued_prompts.is_empty());
    });
    let all_sent: String = sent
        .lock()
        .unwrap()
        .iter()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .collect();
    assert!(
        all_sent.contains("first queued task"),
        "expected steered queued prompt in sent bytes, got {all_sent:?}"
    );
}

#[gpui::test]
fn cross_message_image_lightbox_steps_across_messages_with_counter(cx: &mut TestAppContext) {
    let dir = state_path("chat-cross-msg-lightbox");
    std::fs::create_dir_all(&dir).unwrap();
    let png_a = dir.join("a.png");
    let png_b = dir.join("b.png");
    let png_c = dir.join("c.png");
    for (p, color) in [
        (&png_a, [20, 40, 60, 255]),
        (&png_b, [60, 80, 100, 255]),
        (&png_c, [100, 120, 140, 255]),
    ] {
        image::RgbaImage::from_pixel(16, 16, image::Rgba(color))
            .save(p)
            .unwrap();
    }

    let (shell, cx) = fixture(
        cx,
        "chat-cross-msg-lightbox-ui",
        vec![
            ChatItem {
                kind: ChatItemKind::User,
                text: "[Image #1] [Image #2] first".into(),
                file_path: format!("{}\n{}", png_a.display(), png_b.display()),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::Assistant,
                text: "Looks good.".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::User,
                text: "[Image #1] second".into(),
                file_path: png_c.display().to_string(),
                ..Default::default()
            },
        ],
    );

    // Click image 0 in message 0 (first image overall) -> captures all 3 images across messages.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let base = s.chat.logs[&local_key(SESSION)].base_index;
            s.open_chat_message_image_viewer(
                &local_key(SESSION),
                base,
                0,
                vec![png_a.clone(), png_b.clone()],
                vec![1, 2],
                0,
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-image-viewer").is_some());
    assert!(cx.debug_bounds("chat-image-viewer-counter").is_some());
    shell.read_with(cx, |s, _| {
        let v = s.chat.images.viewer.as_ref().unwrap();
        assert_eq!(v.paths, vec![png_a.clone(), png_b.clone(), png_c.clone()]);
        assert_eq!(v.index, 0);
    });

    // Step across message boundary via Right arrow and Next button; bounds clamp cleanly.
    cx.simulate_keystrokes("right");
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(s.chat.images.viewer.as_ref().unwrap().index, 1);
    });
    let next = cx.debug_bounds("chat-image-viewer-next").unwrap();
    cx.simulate_click(next.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(s.chat.images.viewer.as_ref().unwrap().index, 2);
    });
    cx.simulate_keystrokes("right");
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(
            s.chat.images.viewer.as_ref().unwrap().index,
            2,
            "clamped at last image"
        );
    });
    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(s.chat.images.viewer.as_ref().unwrap().index, 1);
    });
    let prev = cx.debug_bounds("chat-image-viewer-previous").unwrap();
    cx.simulate_click(prev.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(s.chat.images.viewer.as_ref().unwrap().index, 0);
    });
    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(
            s.chat.images.viewer.as_ref().unwrap().index,
            0,
            "clamped at first image"
        );
    });
    std::fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn selection_quote_floating_add_to_chat_appends_after_existing_draft(cx: &mut TestAppContext) {
    let (shell, cx) = fixture(
        cx,
        "chat-selection-add-to-chat-ui",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "First reply line\nSecond reply line".into(),
            ..Default::default()
        }],
    );
    let key = local_key(SESSION);

    // Pre-fill composer draft and move focus out of composer so reply selection is active.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            let input = s.ensure_chat_input(key.clone(), window, cx);
            input.update(cx, |state, cx| {
                state.set_value("Existing note", window, cx);
            });
            window.focus(&s.focus, cx);
            s.chat.selection_quote = Some(selection_quote::ActiveSelectionQuote {
                key: key.clone(),
                text: "First reply line\nSecond reply line".into(),
                _anchor: None,
            });
            cx.notify();
        });
    });
    cx.run_until_parked();

    let add_btn = cx
        .debug_bounds("chat-selection-add-to-chat")
        .expect("floating Add to chat button visible on selection");
    cx.simulate_click(add_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    assert!(cx.debug_bounds("chat-selection-add-to-chat").is_none());
    shell.read_with(cx, |s, cx| {
        let val = s.chat.inputs[&key].0.read(cx).value().to_string();
        assert_eq!(
            val,
            "Existing note\n\n> First reply line\n> Second reply line\n\n"
        );
    });

    // Dismissing selection quote clears the floating button without modifying the draft.
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            window.focus(&s.focus, cx);
            s.chat.selection_quote = Some(selection_quote::ActiveSelectionQuote {
                key: key.clone(),
                text: "Dismiss me".into(),
                _anchor: None,
            });
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-selection-add-to-chat").is_some());
    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            assert!(s.dismiss_chat_selection_quote(&key, window, cx));
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("chat-selection-add-to-chat").is_none());
    shell.read_with(cx, |s, cx| {
        let val = s.chat.inputs[&key].0.read(cx).value().to_string();
        assert_eq!(
            val,
            "Existing note\n\n> First reply line\n> Second reply line\n\n"
        );
    });
}

#[gpui::test]
fn sync_chat_image_tokens_retries_after_ime_and_avoids_duplicate_pill_on_reattach(
    cx: &mut TestAppContext,
) {
    let (shell, cx) = fixture(cx, "chat-ub06-image-tokens", vec![]);
    let key = local_key(SESSION);
    let img_path = std::path::PathBuf::from("/tmp/krill-ub06-shot.png");
    let make_att = || DraftAttachment {
        path: img_path.clone(),
        preview_path: None,
        display_name: "krill-ub06-shot.png".into(),
        byte_len: 128,
        is_image: true,
    };

    let input = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let input = shell.ensure_chat_input(key.clone(), window, cx);
            // Enter active IME composition (Korean syllable preedit).
            input.update(cx, |state, cx| {
                gpui::EntityInputHandler::replace_and_mark_text_in_range(
                    state, None, "한", None, window, cx,
                );
            });
            shell.stage_chat_attachments(&key, vec![make_att()], cx);
            shell.sync_chat_image_tokens(&key, &input, window, cx);
            assert!(
                input.read(cx).tokens().is_empty(),
                "token insert must fail cleanly while IME composition is active"
            );
            assert!(
                shell
                    .chat
                    .image_tokens
                    .get(&key)
                    .is_none_or(|placed| !placed.contains(&img_path)),
                "failed IME insert must not mark image as placed"
            );

            // End IME composition; next sync retries and places the pill once.
            input.update(cx, |state, cx| {
                gpui::EntityInputHandler::unmark_text(state, window, cx);
            });
            shell.sync_chat_image_tokens(&key, &input, window, cx);
            assert_eq!(input.read(cx).tokens().len(), 1);
            assert!(shell.chat.image_tokens[&key].contains(&img_path));

            // Remove attachment (leaving stale token in TextareaState), then re-attach same path.
            shell.remove_chat_attachment(&key, 0, cx);
            shell.sync_chat_image_tokens(&key, &input, window, cx);
            shell.stage_chat_attachments(&key, vec![make_att()], cx);
            shell.sync_chat_image_tokens(&key, &input, window, cx);
            input
        })
    });

    shell.read_with(cx, |shell, app_cx| {
        let state = input.read(app_cx);
        assert_eq!(
            state.tokens().len(),
            1,
            "re-attaching a removed image must update the existing token instead of duplicating pills"
        );
        let atts = &shell.chat.draft_attachments[&key];
        let payload =
            build_chat_payload_with_tokens(state.value().as_ref(), state.tokens(), atts).unwrap();
        assert_eq!(
            payload.matches("/tmp/krill-ub06-shot.png").count(),
            1,
            "re-attached image must expand once in submitted payload, got {payload:?}"
        );
    });
}

#[gpui::test]
fn last_feed_row_stays_live_when_pending_prompt_is_queued(cx: &mut TestAppContext) {
    struct StateSource(krill_app_core::sidebar::Snapshot);
    impl krill_app_core::sidebar::Source for StateSource {
        fn fetch(&self) -> std::io::Result<krill_app_core::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }

    let (shell, cx) = fixture(
        cx,
        "chat-ub07-last-row-pending",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "Streaming answer...".into(),
            ..Default::default()
        }],
    );
    let key = local_key(SESSION);
    let snap = krill_app_core::sidebar::Snapshot {
        sessions: vec![krill_app_core::sidebar::Session {
            id: SESSION,
            status: krill_proto::control::AgentStatus {
                state: krill_proto::control::AgentState::Working,
                agent: "claude".into(),
                ..krill_proto::control::AgentStatus::UNKNOWN
            },
            worktree: None,
            cwd: None,
            account: None,
        }],
        trees: std::collections::BTreeMap::new(),
    };
    shell.update(cx, |shell, cx| {
        shell.sidebar.update(cx, |sb, cx| {
            sb.connect_source(std::sync::Arc::new(StateSource(snap)), cx);
        });
        shell
            .chat
            .pending_prompts
            .entry(key.clone())
            .or_default()
            .push(PendingChatPrompt {
                id: 1,
                origin_item: 1,
                submitted_payload: "queued follow-up".into(),
                display_text: "queued follow-up".into(),
                attachments: Vec::new(),
                status: PendingStatus::Queued,
                submitted_at_ms: 0,
            });
        cx.notify();
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("chat-scroll-top").is_none(),
        "streaming last assistant row must remain is_last_row (footer suppressed) even with queued pending prompt"
    );
}

#[gpui::test]
fn cross_host_paused_resume_and_split_item_note_focus_behave_cleanly(cx: &mut TestAppContext) {
    use krill_app_core::hosts::HostId;
    use krill_proto::bots::{
        BotItem, BotItemActor, BotItemEvent, BotItemEventKind, BotItemStatus, BotRpcRequest,
        BotRpcResponse,
    };

    let (shell, cx) = fixture(
        cx,
        "chat-ub09-cross-host-and-split-focus",
        vec![
            ChatItem {
                kind: ChatItemKind::User,
                text: "Track release".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::Assistant,
                text: "Added item.".into(),
                ..Default::default()
            },
        ],
    );
    cx.simulate_resize(size(px(1200.), px(800.)));

    // Part 1: cross-host paused resume sends RPC to remote host and updates cached_bots.
    let remote = HostId::Remote("iroh:studio".into());
    let local_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let remote_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let local_hits_ref = std::sync::Arc::clone(&local_hits);
    let remote_hits_ref = std::sync::Arc::clone(&remote_hits);
    let remote_key = (remote.clone(), SessionId(888));
    let mut paused_remote_bot = bot_for(SessionId(888), true);
    paused_remote_bot.host = remote.clone();
    let bot_id = paused_remote_bot.id.clone();

    let item = BotItem {
        uid: "item-split-1".into(),
        number: 1,
        owner_bot_id: bot_id.clone(),
        title: "Ship release".into(),
        description: None,
        done_means: None,
        status: BotItemStatus::NeedsYou,
        close_note: None,
        links: Vec::new(),
        events: vec![BotItemEvent {
            id: 1,
            item_uid: "item-split-1".into(),
            actor: BotItemActor::Bot {
                bot_id: bot_id.clone(),
                session_id: Some(SESSION),
            },
            kind: BotItemEventKind::Made,
            text: "Made".into(),
            decisions: Vec::new(),
            created_at_ms: 1_700_000_000_500,
        }],
        snoozed_until_ms: None,
        version: 1,
        last_activity_at_ms: 1_700_000_000_500,
        created_at_ms: 1_700_000_000_500,
        updated_at_ms: 1_700_000_000_500,
        closed_at_ms: None,
        waiting_reason: None,
        waiting_since_ms: None,
    };
    let items_for_rpc = vec![item];

    let (bot_pane, other_pane) = shell.update(cx, |shell, cx| {
        let bot_pane = shell.layout.focused().unwrap();
        shell.bot_rpc_override = Some(std::sync::Arc::new(move |req| match req {
            BotRpcRequest::UpdateBot(_) => {
                local_hits_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(items_for_rpc.clone())),
            _ => Ok(BotRpcResponse::Ok),
        }));
        shell.bot_rpc_overrides.insert(
            remote.clone(),
            std::sync::Arc::new(move |req| {
                if let BotRpcRequest::UpdateBot(params) = req
                    && params.paused == Some(false)
                {
                    remote_hits_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(BotRpcResponse::Ok)
            }),
        );
        shell
            .chat
            .cached_bots
            .insert(remote_key.clone(), paused_remote_bot);
        shell.set_bot_paused_on(&remote, bot_id.clone(), false, cx);

        // Part 2: split view where bot pane is unfocused and right panel is collapsed.
        shell.sidebar_bots = vec![bot_for(SESSION, false)];
        shell.panel_collapsed = true;
        shell.layout.split(krill_app_core::layout::Axis::Horizontal);
        let other_pane = shell.layout.focused().unwrap();
        shell.layout.set_session(other_pane, SessionId(772));
        shell.layout.set_chat_view(other_pane, false);
        cx.notify();
        (bot_pane, other_pane)
    });
    cx.run_until_parked();

    assert_eq!(
        remote_hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "resume on remote bot chat must target remote host RPC"
    );
    assert_eq!(
        local_hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "resume on remote bot chat must not target local host RPC"
    );
    shell.read_with(cx, |s, _| {
        assert!(!s.chat.cached_bots[&remote_key].paused);
        assert_eq!(s.layout.focused(), Some(other_pane));
    });

    let chip = cx
        .debug_bounds("chat-item-note-chip")
        .expect("item note chip in unfocused split bot pane");
    cx.simulate_click(chip.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    shell.read_with(cx, |s, _| {
        assert_eq!(
            s.layout.focused(),
            Some(bot_pane),
            "clicking ItemNote chip in split view must focus its bot pane"
        );
        assert!(!s.panel_collapsed);
    });
}

#[gpui::test]
fn chat_find_recomputes_on_turn_fold_toggle_turn_and_item_notes_and_draft_applies_collapsed(
    cx: &mut TestAppContext,
) {
    let (shell, cx) = fixture(
        cx,
        "chat-ub37-ub42-find-and-draft",
        vec![
            ChatItem {
                kind: ChatItemKind::User,
                text: "Run check".into(),
                at: "2026-11-22T01:00:00Z".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::ToolCall,
                title: "bash".into(),
                text: "needle-tool-cmd".into(),
                at: "2026-11-22T01:00:05Z".into(),
                ..Default::default()
            },
            ChatItem {
                kind: ChatItemKind::Assistant,
                text: "All green.".into(),
                at: "2026-11-22T01:00:16Z".into(),
                ..Default::default()
            },
        ],
    );
    let key = local_key(SESSION);
    let b = bot_for(SESSION, false);
    let bot_id = b.id.clone();

    // UB-42: pending draft inserts into composer even while right panel is collapsed.
    shell.update(cx, |shell, cx| {
        shell.panel_collapsed = true;
        shell.sidebar_bots = vec![b];
        shell.bot_ui.panel.pending_draft = Some((
            krill_app_core::hosts::HostId::Local,
            bot_id,
            "Draft while collapsed".into(),
        ));
        cx.notify();
    });
    cx.run_until_parked();
    shell.read_with(cx, |shell, app_cx| {
        assert!(shell.bot_ui.panel.pending_draft.is_none());
        assert_eq!(
            shell.chat.inputs[&key].0.read(app_cx).value().as_ref(),
            "Draft while collapsed"
        );
    });

    // UB-37: open chat find searching for the folded tool call; expanding the turn updates matches.
    cx.simulate_resize(size(px(1200.), px(800.)));
    let find_input = cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.toggle_chat_find(window, cx);
            shell.chat.finds[&key].input.clone()
        })
    });
    cx.update(|window, cx| {
        find_input.update(cx, |s, cx| s.insert("needle-tool-cmd", window, cx));
    });
    cx.run_until_parked();
    shell.read_with(cx, |shell, _| {
        assert_eq!(shell.chat.finds[&key].query, "needle-tool-cmd");
        assert!(
            shell.chat.finds[&key].matches.is_empty(),
            "folded turn hides tool call from display_feed"
        );
    });

    let bar = cx
        .debug_bounds("chat-turn-bar-0")
        .expect("collapsed bot turn bar");
    // Click the left side of the turn bar (clear of the top-right floating Find bar).
    cx.simulate_click(
        gpui::point(bar.origin.x + px(24.), bar.center().y),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("chat-work-run-toggle").is_some(),
        "clicking collapsed bot turn bar must expand the folded work run"
    );
    shell.read_with(cx, |shell, _| {
        assert_eq!(
            shell.chat.finds[&key].matches.len(),
            1,
            "toggling turn expansion must recompute chat find matches"
        );
    });
}

#[cfg(unix)]
#[gpui::test]
fn remote_chat_image_stages_via_remote_fs_and_recovers_after_transient_error(
    cx: &mut TestAppContext,
) {
    let dir = std::env::temp_dir().join(format!(
        "krill-remote-chat-img-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let staged_png = dir.join("staged-preview.png");
    let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([0x22, 0x88, 0xee, 0xff]));
    image::DynamicImage::ImageRgba8(img)
        .save_with_format(&staged_png, image::ImageFormat::Png)
        .unwrap();

    let remote_host = krill_app_core::hosts::HostId::Remote("macmini-peer".into());
    let remote_path = std::path::PathBuf::from("/remote/tmp/krill-paste/clipboard-test.png");
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let attempts_ref = Arc::clone(&attempts);
    let staged_ref = staged_png.clone();

    let (shell, cx) = open(cx, state_path("chat-remote-image-stage"));
    shell.update(cx, |shell, cx| {
        let pane = shell.layout.focused().unwrap();
        shell.layout.set_host(pane, remote_host.clone());
        shell.layout.set_session(pane, SESSION);
        shell.host_targets.insert(
            remote_host.clone(),
            krill_app_core::hosts::HostTarget::local(krill_proto::daemon::Paths {
                socket: "/unused/remote.sock".into(),
                holder: "/unused/holder".into(),
                krilld: "/unused/krilld".into(),
            }),
        );
        shell.chat.images.remote_fetcher = Some(Arc::new(move |_target, host, path| {
            assert_eq!(
                *host,
                krill_app_core::hosts::HostId::Remote("macmini-peer".into())
            );
            assert_eq!(
                path,
                std::path::Path::new("/remote/tmp/krill-paste/clipboard-test.png")
            );
            let n = attempts_ref.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "transient tunnel reset",
                ))
            } else {
                Ok(staged_ref.clone())
            }
        }));
        shell.apply_chat_poll_for_host(
            SESSION,
            remote_host.clone(),
            snapshot(vec![ChatItem {
                kind: ChatItemKind::User,
                text: "[Image #1] pi does not respond".into(),
                file_path: remote_path.to_string_lossy().into_owned(),
                ..Default::default()
            }]),
            None,
        );
        shell.toggle_chat_view(cx);
        cx.notify();
    });
    cx.run_until_parked();

    // First attempt failed transiently; clearing transient failures (e.g. on poll recovery)
    // immediately retries staging and decodes the thumbnail into Loaded::Ready.
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    shell.update(cx, |shell, cx| {
        shell
            .chat
            .images
            .clear_transient_remote_failures(&remote_host);
        cx.notify();
    });
    cx.run_until_parked();

    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    shell.read_with(cx, |shell, _| {
        assert_eq!(
            shell.chat.images.resolve_remote_preview(&remote_path),
            Some(staged_png.as_path())
        );
        assert!(
            matches!(
                shell.chat.images.thumbs_for_test(&staged_png),
                Some(super::images::Loaded::Ready(_))
            ),
            "staged remote image thumbnail must decode to Loaded::Ready"
        );
    });
    assert!(
        cx.debug_bounds("chat-message-image-0").is_some(),
        "#1 remote image tile must render in the message"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn removing_attached_image_leaves_send_disabled_and_hides_queue_steer(cx: &mut TestAppContext) {
    use krill_proto::control::AgentState;
    let dir = TestTempDir::new("chat-removed-image-empty");
    let img_path = dir.path().join("shot.png");
    image::RgbaImage::from_pixel(32, 32, image::Rgba([200, 40, 40, 255]))
        .save(&img_path)
        .unwrap();

    let (shell, cx) = fixture(
        cx,
        "chat-removed-image-empty",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: "Ready.".into(),
            ..Default::default()
        }],
    );
    let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            attach_live_test_pane(shell, pane, SESSION, cx);
            let recorded = sent.clone();
            shell.chat.send_input = Some(Box::new(move |bytes| {
                recorded.lock().unwrap().push(bytes);
                true
            }));
            let _ = shell.ensure_chat_input(local_key(SESSION), window, cx);
            shell.attach_dropped_paths_to_chat(
                &local_key(SESSION),
                &gpui::ExternalPaths(vec![img_path.clone()].into()),
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();

    // Remove the attached image (leaving only the dead removed-image inline token in TextareaState).
    let remove_btn = cx.debug_bounds("chat-attachment-remove-0").unwrap();
    cx.simulate_click(remove_btn.center(), gpui::Modifiers::default());
    cx.run_until_parked();

    // While idle: clicking Send must be a disabled no-op (zero bytes sent).
    if let Some(send_btn) = cx.debug_bounds("desktop-chat-send") {
        cx.simulate_click(send_btn.center(), gpui::Modifiers::default());
        cx.run_until_parked();
    }
    assert!(sent.lock().unwrap().is_empty());

    // While working: Queue and Steer must be hidden because draft is effectively empty after stripping removed image tokens.
    set_phase2b_session_state(&shell, cx, AgentState::Working, 500);
    assert!(
        cx.debug_bounds("chat-composer-queue").is_none(),
        "UI-12: Queue must be hidden when only a removed-image token remains"
    );
    assert!(
        cx.debug_bounds("chat-composer-steer").is_none(),
        "UI-12: Steer must be hidden when only a removed-image token remains"
    );
}

#[gpui::test]
fn chat_markdown_file_links_after_korean_text_and_inline_code_click_and_open(
    cx: &mut gpui::TestAppContext,
) {
    use krill_proto::agent_history::{ChatItem, ChatItemKind};

    struct LinkClickHarness {
        source: SharedString,
        clicked: Arc<Mutex<Vec<String>>>,
    }
    impl gpui::Render for LinkClickHarness {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            let clicked = self.clicked.clone();
            gpui::div()
                .w(gpui::px(760.))
                .h(gpui::px(420.))
                .child(gpui_component::TextSelectionLayer)
                .child(
                    gpui_component::text::TextView::markdown(
                        "korean-file-links",
                        self.source.clone(),
                    )
                    .scrollable(false)
                    .selectable(true)
                    .on_link_click(move |url, _, _, _| {
                        clicked.lock().unwrap().push(url.to_string());
                    }),
                )
        }
    }

    let dir = std::env::temp_dir().join(format!(
        "krill-chat-md-links-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let outputs = dir.join("Application Support/dev.krill.app/bots/bot-7/outputs");
    std::fs::create_dir_all(&outputs).unwrap();
    let gallery = outputs.join("yunjadong-profile-gallery.html");
    let png1 = outputs.join("01c-balanced-lowkey-depth.png");
    let png2 = outputs.join("verify-before-after.png");
    let png3 = outputs.join("verify-html-final.png");
    let rs_file = outputs.join("view.rs");
    std::fs::write(&gallery, b"<html><body>gallery</body></html>").unwrap();
    std::fs::write(&png1, b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(&png2, b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(&png3, b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(
        &rs_file,
        "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\nline11\nline12\n",
    )
    .unwrap();

    let enc = |p: &std::path::Path| format!("file://{}", p.to_string_lossy().replace(' ', "%20"));
    let gallery_url = enc(&gallery);
    let png1_url = enc(&png1);
    let png2_url = enc(&png2);
    let png3_url = enc(&png3);
    let rs_url = format!("{}#L12-L20", enc(&rs_file));

    // Exact structure from the owner's bug report screenshot: Korean text + inline code + multiple file:// links.
    let markdown = format!(
        "## 결과 파일 및 검증\n\n\
         - 최종 사용 PNG (`1122x1402`, 1.5MB): [01c-balanced-lowkey-depth.png]({png1_url})\n\
         - 업데이트된 갤러리: [yunjadong-profile-gallery.html]({gallery_url}) (새로고침하면 바로 보정된 `#01c`가 4번째 카드에 배치됩니다)\n\
         - 전후 비교 및 최종 렌더링 캡처 검증 완료: [verify-before-after.png]({png2_url}), [verify-html-final.png]({png3_url})\n\
         - 소스 확인: [view.rs]({rs_url})\n"
    );

    // 1. Verify GPUI TextView::markdown hit-testing fires on_link_click for links preceded by Korean text & inline code.
    let clicked_urls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = clicked_urls.clone();
    let (_harness, vcx) = cx.add_window_view({
        let markdown = markdown.clone();
        move |_, cx| {
            gpui_component::init(cx);
            LinkClickHarness {
                source: SharedString::from(markdown),
                clicked: captured,
            }
        }
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    // Scan across the rendered list rows to click every link (including after Korean text, after inline code, and second link on line).
    for y in (20_u16..260).step_by(8) {
        for x in (40_u16..720).step_by(28) {
            let pos = gpui::point(gpui::px(f32::from(x)), gpui::px(f32::from(y)));
            vcx.simulate_mouse_down(pos, gpui::MouseButton::Left, gpui::Modifiers::default());
            vcx.simulate_mouse_up(pos, gpui::MouseButton::Left, gpui::Modifiers::default());
        }
    }
    let fired = clicked_urls.lock().unwrap().clone();
    for expected in [&png1_url, &gallery_url, &png2_url, &png3_url, &rs_url] {
        assert!(
            fired.iter().any(|u| u == expected),
            "expected link {expected} to fire on_link_click after Korean/inline-code text, fired={fired:?}"
        );
    }

    // 2. Verify real clicks inside Shell's rendered desktop-chat-feed open .html/.png via system
    //    opener, .rs#L12-L20 in Krill editor tab, and reject dangerous/disallowed links.
    let (shell, cx) = fixture(
        cx,
        "chat-md-links-shell",
        vec![ChatItem {
            kind: ChatItemKind::Assistant,
            text: markdown.clone(),
            ..Default::default()
        }],
    );
    // Click across the actual rendered assistant message body inside Shell's gpui::list chat feed!
    let body_bounds = cx
        .debug_bounds("chat-message-body")
        .expect("assistant chat-message-body must render");
    let mut y = f32::from(body_bounds.top()) + 8.0;
    while y < f32::from(body_bounds.bottom()) {
        let mut x = f32::from(body_bounds.left()) + 20.0;
        while x < f32::from(body_bounds.right()) {
            let pos = gpui::point(gpui::px(x), gpui::px(y));
            cx.simulate_mouse_down(pos, gpui::MouseButton::Left, gpui::Modifiers::default());
            cx.simulate_mouse_up(pos, gpui::MouseButton::Left, gpui::Modifiers::default());
            x += 36.0;
        }
        y += 10.0;
    }
    cx.run_until_parked();
    shell.read_with(cx, |shell, _| {
        assert!(
            shell.chat.opened_system_paths.contains(&gallery),
            "clicking gallery .html link in Shell chat feed must invoke system opener, got: {:?}",
            shell.chat.opened_system_paths
        );
        assert!(
            shell.chat.opened_system_paths.contains(&png1),
            "clicking png1 link in Shell chat feed must invoke system opener, got: {:?}",
            shell.chat.opened_system_paths
        );
        assert!(
            shell.chat.opened_system_paths.contains(&png2),
            "clicking png2 link in Shell chat feed must invoke system opener, got: {:?}",
            shell.chat.opened_system_paths
        );
        assert!(
            shell.chat.opened_system_paths.contains(&png3),
            "clicking second link on same line (png3) in Shell chat feed must invoke system opener, got: {:?}",
            shell.chat.opened_system_paths
        );
        assert_eq!(
            shell.layout.focused().and_then(|p| shell.layout.file(p)),
            Some(rs_file.as_path()),
            "clicking Rust file://#L12-L20 link in Shell chat feed must focus the opened editor tab"
        );
    });
    // Also verify dangerous executable (.command) reveals rather than executing, and javascript: is blocked.
    let evil_cmd = outputs.join("evil.command");
    std::fs::write(&evil_cmd, b"#!/bin/sh\n").unwrap();
    let evil_url = enc(&evil_cmd);
    cx.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            let left_click = gpui::ClickEvent::Keyboard(gpui::KeyboardClickEvent::default());
            assert!(shell.open_chat_markdown_link(
                Some(pane),
                krill_app_core::hosts::HostId::Local,
                Some(&outputs),
                &evil_url,
                &left_click,
                cx,
            ));
            assert!(shell.chat.revealed_paths.contains(&evil_cmd));
            assert!(!shell.chat.opened_system_paths.contains(&evil_cmd));
            assert!(!shell.open_chat_markdown_link(
                Some(pane),
                krill_app_core::hosts::HostId::Local,
                Some(&outputs),
                "javascript:alert(1)",
                &left_click,
                cx,
            ));
        });
    });

    std::fs::remove_dir_all(&dir).ok();
}
