use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use krill_proto::screen::{Cell, Msg as _, Op, Revision, Seq, encode_update};
use krill_proto::term_modes::{Modes, TERM_MODES};

use super::tests::{open, state_path};
use super::*;

#[expect(
    clippy::unwrap_used,
    reason = "테스트 fixture 준비 실패는 즉시 중단한다"
)]
fn remote_terminal<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
    flags: u16,
) -> (Entity<Shell>, &'a mut VisualTestContext, PaneId, UnixStream) {
    let (view, cx) = open(cx, state_path(name));
    let (ours, theirs) = UnixStream::pair().unwrap();
    let pane = cx.update(|_, cx| view.read(cx).layout.focused().unwrap());
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(3), ours, cx);
            s.layout.set_host(
                pane,
                krill_app::hosts::HostId::Remote("selection-peer".into()),
            );
            let body = postcard::to_stdvec(&Modes { kitty: 0, flags }).unwrap();
            assert!(s.on_frame(pane, TERM_MODES, &body, cx));
            let frames = encode_update(
                Revision(1),
                Seq(0),
                vec![
                    Op::Reset { cols: 20, lines: 2 },
                    Op::Line {
                        line: 0,
                        cells: "hello world"
                            .chars()
                            .map(|ch| Cell {
                                ch,
                                ..Cell::default()
                            })
                            .collect(),
                    },
                ],
            )
            .unwrap();
            let (kind, body) = ipc::read_frame(&mut &frames[0][..]).unwrap().unwrap();
            assert!(s.on_frame(pane, kind, &body, cx));
        });
    });
    cx.run_until_parked();
    (view, cx, pane, theirs)
}

#[gpui::test]
fn remote_drag_without_sgr_mouse_copies_locally(cx: &mut TestAppContext) {
    let (view, cx, pane, _peer) = remote_terminal(
        cx,
        "remote-selection-no-sgr",
        Modes::MOUSE_DRAG | Modes::BRACKETED_PASTE,
    );
    let (bounds, cell) = cx.update(|_, cx| view.read(cx).live[&pane].painted.get());
    let at = |column| {
        point(
            bounds.origin.x + cell.width * column,
            bounds.origin.y + cell.height * 0.5,
        )
    };
    cx.simulate_mouse_down(at(10.5), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(at(6.5), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(at(6.5), MouseButton::Left, Modifiers::default());
    cx.simulate_keystrokes("cmd-c");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("world")
    );
}

#[gpui::test]
fn remote_drag_released_outside_pane_finishes_selection(cx: &mut TestAppContext) {
    let (view, cx, pane, _peer) = remote_terminal(cx, "remote-selection-outside", 0);
    cx.update(|_, cx| krill_app::settings::edit(cx, |s| s.copy_on_select = true));
    let (bounds, cell) = cx.update(|_, cx| view.read(cx).live[&pane].painted.get());
    let at = |column| {
        point(
            bounds.origin.x + cell.width * column,
            bounds.origin.y + cell.height * 0.5,
        )
    };
    cx.simulate_mouse_down(at(0.5), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(at(4.5), MouseButton::Left, Modifiers::default());
    let outside = point(px(5.), bounds.origin.y + cell.height * 0.5);
    cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
    cx.update(|_, cx| {
        let live = &view.read(cx).live[&pane];
        assert!(!live.selecting, "release outside must finish the drag");
        assert_eq!(live.selection_text.as_deref(), Some("hello"));
    });
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("hello"),
        "copy-on-select also applies outside the pane"
    );
    cx.simulate_keystrokes("cmd-c");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("hello")
    );
}

#[gpui::test]
fn remote_sgr_mouse_keeps_program_input_and_shift_selection(cx: &mut TestAppContext) {
    use krill_proto::input::Input;

    let (view, cx, pane, mut peer) = remote_terminal(
        cx,
        "remote-selection-sgr",
        Modes::MOUSE_DRAG | Modes::SGR_MOUSE,
    );
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let (bounds, cell) = cx.update(|_, cx| view.read(cx).live[&pane].painted.get());
    let at = |column| {
        point(
            bounds.origin.x + cell.width * column,
            bounds.origin.y + cell.height * 0.5,
        )
    };
    cx.simulate_mouse_down(at(0.5), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(at(4.5), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(at(4.5), MouseButton::Left, Modifiers::default());
    let mut input = Vec::new();
    while input.len() < 3 {
        let (kind, body) = ipc::read_frame(&mut peer).unwrap().unwrap();
        if let Ok(message) = Input::decode(kind, &body) {
            input.push(message.bytes);
        }
    }
    assert_eq!(
        input,
        [
            b"\x1b[<0;1;1M",
            b"\x1b[<32;5;1M".as_slice(),
            b"\x1b[<0;5;1m"
        ]
    );
    assert!(cx.update(|_, cx| view.read(cx).live[&pane].selection.is_none()));

    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    cx.simulate_mouse_down(at(0.5), MouseButton::Left, shift);
    cx.simulate_mouse_move(at(4.5), MouseButton::Left, shift);
    cx.simulate_mouse_up(at(4.5), MouseButton::Left, shift);
    cx.simulate_keystrokes("cmd-c");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("hello")
    );
}
