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
    /// `⌥ Option`(Alt)+드래그 사각형(열 구간) 선택 여부.
    pub block: bool,
}

impl Selection {
    pub fn new(at: Point) -> Self {
        Self {
            anchor: at,
            head: at,
            block: false,
        }
    }

    /// `⌥ Option`(Alt)+드래그로 시작하는 사각형(열 구간) 선택.
    pub fn new_block(at: Point) -> Self {
        Self {
            anchor: at,
            head: at,
            block: true,
        }
    }

    /// Orca/xterm 단어 경계: 공백·괄호·따옴표만 구분해 경로·URL·하이픈을 함께 선택한다.
    pub fn is_word_char(ch: char) -> bool {
        !ch.is_whitespace() && !"()[]{}'\",`".contains(ch)
    }

    /// 화면 `at` 칸의 글자. 2칸 글자의 둘째 칸(`SPACER`)이면 앞 칸(`WIDE`)의 글자로 푼다.
    /// 행이 짧아 칸이 없으면(잘린 끝 공백) `None`.
    fn cell_char(screen: &Screen, line: u16, column: u16) -> Option<char> {
        let row = screen.rows.get(usize::from(line))?;
        let cell = row.get(usize::from(column))?;
        if cell.flags.contains(Flags::SPACER) && column > 0 {
            let prev = row.get(usize::from(column) - 1)?;
            if prev.flags.contains(Flags::WIDE) {
                return Some(prev.ch);
            }
        }
        Some(cell.ch)
    }

    /// `at` 칸이 속한 단어 구간(OWE-866, 더블클릭). 구분자·공백을 누르면 그 한 칸만.
    /// 항상 `anchor <= head` 순서로 돌려준다.
    pub fn word_at(screen: &Screen, at: Point) -> Self {
        if screen.cols == 0 {
            return Self::new(at);
        }
        let cols = screen.cols;
        // SPACER(오른쪽 절반)를 누르면 앞 칸(WIDE)부터 센다.
        let mut col = at.column.min(cols - 1);
        if let Some(row) = screen.rows.get(usize::from(at.line))
            && let Some(cell) = row.get(usize::from(col))
            && cell.flags.contains(Flags::SPACER)
            && col > 0
            && row
                .get(usize::from(col) - 1)
                .is_some_and(|p| p.flags.contains(Flags::WIDE))
        {
            col -= 1;
        }
        let at = Point {
            line: at.line,
            ..at
        };
        let at = Point { column: col, ..at };
        let is_word = Self::cell_char(screen, at.line, at.column).is_some_and(Self::is_word_char);
        if !is_word {
            return Self::new(at);
        }
        let mut first = at.column;
        while first > 0
            && Self::cell_char(screen, at.line, first - 1).is_some_and(Self::is_word_char)
        {
            first -= 1;
        }
        let mut last = at.column;
        while last + 1 < cols
            && Self::cell_char(screen, at.line, last + 1).is_some_and(Self::is_word_char)
        {
            last += 1;
        }
        // 2칸 글자 경계에 걸치면 그 글자 전체를 포함한다(복사 `text()`도 같은 규칙).
        if let Some(row) = screen.rows.get(usize::from(at.line)) {
            if row
                .get(usize::from(last))
                .is_some_and(|c| c.flags.contains(Flags::WIDE))
                && last + 1 < cols
                && row
                    .get(usize::from(last) + 1)
                    .is_some_and(|c| c.flags.contains(Flags::SPACER))
            {
                last += 1;
            }
            if row
                .get(usize::from(first))
                .is_some_and(|c| c.flags.contains(Flags::SPACER))
                && first > 0
                && row
                    .get(usize::from(first) - 1)
                    .is_some_and(|c| c.flags.contains(Flags::WIDE))
            {
                first -= 1;
            }
        }
        Self {
            anchor: Point {
                line: at.line,
                column: first,
            },
            head: Point {
                line: at.line,
                column: last,
            },
            block: false,
        }
    }

