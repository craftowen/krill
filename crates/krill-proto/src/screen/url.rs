//! Shared plain-text URL boundary detection and terminal screen link normalization.

use std::cmp::Ordering;

use super::{Cell, Color, Flags, Link, is_bidi_control};

/// Fullwidth CJK punctuation and brackets that terminate paths and URLs (#30213, #30093).
pub fn is_fullwidth_punct(c: char) -> bool {
    matches!(
        c,
        '（' | '）'
            | '「'
            | '」'
            | '『'
            | '』'
            | '【'
            | '】'
            | '〈'
            | '〉'
            | '《'
            | '》'
            | '〔'
            | '〕'
            | '［'
            | '］'
            | '｛'
            | '｝'
            | '、'
            | '。'
            | '・'
            | '：'
            | '；'
            | '！'
            | '？'
            | '“'
            | '”'
            | '‘'
            | '’'
    )
}

/// East Asian characters (Hangul, Kana, Han) where particles attach directly without spaces (#30213, #30093, #30037).
pub fn is_cjk_or_hangul(c: char) -> bool {
    matches!(
        u32::from(c),
        0x1100..=0x11FF
            | 0x3040..=0x30FF
            | 0x3130..=0x318F
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
    )
}

/// Characters that unconditionally terminate a plain-text HTTP(S) URL.
pub fn url_breaks(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || is_bidi_control(c)
        || is_fullwidth_punct(c)
        || matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '{' | '}' | '|' | '\\' | '^'
        )
}

fn trim_url_end<T>(slice: &[T], mut ch: impl FnMut(&T) -> char) -> usize {
    let mut end = slice.len();
    while let Some(prev) = end.checked_sub(1) {
        let Some(item) = slice.get(prev) else {
            break;
        };
        let c = ch(item);
        let strip = match c {
            '.' | ',' | ':' | ';' | '!' | '?' | '~' => true,
            ')' => {
                let head = slice.get(..end).unwrap_or_default();
                let open = head.iter().filter(|t| ch(t) == '(').count();
                let close = head.iter().filter(|t| ch(t) == ')').count();
                close > open
            }
            ']' => {
                let head = slice.get(..end).unwrap_or_default();
                let open = head.iter().filter(|t| ch(t) == '[').count();
                let close = head.iter().filter(|t| ch(t) == ']').count();
                close > open
            }
            '(' => {
                let head = slice.get(..end).unwrap_or_default();
                let open = head.iter().filter(|t| ch(t) == '(').count();
                let close = head.iter().filter(|t| ch(t) == ')').count();
                open > close
            }
            '[' => {
                let head = slice.get(..end).unwrap_or_default();
                let open = head.iter().filter(|t| ch(t) == '[').count();
                let close = head.iter().filter(|t| ch(t) == ']').count();
                open > close
            }
            _ => false,
        };
        if strip {
            end = prev;
        } else {
            break;
        }
    }
    end
}

/// Supported external URL schemes in terminal plain-text and OSC 8 links.
pub const URL_SCHEMES: [&str; 4] = ["https://", "http://", "cursor://", "vscode://"];

/// Returns the character length of a supported URL scheme prefix at the start of `slice`,
/// or `0` if `slice` does not start with one.
pub fn url_scheme_len<T>(slice: &[T], mut ch: impl FnMut(&T) -> char) -> usize {
    for scheme in URL_SCHEMES {
        let len = scheme.len();
        if slice
            .get(..len)
            .is_some_and(|s| s.iter().map(&mut ch).eq(scheme.chars()))
        {
            return len;
        }
    }
    0
}

/// Strips a supported URL scheme prefix from `s`.
#[must_use]
pub fn strip_url_scheme(s: &str) -> Option<&str> {
    URL_SCHEMES.iter().find_map(|scheme| s.strip_prefix(scheme))
}

/// Returns the byte length of a supported URL scheme prefix (ASCII case-insensitive),
/// or `0` if `s` does not begin with one.
#[must_use]
pub fn url_scheme_prefix_len_ascii_ci(s: &str) -> usize {
    URL_SCHEMES
        .iter()
        .find_map(|scheme| {
            s.get(..scheme.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
                .then_some(scheme.len())
        })
        .unwrap_or(0)
}

/// Scans a plain-text `http://`, `https://`, `cursor://`, or `vscode://` URL at the start of
/// `slice` and returns its element count after stripping trailing sentence punctuation and
/// unmatched delimiters. Returns `0` if `slice` does not begin with a supported URL prefix.
pub fn scan_url_len<T>(slice: &[T], mut ch: impl FnMut(&T) -> char) -> usize {
    let scheme_len = url_scheme_len(slice, &mut ch);
    if scheme_len == 0 {
        return 0;
    }

    let mut in_path = false;
    let mut seg_ascii = false;
    let mut host_label_ascii = false;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut prev_at = false;
    let mut raw_end = slice.len();

    for (idx, item) in slice.iter().enumerate().skip(scheme_len) {
        let c = ch(item);
        if url_breaks(c) {
            raw_end = idx;
            break;
        }
        if matches!(c, '/' | '?' | '#') {
            in_path = true;
            seg_ascii = false;
            continue;
        }
        if !in_path {
            if matches!(c, '(' | ')' | ',' | ';' | '!') {
                raw_end = idx;
                break;
            }
            if !c.is_ascii() {
                // Allow Unicode IDN host labels while still breaking when an East Asian particle
                // immediately follows an ASCII host label or port (e.g. `https://example.com에서`).
                if !c.is_alphanumeric() || (host_label_ascii && is_cjk_or_hangul(c)) {
                    raw_end = idx;
                    break;
                }
                prev_at = false;
                continue;
            }
            if c == '[' {
                if idx == scheme_len || prev_at {
                    bracket_depth = bracket_depth.saturating_add(1);
                } else {
                    raw_end = idx;
                    break;
                }
            } else if c == ']' {
                if let Some(next_depth) = bracket_depth.checked_sub(1) {
                    bracket_depth = next_depth;
                } else {
                    raw_end = idx;
                    break;
                }
            }
            if matches!(c, '.' | '@') {
                host_label_ascii = false;
            } else if c.is_ascii_alphanumeric() {
                host_label_ascii = true;
            }
            prev_at = c == '@';
            continue;
        }

        if matches!(c, '=' | '&') {
            seg_ascii = false;
            continue;
        }
        if c == '(' {
            paren_depth = paren_depth.saturating_add(1);
            continue;
        }
        if c == ')' {
            if let Some(next_depth) = paren_depth.checked_sub(1) {
                paren_depth = next_depth;
                continue;
            }
            raw_end = idx;
            break;
        }
        if c == '[' {
            bracket_depth = bracket_depth.saturating_add(1);
            continue;
        }
        if c == ']' {
            if let Some(next_depth) = bracket_depth.checked_sub(1) {
                bracket_depth = next_depth;
                continue;
            }
            raw_end = idx;
            break;
        }
        if seg_ascii && is_cjk_or_hangul(c) {
            raw_end = idx;
            break;
        }
        if c.is_ascii_alphanumeric() {
            seg_ascii = true;
        }
    }

    let head = slice.get(..raw_end).unwrap_or(slice);
    let trimmed = trim_url_end(head, ch);
    if trimmed <= scheme_len { 0 } else { trimmed }
}

fn has_valid_authority(url: &str) -> bool {
    let Some(rest) = strip_url_scheme(url) else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return false;
    }
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    !host.is_empty() && !host.chars().all(|c| c == '.')
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b.saturating_sub(b'0')),
        b'a'..=b'f' => Some(b.saturating_sub(b'a').saturating_add(10)),
        b'A'..=b'F' => Some(b.saturating_sub(b'A').saturating_add(10)),
        _ => None,
    }
}

