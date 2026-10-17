//! 마우스 선택과 복사할 텍스트(MK-033). 좌표는 보이는 화면의 칸이고, 두 점 사이를 읽는 순서로 잇는다(줄 단위 흐름).
//!
//! - 각 줄 끝의 공백은 버린다(행은 끝의 기본 칸이 잘려 온다).
//! - 자동 줄바꿈으로 이어진 행([`Flags::WRAPLINE`])은 줄바꿈 없이 붙인다.
//! - 2칸 글자의 둘째 칸([`Flags::SPACER`])은 건너뛰고, 폭 0 문자는 앞 글자 뒤에 붙인다.

use crate::grid::{Cell, Flags, Screen};

/// 화면 칸 하나. 정렬은 읽는 순서(행 먼저).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub line: u16,
    pub column: u16,
}

/// 누른 칸(`anchor`)부터 지금 칸(`head`)까지. 양 끝 칸을 포함한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
}

impl Selection {
    pub fn new(at: Point) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    fn ordered(self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// `line`에서 선택한 열 구간 `(시작 열, 칸 수)`. `cols`는 화면 폭. 선택 밖 행이면 `None`.
    pub fn span(self, line: u16, cols: u16) -> Option<(u16, u16)> {
        let (start, end) = self.ordered();
        if line < start.line || line > end.line || cols == 0 {
            return None;
        }
        let first = if line == start.line { start.column } else { 0 };
        let last = if line == end.line {
            end.column
        } else {
            cols - 1
        };
        let last = last.min(cols - 1);
        (first <= last).then(|| (first, last - first + 1))
    }

    /// 선택한 칸의 텍스트.
    pub fn text(self, screen: &Screen) -> String {
        let (start, end) = self.ordered();
        let mut out = String::new();
        for line in start.line..=end.line.min(screen.lines.saturating_sub(1)) {
            let Some((first, n)) = self.span(line, screen.cols) else {
                continue;
            };
            let row: &[Cell] = screen
                .rows
                .get(usize::from(line))
                .map_or(&[], Vec::as_slice);
            // AS-04: 2칸 글자의 오른쪽 절반(SPACER)에서 시작하면 앞 칸(WIDE)부터
            // 포함한다. 왼쪽 절반에서 끝나면 뒤 칸(SPACER)까지 포함한다.
            // 하이라이트(`span`)는 셀 그대로 두고 복사 텍스트만 글자 단위로 맞춘다.
            // 합은 u16을 넘을 수 있어 usize로 계산한다(열 상한 65,535 대비).
            let mut first_usize = usize::from(first);
            let mut n_usize = usize::from(n);
            if first_usize > 0
                && row
                    .get(first_usize)
                    .is_some_and(|c| c.flags.contains(Flags::SPACER))
                && row
                    .get(first_usize - 1)
                    .is_some_and(|c| c.flags.contains(Flags::WIDE))
            {
                first_usize -= 1;
                n_usize += 1;
            }
            if row
                .get(first_usize + n_usize - 1)
                .is_some_and(|c| c.flags.contains(Flags::WIDE))
                && row
                    .get(first_usize + n_usize)
                    .is_some_and(|c| c.flags.contains(Flags::SPACER))
            {
                n_usize += 1;
            }
            let (first, n) = (first_usize, n_usize);
            let mut seg = String::new();
            for c in row
                .iter()
                .skip(first)
                .take(n)
                .filter(|c| !c.flags.contains(Flags::SPACER))
            {
                seg.push(c.ch);
                seg.extend(&c.zerowidth);
            }
            let to_end = first + n == usize::from(screen.cols);
            let wraps = to_end
                && row.len() == usize::from(screen.cols)
                && row
                    .last()
                    .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
            if wraps {
                out.push_str(&seg);
            } else {
                out.push_str(seg.trim_end_matches(' '));
                if line != end.line {
                    out.push('\n');
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(lines: &[&str], cols: u16) -> Screen {
        let rows = lines
            .iter()
            .map(|l| {
                l.chars()
                    .map(|ch| Cell {
                        ch,
                        ..Cell::default()
                    })
                    .collect()
            })
            .collect();
        Screen {
            cols,
            lines: u16::try_from(lines.len()).unwrap(),
            rows,
            ..Screen::default()
        }
    }

    fn sel(a: (u16, u16), b: (u16, u16)) -> Selection {
        Selection {
            anchor: Point {
                line: a.0,
                column: a.1,
            },
            head: Point {
                line: b.0,
                column: b.1,
            },
        }
    }

    #[test]
    fn flows_lines_trims_and_reverses() {
        let s = screen(&["hello world", "ab", "xyz   "], 12);
        assert_eq!(sel((0, 6), (0, 10)).text(&s), "world");
        // 거꾸로 끌어도 같다. 가운데 행은 통째로, 끝 공백은 버린다.
        assert_eq!(sel((2, 1), (0, 6)).text(&s), "world\nab\nxy");
        assert_eq!(sel((1, 0), (2, 11)).text(&s), "ab\nxyz");
        assert_eq!(sel((0, 0), (0, 0)).span(1, 12), None);
        assert_eq!(sel((0, 3), (2, 1)).span(1, 12), Some((0, 12)));
        assert_eq!(sel((0, 3), (2, 1)).span(2, 12), Some((0, 2)));
    }

    #[test]
    fn wide_zerowidth_and_wrapped_rows() {
        let mut s = screen(&["한X", "abcd", "ef"], 4);
        // "한"은 2칸: WIDE + SPACER.
        s.rows[0] = vec![
            Cell {
                ch: '한',
                flags: Flags::WIDE,
                ..Cell::default()
            },
            Cell {
                ch: ' ',
                flags: Flags::SPACER,
                ..Cell::default()
            },
            Cell {
                ch: 'e',
                zerowidth: vec!['\u{301}'],
                ..Cell::default()
            },
        ];
        // 1행은 2행으로 자동 줄바꿈된다.
        s.rows[1][3].flags = Flags::WRAPLINE;
        assert_eq!(sel((0, 0), (0, 3)).text(&s), "한e\u{301}");
        assert_eq!(sel((1, 2), (2, 3)).text(&s), "cdef");
        assert_eq!(sel((0, 0), (2, 0)).text(&s), "한e\u{301}\nabcde");
    }

    #[test]
    fn wide_start_on_right_half_keeps_char() {
        // [a, 한(WIDE), SPACER, b]: 열 2(SPACER, 오른쪽 절반)에서 시작해도
        // 그 글자를 포함한다(AS-04). 하이라이트(span)는 셀 그대로 둔다.
        let mut s = screen(&["    "], 4);
        s.rows[0] = vec![
            Cell {
                ch: 'a',
                ..Cell::default()
            },
            Cell {
                ch: '한',
                flags: Flags::WIDE,
                ..Cell::default()
            },
            Cell {
                ch: ' ',
                flags: Flags::SPACER,
                ..Cell::default()
            },
            Cell {
                ch: 'b',
                ..Cell::default()
            },
        ];
        assert_eq!(sel((0, 2), (0, 3)).text(&s), "한b");
        assert_eq!(sel((0, 3), (0, 2)).text(&s), "한b");
        assert_eq!(sel((0, 1), (0, 1)).text(&s), "한");
        // 왼쪽 절반에서 끝나도 그 글자를 포함한다.
        assert_eq!(sel((0, 0), (0, 1)).text(&s), "a한");
    }
}