    /// `at` 행 전체(OWE-866, 트리플클릭). 항상 `anchor <= head` 순서로 돌려준다.
    pub fn line_at(screen: &Screen, at: Point) -> Self {
        if screen.cols == 0 {
            return Self::new(at);
        }
        Self {
            anchor: Point {
                line: at.line,
                column: 0,
            },
            head: Point {
                line: at.line,
                column: screen.cols - 1,
            },
            block: false,
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
        if self.block {
            let first = self.anchor.column.min(self.head.column);
            let last = self.anchor.column.max(self.head.column).min(cols - 1);
            return (first <= last).then(|| (first, last - first + 1));
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
            let wraps = !self.block
                && to_end
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

    /// TUI 장식(`| `, `│ `, 박스 테두리)과 강제 줄바꿈·들여쓰기를 정리한 텍스트.
    pub fn clean_text(self, screen: &Screen) -> String {
        clean_text(&self.text(screen))
    }
}

fn str_display_width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

fn is_box_border_line(line: &str) -> bool {
    let trimmed = line.trim();
    let count = trimmed.chars().count();
    count >= 3
        && trimmed.chars().any(|c| {
            matches!(
                c,
                '─' | '━'
                    | '═'
                    | '╭'
                    | '╮'
                    | '╯'
                    | '╰'
                    | '┌'
                    | '┐'
                    | '┘'
                    | '└'
                    | '├'
                    | '┤'
                    | '┬'
                    | '┴'
                    | '┼'
            )
        })
        && trimmed.chars().all(|c| {
            matches!(
                c,
                '─' | '━'
                    | '═'
                    | '╭'
                    | '╮'
                    | '╯'
                    | '╰'
                    | '┌'
                    | '┐'
                    | '┘'
                    | '└'
                    | '├'
                    | '┤'
                    | '┬'
                    | '┴'
                    | '┼'
                    | '-'
                    | '+'
                    | ' '
            )
        })
}

fn parse_gutter_prefix(line: &str) -> Option<(char, &str)> {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if !matches!(first, '|' | '│' | '┃' | '丨' | '❯' | '>') {
        return None;
    }
    let rest = chars.as_str();
    if rest.is_empty() {
        Some((first, ""))
    } else if let Some(after_space) = rest.strip_prefix(' ') {
        Some((first, after_space))
    } else if matches!(first, '│' | '┃' | '丨') {
        Some((first, rest))
    } else {
        None
    }
}

fn strip_right_gutter(line: &str, gutter: char) -> &str {
    let trimmed = line.trim_end_matches(' ');
    if !matches!(gutter, '|' | '│' | '┃' | '丨') {
        return trimmed;
    }
    if let Some(before) = trimmed.strip_suffix(gutter)
        && (before.is_empty() || before.ends_with(' ') || matches!(gutter, '│' | '┃' | '丨'))
    {
        return before.trim_end_matches(' ');
    }
    trimmed
}

fn strip_gutters(mut lines: Vec<&str>) -> (Vec<&str>, bool) {
    let mut stripped_box = false;
    while lines.first().is_some_and(|l| is_box_border_line(l)) {
        lines.remove(0);
        stripped_box = true;
    }
    while lines.last().is_some_and(|l| is_box_border_line(l)) {
        lines.pop();
        stripped_box = true;
    }
    if lines.is_empty() {
        return (lines, stripped_box);
    }
    if lines.len() == 1 {
        if let Some((g, rest)) = parse_gutter_prefix(lines[0])
            && matches!(g, '|' | '│' | '┃' | '丨')
        {
            return (vec![strip_right_gutter(rest, g)], true);
        }
        return (lines, stripped_box);
    }
    let non_empty: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();
    if let Some((first_g, _)) = non_empty.first().and_then(|l| parse_gutter_prefix(l))
        && non_empty
            .iter()
            .all(|l| parse_gutter_prefix(l).is_some_and(|(g, _)| g == first_g))
    {
        let out = lines
            .into_iter()
            .map(|l| parse_gutter_prefix(l).map_or(l, |(g, rest)| strip_right_gutter(rest, g)))
            .collect();
        return (out, true);
    }
    // 첫 줄에서 기호 오른쪽부터 드래그를 시작해 2번째 줄부터만 기호가 붙은 경우.
    let tail_non_empty: Vec<&str> = lines[1..]
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();
    if let Some((tail_g, _)) = tail_non_empty.first().and_then(|l| parse_gutter_prefix(l))
        && tail_non_empty
            .iter()
            .all(|l| parse_gutter_prefix(l).is_some_and(|(g, _)| g == tail_g))
    {
        let mut out = Vec::with_capacity(lines.len());
        out.push(strip_right_gutter(lines[0], tail_g));
        for l in &lines[1..] {
            out.push(parse_gutter_prefix(l).map_or(*l, |(g, rest)| strip_right_gutter(rest, g)));
        }
        return (out, true);
    }
    (lines, stripped_box)
}

#[derive(Default, Clone, Copy)]
struct ScanState {
    in_single: bool,
    in_double: bool,
    escaped: bool,
    paren: usize,
    bracket: usize,
}

impl ScanState {
    fn feed(&mut self, line: &str) {
        for ch in line.chars() {
            if self.escaped {
                self.escaped = false;
                continue;
            }
            if self.in_single {
                if ch == '\'' {
                    self.in_single = false;
                }
                continue;
            }
            if ch == '\\' {
                self.escaped = true;
                continue;
            }
            if self.in_double {
                if ch == '"' {
                    self.in_double = false;
                }
                continue;
            }
            match ch {
                '\'' => self.in_single = true,
                '"' => self.in_double = true,
                '(' => self.paren = self.paren.saturating_add(1),
                ')' => self.paren = self.paren.saturating_sub(1),
                '[' => self.bracket = self.bracket.saturating_add(1),
                ']' => self.bracket = self.bracket.saturating_sub(1),
                _ => {}
            }
        }
        self.escaped = false;
    }

    fn is_open(self) -> bool {
        self.in_single || self.in_double || self.paren > 0 || self.bracket > 0
    }
}

fn leading_spaces(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b' ').count()
}

fn should_join_without_space(cur: &str, next: &str, max_w: usize) -> bool {
    if cur.is_empty() || next.is_empty() {
        return true;
    }
    if max_w >= 45 && str_display_width(cur) + 1 >= max_w {
        return true;
    }
    if cur.ends_with(['=', '/', '(', '[', '{', '@'])
        || next.starts_with([',', ';', ')', ']', '}', '.'])
    {
        return true;
    }
    // 따옴표·인자 안 경로가 줄바꿈으로 잘린 경우(예: `"/Users/macm` + `ini/Library`).
    let last_token = cur.split_whitespace().last().unwrap_or("");
    let first_token = next.split_whitespace().next().unwrap_or("");
    if last_token.contains('/')
        && first_token.contains('/')
        && first_token
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '/')
    {
        return true;
    }
    false
}

/// TUI 출력에서 복사한 문자열의 박스/인용 기호(`| `, `│ `)와 강제 줄바꿈·들여쓰기를 정리한다.
#[must_use]
pub fn clean_text(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let raw_lines: Vec<&str> = raw.split('\n').collect();
    let (mut lines, had_gutter) = strip_gutters(raw_lines);
    if lines.is_empty() {
        return String::new();
    }
    if lines.len() == 1 {
        return lines[0].to_owned();
    }

    // 공통 앞 공백 또는 행잉 인덴트(첫 줄 0칸, 2번째 줄부터 균일한 2~12칸 공백)를 계산한다.
    let non_empty_indents: Vec<usize> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| leading_spaces(l))
        .collect();
    let min_all = non_empty_indents.iter().copied().min().unwrap_or(0);
    if min_all > 0 {
        for l in &mut lines {
            if leading_spaces(l) >= min_all {
                *l = l.get(min_all..).unwrap_or(l);
            }
        }
    }

