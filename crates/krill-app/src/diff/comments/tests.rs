use std::{cell::RefCell, rc::Rc};

use gpui::{Modifiers, TestAppContext};
use krill_proto::input::{Input, Now};
use krill_proto::screen::Msg as _;
use krill_proto::term_modes::Modes;

use super::*;

#[derive(Clone, Default)]
struct Memory(Rc<RefCell<Vec<u8>>>);
impl std::io::Write for Memory {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn bracketed() -> Modes {
    Modes {
        flags: Modes::BRACKETED_PASTE,
        ..Modes::default()
    }
}

fn comment(path: &str, side: DiffSide, body: &str) -> Comment {
    Comment {
        range: DiffLineRange::new(path.to_owned(), side, 9, 11),
        comparison: Comparison::Worktree,
        body: body.to_owned(),
    }
}

#[test]
fn aggregates_context_and_escapes_user_control_characters() {
    let mut deleted = comment("한🙂.rs", DiffSide::Original, "복원\n둘째 줄\u{1b}[201~\r");
    deleted.comparison = Comparison::Index;
    deleted.range = deleted.range.with_end_side(DiffSide::Modified);
    let comments = [deleted, comment("b.rs", DiffSide::Modified, "두 번째")];
    let text = prompt(&comments);
    assert!(text.contains("1. \"한🙂.rs\" [index] old:9–new:11"));
    assert!(text.contains("2. \"b.rs\" [worktree] new:9–new:11"));
    assert!(text.contains("복원\\n둘째 줄\\u{1b}[201~\\r"));
    assert!(!text.contains(['\r', '\u{1b}']));
    for modes in [Modes::default(), bracketed()] {
        let bytes = input_bytes(&text, modes);
        assert_eq!(
            bytes.strip_suffix(b"\r").map(|s| s.contains(&b'\r')),
            Some(false)
        );
        assert_eq!(bytes.last(), Some(&b'\r'));
        if modes.contains(Modes::BRACKETED_PASTE) {
            assert!(bytes.starts_with(b"\x1b[200~"));
            assert!(bytes.ends_with(b"\x1b[201~\r"));
        } else {
            assert!(!bytes.contains(&b'\n'));
        }
    }
}

#[gpui::test]
fn compose_two_files_click_send_and_deliver_one_existing_input(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (panel, cx) = cx.add_window_view(|_, cx| Panel::unconnected("/review".into(), cx));
    let got = Rc::new(RefCell::new(Vec::new()));
    let events = got.clone();
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, e: &SendComments, _| {
            events.borrow_mut().push(e.clone());
        })
        .detach();
    });
    for (path, side, body) in [
        ("a.rs", DiffSide::Original, "고쳐 주세요"),
        ("b.rs", DiffSide::Modified, "한🙂\n둘째 줄"),
    ] {
        cx.update(|window, cx| {
            panel.update(cx, |p, cx| {
                p.begin_comment(DiffLineRange::new(path, side, 2, 3), window, cx);
            });
        });
        cx.run_until_parked();
        cx.simulate_input(body);
        cx.run_until_parked();
        let save = cx.debug_bounds("comment-save").unwrap();
        cx.simulate_click(save.center(), Modifiers::default());
        cx.run_until_parked();
    }
    cx.update(|_, cx| assert_eq!(panel.read(cx).review.comments.len(), 2));
    let send = cx.debug_bounds("comments-send").unwrap();
    cx.simulate_click(send.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(got.borrow().len(), 1);
    let expected = input_bytes(&got.borrow()[0].0, bracketed());
    let memory = Memory::default();
    let mut pane = crate::pane::Pane::default();
    assert!(
        pane.link(
            krill_proto::SessionId(7),
            1,
            memory.clone(),
            krill_proto::input::FEAT_INPUT,
            Now::get()
        )
        .is_empty()
    );
    assert_eq!(pane.input(&expected, Now::get()), None);
    let data = memory.0.borrow();
    let mut wire = &data[..];
    let (kind, body) = krill_proto::ipc::read_frame(&mut wire).unwrap().unwrap();
    assert_eq!(Input::decode(kind, &body).unwrap().bytes, expected);
    assert!(wire.is_empty(), "one input frame for the entire review");
    cx.update(|_, cx| {
        panel.update(cx, |p, cx| {
            p.comments_sent(false, cx);
            assert_eq!(p.review.comments.len(), 2);
            p.comments_sent(true, cx);
            assert!(p.review.comments.is_empty());
        });
    });
}

#[gpui::test]
fn draft_anchor_is_stable_and_limits_preserve_it(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (panel, cx) = cx.add_window_view(|_, cx| Panel::unconnected("/review".into(), cx));
    cx.update(|window, cx| {
        panel.update(cx, |p, cx| {
            let range = DiffLineRange::new("old.rs", DiffSide::Original, 10, 12);
            p.begin_comment(range.clone(), window, cx);
            p.comparison = Comparison::Index;
            p.begin_comment(
                DiffLineRange::new("other.rs", DiffSide::Modified, 1, 1),
                window,
                cx,
            );
            assert_eq!(p.review.draft.as_ref().unwrap().0, range);
            assert_eq!(p.review.draft.as_ref().unwrap().1, Comparison::Worktree);
            let input = p.review.draft.as_ref().unwrap().2.clone();
            input.update(cx, |i, cx| i.set_value(" \n ", window, cx));
            p.save_comment(window, cx);
            assert!(p.review.comments.is_empty());
            input.update(cx, |i, cx| i.set_value("x".repeat(MAX_PROMPT), window, cx));
            p.save_comment(window, cx);
            assert!(p.review.comments.is_empty());
            assert!(p.review.draft.is_some());
            assert_eq!(p.review.status, Some(text::LIMIT));
            input.update(cx, |i, cx| i.set_value("valid", window, cx));
            p.save_comment(window, cx);
            assert_eq!(p.review.comments.len(), 1);
            assert_eq!(p.review.comments[0].range, range);
            assert_eq!(p.review.comments[0].comparison, Comparison::Worktree);
            assert!(p.review.draft.is_none());
            assert_eq!(p.review.status, None);
            p.review.comments = vec![comment("a.rs", DiffSide::Modified, "saved"); MAX_COMMENTS];
            p.begin_comment(
                DiffLineRange::new("b.rs", DiffSide::Modified, 1, 1),
                window,
                cx,
            );
            let input = p.review.draft.as_ref().unwrap().2.clone();
            input.update(cx, |i, cx| i.set_value("one too many", window, cx));
            p.save_comment(window, cx);
            assert_eq!(p.review.comments.len(), MAX_COMMENTS);
            assert!(p.review.draft.is_some());
            assert_eq!(p.review.status, Some(text::LIMIT));
            p.set_repo("/other-review".into(), cx);
            assert!(p.review.comments.is_empty());
            assert!(p.review.draft.is_none());
        });
    });
}
