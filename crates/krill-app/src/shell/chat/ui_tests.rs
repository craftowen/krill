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
            shell.apply_chat_poll_for_host(SESSION, local_host, snapshot(vec![item("local")]));
            shell.apply_chat_poll_for_host(SESSION, remote_host, snapshot(vec![item("remote")]));
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
