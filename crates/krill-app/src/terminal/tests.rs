use gpui::{TestAppContext, point};
use krill_proto::theme::DEFAULT;
use krill_term::grid::{Cell, Color, Flags};

use super::*;

/// 한글·truecolor 배경·굵게·커서가 섞인 화면.
fn sample(tag: char) -> Screen {
    let mut screen = Screen::blank(20, 3);
    screen.rows[0] = "$ ls"
        .chars()
        .map(|ch| Cell {
            ch,
            ..Cell::default()
        })
        .collect();
    screen.rows[1] = vec![
        Cell {
            ch: '한',
            bg: Color::Rgb(Rgb { r: 200, g: 0, b: 0 }),
            flags: Flags::WIDE | Flags::BOLD,
            ..Cell::default()
        },
        Cell {
            flags: Flags::SPACER,
            ..Cell::default()
        },
        Cell {
            ch: tag,
            fg: Color::Indexed(196),
            ..Cell::default()
        },
    ];
    // 밑줄(색 지정)·링크·취소선·흐림·숨김(OWE-481)
    screen.rows[2] = [
        Flags::UNDERLINE,
        Flags::LINK,
        Flags::STRIKEOUT,
        Flags::DIM,
        Flags::HIDDEN,
    ]
    .into_iter()
    .map(|flags| Cell {
        ch: 'z',
        underline: Color::Indexed(4),
        flags,
        ..Cell::default()
    })
    .collect();
    screen.cursor.line = 1;
    screen
}

#[gpui::test]
fn paints_and_sizes_to_grid(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Terminal::new(Screen::blank(1, 1), DEFAULT));
    let screen = Rc::new(sample('x'));
    let (cell, ()) = cx.draw(point(px(0.), px(0.)), size(px(800.), px(600.)), |_, _| {
        TerminalElement::new(screen.clone(), DEFAULT)
    });
    // 기본 글꼴 13pt × 1.25줄 높이 = 16px(반올림).
    assert!(cell.width > px(0.) && cell.height == px(18.));
}

#[gpui::test]
fn latest_screen_wins_between_frames(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| Terminal::new(Screen::blank(20, 3), DEFAULT));
    view.update(cx, |t, cx| {
        for tag in ['a', 'b', 'c'] {
            t.set_screen(sample(tag), cx);
        }
    });
    cx.run_until_parked();
    view.read_with(cx, |t, _| assert_eq!(t.screen().rows[1][2].ch, 'c'));
}

#[gpui::test]
fn link_hit_testing_excludes_padding_and_unsafe_schemes(_cx: &mut TestAppContext) {
    let painted: Painted = Rc::new(std::cell::Cell::new((
        Bounds::new(point(px(10.), px(20.)), size(px(40.), px(36.))),
        size(px(10.), px(18.)),
    )));
    let mut s = Screen::blank(4, 2);
    s.links[0].push(krill_proto::screen::Link {
        column: 1,
        cols: 2,
        uri: "https://example.com/".into(),
    });
    assert_eq!(
        link_target(&s, &painted, point(px(25.), px(25.))),
        Some("https://example.com/")
    );
    assert_eq!(link_target(&s, &painted, point(px(9.), px(25.))), None);
    assert_eq!(cell_at(&painted, &s, point(px(50.), px(25.))), None);
    assert_eq!(cell_at(&painted, &s, point(px(25.), px(56.))), None);
    s.links[0][0].uri = "javascript:alert(1)".into();
    assert_eq!(link_target(&s, &painted, point(px(25.), px(25.))), None);
    s.links[0][0].uri = "file:///tmp/evil.command".into();
    assert_eq!(link_target(&s, &painted, point(px(25.), px(25.))), None);
    // MK-076: 양방향(bidi) 재정의 문자가 섞인 스푸핑 URL도 거부한다.
    s.links[0][0].uri = "https://example.com/\u{202e}gpj.exe".into();
    assert_eq!(link_target(&s, &painted, point(px(25.), px(25.))), None);
    assert_eq!(
        osc_link_highlight(&s, &painted, point(px(25.), px(25.))),
        None
    );
}

#[test]
fn plaintext_url_and_osc8_highlight_hit_testing() {
    let painted: Painted = Rc::new(std::cell::Cell::new((
        Bounds::new(point(px(0.), px(0.)), size(px(300.), px(36.))),
        size(px(10.), px(18.)),
    )));
    let mut s = Screen::blank(30, 2);
    s.rows[0] = "go https://example.com/a now"
        .chars()
        .map(|ch| Cell {
            ch,
            ..Cell::default()
        })
        .collect();
    assert_eq!(
        url_link_target(&s, &painted, point(px(55.), px(5.))),
        Some((
            Highlight {
                line: 0,
                column: 3,
                cols: 21,
            },
            "https://example.com/a".to_owned(),
        ))
    );
    assert_eq!(url_link_target(&s, &painted, point(px(15.), px(5.))), None);

    // OSC 8 링크가 있는 칸은 OSC 8이 우선한다.
    s.links[1].push(krill_proto::screen::Link {
        column: 2,
        cols: 4,
        uri: "https://osc8.example/".into(),
    });
    assert_eq!(
        osc_link_highlight(&s, &painted, point(px(35.), px(25.))),
        Some(Highlight {
            line: 1,
            column: 2,
            cols: 4,
        })
    );
}