fn percent_decode_utf8(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        if b == b'%' {
            let hi = hex_nibble(*bytes.get(i.checked_add(1)?)?)?;
            let lo = hex_nibble(*bytes.get(i.checked_add(2)?)?)?;
            out.push((hi << 4) | lo);
            i = i.saturating_add(3);
        } else {
            out.push(b);
            i = i.saturating_add(1);
        }
    }
    String::from_utf8(out).ok()
}

fn strip_decoded_suffix(uri: &str, target: &[u8], tail_text: &str) -> Option<String> {
    let bytes = uri.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut split = None;
    let mut i = 0usize;
    while i <= bytes.len() {
        if split.is_none() && decoded.as_slice() == target {
            if !uri.is_char_boundary(i) {
                return None;
            }
            split = Some(i);
            decoded.clear();
        }
        let Some(&b) = bytes.get(i) else {
            break;
        };
        if b == b'%' {
            let hi = hex_nibble(*bytes.get(i.checked_add(1)?)?)?;
            let lo = hex_nibble(*bytes.get(i.checked_add(2)?)?)?;
            decoded.push((hi << 4) | lo);
            i = i.saturating_add(3);
        } else {
            decoded.push(b);
            i = i.saturating_add(1);
        }
    }
    let split = split?;
    let tail = std::str::from_utf8(&decoded).ok()?;
    if tail == tail_text {
        uri.get(..split).map(str::to_owned)
    } else {
        None
    }
}

fn tail_matches_uri(uri: &str, clean_text: &str, tail_text: &str) -> Option<String> {
    if let Some(rem) = uri.strip_prefix(clean_text)
        && (rem == tail_text || percent_decode_utf8(rem).as_deref() == Some(tail_text))
    {
        return Some(clean_text.to_owned());
    }
    if let Some(clean) = strip_decoded_suffix(uri, clean_text.as_bytes(), tail_text) {
        return Some(clean);
    }
    if clean_text.contains('%')
        && let Some(decoded_clean) = percent_decode_utf8(clean_text)
    {
        return strip_decoded_suffix(uri, decoded_clean.as_bytes(), tail_text);
    }
    None
}

#[derive(Debug, Clone, Copy)]
struct SpanChar {
    line: usize,
    col: u16,
    width: u16,
    ch: char,
}

fn push_row_span_chars(
    row: &[Cell],
    line: usize,
    start_col: u16,
    end_col: u16,
    out: &mut Vec<SpanChar>,
) -> bool {
    let start = usize::from(start_col);
    let end = usize::from(end_col).min(row.len());
    let Some(slice) = row.get(start..end) else {
        return false;
    };
    for (offset, cell) in slice.iter().enumerate() {
        let col = start_col.saturating_add(u16::try_from(offset).unwrap_or(u16::MAX));
        if cell.flags.contains(Flags::HIDDEN) {
            return false;
        }
        if cell.flags.contains(Flags::SPACER) {
            if let Some(prev) = out.iter_mut().rev().find(|s| s.width > 0)
                && prev.line == line
                && prev.col.saturating_add(prev.width) == col
                && row
                    .get(usize::from(prev.col))
                    .is_some_and(|c| c.flags.contains(Flags::WIDE))
            {
                prev.width = prev.width.saturating_add(1);
            }
            continue;
        }
        out.push(SpanChar {
            line,
            col,
            width: 1,
            ch: cell.ch,
        });
        for &zw in &cell.zerowidth {
            out.push(SpanChar {
                line,
                col,
                width: 0,
                ch: zw,
            });
        }
    }
    true
}

fn row_wraps(rows: &[Vec<Cell>], line: usize) -> bool {
    rows.get(line)
        .and_then(|row| row.last())
        .is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE))
}

fn prev_neighbor(rows: &[Vec<Cell>], line: usize, col: u16) -> Option<&Cell> {
    if let Some(prev_col) = col.checked_sub(1) {
        let idx = usize::from(prev_col);
        let row = rows.get(line)?;
        let cell = row.get(idx)?;
        if cell.flags.contains(Flags::SPACER)
            && let Some(wide_idx) = idx.checked_sub(1)
            && row
                .get(wide_idx)
                .is_some_and(|c| c.flags.contains(Flags::WIDE))
        {
            return row.get(wide_idx);
        }
        return Some(cell);
    }
    let prev_line = line.checked_sub(1)?;
    if !row_wraps(rows, prev_line) {
        return None;
    }
    let prev_row = rows.get(prev_line)?;
    let last_idx = prev_row.len().checked_sub(1)?;
    let cell = prev_row.get(last_idx)?;
    if cell.flags.contains(Flags::SPACER)
        && let Some(wide_idx) = last_idx.checked_sub(1)
        && prev_row
            .get(wide_idx)
            .is_some_and(|c| c.flags.contains(Flags::WIDE))
    {
        return prev_row.get(wide_idx);
    }
    Some(cell)
}

fn next_neighbor(rows: &[Vec<Cell>], line: usize, end_col: u16) -> Option<&Cell> {
    let row = rows.get(line)?;
    let idx = usize::from(end_col);
    if let Some(cell) = row.get(idx) {
        return Some(cell);
    }
    if idx >= row.len() && row_wraps(rows, line) {
        return rows.get(line.checked_add(1)?)?.first();
    }
    None
}

