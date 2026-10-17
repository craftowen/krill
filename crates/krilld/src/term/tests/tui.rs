//! MK-029: 에이전트가 쓰는 kitty 스택·sync·재생 및 OSC 8·DECSCUSR 회귀.

use super::*;

#[test]
fn kitty_agent_modes_stack_and_replay() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    // MK-008 실측: Claude는 5, Codex는 7을 push한다. 질의마다 그 시점의 값을 답한다.
    let queries = b"\x1b[?u\x1b[>5u\x1b[?u\x1b[>7u\x1b[?u\x1b[<u\x1b[?u\x1b[<u\x1b[?u";
    assert_eq!(
        e.feed(queries, Feed::Live, &look),
        "\x1b[?0u\x1b[?5u\x1b[?7u\x1b[?5u\x1b[?0u"
    );
    assert_eq!(e.feed(b"\x1b[>7u\x1b[?u", Feed::Replay, &look), "");
    assert_eq!(e.feed(b"\x1b[?u", Feed::Live, &look), "\x1b[?7u");
}

#[test]
fn kitty_queries_are_deferred_until_sync_ends() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    assert_eq!(e.feed(b"\x1b[?2026h\x1b[>5u\x1b[?u", Feed::Live, &look), "");
    assert_eq!(e.end_sync(Feed::Live, &look), "\x1b[?5u");
    assert_eq!(
        e.feed(b"\x1b[?2026h\x1b[<u\x1b[?u", Feed::Replay, &look),
        ""
    );
    assert_eq!(e.end_sync(Feed::Replay, &look), "");
    assert_eq!(e.feed(b"\x1b[?u", Feed::Live, &look), "\x1b[?0u");
}

#[test]
fn sgr_mouse_modes_and_application_cursor_restore_for_cli() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    e.feed(b"\x1b[?1;1002;1006h\x1b[>7u", Feed::Live, &look);
    let modes = host_modes(*e.term.mode());
    for enabled in ["\x1b[?1h", "\x1b[?1002h", "\x1b[?1006h", "\x1b[=7u"] {
        assert!(modes.contains(enabled), "{modes:?}");
    }
    e.feed(b"\x1b[?1;1002;1006l\x1b[<u", Feed::Live, &look);
    let modes = host_modes(*e.term.mode());
    for disabled in ["\x1b[?1l", "\x1b[?1002l", "\x1b[?1006l", "\x1b[=0u"] {
        assert!(modes.contains(disabled), "{modes:?}");
    }
}

#[test]
fn fragmented_osc8_and_all_decscusr_shapes() {
    let look = fresh();
    let mut e = Engine::new(SIZE);
    let link = "\x1b]8;id=문서;https://example.com/한글\x1b\\문서\x1b]8;;\x1b\\!";
    // UTF-8와 ST 사이를 포함해 모든 바이트를 따로 공급한다.
    for byte in link.as_bytes() {
        assert_eq!(e.feed(&[*byte], Feed::Live, &look), "");
    }
    let s = e.screen();
    for column in 0..4 {
        assert_eq!(s.link_at(0, column), Some("https://example.com/한글"));
    }
    assert_eq!(s.link_at(0, 4), None);
    for (n, shape, blink) in [
        (1, CursorShape::Block, true),
        (2, CursorShape::Block, false),
        (3, CursorShape::Underline, true),
        (4, CursorShape::Underline, false),
        (5, CursorShape::Bar, true),
        (6, CursorShape::Bar, false),
    ] {
        e.feed(format!("\x1b[{n} q").as_bytes(), Feed::Live, &look);
        assert_eq!(e.screen().cursor_style, CursorStyle { shape, blink });
    }
}
