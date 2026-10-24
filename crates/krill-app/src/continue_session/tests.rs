use std::sync::{Arc, Mutex};

use gpui::TestAppContext;

use super::*;

#[test]
fn format_handoff_brief_includes_instructions_and_context() {
    let brief = format_handoff_brief(
        SessionId(42),
        "codex",
        "  verify edge cases  ",
        "line 1\nline 2\n",
    );
    assert!(brief.contains("Source session: s42"), "{brief}");
    assert!(brief.contains("Target agent: codex"), "{brief}");
    assert!(brief.contains("Instructions: verify edge cases"), "{brief}");
    assert!(brief.contains("line 1\nline 2"), "{brief}");

    let fallback = format_handoff_brief(SessionId(7), "claude", "   ", "");
    assert!(fallback.contains(DEFAULT_PROMPT), "{fallback}");
    assert!(fallback.contains("(empty)"), "{fallback}");
}

fn open(
    cx: &mut TestAppContext,
) -> (
    Entity<Dialog>,
    Arc<Mutex<Vec<Event>>>,
    &mut gpui::VisualTestContext,
) {
    cx.update(gpui_component::init);
    let agents = vec![
        ("claude".to_owned(), "Claude Code".to_owned()),
        ("codex".to_owned(), "Codex".to_owned()),
    ];
    let (dialog, cx) = cx.add_window_view(|window, cx| {
        Dialog::new(SessionId(9), agents, Some("claude"), window, cx)
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&dialog, move |_, e: &Event, _| {
            sink.lock().unwrap().push(e.clone());
        })
        .detach();
    });
    (dialog, events, cx)
}

#[gpui::test]
fn dialog_selects_agent_and_submits_prompt_or_closes_on_escape(cx: &mut TestAppContext) {
    let (dialog, events, cx) = open(cx);
    cx.update(|window, cx| {
        assert_eq!(dialog.read(cx).selected_agent(), "claude");
        dialog.update(cx, |d, cx| {
            d.select_agent("codex", cx);
            d.set_prompt("Finish the remaining tests", window, cx);
        });
    });
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("escape");
    assert_eq!(
        *events.lock().unwrap(),
        [
            Event::Submit {
                source: SessionId(9),
                agent: "codex".to_owned(),
                prompt: "Finish the remaining tests".to_owned(),
            },
            Event::Close,
        ]
    );
}

#[gpui::test]
fn continue_agent_keyboard_wraps_and_space_selects_without_submitting(cx: &mut TestAppContext) {
    let (dialog, events, cx) = open(cx);
    cx.run_until_parked();
    cx.update(|window, cx| {
        let focus = dialog.read(cx).agent_focus[0].clone();
        focus.focus(window, cx);
    });
    cx.simulate_keystrokes("right");
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).selected_agent().to_owned()),
        "codex"
    );
    cx.simulate_keystrokes("right");
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).selected_agent().to_owned()),
        "claude"
    );
    cx.simulate_keystrokes("left");
    cx.run_until_parked();
    dialog.update(cx, |d, cx| d.select_agent("claude", cx));
    cx.run_until_parked();
    for key in ["space", "enter"] {
        cx.simulate_keystrokes(key);
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
        });
        cx.run_until_parked();
    }
    assert_eq!(
        cx.update(|_, cx| dialog.read(cx).selected_agent().to_owned()),
        "codex"
    );
    assert!(
        events.lock().unwrap().is_empty(),
        "agent activation must not submit the dialog"
    );
}

#[gpui::test]
fn continue_modal_keeps_submit_visible_with_many_agents_at_large_zoom(cx: &mut TestAppContext) {
    let (dialog, _, cx) = open(cx);
    dialog.update(cx, |d, cx| {
        d.agents = (0..30)
            .map(|i| {
                (
                    format!("agent-{i}"),
                    format!("Long custom agent profile {i}"),
                )
            })
            .collect();
        d.agent_focus = d
            .agents
            .iter()
            .map(|_| cx.focus_handle().tab_stop(true))
            .collect();
        d.selected_agent = "agent-0".into();
        cx.notify();
    });
    cx.simulate_resize(gpui::size(gpui::px(640.), gpui::px(480.)));
    cx.update(|window, _| window.set_rem_size(gpui::px(32.)));
    cx.run_until_parked();
    let card = cx.debug_bounds("continue-session-dialog").unwrap();
    let submit = cx.debug_bounds("continue-session-submit").unwrap();
    let body = cx.debug_bounds("continue-session-body").unwrap();
    assert!(body.size.height > gpui::px(0.) && body.bottom() <= submit.top());
    assert!(
        card.size.width <= gpui::px(608.) && card.size.height <= gpui::px(448.),
        "{card:?}"
    );
    assert!(
        submit.left() >= card.left()
            && submit.right() <= card.right()
            && submit.top() >= card.top()
            && submit.bottom() <= card.bottom(),
        "{submit:?} outside {card:?}"
    );
    assert!(
        cx.debug_bounds("continue-session-body")
            .unwrap()
            .size
            .height
            > gpui::px(0.)
    );
}