fn is_default_space(cell: &Cell) -> bool {
    cell.ch == ' '
        && cell.zerowidth.is_empty()
        && cell.fg == Color::Default
        && !cell.flags.contains(Flags::LINK)
        && !cell.flags.contains(Flags::UNDERLINE)
}

fn uniform_fg(rows: &[Vec<Cell>], segments: &[(usize, u16, u16)]) -> Option<Color> {
    let mut found: Option<Color> = None;
    for &(line, start_col, end_col) in segments {
        let row = rows.get(line)?;
        let start = usize::from(start_col);
        let end = usize::from(end_col).min(row.len());
        for cell in row.get(start..end)? {
            if cell.flags.contains(Flags::SPACER) {
                continue;
            }
            match found {
                None => found = Some(cell.fg),
                Some(prev) if prev != cell.fg => return None,
                Some(_) => {}
            }
        }
    }
    found
}

fn all_underlined(rows: &[Vec<Cell>], segments: &[(usize, u16, u16)]) -> bool {
    let mut any = false;
    for &(line, start_col, end_col) in segments {
        let Some(row) = rows.get(line) else {
            return false;
        };
        let start = usize::from(start_col);
        let end = usize::from(end_col).min(row.len());
        let Some(slice) = row.get(start..end) else {
            return false;
        };
        for cell in slice {
            if cell.flags.contains(Flags::SPACER) {
                continue;
            }
            any = true;
            if !cell.flags.contains(Flags::UNDERLINE) {
                return false;
            }
        }
    }
    any
}

fn pick_restore_fg(
    link_fg: Option<Color>,
    prev_cell: Option<&Cell>,
    next_cell: Option<&Cell>,
) -> Option<Color> {
    let link_fg = link_fg?;
    if let Some(prev) = prev_cell {
        if !prev.flags.contains(Flags::LINK)
            && prev.fg != link_fg
            && next_cell.is_none_or(|next| {
                (!next.flags.contains(Flags::LINK) && next.fg == prev.fg) || is_default_space(next)
            })
        {
            return Some(prev.fg);
        }
        return None;
    }
    next_cell
        .filter(|next| !next.flags.contains(Flags::LINK) && next.fg != link_fg)
        .map(|next| next.fg)
}

fn clear_trimmed_cells(
    row: &mut [Cell],
    start_col: u16,
    end_col: u16,
    strip_underline: bool,
    restore_fg: Option<Color>,
) {
    let start = usize::from(start_col);
    let end = usize::from(end_col).min(row.len());
    let Some(slice) = row.get_mut(start..end) else {
        return;
    };
    for cell in slice {
        cell.flags.0 &= !Flags::LINK.0;
        if strip_underline {
            cell.flags.0 &= !Flags::UNDERLINE.0;
            cell.underline = Color::Default;
        }
        if let Some(fg) = restore_fg {
            cell.fg = fg;
        }
    }
}

fn visible_text_matches_prefix(uri: &str, decoded_uri: Option<&str>, text: &str) -> (bool, bool) {
    let exact = uri == text || decoded_uri == Some(text);
    let prefix = uri.starts_with(text) || decoded_uri.is_some_and(|d| d.starts_with(text));
    (exact, prefix)
}

fn covered_by_link(row_links: &[Link], col: u16) -> bool {
    row_links
        .iter()
        .any(|l| (l.column..l.column.saturating_add(l.cols)).contains(&col))
}

fn should_extend_osc8_group(
    rows: &[Vec<Cell>],
    links: &[Vec<Link>],
    visited: &[Vec<bool>],
    segs: &[(usize, usize)],
    uri: &str,
    decoded_uri: Option<&str>,
    row_limit: usize,
) -> Option<(usize, usize)> {
    let &(cur_line, cur_idx) = segs.last()?;
    let cur_row_links = links.get(cur_line)?;
    if cur_idx.saturating_add(1) != cur_row_links.len() {
        return None;
    }
    let next_line = cur_line.checked_add(1)?;
    if next_line >= row_limit {
        return None;
    }
    if visited.get(next_line).and_then(|v| v.first()).copied() != Some(false) {
        return None;
    }
    let cur_link = cur_row_links.get(cur_idx)?;
    let next_link = links.get(next_line).and_then(|r| r.first())?;
    if next_link.cols == 0 || next_link.uri != uri {
        return None;
    }
    let soft_wrap = rows.get(cur_line).is_some_and(|r| {
        let end = usize::from(cur_link.column.saturating_add(cur_link.cols));
        let at_end = end >= r.len()
            || (end.saturating_add(1) == r.len()
                && r.last().is_some_and(|c| c.flags.contains(Flags::SPACER)));
        at_end && r.last().is_some_and(|c| c.flags.contains(Flags::WRAPLINE))
    }) && next_link.column == 0;

    if soft_wrap {
        return Some((next_line, 0));
    }

    let mut cur_chars = Vec::new();
    for &(l, i) in segs {
        let link = links.get(l).and_then(|r| r.get(i))?;
        let row = rows.get(l)?;
        if !push_row_span_chars(
            row,
            l,
            link.column,
            link.column.saturating_add(link.cols),
            &mut cur_chars,
        ) {
            return None;
        }
    }
    let cur_text: String = cur_chars.iter().map(|c| c.ch).collect();
    let (exact_cur, _) = visible_text_matches_prefix(uri, decoded_uri, &cur_text);
    if exact_cur {
        return None;
    }
    let next_row = rows.get(next_line)?;
    if !push_row_span_chars(
        next_row,
        next_line,
        next_link.column,
        next_link.column.saturating_add(next_link.cols),
        &mut cur_chars,
    ) {
        return None;
    }
    let combined: String = cur_chars.iter().map(|c| c.ch).collect();
    let (_, is_prefix) = visible_text_matches_prefix(uri, decoded_uri, &combined);
    is_prefix.then_some((next_line, 0))
}

