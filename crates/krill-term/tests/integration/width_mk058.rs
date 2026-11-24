//! MK-058 폭 보정 스냅샷: 불일치 목록 0.
//!
//! 엔진(`alacritty_terminal` 0.26)은 문자 단위 폭으로 칸을 잡는다. 기대폭(문자열 기준 2칸)과 어긋나는
//! 15건을 실측으로 확정했고, 보정 뒤에는 전부 한 셰이핑 묶음으로 2칸에 그린다.
//! 엔진 셀·커서·주소는 그대로다(지휘자 결정 A'). 행 픽스처는 2026-10-15 실측 배치와 같다.
//! 새 불일치가 생기면 이 테스트가 깨진다. 그때는 묶음 규칙(`krill_term::width`)을 고치고 목록을 갱신한다.

use krill_proto::screen::{Cell, Color, Flags};
use krill_proto::theme::DEFAULT;
use krill_term::grid::plan_row;

fn narrow(ch: char, zerowidth: &[char]) -> Cell {
    Cell {
        ch,
        zerowidth: zerowidth.to_vec(),
        fg: Color::Default,
        bg: Color::Default,
        underline: Color::Default,
        flags: Flags::default(),
    }
}

fn wide(ch: char, zerowidth: &[char]) -> [Cell; 2] {
    let mut first = narrow(ch, zerowidth);
    first.flags = Flags::WIDE;
    [
        first,
        Cell {
            ch: ' ',
            zerowidth: Vec::new(),
            fg: Color::Default,
            bg: Color::Default,
            underline: Color::Default,
            flags: Flags::SPACER,
        },
    ]
}

fn pair(ch: char, zerowidth: &[char]) -> Vec<Cell> {
    wide(ch, zerowidth).to_vec()
}

fn check(name: &str, cells: Vec<Cell>, text: &str) {
    // 뒤에 빈칸을 둔다(넘침 판정용).
    let mut full = cells;
    full.push(Cell::default());
    let plan = plan_row(&full, &DEFAULT);
    assert_eq!(plan.segments.len(), 1, "{name}: 묶음 하나여야 한다");
    let seg = &plan.segments[0];
    assert_eq!(seg.column, 0, "{name}: 왼쪽 끝에서 그린다");
    assert_eq!(seg.text, text, "{name}: 합친 글자");
    assert!(
        seg.wide || seg.overflow,
        "{name}: 그리는 폭이 2칸이어야 한다"
    );
}

#[test]
fn mismatch_list_is_empty() {
    // 전부 보정 뒤 한 묶음으로 2칸에 그린다.
    let cases: &[(&str, Vec<Cell>, &str)] = &[
        ("heart-vs16", vec![narrow('❤', &['\u{FE0F}'])], "❤\u{FE0F}"),
        ("sun-vs16", vec![narrow('☀', &['\u{FE0F}'])], "☀\u{FE0F}"),
        ("female-vs16", vec![narrow('♀', &['\u{FE0F}'])], "♀\u{FE0F}"),
        (
            "keycap-1",
            vec![narrow('1', &['\u{FE0F}', '\u{20E3}'])],
            "1\u{FE0F}\u{20E3}",
        ),
        (
            "keycap-hash",
            vec![narrow('#', &['\u{FE0F}', '\u{20E3}'])],
            "#\u{FE0F}\u{20E3}",
        ),
        (
            "keycap-star",
            vec![narrow('*', &['\u{FE0F}', '\u{20E3}'])],
            "*\u{FE0F}\u{20E3}",
        ),
        (
            "woman-tech",
            [
                pair('👩', &[]),
                pair('\u{1F3FD}', &['\u{200D}']),
                pair('💻', &[]),
            ]
            .concat(),
            "👩\u{1F3FD}\u{200D}💻",
        ),
        (
            "family",
            [
                pair('👨', &['\u{200D}']),
                pair('👩', &['\u{200D}']),
                pair('👧', &['\u{200D}']),
                pair('👦', &[]),
            ]
            .concat(),
            "👨\u{200D}👩\u{200D}👧\u{200D}👦",
        ),
        (
            "rainbow-flag",
            [
                vec![narrow('🏳', &['\u{FE0F}', '\u{200D}'])],
                pair('🌈', &[]),
            ]
            .concat(),
            "🏳\u{FE0F}\u{200D}🌈",
        ),
        (
            "pirate-flag",
            [pair('🏴', &['\u{200D}']), vec![narrow('☠', &['\u{FE0F}'])]].concat(),
            "🏴\u{200D}☠\u{FE0F}",
        ),
        (
            "man-laptop",
            [pair('👨', &['\u{200D}']), pair('💻', &[])].concat(),
            "👨\u{200D}💻",
        ),
        (
            "mx-claus",
            [pair('🧑', &['\u{200D}']), pair('🎄', &[])].concat(),
            "🧑\u{200D}🎄",
        ),
        (
            "handshake",
            [
                pair('🫱', &[]),
                pair('\u{1F3FB}', &['\u{200D}']),
                pair('🫲', &[]),
                pair('\u{1F3FC}', &[]),
            ]
            .concat(),
            "🫱\u{1F3FB}\u{200D}🫲\u{1F3FC}",
        ),
        (
            "skin-alone",
            [pair('👋', &[]), pair('\u{1F3FD}', &[])].concat(),
            "👋\u{1F3FD}",
        ),
        (
            "couple-kiss",
            [
                pair('👩', &['\u{200D}']),
                vec![narrow('❤', &['\u{FE0F}', '\u{200D}'])],
                pair('💋', &['\u{200D}']),
                pair('👨', &[]),
            ]
            .concat(),
            "👩\u{200D}❤\u{FE0F}\u{200D}💋\u{200D}👨",
        ),
    ];
    assert_eq!(cases.len(), 15, "불일치 목록 15건");
    for (name, cells, text) in cases {
        check(name, cells.clone(), text);
    }
}

#[test]
fn unaffected_rows_keep_segments() {
    // 국기·결합·한글: 묶음 규칙 밖이라 예전 그대로다.
    // 엔진 행은 끝 빈칸을 잘라낸다. 빈칸 없는 행으로 검사한다.
    let flag = vec![narrow('🇰', &[]), narrow('🇷', &[])];
    let plan = plan_row(&flag, &DEFAULT);
    assert_eq!(plan.segments.len(), 1);
    assert_eq!(plan.segments[0].text, "🇰🇷");
    assert!(!plan.segments[0].wide && !plan.segments[0].overflow);

    let combining = vec![narrow('a', &['\u{301}']), narrow('b', &[])];
    let plan = plan_row(&combining, &DEFAULT);
    let texts: Vec<_> = plan.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["a\u{301}", "b"]);

    let mut hangul = pair('한', &[]);
    hangul.push(narrow('x', &[]));
    let plan = plan_row(&hangul, &DEFAULT);
    let texts: Vec<_> = plan
        .segments
        .iter()
        .map(|s| (s.column, s.wide, s.text.as_str()))
        .collect();
    assert_eq!(texts, [(0, true, "한"), (2, false, "x")]);
}