#[test]
fn file_links_only_produce_local_reveal_paths() {
    let painted: Painted = Rc::new(std::cell::Cell::new((
        Bounds::new(point(px(0.), px(0.)), size(px(40.), px(18.))),
        size(px(10.), px(18.)),
    )));
    let mut screen = Screen::blank(4, 1);
    screen.links[0].push(krill_proto::screen::Link {
        column: 0,
        cols: 4,
        uri: String::new(),
    });
    for uri in [
        "file:///tmp/test%20app.command",
        "file://localhost/tmp/test%20app.command",
    ] {
        screen.links[0][0].uri = uri.into();
        assert_eq!(link_target(&screen, &painted, point(px(5.), px(5.))), None);
        assert_eq!(
            file_link_target(&screen, &painted, point(px(5.), px(5.))),
            Some(std::path::PathBuf::from("/tmp/test app.command"))
        );
    }
    for uri in [
        "file://evil.example/tmp/app",
        "https://example.com/app",
        "file:///tmp/%00app",
        "file:///tmp/%E2%80%AEapp",
    ] {
        screen.links[0][0].uri = uri.into();
        assert_eq!(
            file_link_target(&screen, &painted, point(px(5.), px(5.))),
            None,
            "{uri}"
        );
    }
}

/// MK-058: 이모지·ZWJ·VS16 행을 그린다(겹침 없이 한 요소로 배치되는지만 본다, 픽셀 판정은 사람 확인).
#[gpui::test]
fn paints_emoji_zwj_vs16_rows(cx: &mut TestAppContext) {
    use krill_term::grid::plan_row;
    let mut screen = Screen::blank(20, 3);
    // 👨‍👩‍👧‍👦(엔진 8칸) + 글자: 한 묶음으로 왼쪽 2칸에 그린다.
    let mut family: Vec<Cell> = Vec::new();
    for (ch, zw) in [
        ('👨', vec!['\u{200D}']),
        ('👩', vec!['\u{200D}']),
        ('👧', vec!['\u{200D}']),
        ('👦', vec![]),
    ] {
        family.push(Cell {
            ch,
            zerowidth: zw,
            flags: Flags::WIDE,
            ..Cell::default()
        });
        family.push(Cell {
            flags: Flags::SPACER,
            ..Cell::default()
        });
    }
    family.push(Cell {
        ch: 'x',
        ..Cell::default()
    });
    screen.rows[0] = family;
    let plan = plan_row(&screen.rows[0], &DEFAULT);
    assert_eq!(plan.segments.len(), 2);
    assert_eq!((plan.segments[0].wide, plan.segments[0].column), (true, 0));
    // ❤️(엔진 1칸) + 빈칸: 넘쳐 그린다.
    screen.rows[1] = vec![
        Cell {
            ch: '❤',
            zerowidth: vec!['\u{FE0F}'],
            ..Cell::default()
        },
        Cell::default(),
    ];
    let plan = plan_row(&screen.rows[1], &DEFAULT);
    assert_eq!(plan.segments.len(), 1);
    assert!(plan.segments[0].overflow);
    let screen = Rc::new(screen);
    let (_, cx) = cx.add_window_view(|_, _| Terminal::new(Screen::blank(1, 1), DEFAULT));
    let (cell, ()) = cx.draw(point(px(0.), px(0.)), size(px(800.), px(600.)), |_, _| {
        TerminalElement::new(screen.clone(), DEFAULT)
    });
    assert!(cell.width > px(0.) && cell.height == px(18.));
}

#[test]
fn grid_floors_and_never_reaches_zero() {
    let cell = size(px(8.), px(16.));
    assert_eq!(grid(size(px(803.), px(479.)), cell), (100, 29));
    assert_eq!(grid(size(px(3.), px(0.)), cell), (1, 1));
    assert_eq!(
        grid(size(px(100.), px(100.)), size(px(0.), px(f32::NAN))),
        (1, 1)
    );
}