fn normalize_osc8_links(rows: &mut [Vec<Cell>], links: &mut [Vec<Link>]) {
    let mut visited: Vec<Vec<bool>> = links.iter().map(|r| vec![false; r.len()]).collect();
    let row_limit = links.len().min(rows.len());
    for start_line in 0..row_limit {
        let link_count = links.get(start_line).map_or(0, Vec::len);
        for start_idx in 0..link_count {
            if visited
                .get(start_line)
                .and_then(|v| v.get(start_idx))
                .copied()
                != Some(false)
            {
                continue;
            }
            let Some(first_link) = links.get(start_line).and_then(|r| r.get(start_idx)) else {
                continue;
            };
            if first_link.cols == 0 || strip_url_scheme(&first_link.uri).is_none() {
                continue;
            }
            let uri = first_link.uri.clone();
            let decoded_uri = percent_decode_utf8(&uri);
            if let Some(slot) = visited
                .get_mut(start_line)
                .and_then(|v| v.get_mut(start_idx))
            {
                *slot = true;
            }

            let mut segs: Vec<(usize, usize)> = vec![(start_line, start_idx)];
            while let Some((next_line, next_idx)) = should_extend_osc8_group(
                rows,
                links,
                &visited,
                &segs,
                &uri,
                decoded_uri.as_deref(),
                row_limit,
            ) {
                if let Some(slot) = visited.get_mut(next_line).and_then(|v| v.get_mut(next_idx)) {
                    *slot = true;
                }
                segs.push((next_line, next_idx));
            }

            let mut span_chars = Vec::new();
            let mut seg_ranges: Vec<(usize, u16, u16)> = Vec::with_capacity(segs.len());
            let mut valid_span = true;
            for &(l, i) in &segs {
                let Some(link) = links.get(l).and_then(|r| r.get(i)) else {
                    valid_span = false;
                    break;
                };
                let Some(row) = rows.get(l) else {
                    valid_span = false;
                    break;
                };
                let end_col = link.column.saturating_add(link.cols);
                if !push_row_span_chars(row, l, link.column, end_col, &mut span_chars) {
                    valid_span = false;
                    break;
                }
                seg_ranges.push((l, link.column, end_col));
            }
            if !valid_span || span_chars.is_empty() {
                continue;
            }

            let clean_len = scan_url_len(&span_chars, |c| c.ch);
            if clean_len == 0
                || clean_len >= span_chars.len()
                || span_chars.get(clean_len).is_none_or(|c| c.width == 0)
            {
                continue;
            }
            let Some(clean_slice) = span_chars.get(..clean_len) else {
                continue;
            };
            let Some(tail_slice) = span_chars.get(clean_len..) else {
                continue;
            };
            let clean_text: String = clean_slice.iter().map(|c| c.ch).collect();
            let tail_text: String = tail_slice.iter().map(|c| c.ch).collect();
            if !has_valid_authority(&clean_text) {
                continue;
            }
            let Some(clean_uri) = tail_matches_uri(&uri, &clean_text, &tail_text) else {
                continue;
            };
            let Some(last_kept) = clean_slice.iter().rev().find(|c| c.width > 0) else {
                continue;
            };
            let kept_end_line = last_kept.line;
            let kept_end_col = last_kept.col.saturating_add(last_kept.width);

            let Some(&(first_line, first_col, _)) = seg_ranges.first() else {
                continue;
            };
            let Some(&(last_line, _, last_end)) = seg_ranges.last() else {
                continue;
            };
            let prev_cell = prev_neighbor(rows, first_line, first_col);
            let next_cell = next_neighbor(rows, last_line, last_end);
            let strip_underline = all_underlined(rows, &seg_ranges)
                && prev_cell.is_none_or(|c| !c.flags.contains(Flags::UNDERLINE))
                && next_cell.is_none_or(|c| !c.flags.contains(Flags::UNDERLINE));
            let link_fg = uniform_fg(rows, &seg_ranges);
            let restore_fg = pick_restore_fg(link_fg, prev_cell, next_cell);

            for &(l, i) in &segs {
                let Some(link) = links.get_mut(l).and_then(|r| r.get_mut(i)) else {
                    continue;
                };
                let orig_start = link.column;
                let orig_end = orig_start.saturating_add(link.cols);
                match l.cmp(&kept_end_line) {
                    Ordering::Less => {
                        link.uri.clone_from(&clean_uri);
                    }
                    Ordering::Equal => {
                        link.cols = kept_end_col.saturating_sub(orig_start);
                        link.uri.clone_from(&clean_uri);
                        if kept_end_col < orig_end
                            && let Some(row) = rows.get_mut(l)
                        {
                            clear_trimmed_cells(
                                row,
                                kept_end_col,
                                orig_end,
                                strip_underline,
                                restore_fg,
                            );
                        }
                    }
                    Ordering::Greater => {
                        link.cols = 0;
                        if let Some(row) = rows.get_mut(l) {
                            clear_trimmed_cells(
                                row,
                                orig_start,
                                orig_end,
                                strip_underline,
                                restore_fg,
                            );
                        }
                    }
                }
            }
        }
    }
    for row_links in links.iter_mut() {
        row_links.retain(|l| l.cols > 0);
    }
}

fn is_unlinked_styled_cell(cell: &Cell, row_links: &[Link], col_u16: u16) -> bool {
    !cell.flags.contains(Flags::HIDDEN)
        && (cell.flags.contains(Flags::UNDERLINE) || cell.flags.contains(Flags::LINK))
        && !covered_by_link(row_links, col_u16)
}