    let has_empty = lines.iter().any(|l| l.trim().is_empty());
    let tail_indents: Vec<usize> = lines[1..]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| leading_spaces(l))
        .collect();
    let uniform_hanging =
        !has_empty && leading_spaces(lines[0]) == 0 && !tail_indents.is_empty() && {
            let first = tail_indents[0];
            (2..=12).contains(&first) && tail_indents.iter().all(|&n| n == first)
        };
    let uniform_flat =
        !has_empty && (min_all > 0 || had_gutter) && tail_indents.iter().all(|&n| n == 0);

    if uniform_hanging || uniform_flat {
        let stripped: Vec<&str> = if uniform_hanging {
            let hang = tail_indents[0];
            std::iter::once(lines[0])
                .chain(lines[1..].iter().map(|l| l.get(hang..).unwrap_or(l)))
                .collect()
        } else {
            lines.clone()
        };
        let max_w = stripped
            .iter()
            .map(|l| str_display_width(l))
            .max()
            .unwrap_or(0);
        let mut scan = ScanState::default();
        let mut open_breaks = 0usize;
        for l in &stripped[..stripped.len() - 1] {
            scan.feed(l);
            if scan.is_open() {
                open_breaks += 1;
            }
        }
        let first_ends_block = stripped[0].trim_end().ends_with([':', '{', '}', ';']);
        let looks_wrapped = open_breaks > 0 || (!first_ends_block && max_w >= 50);
        if looks_wrapped {
            let mut out = String::new();
            for (idx, &line) in stripped.iter().enumerate() {
                if idx == 0 {
                    out.push_str(line);
                    continue;
                }
                let prev = stripped[idx - 1];
                if let Some(before_bs) = out.strip_suffix('\\') {
                    let trimmed_len = before_bs.trim_end_matches(' ').len();
                    out.truncate(trimmed_len);
                    out.push(' ');
                    out.push_str(line);
                } else if should_join_without_space(prev, line, max_w) {
                    out.push_str(line);
                } else {
                    out.push(' ');
                    out.push_str(line);
                }
            }
            return out;
        }
    }

    lines.join("\n")
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
            block: false,
        }
    }

    fn block_sel(a: (u16, u16), b: (u16, u16)) -> Selection {
        Selection {
            block: true,
            ..sel(a, b)
        }
    }

    #[test]
    fn rectangular_selection_keeps_columns_and_row_breaks_in_both_directions() {
        let mut screen = screen(&["abcd", "efgh"], 4);
        screen.rows[0][3].flags = Flags::WRAPLINE;
        let mut selection = Selection::new_block(Point { line: 1, column: 2 });
        selection.head = Point { line: 0, column: 1 };
        assert_eq!(selection.span(0, 4), Some((1, 2)));
        assert_eq!(selection.span(1, 4), Some((1, 2)));
        assert_eq!(selection.text(&screen), "bc\nfg");
        selection.anchor = Point { line: 0, column: 0 };
        selection.head = Point { line: 1, column: 3 };
        assert_eq!(selection.text(&screen), "abcd\nefgh");
        assert_eq!(sel((0, 0), (1, 3)).text(&screen), "abcdefgh");
    }

    #[test]
    fn clean_copy_preserves_unicode_blank_lines_and_plain_multiline_text() {
        assert_eq!(clean_text("  한글\n\u{3000}\n  끝"), "한글\n\u{3000}\n끝");
        assert_eq!(clean_text("hello\nworld"), "hello\nworld");
        assert_eq!(
            clean_text("╭───╮\n│ alpha │\n│ beta │\n╰───╯"),
            "alpha\nbeta"
        );
        assert_eq!(
            clean_text("tool --path \"/Users/macm\n  ini/Library/file\""),
            "tool --path \"/Users/macmini/Library/file\""
        );
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

    /// OWE-866: 더블클릭 단어 경계(공백·구두점 구분자).
    #[test]
    fn word_at_expands_to_word_boundaries() {
        let s = screen(&["hello world,foo-bar"], 19);
        let at = |column: u16| Point { line: 0, column };
        assert_eq!(
            Selection::word_at(&s, at(1)),
            Selection {
                anchor: at(0),
                head: at(4),
                block: false,
            }
        );
        assert_eq!(Selection::word_at(&s, at(1)).text(&s), "hello");
        assert_eq!(Selection::word_at(&s, at(7)).text(&s), "world");
        // 구두점은 구분자: ',' 위는 한 칸만.
        assert_eq!(
            Selection::word_at(&s, at(11)),
            Selection {
                anchor: at(11),
                head: at(11),
                block: false,
            }
        );
        assert_eq!(Selection::word_at(&s, at(13)).text(&s), "foo-bar");
        assert_eq!(Selection::word_at(&s, at(16)).text(&s), "foo-bar");
        // 공백 위는 한 칸만.
        assert_eq!(
            Selection::word_at(&s, at(5)),
            Selection {
                anchor: at(5),
                head: at(5),
                block: false,
            }
        );
    }

    /// OWE-866: 트리플클릭 줄 전체.
    #[test]
    fn line_at_covers_full_row() {
        let s = screen(&["hello world", "ab"], 12);
        let at = Point { line: 1, column: 3 };
        let line = Selection::line_at(&s, at);
        assert_eq!(line.anchor, Point { line: 1, column: 0 });
        assert_eq!(
            line.head,
            Point {
                line: 1,
                column: 11
            }
        );
        assert_eq!(line.text(&s), "ab");
    }

    /// OWE-866: 2칸 글자 위 더블클릭은 그 글자를 포함한다.
    #[test]
    fn word_at_on_wide_char_keeps_glyph() {
        let mut s = screen(&["a    "], 5);
        s.rows[0] = vec![
            Cell {
                ch: 'a',
                ..Cell::default()
            },
            Cell {
                ch: ' ',
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
                ch: ' ',
                ..Cell::default()
            },
        ];
        // 왼쪽·오른쪽 절반 어디를 눌러도 '한'을 가리킨다.
        for column in [2, 3] {
            let word = Selection::word_at(&s, Point { line: 0, column });
            assert_eq!(word.text(&s), "한", "column {column}");
        }
    }

    #[test]
    fn block_selection_selects_rectangular_columns() {
        let s = screen(&["| hello  ", "| world  ", "| foo    "], 10);
        // 열 2..=6 사각형만 선택하면 앞쪽 `| `가 제외된다.
        let rect = block_sel((0, 2), (2, 6));
        assert_eq!(rect.span(0, 10), Some((2, 5)));
        assert_eq!(rect.span(1, 10), Some((2, 5)));
        assert_eq!(rect.span(2, 10), Some((2, 5)));
        assert_eq!(rect.text(&s), "hello\nworld\nfoo");
        // 반대 대각선(우하단 -> 좌상단)으로 끌어도 같은 열 구간이다.
        assert_eq!(block_sel((2, 6), (0, 2)).text(&s), "hello\nworld\nfoo");
    }

    #[test]
    fn clean_text_strips_tui_gutters_and_boxes() {
        assert_eq!(
            clean_text("| 안녕하세요\n| ㅋㅋ예시입니다."),
            "안녕하세요\nㅋㅋ예시입니다."
        );
        // 첫 줄은 `| ` 뒤부터 드래그해 2번째 줄부터만 `| `가 붙은 경우.
        assert_eq!(
            clean_text("안녕하세요\n| ㅋㅋ예시입니다."),
            "안녕하세요\nㅋㅋ예시입니다."
        );
        // 유니코드 박스 테두리 제거.
        let boxed =
            "╭────────────────╮\n│ 안녕하세요     │\n│ ㅋㅋ예시입니다 │\n╰────────────────╯";
        assert_eq!(clean_text(boxed), "안녕하세요\nㅋㅋ예시입니다");
    }

    #[test]
    fn clean_text_unwraps_tui_hard_wrapped_oneliner_and_preserves_code_blocks() {
        let wrapped = "\
IFS=,;a='shortcuts,run';python3,-c,'import
    Vision,Quartz,pathlib;u=Quartz.CFURLCreateWithFileSystemPath(None,\"/Users/macm
    ini/Library/Application
    Support/com.raycast.macos/clipboard/file-eaab871d237b31ea53c234c3af2d8b12.png\"
    ,0,False);h=Vision.VNImageRequestHandler.alloc().initWithURL_options_(u,{});r=
    Vision.VNRecognizeTextRequest.alloc().init();r.setRecognitionLanguages_([\"ko-K
    R\",\"en-US\"]);h.performRequests_error_([r],None);print(\"\\n\".join(o.topCandidate
    s_(1)[0].string()for o in r.results()))";
        let expected = "IFS=,;a='shortcuts,run';python3,-c,'import Vision,Quartz,pathlib;u=Quartz.CFURLCreateWithFileSystemPath(None,\"/Users/macmini/Library/Application Support/com.raycast.macos/clipboard/file-eaab871d237b31ea53c234c3af2d8b12.png\",0,False);h=Vision.VNImageRequestHandler.alloc().initWithURL_options_(u,{});r=Vision.VNRecognizeTextRequest.alloc().init();r.setRecognitionLanguages_([\"ko-KR\",\"en-US\"]);h.performRequests_error_([r],None);print(\"\\n\".join(o.topCandidates_(1)[0].string()for o in r.results()))";
        assert_eq!(clean_text(wrapped), expected);

        // 일반 들여쓰기 코드 블록은 줄바꿈과 들여쓰기를 유지한다.
        let py = "def hello():\n    print(\"world\")\n    return 42";
        assert_eq!(clean_text(py), py);
    }
}
