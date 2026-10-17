use krill_proto::screen::{Cell, Color, Flags};

use super::*;

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

fn wide(ch: char, zerowidth: &[char]) -> (Cell, Cell) {
    let mut first = narrow(ch, zerowidth);
    first.flags = Flags::WIDE;
    (
        first,
        Cell {
            ch: ' ',
            zerowidth: Vec::new(),
            fg: Color::Default,
            bg: Color::Default,
            underline: Color::Default,
            flags: Flags::SPACER,
        },
    )
}

/// 행을 `clusters`에 넣고 (start, end, overflow)만 본다.
fn plan(cells: &[Cell]) -> Vec<(u16, u16, bool)> {
    clusters(cells)
        .iter()
        .map(|c| (c.start, c.end, c.overflow))
        .collect()
}

#[test]
fn zwj_family_is_one_cluster() {
    // 👨‍👩‍👧‍👦: 엔진 8칸(W S × 4), ZWJ는 앞 칸에 붙는다.
    let (a0, a1) = wide('👨', &['\u{200D}']);
    let (b0, b1) = wide('👩', &['\u{200D}']);
    let (c0, c1) = wide('👧', &['\u{200D}']);
    let (d0, d1) = wide('👦', &[]);
    let row = [a0, a1, b0, b1, c0, c1, d0, d1, narrow('x', &[])];
    assert_eq!(plan(&row), [(0, 8, false), (8, 9, false)]);
}

#[test]
fn skin_tone_joins_backwards_and_zwj_forwards() {
    // 👩🏽‍💻: 👩(W) + 🏽(W, 뒤에 붙는 꾸밈) + ZWJ가 🏽 칸에 붙어 💻와 잇는다.
    let (a0, a1) = wide('👩', &[]);
    let (b0, b1) = wide('\u{1F3FD}', &['\u{200D}']);
    let (c0, c1) = wide('💻', &[]);
    let row = [a0, a1, b0, b1, c0, c1];
    assert_eq!(plan(&row), [(0, 6, false)]);
}

#[test]
fn narrow_base_zwj_flag_is_one_cluster() {
    // 🏳️‍🌈: 🏳[FE0F, ZWJ](1칸) + 🌈(W).
    let (g0, g1) = wide('🌈', &[]);
    let row = [
        narrow('🏳', &['\u{FE0F}', '\u{200D}']),
        g0,
        g1,
        narrow('x', &[]),
    ];
    assert_eq!(plan(&row), [(0, 3, false), (3, 4, false)]);
}

#[test]
fn vs16_single_overflows_only_into_blank() {
    // 빈칸도 묶음 하나로 나온다(그리기 계획에서는 공백이라 버린다).
    let blank = (1, 2, false);
    // ❤️(1칸) + 빈칸: 넘쳐 그린다. ❤️ + 글자: 1칸에 맞춘다.
    let heart = || narrow('❤', &['\u{FE0F}']);
    assert_eq!(plan(&[heart(), Cell::default()]), [(0, 1, true), blank]);
    assert_eq!(
        plan(&[heart(), narrow('x', &[])]),
        [(0, 1, false), (1, 2, false)]
    );
    // 키캡 1️⃣(1칸) + 빈칸: 넘쳐 그린다.
    let key = narrow('1', &['\u{FE0F}', '\u{20E3}']);
    assert_eq!(plan(&[key, Cell::default()]), [(0, 1, true), blank]);
    // VS15(❤︎)는 넘치지 않는다.
    assert_eq!(
        plan(&[narrow('❤', &['\u{FE0E}']), Cell::default()]),
        [(0, 1, false), blank]
    );
    // ZWJ가 붙은 1칸(예: 'a' + 뒤이은 ZWJ)은 넘치지 않는다.
    assert_eq!(
        plan(&[narrow('a', &['\u{200D}']), Cell::default()]),
        [(0, 1, false), blank]
    );
}

#[test]
fn flags_and_plain_text_do_not_join() {
    // 🇰🇷: 잇는 표시가 없어 따로 둔다(서체에서 낱글자로 보일 수 있음, 범위 밖).
    let row = [narrow('🇰', &[]), narrow('🇷', &[])];
    let row2 = [row[0].clone(), row[1].clone(), narrow('a', &[])];
    assert_eq!(plan(&row2), [(0, 1, false), (1, 2, false), (2, 3, false)]);
}

#[test]
fn zwj_joins_following_text_for_shaping() {
    // ZWJ 뒤에 오는 칸은 셰이핑을 함께 한다(아랍·히브리 잇기 의도와 같다).
    let (a0, a1) = wide('👨', &['\u{200D}']);
    let row = [a0, a1, narrow('x', &[]), narrow('y', &[])];
    assert_eq!(plan(&row), [(0, 3, false), (3, 4, false)]);
}

#[test]
fn huge_row_does_not_panic_and_stays_in_u16() {
    // AS-14: 규약을 어긴 65,535칸 초과 행도 panic 없이 자른다(정상 krilld는 2048칸).
    let row = vec![narrow('x', &[]); 70_000];
    let cs = clusters(&row);
    assert!(!cs.is_empty());
    assert!(cs.iter().all(|c| c.start < c.end));
    assert_eq!(cs.last().map(|c| c.end), Some(u16::MAX));
}

#[test]
fn vs16_single_at_row_end_may_overflow() {
    // AS-16: 잘린 행 끝의 VS16 1칸 묶음도 넘쳐 그릴 수 있다(오른쪽에 가릴 칸이 없음).
    let heart = || narrow('❤', &['\u{FE0F}']);
    assert_eq!(plan(&[heart()]), [(0, 1, true)]);
}