fn normalize_unstyled_runs(rows: &mut [Vec<Cell>], links: &[Vec<Link>]) {
    let mut visited_cells: Vec<Vec<bool>> = rows.iter().map(|r| vec![false; r.len()]).collect();
    for start_line in 0..rows.len() {
        let row_len = rows.get(start_line).map_or(0, Vec::len);
        for start_col in 0..row_len {
            if visited_cells
                .get(start_line)
                .and_then(|v| v.get(start_col))
                .copied()
                != Some(false)
            {
                continue;
            }
            let Some(first_cell) = rows.get(start_line).and_then(|r| r.get(start_col)) else {
                continue;
            };
            let start_col_u16 = u16::try_from(start_col).unwrap_or(u16::MAX);
            let row_links = links.get(start_line).map_or(&[][..], Vec::as_slice);
            if first_cell.flags.contains(Flags::SPACER)
                || !is_unlinked_styled_cell(first_cell, row_links, start_col_u16)
            {
                continue;
            }

            let mut seg_ranges: Vec<(usize, u16, u16)> = Vec::new();
            let mut cur_line = start_line;
            let mut cur_col = start_col;
            while let Some(row) = rows.get(cur_line) {
                let cur_links = links.get(cur_line).map_or(&[][..], Vec::as_slice);
                let seg_start = cur_col;
                while let Some(cell) = row.get(cur_col) {
                    if visited_cells
                        .get(cur_line)
                        .and_then(|v| v.get(cur_col))
                        .copied()
                        != Some(false)
                    {
                        break;
                    }
                    let col_u16 = u16::try_from(cur_col).unwrap_or(u16::MAX);
                    if !is_unlinked_styled_cell(cell, cur_links, col_u16) {
                        break;
                    }
                    if let Some(slot) = visited_cells
                        .get_mut(cur_line)
                        .and_then(|v| v.get_mut(cur_col))
                    {
                        *slot = true;
                    }
                    cur_col = cur_col.saturating_add(1);
                }
                if cur_col > seg_start {
                    seg_ranges.push((
                        cur_line,
                        u16::try_from(seg_start).unwrap_or(u16::MAX),
                        u16::try_from(cur_col).unwrap_or(u16::MAX),
                    ));
                }
                if cur_col == row.len()
                    && row_wraps(rows, cur_line)
                    && let Some(next_line) = cur_line.checked_add(1)
                    && next_line < rows.len()
                {
                    cur_line = next_line;
                    cur_col = 0;
                } else {
                    break;
                }
            }
            if seg_ranges.is_empty() {
                continue;
            }

            let mut run_chars = Vec::new();
            let mut valid_run = true;
            for &(l, s, e) in &seg_ranges {
                let Some(row) = rows.get(l) else {
                    valid_run = false;
                    break;
                };
                if !push_row_span_chars(row, l, s, e, &mut run_chars) {
                    valid_run = false;
                    break;
                }
            }
            if !valid_run || run_chars.is_empty() || run_chars.iter().any(|c| c.ch.is_whitespace())
            {
                continue;
            }
            let clean_len = scan_url_len(&run_chars, |c| c.ch);
            if clean_len == 0
                || clean_len >= run_chars.len()
                || run_chars.get(clean_len).is_none_or(|c| c.width == 0)
            {
                continue;
            }
            let Some(clean_slice) = run_chars.get(..clean_len) else {
                continue;
            };
            let clean_text: String = clean_slice.iter().map(|c| c.ch).collect();
            if !has_valid_authority(&clean_text) {
                continue;
            }
            let Some(last_kept) = clean_slice.iter().rev().find(|c| c.width > 0) else {
                continue;
            };
            let kept_end_line = last_kept.line;
            let kept_end_col = last_kept.col.saturating_add(last_kept.width);

            let Some(&(first_line, first_col, _)) = seg_ranges.first() else {
                continue;
            };
            let Some(&(last_line, _, last_end)) = seg_ranges.last() else {
                continue;
            };
            let prev_cell = prev_neighbor(rows, first_line, first_col);
            let next_cell = next_neighbor(rows, last_line, last_end);
            let run_fg = uniform_fg(rows, &seg_ranges);
            let restore_fg = pick_restore_fg(run_fg, prev_cell, next_cell);

            for &(l, seg_start, seg_end) in &seg_ranges {
                if l < kept_end_line {
                    continue;
                }
                let clear_start = if l == kept_end_line {
                    kept_end_col
                } else {
                    seg_start
                };
                if clear_start < seg_end
                    && let Some(row) = rows.get_mut(l)
                {
                    clear_trimmed_cells(row, clear_start, seg_end, true, restore_fg);
                }
            }
        }
    }
}

