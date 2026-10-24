use super::*;
use crate::terminal::path_link::tests::{TempDir, row};
use krill_proto::screen::Link;

fn screen(cols: u16, text: &[&str], soft: bool) -> Screen {
    let mut s = Screen::blank(cols, u16::try_from(text.len()).unwrap());
    for (i, text) in text.iter().enumerate() {
        s.rows[i] = row(text);
        assert!(s.rows[i].len() <= usize::from(cols));
        if soft && i + 1 < s.rows.len() {
            assert_eq!(s.rows[i].len(), usize::from(cols));
            s.rows[i].last_mut().unwrap().flags |= Flags::WRAPLINE;
        }
    }
    s
}

fn resolve(s: &Screen, line: u16, column: u16, dir: &TempDir) -> Option<PathLink> {
    Input::at(s, line, column)?.resolve(&mut Cache::default(), Some(&dir.0))
}

#[test]
fn soft_wraps_preserve_full_path_locations_and_prefer_full_file() {
    let dir = TempDir::new("wrapped");
    std::fs::write(dir.0.join("src/long-name.rs"), "a\nb\n").unwrap();
    std::fs::write(dir.0.join("src/long"), "wrong").unwrap();
    let s = screen(8, &["src/long", "-name.rs", ":12:3:xx"], true);
    for (line, col) in [(0, 2), (1, 4), (2, 2)] {
        let link = resolve(&s, line, col, &dir).unwrap();
        assert_eq!(link.path, dir.0.join("src/long-name.rs"));
        assert_eq!((link.line, link.col), (Some(12), Some(3)));
        assert!((link.column..link.column + link.cols).contains(&col));
    }
    assert!(
        resolve(&s, 2, 6, &dir).is_none(),
        "grep 본문은 링크가 아니다"
    );
}

