use krill_proto::theme::DEFAULT;

use super::*;

const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };
const BLUE: Rgb = Rgb { r: 0, g: 0, b: 255 };

fn cell(ch: char) -> Cell {
    Cell {
        ch,
        ..Cell::default()
    }
}

fn styled(ch: char, fg: Color, bg: Color, flags: Flags) -> Cell {
    Cell {
        ch,
        zerowidth: Vec::new(),
        fg,
        bg,
        underline: Color::Default,
        flags,
    }
}

/// 2칸 글자 + 자리 칸.
fn wide(ch: char) -> [Cell; 2] {
    [
        styled(ch, Color::Default, Color::Default, Flags::WIDE),
        styled(' ', Color::Default, Color::Default, Flags::SPACER),
    ]
}

fn row(s: &str) -> Vec<Cell> {
    s.chars().map(cell).collect()
}

#[test]
fn plain_row_is_one_segment_one_run() {
    let plan = plan_row(&row("ls -la  x"), &DEFAULT);
    assert!(plan.bg.is_empty());
    assert_eq!(plan.segments.len(), 1);
    let s = &plan.segments[0];
    assert_eq!((s.column, s.wide, s.text.as_str()), (0, false, "ls -la  x"));
    assert_eq!(
        s.runs,
        [Run {
            len: 9,
            bold: false,
            italic: false
        }]
    );
}

#[test]
fn color_does_not_split_shaping_runs() {
    // truecolor: 칸마다 전경색이 달라도 셰이핑 run은 하나, 색은 칸마다 따로(MK-013 결정 1).
    let cells: Vec<Cell> = (0u8..4)
        .map(|i| {
            styled(
                'a',
                Color::Rgb(Rgb { r: i, g: 0, b: 0 }),
                Color::Default,
                Flags::default(),
            )
        })
        .collect();
    let plan = plan_row(&cells, &DEFAULT);
    let s = &plan.segments[0];
    assert_eq!(s.runs.len(), 1);
    assert_eq!(s.colors.len(), 4);
    assert_eq!(s.color_at(2), Some(Rgb { r: 2, g: 0, b: 0 }));
}

#[test]
fn bold_and_italic_split_runs_not_segments() {
    let cells = [
        cell('a'),
        styled('b', Color::Default, Color::Default, Flags::BOLD),
        styled(
            'c',
            Color::Default,
            Color::Default,
            Flags::BOLD | Flags::ITALIC,
        ),
        cell('d'),
    ];
    let plan = plan_row(&cells, &DEFAULT);
    assert_eq!(plan.segments.len(), 1);
    let flags: Vec<_> = plan.segments[0]
        .runs
        .iter()
        .map(|r| (r.len, r.bold, r.italic))
        .collect();
    assert_eq!(
        flags,
        [
            (1, false, false),
            (1, true, false),
            (1, true, true),
            (1, false, false)
        ]
    );
}

#[test]
fn background_spans_merge_same_color_neighbors() {
    let red = Color::Rgb(RED);
    let cells = [
        styled('a', Color::Default, red, Flags::default()),
        styled('b', Color::Default, red, Flags::default()),
        cell('c'),
        styled('d', Color::Default, red, Flags::default()),
        styled('e', Color::Default, Color::Rgb(BLUE), Flags::default()),
        styled('f', Color::Default, Color::Rgb(BLUE), Flags::default()),
    ];
    let plan = plan_row(&cells, &DEFAULT);
    assert_eq!(
        plan.bg,
        [
            Span {
                column: 0,
                cols: 2,
                color: RED
            },
            Span {
                column: 3,
                cols: 1,
                color: RED
            },
            Span {
                column: 4,
                cols: 2,
                color: BLUE
            },
        ]
    );
    // 배경색은 셰이핑을 나누지 않는다.
    assert_eq!(plan.segments.len(), 1);
}

#[test]
fn wide_chars_get_own_segment_and_two_column_background() {
    // "a한글b": 1칸 / 2칸 묶음(한글) / 1칸
    let mut cells = vec![cell('a')];
    cells.extend(wide('한'));
    cells.extend(wide('글'));
    cells.push(cell('b'));
    cells[1].bg = Color::Rgb(RED);
    cells[3].bg = Color::Rgb(RED);
    let plan = plan_row(&cells, &DEFAULT);
    let segs: Vec<_> = plan
        .segments
        .iter()
        .map(|s| (s.column, s.wide, s.text.as_str()))
        .collect();
    assert_eq!(segs, [(0, false, "a"), (1, true, "한글"), (5, false, "b")]);
    assert_eq!(
        plan.bg,
        [Span {
            column: 1,
            cols: 4,
            color: RED
        }]
    );
    // 2칸 묶음의 칸별 색 위치는 바이트 기준(한글은 3바이트)
    assert_eq!(
        plan.segments[1].colors,
        [(0, DEFAULT.foreground), (3, DEFAULT.foreground)]
    );
}

#[test]
fn zerowidth_cell_is_shaped_alone() {
    let mut e = cell('e');
    e.zerowidth.push('\u{301}');
    let cells = [cell('a'), e, cell('b')];
    let plan = plan_row(&cells, &DEFAULT);
    let texts: Vec<_> = plan.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["a", "e\u{301}", "b"]);
}