/// Normalizes over-captured bare `http://`/`https://` OSC 8 links, SGR-4 bare URL runs, and
/// orphaned `Flags::LINK` runs so trailing non-URL delimiters and suffixes (e.g. `)하면`) are
/// excluded from link underlines, targets, and link foreground colors.
pub fn normalize_screen_links(rows: &mut [Vec<Cell>], links: &mut [Vec<Link>]) {
    let has_links = links.iter().any(|row| !row.is_empty());
    let has_styled_cells = rows.iter().any(|row| {
        row.iter()
            .any(|c| c.flags.contains(Flags::LINK) || c.flags.contains(Flags::UNDERLINE))
    });
    if !has_links && !has_styled_cells {
        return;
    }
    if has_links {
        normalize_osc8_links(rows, links);
    }
    normalize_unstyled_runs(rows, links);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_from_str(s: &str, flags: Flags, fg: Color) -> Vec<Cell> {
        let mut out = Vec::new();
        for ch in s.chars() {
            let wide = is_cjk_or_hangul(ch) || is_fullwidth_punct(ch);
            out.push(Cell {
                ch,
                fg,
                flags: if wide { flags | Flags::WIDE } else { flags },
                ..Cell::default()
            });
            if wide {
                out.push(Cell {
                    fg,
                    flags: flags | Flags::SPACER,
                    ..Cell::default()
                });
            }
        }
        out
    }

    fn scan_str(s: &str) -> &str {
        let chars: Vec<char> = s.chars().collect();
        let n = scan_url_len(&chars, |&c| c);
        let byte_end: usize = chars.iter().take(n).map(|c| c.len_utf8()).sum();
        s.get(..byte_end).unwrap_or_default()
    }

    #[test]
    fn scan_url_len_handles_delimiters_and_particles() {
        assert_eq!(
            scan_str("http://127.0.0.1:8787)하면"),
            "http://127.0.0.1:8787"
        );
        assert_eq!(
            scan_str("http://127.0.0.1:8787/)하면"),
            "http://127.0.0.1:8787/"
        );
        assert_eq!(
            scan_str("https://example.com/문서)하면"),
            "https://example.com/문서"
        );
        assert_eq!(
            scan_str("https://example.com/문서]에서"),
            "https://example.com/문서"
        );
        assert_eq!(
            scan_str("http://127.0.0.1:8787)foo"),
            "http://127.0.0.1:8787"
        );
        assert_eq!(
            scan_str("https://example.com/a/b)foo"),
            "https://example.com/a/b"
        );
        assert_eq!(
            scan_str("http://127.0.0.1:8787(로컬)"),
            "http://127.0.0.1:8787"
        );
        assert_eq!(
            scan_str("http://127.0.0.1:8787/dashboard(차트)"),
            "http://127.0.0.1:8787/dashboard"
        );
        assert_eq!(
            scan_str("http://127.0.0.1:8787,http://127.0.0.1:8788"),
            "http://127.0.0.1:8787"
        );
        assert_eq!(
            scan_str("https://en.wikipedia.org/wiki/Rust_(programming_language))하면"),
            "https://en.wikipedia.org/wiki/Rust_(programming_language)"
        );
        assert_eq!(scan_str("http://[::1]:8787]하면"), "http://[::1]:8787");
        assert_eq!(
            scan_str("http://localhost:3000/문서!"),
            "http://localhost:3000/문서"
        );
        assert_eq!(scan_str("https://..."), "");
        assert_eq!(
            scan_str("cursor://vscode-remote/ssh-remote+box/home/user/repo)하면"),
            "cursor://vscode-remote/ssh-remote+box/home/user/repo"
        );
        assert_eq!(
            scan_str("vscode://vscode-remote/wsl+Ubuntu/workspace/main.rs:42:5."),
            "vscode://vscode-remote/wsl+Ubuntu/workspace/main.rs:42:5"
        );
        assert_eq!(scan_str("cursor://..."), "");
        assert_eq!(scan_str("vscode://"), "");
    }

    #[test]
    fn normalize_trims_cursor_and_vscode_osc8_links() {
        for uri_base in [
            "cursor://vscode-remote/ssh-remote+dev/home/user/app",
            "vscode://vscode-remote/wsl+Ubuntu/home/user/app",
        ] {
            let over = format!("{uri_base})하면");
            let mut row = row_from_str("(", Flags::default(), Color::Default);
            let start = u16::try_from(row.len()).unwrap();
            let linked = row_from_str(&over, Flags::LINK, Color::Default);
            let link_cols = u16::try_from(linked.len()).unwrap();
            row.extend(linked);
            let mut rows = vec![row];
            let mut links = vec![vec![Link {
                column: start,
                cols: link_cols,
                uri: over,
            }]];
            normalize_screen_links(&mut rows, &mut links);
            assert_eq!(
                links
                    .first()
                    .and_then(|r| r.first())
                    .map(|l| l.uri.as_str()),
                Some(uri_base)
            );
            assert_eq!(
                links.first().and_then(|r| r.first()).map(|l| l.cols),
                Some(u16::try_from(uri_base.len()).unwrap())
            );
        }
    }

    #[test]
    fn scan_url_len_supports_idn_unicode_hosts_without_swallowing_particles() {
        assert_eq!(scan_str("https://münchen.de"), "https://münchen.de");
        assert_eq!(
            scan_str("https://münchen.de/über/straße."),
            "https://münchen.de/über/straße"
        );
        assert_eq!(scan_str("https://예시.한국"), "https://예시.한국");
        assert_eq!(
            scan_str("https://sub.예시.한국/문서（참고）"),
            "https://sub.예시.한국/문서"
        );
        assert_eq!(scan_str("https://пример.рф/тест"), "https://пример.рф/тест");
        assert_eq!(scan_str("https://例え.jp/path"), "https://例え.jp/path");
        assert_eq!(scan_str("https://example.com에서"), "https://example.com");
        assert_eq!(
            scan_str("http://127.0.0.1:8787에서"),
            "http://127.0.0.1:8787"
        );
        assert_eq!(scan_str("https://예시.한국（링크）"), "https://예시.한국");
    }

    #[test]
    fn normalize_trims_osc8_only_and_osc8_with_sgr4_and_color() {
        // OSC 8 only
        let mut row = row_from_str("새로고침(", Flags::default(), Color::Default);
        let start = u16::try_from(row.len()).unwrap();
        let linked = row_from_str("http://127.0.0.1:8787)하면", Flags::LINK, Color::Default);
        let link_cols = u16::try_from(linked.len()).unwrap();
        row.extend(linked);
        row.extend(row_from_str(" 캔들", Flags::default(), Color::Default));

        let mut rows = vec![row];
        let mut links = vec![vec![Link {
            column: start,
            cols: link_cols,
            uri: "http://127.0.0.1:8787)하면".into(),
        }]];

        normalize_screen_links(&mut rows, &mut links);
        assert_eq!(
            links.first().map(Vec::as_slice),
            Some(
                [Link {
                    column: start,
                    cols: 21,
                    uri: "http://127.0.0.1:8787".into(),
                }]
                .as_slice()
            )
        );
        let kept_end = usize::from(start).saturating_add(21);
        let orig_end = usize::from(start.saturating_add(link_cols));
        for c in rows
            .first()
            .unwrap()
            .get(usize::from(start)..kept_end)
            .unwrap()
        {
            assert!(c.flags.contains(Flags::LINK));
        }
        for c in rows.first().unwrap().get(kept_end..orig_end).unwrap() {
            assert!(!c.flags.contains(Flags::LINK));
        }
        // Idempotent when called a second time.
        let snapshot_rows = rows.clone();
        let snapshot_links = links.clone();
        normalize_screen_links(&mut rows, &mut links);
        assert_eq!(rows, snapshot_rows);
        assert_eq!(links, snapshot_links);

        // OSC 8 + SGR 4 + blue fg (Codex-style)
        let mut row = row_from_str("새로고침(", Flags::default(), Color::Default);
        let start = u16::try_from(row.len()).unwrap();
        let blue = Color::Rgb(crate::theme::Rgb {
            r: 99,
            g: 168,
            b: 248,
        });
        let linked = row_from_str(
            "http://127.0.0.1:8787)하면",
            Flags::LINK | Flags::UNDERLINE,
            blue,
        );
        let link_cols = u16::try_from(linked.len()).unwrap();
        row.extend(linked);
        row.extend(row_from_str(" 캔들", Flags::default(), Color::Default));
        let mut rows = vec![row];
        let mut links = vec![vec![Link {
            column: start,
            cols: link_cols,
            uri: "http://127.0.0.1:8787)하면".into(),
        }]];
        normalize_screen_links(&mut rows, &mut links);
        assert_eq!(links.first().unwrap().first().unwrap().cols, 21);
        assert_eq!(
            links.first().unwrap().first().unwrap().uri,
            "http://127.0.0.1:8787"
        );
        let kept_end = usize::from(start).saturating_add(21);
        let orig_end = usize::from(start.saturating_add(link_cols));
        for c in rows.first().unwrap().get(kept_end..orig_end).unwrap() {
            assert!(!c.flags.contains(Flags::LINK));
            assert!(!c.flags.contains(Flags::UNDERLINE));
            assert_eq!(c.fg, Color::Default);
            assert_eq!(c.underline, Color::Default);
        }
    }

    #[test]
    fn normalize_handles_percent_encoded_uris_and_preserves_titled_or_spoofed_osc8() {
        // Percent-encoded URI
        let mut row = row_from_str("(", Flags::default(), Color::Default);
        let start = u16::try_from(row.len()).unwrap();
        let linked = row_from_str("http://127.0.0.1:8787)하면", Flags::LINK, Color::Default);
        let link_cols = u16::try_from(linked.len()).unwrap();
        row.extend(linked);
        let mut rows = vec![row];
        let mut links = vec![vec![Link {
            column: start,
            cols: link_cols,
            uri: "http://127.0.0.1:8787)%ED%95%98%EB%A9%B4".into(),
        }]];
        normalize_screen_links(&mut rows, &mut links);
        assert_eq!(links.first().unwrap().first().unwrap().cols, 21);
        assert_eq!(
            links.first().unwrap().first().unwrap().uri,
            "http://127.0.0.1:8787"
        );

        // Percent-encoded path + tail
        let mut row = row_from_str("(", Flags::default(), Color::Default);
        let start = u16::try_from(row.len()).unwrap();
        let linked = row_from_str("https://example.com/문서)하면", Flags::LINK, Color::Default);
        let link_cols = u16::try_from(linked.len()).unwrap();
        row.extend(linked);
        let mut rows = vec![row];
        let mut links = vec![vec![Link {
            column: start,
            cols: link_cols,
            uri: "https://example.com/%EB%AC%B8%EC%84%9C)%ED%95%98%EB%A9%B4".into(),
        }]];
        normalize_screen_links(&mut rows, &mut links);
        assert_eq!(links.first().unwrap().first().unwrap().cols, 24);
        assert_eq!(
            links.first().unwrap().first().unwrap().uri,
            "https://example.com/%EB%AC%B8%EC%84%9C"
        );

        // Titled, truncated, and spoofed OSC 8 links are NOT rewritten
        for (text, uri) in [
            ("문서", "https://example.com/한글"),
            (
                "https://example.com/long...",
                "https://example.com/long/full",
            ),
            ("https://safe.example)x", "https://evil.example/x"),
        ] {
            let row = row_from_str(text, Flags::LINK, Color::Default);
            let cols = u16::try_from(row.len()).unwrap();
            let mut rows = vec![row];
            let mut links = vec![vec![Link {
                column: 0,
                cols,
                uri: uri.into(),
            }]];
            normalize_screen_links(&mut rows, &mut links);
            assert_eq!(links.first().unwrap().first().unwrap().cols, cols, "{text}");
            assert_eq!(links.first().unwrap().first().unwrap().uri, uri, "{text}");
        }
    }

    #[test]
    fn normalize_trims_sgr4_only_and_orphaned_history_but_preserves_enclosing_underline() {
        // SGR 4 only bare URL
        let mut row = row_from_str("새로고침(", Flags::default(), Color::Default);
        let start = row.len();
        row.extend(row_from_str(
            "http://127.0.0.1:8787)하면",
            Flags::UNDERLINE,
            Color::Indexed(4),
        ));
        let end = row.len();
        row.extend(row_from_str(" 캔들", Flags::default(), Color::Default));
        let mut rows = vec![row];
        let mut links = vec![Vec::new()];
        normalize_screen_links(&mut rows, &mut links);
        let kept_end = start.saturating_add(21);
        for c in rows.first().unwrap().get(start..kept_end).unwrap() {
            assert!(c.flags.contains(Flags::UNDERLINE));
            assert_eq!(c.fg, Color::Indexed(4));
        }
        for c in rows.first().unwrap().get(kept_end..end).unwrap() {
            assert!(!c.flags.contains(Flags::UNDERLINE));
            assert_eq!(c.fg, Color::Default);
        }

        // Enclosing SGR 4 paragraph: underline is preserved on `)하면`
        let mut row = row_from_str("새로고침(", Flags::UNDERLINE, Color::Default);
        let start = u16::try_from(row.len()).unwrap();
        let linked = row_from_str(
            "http://127.0.0.1:8787)하면",
            Flags::LINK | Flags::UNDERLINE,
            Color::Default,
        );
        let link_cols = u16::try_from(linked.len()).unwrap();
        row.extend(linked);
        row.extend(row_from_str(" 캔들", Flags::UNDERLINE, Color::Default));
        let mut rows = vec![row];
        let mut links = vec![vec![Link {
            column: start,
            cols: link_cols,
            uri: "http://127.0.0.1:8787)하면".into(),
        }]];
        normalize_screen_links(&mut rows, &mut links);
        assert_eq!(links.first().unwrap().first().unwrap().cols, 21);
        assert_eq!(
            links.first().unwrap().first().unwrap().uri,
            "http://127.0.0.1:8787"
        );
        let kept_end = usize::from(start).saturating_add(21);
        let orig_end = usize::from(start.saturating_add(link_cols));
        for c in rows.first().unwrap().get(kept_end..orig_end).unwrap() {
            assert!(!c.flags.contains(Flags::LINK));
            assert!(c.flags.contains(Flags::UNDERLINE));
        }

        // Orphaned history `Flags::LINK` without `links` entry
        let mut row = row_from_str("(", Flags::default(), Color::Default);
        let start = row.len();
        row.extend(row_from_str(
            "http://127.0.0.1:8787)하면",
            Flags::LINK,
            Color::Default,
        ));
        let end = row.len();
        let mut rows = vec![row];
        let mut links = Vec::new();
        normalize_screen_links(&mut rows, &mut links);
        let kept_end = start.saturating_add(21);
        for c in rows.first().unwrap().get(start..kept_end).unwrap() {
            assert!(c.flags.contains(Flags::LINK));
        }
        for c in rows.first().unwrap().get(kept_end..end).unwrap() {
            assert!(!c.flags.contains(Flags::LINK));
        }
    }

    #[test]
    fn normalize_handles_soft_and_hard_wrapped_two_row_osc8_links() {
        // Soft-wrapped where break happens right before `)하면`
        let mut r0 = row_from_str("새로고침(", Flags::default(), Color::Default);
        let start0 = u16::try_from(r0.len()).unwrap();
        let part0 = row_from_str("http://127.0.0.1:8787", Flags::LINK, Color::Default);
        let cols0 = u16::try_from(part0.len()).unwrap();
        r0.extend(part0);
        r0.last_mut().unwrap().flags |= Flags::WRAPLINE;

        let r1 = row_from_str(")하면", Flags::LINK, Color::Default);
        let cols1 = u16::try_from(r1.len()).unwrap();
        let mut rows = vec![r0, r1];
        let mut links = vec![
            vec![Link {
                column: start0,
                cols: cols0,
                uri: "http://127.0.0.1:8787)하면".into(),
            }],
            vec![Link {
                column: 0,
                cols: cols1,
                uri: "http://127.0.0.1:8787)하면".into(),
            }],
        ];
        normalize_screen_links(&mut rows, &mut links);
        assert_eq!(
            links.first().map(Vec::as_slice),
            Some(
                [Link {
                    column: start0,
                    cols: cols0,
                    uri: "http://127.0.0.1:8787".into(),
                }]
                .as_slice()
            )
        );
        assert!(links.get(1).unwrap().is_empty());
        assert!(
            rows.get(1)
                .unwrap()
                .iter()
                .all(|c| !c.flags.contains(Flags::LINK))
        );

        // Hard-wrapped 2-row OSC 8 where line 1 contains part of the URL + `)하면`
        let mut r0 = row_from_str("  ", Flags::default(), Color::Default);
        let p0 = row_from_str("http://127.0.0.1:", Flags::LINK, Color::Default);
        let c0 = u16::try_from(p0.len()).unwrap();
        r0.extend(p0);
        let mut r1 = row_from_str("  ", Flags::default(), Color::Default);
        let p1 = row_from_str("8787/abc)하면", Flags::LINK, Color::Default);
        let c1 = u16::try_from(p1.len()).unwrap();
        r1.extend(p1);
        let mut rows = vec![r0, r1];
        let mut links = vec![
            vec![Link {
                column: 2,
                cols: c0,
                uri: "http://127.0.0.1:8787/abc)하면".into(),
            }],
            vec![Link {
                column: 2,
                cols: c1,
                uri: "http://127.0.0.1:8787/abc)하면".into(),
            }],
        ];
        normalize_screen_links(&mut rows, &mut links);
        assert_eq!(
            links.first().unwrap().first().unwrap().uri,
            "http://127.0.0.1:8787/abc"
        );
        assert_eq!(
            links.get(1).map(Vec::as_slice),
            Some(
                [Link {
                    column: 2,
                    cols: 8,
                    uri: "http://127.0.0.1:8787/abc".into(),
                }]
                .as_slice()
            )
        );
        let r1_row = rows.get(1).unwrap();
        for c in r1_row.get(2..10).unwrap() {
            assert!(c.flags.contains(Flags::LINK));
        }
        let end_col = 2usize.saturating_add(usize::from(c1));
        for c in r1_row.get(10..end_col).unwrap() {
            assert!(!c.flags.contains(Flags::LINK));
        }
    }

    #[test]
    fn normalize_keeps_final_cell_with_combining_or_zerowidth_marks() {
        let blue = Color::Indexed(4);
        let make_url_cells = |flags: Flags, fg: Color| -> Vec<Cell> {
            let mut cells = row_from_str("https://example.com/cafe", flags, fg);
            cells.last_mut().unwrap().zerowidth.push('\u{0301}');
            cells.extend(row_from_str(")하면", flags, fg));
            cells
        };

        // OSC 8 URL ending with combining mark (`e` + U+0301) followed by `)하면`.
        let mut row = row_from_str("(", Flags::default(), Color::Default);
        let start = u16::try_from(row.len()).unwrap();
        let linked = make_url_cells(Flags::LINK | Flags::UNDERLINE, blue);
        let link_cols = u16::try_from(linked.len()).unwrap();
        row.extend(linked);
        let mut rows = vec![row];
        let mut links = vec![vec![Link {
            column: start,
            cols: link_cols,
            uri: "https://example.com/cafe\u{0301})하면".into(),
        }]];
        normalize_screen_links(&mut rows, &mut links);
        assert_eq!(links[0][0].cols, 24);
        assert_eq!(links[0][0].uri, "https://example.com/cafe\u{0301}");
        let kept_end = usize::from(start) + 24;
        let orig_end = usize::from(start + link_cols);
        for c in &rows[0][usize::from(start)..kept_end] {
            assert!(c.flags.contains(Flags::LINK));
            assert!(c.flags.contains(Flags::UNDERLINE));
            assert_eq!(c.fg, blue);
        }
        for c in &rows[0][kept_end..orig_end] {
            assert!(!c.flags.contains(Flags::LINK));
            assert!(!c.flags.contains(Flags::UNDERLINE));
            assert_eq!(c.fg, Color::Default);
        }

        // SGR-4 bare URL ending with combining mark followed by `)하면`.
        let mut row = row_from_str("(", Flags::default(), Color::Default);
        let start = row.len();
        row.extend(make_url_cells(Flags::UNDERLINE, blue));
        let end = row.len();
        let mut rows = vec![row];
        let mut links = vec![Vec::new()];
        normalize_screen_links(&mut rows, &mut links);
        let kept_end = start + 24;
        for c in &rows[0][start..kept_end] {
            assert!(c.flags.contains(Flags::UNDERLINE));
            assert_eq!(c.fg, blue);
        }
        for c in &rows[0][kept_end..end] {
            assert!(!c.flags.contains(Flags::UNDERLINE));
            assert_eq!(c.fg, Color::Default);
        }

        // Wide char with zerowidth variation selector before `)하면`.
        let mut row = row_from_str("(", Flags::default(), Color::Default);
        let start = u16::try_from(row.len()).unwrap();
        let mut linked = row_from_str("https://example.com/한", Flags::LINK, blue);
        // Attach U+FE0F to the wide base cell (second-to-last, before its SPACER).
        let base_idx = linked.len() - 2;
        linked[base_idx].zerowidth.push('\u{fe0f}');
        let wide_url_cols = u16::try_from(linked.len()).unwrap();
        linked.extend(row_from_str(")하면", Flags::LINK, blue));
        let link_cols = u16::try_from(linked.len()).unwrap();
        row.extend(linked);
        let mut rows = vec![row];
        let mut links = vec![vec![Link {
            column: start,
            cols: link_cols,
            uri: "https://example.com/한\u{fe0f})하면".into(),
        }]];
        normalize_screen_links(&mut rows, &mut links);
        assert_eq!(links[0][0].cols, wide_url_cols);
        assert_eq!(links[0][0].uri, "https://example.com/한\u{fe0f}");
        let kept_end = usize::from(start + wide_url_cols);
        let orig_end = usize::from(start + link_cols);
        for c in &rows[0][usize::from(start)..kept_end] {
            assert!(c.flags.contains(Flags::LINK));
        }
        for c in &rows[0][kept_end..orig_end] {
            assert!(!c.flags.contains(Flags::LINK));
        }
    }
}
