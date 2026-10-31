use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Modifiers, TestAppContext};
use krill_proto::screen::{Cell, Revision, Seq};
use krill_proto::search::{Direction, Hit, SearchResult, Snapshot, Source};

use super::*;

#[test]
fn status_formats_hit_and_empty() {
    assert_eq!(status_text(None), "");
    let empty = SearchResult {
        req: 1,
        snapshot: Snapshot::default(),
        total: 0,
        hit: None,
        wrapped: false,
        stale: false,
        incomplete: false,
        approximate: false,
    };
    assert_eq!(status_text(Some(&empty)), "0건");
    let found = SearchResult {
        req: 2,
        snapshot: Snapshot {
            revision: Revision(1),
            seq: Seq(5),
            oldest: Seq(0),
            pages: 0,
        },
        total: 3,
        hit: Some(Hit {
            index: 1,
            source: Source::Screen,
            row: 0,
            total_rows: 1,
            column: 0,
            cols: 2,
            cells: vec![Cell::default(); 2],
        }),
        wrapped: false,
        stale: false,
        incomplete: false,
        approximate: false,
    };
    assert_eq!(status_text(Some(&found)), "2/3");
}

#[test]
fn unfinished_search_is_not_reported_as_zero_matches() {
    let mut result = SearchResult {
        req: 1,
        snapshot: Snapshot::default(),
        total: 0,
        hit: None,
        wrapped: false,
        stale: false,
        incomplete: true,
        approximate: false,
    };
    assert_eq!(status_text(Some(&result)), text::INCOMPLETE.get());
    result.incomplete = false;
    assert_eq!(status_text(Some(&result)), text::EMPTY.get());
    assert_eq!(
        text::INCOMPLETE.in_language(crate::i18n::Language::En),
        "Partial"
    );
}

#[gpui::test]
fn incomplete_search_status_keeps_bar_and_close_bounds(cx: &mut TestAppContext) {
    struct Host(Entity<Bar>);
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().relative().child(self.0.clone())
        }
    }
    cx.update(gpui_component::init);
    let original = crate::i18n::language();
    for language in [crate::i18n::Language::Ko, crate::i18n::Language::En] {
        cx.update(|cx| crate::i18n::set_language(language, cx));
        for width in [600., 240.] {
            let (host, vcx) =
                cx.add_window_view(|window, cx| Host(cx.new(|cx| Bar::new(window, cx))));
            let bar = vcx.update(|_, cx| host.read(cx).0.clone());
            bar.update(vcx, |bar, cx| {
                bar.set_options_supported(true);
                cx.notify();
            });
            vcx.simulate_resize(gpui::size(px(width), px(400.)));
            vcx.run_until_parked();
            let before = vcx.debug_bounds("search-bar").unwrap();
            let close = vcx.debug_bounds("search-close").unwrap();
            let status = vcx.debug_bounds("search-status").unwrap();
            for incomplete in [true, false, true] {
                let result = SearchResult {
                    req: 1,
                    snapshot: Snapshot::default(),
                    total: 0,
                    hit: None,
                    wrapped: false,
                    stale: false,
                    incomplete,
                    approximate: false,
                };
                bar.update(vcx, |bar, cx| bar.set_result(Some(&result), cx));
                vcx.run_until_parked();
                assert_eq!(vcx.debug_bounds("search-bar"), Some(before));
                assert_eq!(vcx.debug_bounds("search-close"), Some(close));
                assert_eq!(vcx.debug_bounds("search-status"), Some(status));
            }
            vcx.simulate_mouse_move(status.center(), None, Modifiers::default());
            vcx.executor().advance_clock(Duration::from_millis(600));
            vcx.run_until_parked();
            assert!(vcx.debug_bounds("tooltip:search-incomplete").is_some());
            assert_eq!(vcx.debug_bounds("search-bar"), Some(before));
            assert_eq!(vcx.debug_bounds("search-close"), Some(close));
            vcx.simulate_mouse_move(gpui::point(px(0.), px(0.)), None, Modifiers::default());
            bar.update(vcx, |bar, cx| bar.set_result(None, cx));
            vcx.run_until_parked();
            assert_eq!(vcx.debug_bounds("search-bar"), Some(before));
            assert_eq!(vcx.debug_bounds("search-close"), Some(close));
        }
    }
    cx.update(|cx| crate::i18n::set_language(original, cx));
}

#[test]
fn status_marks_stale_and_wrapped() {
    let mut stale = SearchResult {
        req: 3,
        snapshot: Snapshot::default(),
        total: 3,
        hit: Some(Hit {
            index: 0,
            source: Source::Screen,
            row: 0,
            total_rows: 1,
            column: 0,
            cols: 1,
            cells: vec![Cell::default()],
        }),
        wrapped: false,
        stale: true,
        incomplete: false,
        approximate: false,
    };
    assert_eq!(status_text(Some(&stale)), "1/3 오래됨");
    stale.stale = false;
    stale.wrapped = true;
    assert_eq!(status_text(Some(&stale)), "1/3 순환");
    stale.hit = None;
    stale.total = 0;
    stale.wrapped = false;
    stale.stale = true;
    assert_eq!(status_text(Some(&stale)), "0건 오래됨");
}

/// AS-15: 데몬이 `u64::MAX`를 보내도 panic 없이 포화 표시한다.
#[test]
fn status_saturates_huge_hit_index() {
    let mut huge = SearchResult {
        req: 4,
        snapshot: Snapshot::default(),
        total: u64::MAX,
        hit: Some(Hit {
            index: u64::MAX,
            source: Source::Screen,
            row: 0,
            total_rows: 1,
            column: 0,
            cols: 1,
            cells: vec![Cell::default()],
        }),
        wrapped: false,
        stale: false,
        incomplete: false,
        approximate: false,
    };
    assert_eq!(
        status_text(Some(&huge)),
        format!("{}/{}", u64::MAX, u64::MAX)
    );
    huge.stale = true;
    assert!(status_text(Some(&huge)).ends_with("오래됨"));
}

