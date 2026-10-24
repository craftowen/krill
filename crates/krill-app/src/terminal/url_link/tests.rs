use super::*;

fn screen(cols: u16, rows: &[&str], wrap: bool) -> Screen {
    let mut screen = Screen::blank(cols, u16::try_from(rows.len()).unwrap());
    for (i, text) in rows.iter().enumerate() {
        screen.rows[i] = text
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        if wrap && i + 1 < rows.len() {
            assert_eq!(screen.rows[i].len(), usize::from(cols));
            screen.rows[i].last_mut().unwrap().flags |= Flags::WRAPLINE;
        }
    }
    screen
}

#[test]
fn soft_wrapped_url_resolves_on_every_row_including_split_scheme() {
    let s = screen(
        16,
        &["go https://examp", "le.com/?a=1#pair", "=abc&key=def end"],
        true,
    );
    let expected = "https://example.com/?a=1#pair=abc&key=def";
    for (line, column, start, cols) in [(0, 6, 3, 13), (1, 4, 0, 16), (2, 5, 0, 12)] {
        let (h, uri) = at(&s, line, column).unwrap();
        assert_eq!(uri, expected);
        assert_eq!(
            h,
            Highlight {
                line,
                column: start,
                cols
            }
        );
    }
    assert!(at(&s, 0, 1).is_none());
    assert!(at(&s, 2, 13).is_none());
    let s = screen(5, &["https", "://ex", "ample", ".com/", "abc"], true);
    assert_eq!(at(&s, 1, 2).unwrap().1, "https://example.com/abc");
}

#[test]
fn claude_indented_hard_wraps_keep_pairing_fragment_and_key() {
    let s = screen(
        35,
        &[
            "  http://127.0.0.1:48900/#pair=abc",
            "  def0123456789&key=testtesttestte",
            "  st00",
            "",
            "  Open this link in your browser.",
        ],
        false,
    );
    let expected = "http://127.0.0.1:48900/#pair=abcdef0123456789&key=testtesttesttest00";
    for line in 0..3 {
        assert_eq!(at(&s, line, 3).unwrap().1, expected);
        assert!(at(&s, line, 1).is_none(), "들여쓰기는 링크가 아니다");
    }
    assert!(at(&s, 4, 3).is_none());
}

#[test]
fn hard_breaks_preserve_distinct_urls_prose_and_styles() {
    for tail in [
        "  https://other.example/",
        "  next paragraph here",
        "   different-indent",
        "",
    ] {
        let s = screen(26, &["  https://example.com/abc", tail], false);
        assert_eq!(at(&s, 0, 5).unwrap().1, "https://example.com/abc");
    }
    let s = screen(80, &["  https://example.com/abc", "  unrelated"], false);
    assert_eq!(at(&s, 0, 5).unwrap().1, "https://example.com/abc");
    let mut s = screen(26, &["  https://example.com/abc", "  unrelated"], false);
    s.rows[1][2].fg = krill_proto::screen::Color::Indexed(2);
    assert_eq!(at(&s, 0, 5).unwrap().1, "https://example.com/abc");
}

#[test]
fn soft_wrapped_url_trims_punctuation_and_rejects_bidi_across_rows() {
    let s = screen(16, &["(https://example", ".com/abc)."], true);
    assert_eq!(at(&s, 1, 2).unwrap().1, "https://example.com/abc");
    assert!(at(&s, 1, 8).is_none());
    let s = screen(16, &["(https://example", ".com/\u{202e}evil"], true);
    assert!(at(&s, 0, 3).is_none());
}

#[test]
fn excessive_soft_wraps_are_rejected_instead_of_opening_a_prefix() {
    let mut s = Screen::blank(256, 128);
    for row in &mut s.rows {
        *row = vec![
            Cell {
                ch: 'a',
                ..Cell::default()
            };
            256
        ];
        row.last_mut().unwrap().flags |= Flags::WRAPLINE;
    }
    for (cell, ch) in s.rows[0].iter_mut().zip("https://example.com/".chars()) {
        cell.ch = ch;
    }
    assert!(at(&s, 0, 10).is_none());
}

proptest::proptest! {
    #[test]
    fn url_survives_arbitrary_soft_wrap_positions(
        token in "[A-Za-z0-9_-]{1,400}",
        cols in 8u16..80,
    ) {
        let expected = format!("https://example.com/#pair=abc&key={token}");
        let chars: Vec<char> = expected.chars().collect();
        let rows: Vec<String> = chars.chunks(usize::from(cols)).map(|c| c.iter().collect()).collect();
        let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
        let s = screen(cols, &refs, true);
        for line in 0..s.lines {
            let (h, uri) = at(&s, line, 0).unwrap();
            proptest::prop_assert_eq!(uri, expected.clone());
            proptest::prop_assert_eq!(h.line, line);
            proptest::prop_assert_eq!(h.column, 0);
            proptest::prop_assert!(h.cols > 0 && h.cols <= cols);
        }
    }
}