#[test]
fn inverse_swaps_and_default_background_is_skipped() {
    let cells = [
        styled('a', Color::Default, Color::Default, Flags::INVERSE),
        cell(' '),
    ];
    let plan = plan_row(&cells, &DEFAULT);
    assert_eq!(
        plan.bg,
        [Span {
            column: 0,
            cols: 1,
            color: DEFAULT.foreground
        }]
    );
    assert_eq!(plan.segments[0].color_at(0), Some(DEFAULT.background));
}

#[test]
fn blank_rows_draw_nothing() {
    assert_eq!(plan_row(&row("      "), &DEFAULT), RowPlan::default());
    assert_eq!(plan_row(&[], &DEFAULT), RowPlan::default());
}

#[test]
fn resolve_palette() {
    assert_eq!(resolve(&DEFAULT, Color::Indexed(1), true), DEFAULT.ansi[1]);
    assert_eq!(
        resolve(&DEFAULT, Color::Indexed(16), true),
        Rgb { r: 0, g: 0, b: 0 }
    );
    assert_eq!(
        resolve(&DEFAULT, Color::Indexed(231), true),
        Rgb {
            r: 255,
            g: 255,
            b: 255
        }
    );
    assert_eq!(
        resolve(&DEFAULT, Color::Indexed(196), true),
        Rgb { r: 255, g: 0, b: 0 }
    );
    assert_eq!(
        resolve(&DEFAULT, Color::Indexed(232), true),
        Rgb { r: 8, g: 8, b: 8 }
    );
    assert_eq!(
        resolve(&DEFAULT, Color::Indexed(255), true),
        Rgb {
            r: 238,
            g: 238,
            b: 238
        }
    );
    assert_eq!(resolve(&DEFAULT, Color::Default, false), DEFAULT.background);
}

#[test]
fn cursor_width_follows_wide_cell() {
    let mut screen = Screen::blank(10, 2);
    screen.rows[0] = wide('한').to_vec();
    assert_eq!(screen.cursor_width(), 2);
    screen.cursor.column = 1;
    assert_eq!(screen.cursor_width(), 1);
}

#[test]
fn underline_and_strike_merge_like_background() {
    // 밑줄(SGR 4)·링크(OSC 8)는 같은 색이면 한 구간, 밑줄 색(SGR 58)이 다르면 나눈다. 셰이핑은 나누지 않는다.
    let u = Flags::UNDERLINE;
    let mut cells = vec![
        styled('a', Color::Default, Color::Default, u),
        styled('b', Color::Default, Color::Default, Flags::LINK),
        styled('c', Color::Default, Color::Default, u),
        styled('d', Color::Default, Color::Default, Flags::STRIKEOUT),
    ];
    cells[2].underline = Color::Rgb(RED);
    cells.extend(wide('한'));
    cells[4].flags = Flags::WIDE | u;
    let plan = plan_row(&cells, &DEFAULT);
    let fg = DEFAULT.foreground;
    assert_eq!(
        plan.underlines,
        [
            Span {
                column: 0,
                cols: 2,
                color: fg
            },
            Span {
                column: 2,
                cols: 1,
                color: RED
            },
            Span {
                column: 4,
                cols: 2,
                color: fg
            },
        ]
    );
    assert_eq!(
        plan.strikes,
        [Span {
            column: 3,
            cols: 1,
            color: fg
        }]
    );
    assert!(plan.bg.is_empty());
    let texts: Vec<_> = plan.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["abcd", "한"]);
}

#[test]
fn underline_follows_inverse_and_dim_glyph_color() {
    let cells = [
        styled(
            'a',
            Color::Rgb(RED),
            Color::Default,
            Flags::UNDERLINE | Flags::INVERSE,
        ),
        styled(
            'b',
            Color::Rgb(RED),
            Color::Default,
            Flags::UNDERLINE | Flags::DIM,
        ),
    ];
    let plan = plan_row(&cells, &DEFAULT);
    // 흐림: (2 × 글자 + 배경) / 3. 빨강 255·0·0, DEFAULT(Krill Deep) 배경 16·19·27.
    let dim = Rgb { r: 175, g: 6, b: 9 };
    assert_eq!(plan.segments[0].color_at(0), Some(DEFAULT.background));
    assert_eq!(plan.segments[0].color_at(1), Some(dim));
    let colors: Vec<_> = plan.underlines.iter().map(|s| s.color).collect();
    assert_eq!(colors, [DEFAULT.background, dim]);
}

#[test]
fn hidden_draws_background_only() {
    let mut x = styled(
        'x',
        Color::Default,
        Color::Rgb(BLUE),
        Flags::HIDDEN | Flags::UNDERLINE | Flags::STRIKEOUT,
    );
    x.zerowidth.push('\u{301}');
    let cells = [cell('a'), x, cell('b')];
    let plan = plan_row(&cells, &DEFAULT);
    assert_eq!(
        plan.bg,
        [Span {
            column: 1,
            cols: 1,
            color: BLUE
        }]
    );
    assert!(plan.underlines.is_empty() && plan.strikes.is_empty());
    // 숨긴 칸은 공백이 되고 묶음을 끊지 않는다(폭 0 문자도 버린다).
    let texts: Vec<_> = plan.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["a b"]);
    let only_hidden = [styled('x', Color::Default, Color::Default, Flags::HIDDEN)];
    assert_eq!(plan_row(&only_hidden, &DEFAULT), RowPlan::default());
}