#[gpui::test]
fn terminal_cell_size_scales_with_window_zoom(cx: &mut TestAppContext) {
    // 설정 전(기본값)은 Menlo(OWE-658).
    cx.update(|cx| {
        assert_eq!(
            crate::settings::terminal_font_family(cx),
            crate::ui::FONT_MONO
        );
    });
    let (_, cx) = cx.add_window_view(|_, _| Terminal::new(Screen::blank(1, 1), DEFAULT));
    let base = cx.update(|window, cx| cell_size(window, cx));
    cx.update(|window, _| window.set_rem_size(px(32.)));
    let zoomed = cx.update(|window, cx| cell_size(window, cx));
    assert!(
        zoomed.height > base.height && zoomed.width > base.width,
        "base={base:?} zoomed={zoomed:?}"
    );
    // 13pt × 2배 줌 = 26px × 1.25 = 32.5 → 33px(반올림).
    assert_eq!(zoomed.height, px(35.));
}

/// OWE-615 원인 3: preedit 묶음은 확정 셀과 같은 칸 단위로 나뉜다(2칸 글자는 2칸 강제 폭).
#[test]
fn preedit_groups_split_by_cell_for_forced_width() {
    let at = CellPoint { line: 0, column: 2 };
    let p = Preedit {
        origin: at,
        text: "한ㄱ".into(),
        columns: 4,
    };
    assert_eq!(
        p.groups(),
        vec![(0, "한".to_owned(), 2), (2, "ㄱ".to_owned(), 2)]
    );
    // NFD 자모(U+1112 U+1161 U+11AB): 폭 0 글자는 앞 칸에 붙는다.
    let p = Preedit {
        origin: at,
        text: "\u{1112}\u{1161}\u{11ab}a".into(),
        columns: 3,
    };
    assert_eq!(
        p.groups(),
        vec![
            (0, "\u{1112}\u{1161}\u{11ab}".to_owned(), 2),
            (2, "a".to_owned(), 1)
        ]
    );
}

/// OWE-655: 배경 알파는 배경 quad에만 곱하고 글자색은 그대로 둔다.
#[test]
fn terminal_background_alpha_applies_only_to_background() {
    let mut element = TerminalElement::new(Screen::blank(1, 1).into(), DEFAULT);
    assert!((element.opacity - 1.).abs() < f32::EPSILON);
    element.opacity = 0.5;
    let dimmed = element.background();
    let opaque = hsla(DEFAULT.background);
    assert!((dimmed.a - opaque.a * 0.5).abs() < 1e-6);
    assert!((dimmed.h - opaque.h).abs() < 1e-6);
    assert!((dimmed.s - opaque.s).abs() < 1e-6);
}

/// OWE-655: 설정 글꼴 크기 → 칸 크기 → PTY 그리드까지 이어진다.
#[gpui::test]
fn terminal_font_size_setting_flows_to_cell_and_pty_grid(cx: &mut TestAppContext) {
    let d = std::env::temp_dir().join(format!("mk-terminal-font-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::settings::init_at(Some(d.clone()), cx);
    });
    let mut cells = Vec::new();
    for pt in [10u8, 13, 24] {
        std::fs::write(
            d.join("settings.toml"),
            format!("terminal_font_size = {pt}\n"),
        )
        .unwrap();
        cx.update(crate::settings::reload);
        let (_, vcx) = cx.add_window_view(|_, _| Terminal::new(Screen::blank(1, 1), DEFAULT));
        cells.push(vcx.update(|window, cx| cell_size(window, cx)));
    }
    let (small, medium, large) = (cells[0], cells[1], cells[2]);
    assert!(small.height < medium.height && medium.height < large.height);
    assert!(small.width < large.width);
    // 같은 면적에 작은 칸이 더 많은 열·행을 만든다(PTY 리사이즈 방향).
    let area = size(px(800.), px(600.));
    let (small_cols, small_rows) = grid(area, small);
    let (large_cols, large_rows) = grid(area, large);
    assert!(small_cols > large_cols && small_rows > large_rows);
    std::fs::remove_dir_all(&d).ok();
}

/// OWE-658: 프로그램이 커서를 지정하지 않았을 때만 설정 기본 커서를 쓴다.
#[test]
fn effective_cursor_uses_setting_only_when_program_is_silent() {
    use krill_proto::screen::{CursorShape, CursorStyle};
    let setting = CursorStyle {
        shape: CursorShape::Bar,
        blink: true,
    };
    assert_eq!(effective_cursor(CursorStyle::default(), setting), setting);
    let program = CursorStyle {
        shape: CursorShape::Underline,
        blink: false,
    };
    assert_eq!(effective_cursor(program, setting), program);
    // 설정도 기본값이면 지금과 같은 깜박이지 않는 블록.
    assert_eq!(
        effective_cursor(CursorStyle::default(), CursorStyle::default()),
        CursorStyle::default()
    );
}