#[test]
fn wide_placeholders_spaces_quotes_and_combining_characters_survive() {
    let dir = TempDir::new("wide-wrap");
    let file = dir.0.join("src/ab한글 완료 (최종).txt");
    std::fs::write(&file, "x").unwrap();
    let mut rows = Vec::new();
    let mut current = Vec::new();
    for cell in row("`src/ab한글 완료 (최종).txt`") {
        if cell.flags.contains(Flags::WIDE) && current.len() == 7 {
            current.push(Cell {
                flags: Flags::SPACER,
                ..Cell::default()
            });
            rows.push(std::mem::take(&mut current));
        }
        current.push(cell);
        if current.len() == 8 {
            rows.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }
    let mut s = Screen::blank(8, u16::try_from(rows.len()).unwrap());
    s.rows = rows;
    for i in 0..s.rows.len() - 1 {
        s.rows[i].last_mut().unwrap().flags |= Flags::WRAPLINE;
    }
    assert!(resolve(&s, 0, 7, &dir).is_none());
    for (line, col) in [(0, 2), (1, 0), (1, 1), (2, 0), (3, 1)] {
        assert_eq!(resolve(&s, line, col, &dir).unwrap().path, file);
    }
    let file = dir.0.join("src/e\u{301}.txt");
    std::fs::write(&file, "x").unwrap();
    let mut s = screen(20, &["src/e.txt"], false);
    s.rows[0][4].zerowidth.push('\u{301}');
    assert_eq!(resolve(&s, 0, 4, &dir).unwrap().path, file);
}

#[test]
fn hard_wraps_support_indented_unicode_spaces_and_ambiguous_boundaries() {
    let dir = TempDir::new("hard-wrap");
    let file = dir.0.join("src/한글 파일.txt");
    std::fs::write(&file, "x").unwrap();
    let s = screen(14, &["  src/한글 파", "  일.txt:2:3"], false);
    for (line, col) in [(0, 3), (1, 3)] {
        let link = resolve(&s, line, col, &dir).unwrap();
        assert_eq!(link.path, file);
        assert_eq!((link.line, link.col), (Some(2), Some(3)));
    }
    std::fs::write(dir.0.join("src/long name.rs"), "x").unwrap();
    let s = screen(12, &["  src/long", "  name.rs"], false);
    assert_eq!(
        resolve(&s, 1, 3, &dir).unwrap().path,
        dir.0.join("src/long name.rs")
    );
    std::fs::write(dir.0.join("src/longname.rs"), "other").unwrap();
    assert!(
        resolve(&s, 1, 3, &dir).is_none(),
        "공백 유무가 모호하면 자동 확정하지 않는다"
    );
}

#[test]
fn equally_long_overlapping_hard_paths_are_ambiguous() {
    let dir = TempDir::new("ambiguous-hard-groups");
    std::fs::write(dir.0.join("src/longabmiddlefile"), "a").unwrap();
    std::fs::write(dir.0.join("middlefileend.rs"), "b").unwrap();
    let s = screen(13, &["  src/longab", "  middlefile", "  end.rs"], false);
    assert!(resolve(&s, 1, 3, &dir).is_none());
}

#[test]
fn source_changes_and_osc8_boundaries_invalidate_the_input() {
    let dir = TempDir::new("source");
    let mut s = screen(6, &["src/ma", "in.rs"], true);
    let input = Input::at(&s, 1, 1).unwrap();
    s.rows[0][0].ch = 'x';
    assert!(!input.matches(&s));
    s.rows[0][0].ch = 's';
    s.links[0].push(Link {
        column: 0,
        cols: 6,
        uri: "https://example.com".into(),
    });
    assert!(!input.matches(&s));
    assert!(resolve(&s, 1, 1, &dir).is_none());
    s.links[0].clear();
    s.cols = 7;
    assert!(!input.matches(&s));
}

#[test]
fn excessive_and_incomplete_wraps_do_not_resolve_existing_prefixes() {
    let dir = TempDir::new("limits");
    let mut s = screen(11, &["src/main.rs"], false);
    s.rows[0].last_mut().unwrap().flags |= Flags::WRAPLINE;
    assert!(Input::at(&s, 0, 2).is_none());
    let rows = vec!["src/main.rs"; MAX_ROWS + 1];
    let s = screen(11, &rows, true);
    assert!(resolve(&s, 0, 2, &dir).is_none());
    let mut cells = vec![
        Cell {
            ch: 'a',
            zerowidth: vec!['\u{301}'; 8],
            ..Cell::default()
        };
        2048
    ];
    for (cell, ch) in cells.iter_mut().zip("src/main.rs".chars()) {
        cell.ch = ch;
    }
    assert!(crate::terminal::path_link::row_chars(&cells).is_empty());
}

proptest::proptest! {
    #[test]
    fn arbitrary_multirow_input_is_bounded_and_projects_valid_ranges(
        chars in proptest::collection::vec(proptest::char::any(), 0..400),
        cols in 1u16..40,
        line in 0u16..40,
        column in 0u16..40,
    ) {
        let mut s = Screen::blank(cols, 40);
        for (i, chunk) in chars.chunks(usize::from(cols)).take(40).enumerate() {
            s.rows[i] = chunk.iter().map(|&ch| Cell { ch, ..Cell::default() }).collect();
            if s.rows[i].len() == usize::from(cols) {
                s.rows[i].last_mut().unwrap().flags |= Flags::WRAPLINE;
            }
        }
        if let Some(input) = Input::at(&s, line, column) {
            proptest::prop_assert!(input.rows.len() <= MAX_ROWS);
            if let Some(link) = input.resolve(&mut Cache::default(), None) {
                proptest::prop_assert!(link.cols > 0);
                proptest::prop_assert!((link.column..link.column + link.cols).contains(&column));
            }
        }
    }
}

#[test]
fn hard_whole_file_wins_over_a_fragment_in_the_spaced_variant() {
    let dir = TempDir::new("hard-fragment");
    let file = dir.0.join("src/longname.rs");
    std::fs::write(&file, "whole").unwrap();
    std::fs::write(dir.0.join("name.rs"), "fragment").unwrap();
    let s = screen(12, &["  src/long", "  name.rs"], false);
    for line in 0..2 {
        assert_eq!(resolve(&s, line, 3, &dir).unwrap().path, file);
    }
}

#[test]
fn osc8_and_hidden_cells_are_hard_boundaries_even_inside_quotes() {
    let dir = TempDir::new("osc8-barrier");
    std::fs::write(dir.0.join("src/ main.rs"), "must not open").unwrap();
    for (cols, rows, column) in [
        (7, ["src/Xma", "in.rs"], 4),
        (8, ["\"src/Xma", "in.rs\""], 5),
    ] {
        let mut s = screen(cols, &rows, true);
        s.links[0].push(Link {
            column,
            cols: 1,
            uri: "https://example.com".into(),
        });
        for (line, col) in [(0, 2), (1, 1)] {
            assert!(resolve(&s, line, col, &dir).is_none());
        }
        s.links[0].clear();
        s.rows[0][usize::from(column)].flags |= Flags::HIDDEN;
        for (line, col) in [(0, 2), (1, 1)] {
            assert!(resolve(&s, line, col, &dir).is_none());
        }
    }
}