#[gpui::test]
fn search_bar_emits_query_steps_and_dismiss(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(Bar::new);
    cx.update(|window, cx| view.update(cx, |b, cx| b.focus(window, cx)));
    let events = Rc::new(RefCell::new(Vec::new()));
    let got = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, e: &Event, _| {
            got.borrow_mut().push(e.clone());
        })
        .detach();
    });
    cx.run_until_parked();

    cx.simulate_input("err");
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).query().to_owned()), "err");
    assert!(events.borrow().is_empty(), "typing must be debounced");
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(149));
    cx.run_until_parked();
    assert!(events.borrow().is_empty());
    // 새 입력이 들어오면 앞선 타이머를 취소하고 마지막 입력부터 150ms를 센다.
    cx.simulate_input("or");
    cx.run_until_parked();
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(149));
    cx.run_until_parked();
    assert!(events.borrow().is_empty());
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(1));
    cx.run_until_parked();
    assert_eq!(*events.borrow(), [Event::Query("error".to_owned())]);

    cx.simulate_keystrokes("enter up down escape");
    assert_eq!(
        *events.borrow(),
        [
            Event::Query("error".to_owned()),
            Event::Step(Direction::Next),
            Event::Step(Direction::Previous),
            Event::Step(Direction::Next),
            Event::Dismiss,
        ]
    );
}

/// OWE-868: ⇧Enter는 이전 찾기다(Enter·↓ 다음, ↑ 이전).
#[gpui::test]
fn shift_enter_steps_previous(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(Bar::new);
    cx.update(|window, cx| view.update(cx, |b, cx| b.focus(window, cx)));
    let events = Rc::new(RefCell::new(Vec::new()));
    let got = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, e: &Event, _| {
            got.borrow_mut().push(e.clone());
        })
        .detach();
    });
    cx.run_until_parked();
    cx.simulate_input("err");
    cx.run_until_parked();
    cx.simulate_modifiers_change(Modifiers {
        shift: true,
        ..Modifiers::default()
    });
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_modifiers_change(Modifiers::default());
    assert_eq!(*events.borrow(), [Event::Step(Direction::Previous)]);
}

#[gpui::test]
fn navigation_and_dismiss_cancel_pending_query(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    for (key, expected) in [
        ("enter", Event::Step(Direction::Next)),
        ("up", Event::Step(Direction::Previous)),
        ("down", Event::Step(Direction::Next)),
        ("escape", Event::Dismiss),
    ] {
        let (view, vcx) = cx.add_window_view(Bar::new);
        vcx.update(|window, cx| view.update(cx, |b, cx| b.focus(window, cx)));
        let events = Rc::new(RefCell::new(Vec::new()));
        let got = events.clone();
        vcx.update(|_, cx| {
            cx.subscribe(&view, move |_, e: &Event, _| {
                got.borrow_mut().push(e.clone());
            })
            .detach();
        });
        vcx.run_until_parked();
        vcx.simulate_input("pending");
        vcx.run_until_parked();
        assert!(events.borrow().is_empty());
        vcx.simulate_keystrokes(key);
        vcx.run_until_parked();
        assert_eq!(events.borrow().as_slice(), std::slice::from_ref(&expected));
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(150));
        vcx.run_until_parked();
        assert_eq!(
            *events.borrow(),
            [expected],
            "no delayed duplicate after {key}"
        );
    }
}

#[gpui::test]
fn clearing_query_is_immediate_and_cancels_pending_search(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(Bar::new);
    cx.update(|window, cx| view.update(cx, |b, cx| b.focus(window, cx)));
    let events = Rc::new(RefCell::new(Vec::new()));
    let got = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, e: &Event, _| {
            got.borrow_mut().push(e.clone());
        })
        .detach();
    });
    cx.run_until_parked();
    cx.simulate_input("x");
    cx.run_until_parked();
    cx.simulate_keystrokes("backspace");
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).query().to_owned()), "");
    assert_eq!(*events.borrow(), [Event::Query(String::new())]);
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(150));
    cx.run_until_parked();
    assert_eq!(*events.borrow(), [Event::Query(String::new())]);
}

#[gpui::test]
fn terminal_find_wraps_controls_inside_narrow_viewport(cx: &mut TestAppContext) {
    struct SearchViewport(Entity<Bar>);
    impl Render for SearchViewport {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().relative().child(self.0.clone())
        }
    }
    cx.update(gpui_component::init);
    let (host, cx) =
        cx.add_window_view(|window, cx| SearchViewport(cx.new(|cx| Bar::new(window, cx))));
    let view = cx.update(|_, cx| host.read(cx).0.clone());
    view.update(cx, |bar, cx| {
        bar.set_options_supported(true);
        cx.notify();
    });
    cx.simulate_resize(gpui::size(px(240.), px(300.)));
    cx.update(|window, _| window.set_rem_size(px(32.)));
    cx.run_until_parked();
    let bar = cx.debug_bounds("search-bar").unwrap();
    let close = cx.debug_bounds("search-close").unwrap();
    assert!(bar.left() >= px(16.) && bar.right() <= px(224.), "{bar:?}");
    assert!(
        close.left() >= bar.left()
            && close.right() <= bar.right()
            && close.top() >= bar.top()
            && close.bottom() <= bar.bottom(),
        "{close:?} outside {bar:?}"
    );
}
