use std::time::{Duration, Instant};

use gpui::TestAppContext;
use krill_proto::theme::DEFAULT;
use krill_term::grid::Screen;

use super::*;
use crate::terminal::Terminal;

#[test]
fn draws_once_per_key_inside_window() {
    let t0 = Instant::now();
    let mut echo = EchoDraw::default();
    assert!(!echo.take(t0), "키 없이 온 화면");

    echo.key_sent(t0);
    assert!(echo.take(t0 + WINDOW), "창 끝 경계는 포함");
    assert!(!echo.take(t0 + WINDOW), "같은 키로 두 번 그리지 않는다");

    echo.key_sent(t0);
    assert!(!echo.take(t0 + WINDOW + Duration::from_millis(1)), "창 밖");
    assert!(!echo.take(t0), "창 밖 판정도 키를 소비한다");
}

/// 테스트 플랫폼 창은 `AppKit` 핸들이 없다 → 즉시 그리기 태스크가 아무것도 하지 않고 끝난다(빌림 충돌·panic 없음).
#[gpui::test]
fn screen_changed_after_key_is_safe_without_appkit(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Terminal::new(Screen::blank(4, 1), DEFAULT));
    let mut echo = EchoDraw::default();
    echo.key_sent(Instant::now());
    view.update_in(cx, |term, window, cx| {
        term.set_screen(Screen::blank(4, 1), cx);
        echo.screen_changed(window.window_handle(), cx);
    });
    cx.run_until_parked();
    assert!(!echo.take(Instant::now()));
}
