//! 터미널 출력 속 파일 경로 링크(OWE-663 수용 기준 4, MK-119). 컴파일러·grep·git이 찍는
//! `path`, `path:line`, `path:line:col`을 ⌘호버 밑줄·⌘클릭 대상으로 본다.
//!
//! 동작(존재하는 파일만, URL 제외, 상대 경로는 세션 cwd 기준, git `a/`·`b/` 접두사)은 Orca의 터미널 파일 링크
//! (`.ref/orca/src/renderer/src/components/terminal-pane/terminal-file-link-*.ts`, MIT)를 참고했다. 코드 차용 없음.
//!
//! 비용: 포인터 아래 행 하나만 본다(보이는 칸 전체를 훑지 않는다). 후보 글자열이 같으면 디스크를 다시 보지 않는다([`Cache`]).

use std::path::{Path, PathBuf};

use krill_proto::screen::{Cell, Flags, is_bidi_control};

/// ⌘ 아래에서 찾은 파일 링크. `column`부터 `cols`칸에 밑줄을 긋는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathLink {
    pub column: u16,
    pub cols: u16,
    pub path: PathBuf,
    /// 1부터 센 줄·열. 없으면 파일 처음.
    pub line: Option<u32>,
    pub col: Option<u32>,
}

/// ⌘ 아래에서 찾은 평문 웹 URL(`http://`·`https://`). `column`부터 `cols`칸에 밑줄을 긋는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlLink {
    pub column: u16,
    pub cols: u16,
    pub uri: String,
}

/// 전각 문장부호·괄호(한·중·일 문장 안 경로·URL 경계, #30213, #30093).
fn is_fullwidth_punct(c: char) -> bool {
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

/// 한글·가나·한자 등 공백 없이 조사·서술어가 이어지는 동아시아 문자(#30213, #30093, #30037).
fn is_cjk_or_hangul(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x11FF
            | 0x3040..=0x30FF
            | 0x3130..=0x318F
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
    )
}

/// 경로를 끊는 글자. `:`는 `path:line`에 쓰여 끊지 않는다.
fn breaks(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || is_bidi_control(c)
        || is_fullwidth_punct(c)
        || matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | ',' | ';'
        )
}

/// 평문 URL 본문을 끊는 글자. 쿼리·경로 안 `,`·`;`·`:`·`()`·`[]`는 본문에 두고 문장 끝에서만 잘라낸다.
fn url_breaks(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || is_bidi_control(c)
        || is_fullwidth_punct(c)
        || matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '{' | '}' | '|' | '\\' | '^'
        )
}

fn trim_url_end(slice: &[(u16, char, u16)]) -> usize {
    let mut end = slice.len();
    while end > 0 {
        let Some(&(_, c, _)) = slice.get(end - 1) else {
            break;
        };
        let strip = match c {
            '.' | ',' | ':' | ';' | '!' | '?' | '~' => true,
            ')' => {
                let head = slice.get(..end).unwrap_or_default();
                let open = head.iter().filter(|&&(_, ch, _)| ch == '(').count();
                let close = head.iter().filter(|&&(_, ch, _)| ch == ')').count();
                close > open
            }
            ']' => {
                let head = slice.get(..end).unwrap_or_default();
                let open = head.iter().filter(|&&(_, ch, _)| ch == '[').count();
                let close = head.iter().filter(|&&(_, ch, _)| ch == ']').count();
                close > open
            }
            _ => false,
        };
        if strip {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// (시작 칸, 글자, 폭). 2칸 글자의 둘째 칸(SPACER)은 앞 글자에 붙인다.
fn row_chars(row: &[Cell]) -> Vec<(u16, char, u16)> {
    let mut chars: Vec<(u16, char, u16)> = Vec::with_capacity(row.len());
    for (col, cell) in (0u16..).zip(row) {
        if cell.flags.contains(Flags::SPACER) {
            if let Some(last) = chars.last_mut() {
                last.2 = last.2.saturating_add(1);
            }
            continue;
        }
        let ch = if cell.flags.contains(Flags::HIDDEN) {
            ' '
        } else {
            cell.ch
        };
        chars.push((col, ch, 1));
    }
    chars
}

/// `row`의 `column` 칸을 덮는 평문 `http://`·`https://` URL을 찾는다(OWE-867).
pub fn url_at(row: &[Cell], column: u16) -> Option<UrlLink> {
    let chars = row_chars(row);
    let len = chars.len();
    let mut i = 0;
    while i < len {
        let start = i;
        let rest = chars.get(start..)?;
        let scheme_len = if rest
            .get(..8)
            .is_some_and(|s| s.iter().map(|t| t.1).eq("https://".chars()))
        {
            8
        } else if rest
            .get(..7)
            .is_some_and(|s| s.iter().map(|t| t.1).eq("http://".chars()))
        {
            7
        } else {
            i += 1;
            continue;
        };
        let boundary = start == 0
            || chars
                .get(start - 1)
                .is_none_or(|&(_, c, _)| !c.is_ascii_alphanumeric() && c != '_');
        if !boundary {
            i = start + scheme_len;
            continue;
        }
        let mut in_path = false;
        let mut seg_ascii = false;
        let raw_end = rest
            .iter()
            .enumerate()
            .position(|(idx, &(_, c, _))| {
                if url_breaks(c) {
                    return true;
                }
                if idx >= scheme_len {
                    if matches!(c, '/' | '?' | '#' | '=' | '&') {
                        in_path = true;
                        seg_ascii = false;
                        return false;
                    }
                    if !in_path && !c.is_ascii() {
                        return true;
                    }
                    if in_path && seg_ascii && is_cjk_or_hangul(c) {
                        return true;
                    }
                    if c.is_ascii_alphanumeric() {
                        seg_ascii = true;
                    }
                }
                false
            })
            .map_or(len, |off| start + off);
        // 같은 공백 단위 토큰 안에 제어·양방향 위장 문자가 섞여 있으면 전체 후보를 버린다.
        let token_poisoned = rest
            .iter()
            .take_while(|&&(_, c, _)| !c.is_whitespace())
            .any(|&(_, c, _)| c.is_control() || is_bidi_control(c));
        if token_poisoned {
            i = raw_end.max(start + scheme_len);
            continue;
        }
        i = start + scheme_len;
        let Some(raw_slice) = chars.get(start..raw_end) else {
            continue;
        };
        let trimmed_len = trim_url_end(raw_slice);
        if trimmed_len <= scheme_len {
            continue;
        }
        let Some(token) = raw_slice.get(..trimmed_len) else {
            continue;
        };
        let Some(&(start_col, _, _)) = token.first() else {
            continue;
        };
        let Some(&(last_col, _, last_w)) = token.last() else {
            continue;
        };
        let end_col = last_col.saturating_add(last_w);
        if !(start_col..end_col).contains(&column) {
            continue;
        }
        let uri: String = token.iter().map(|t| t.1).collect();
        let valid = gpui::http_client::Url::parse(&uri).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https")
                && u.host_str()
                    .is_some_and(|h| !h.is_empty() && !h.chars().all(|c| c == '.'))
        });
        if valid {
            return Some(UrlLink {
                column: start_col,
                cols: end_col.saturating_sub(start_col),
                uri,
            });
        }
    }
    None
}

/// 포인터 칸을 덮는 후보 글자열(디스크를 보지 않는다). 글자마다 시작 칸, 마지막 글자 다음 칸을 함께 둔다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    chars: Vec<char>,
    starts: Vec<u16>,
    end: u16,
}

fn candidate_at(row: &[Cell], column: u16) -> Option<Candidate> {
    let chars = row_chars(row);
    let hit = chars
        .iter()
        .position(|&(c, _, w)| (c..c.saturating_add(w)).contains(&column))?;
    if chars.get(hit).is_none_or(|&(_, ch, _)| breaks(ch)) {
        return None;
    }
    let first = chars
        .get(..hit)?
        .iter()
        .rposition(|&(_, ch, _)| breaks(ch))
        .map_or(0, |i| i + 1);
    let mut last = chars
        .get(hit..)?
        .iter()
        .position(|&(_, ch, _)| breaks(ch))
        .map_or(chars.len(), |i| hit + i);
    // 문장 끝 `.`·`:`(예: "see src/a.rs.", "error in src/a.rs:")는 경로가 아니다.
    while last > first
        && chars
            .get(last - 1)
            .is_some_and(|c| matches!(c.1, '.' | ':'))
    {
        last -= 1;
    }
    let token = chars.get(first..last)?;
    let end = token.last().map(|&(c, _, w)| c.saturating_add(w))?;
    Some(Candidate {
        chars: token.iter().map(|t| t.1).collect(),
        starts: token.iter().map(|t| t.0).collect(),
        end,
    })
}

/// `path[:line[:col]]`를 나눈다. 뒤에 붙은 나머지(grep의 `:내용`)는 버린다. 마지막 값은 링크에 드는 글자 수.
fn split_location(text: &[char]) -> (String, Option<u32>, Option<u32>, usize) {
    let digit_at = |i: usize| text.get(i).is_some_and(char::is_ascii_digit);
    let Some(colon) = (0..text.len()).find(|&i| text.get(i) == Some(&':') && digit_at(i + 1))
    else {
        let used = trim_cjk_suffix_after_ascii_ext(text);
        return (text.iter().take(used).collect(), None, None, used);
    };
    let number = |from: usize| {
        let s: String = text
            .iter()
            .skip(from)
            .take_while(|c| c.is_ascii_digit())
            .collect();
        (s.parse::<u32>().ok(), s.len())
    };
    let (line, n) = number(colon + 1);
    let mut used = colon + 1 + n;
    let mut col = None;
    if text.get(used) == Some(&':') && digit_at(used + 1) {
        let (c, m) = number(used + 1);
        col = c;
        used += 1 + m;
    }
    (text.iter().take(colon).collect(), line, col, used)
}

/// `src/main.rs에서`처럼 ASCII 확장자 뒤에 바로 이어진 한·중·일 조사를 잘라낸다(#30093).
fn trim_cjk_suffix_after_ascii_ext(text: &[char]) -> usize {
    let Some(dot) = text.iter().rposition(|&c| c == '.') else {
        return text.len();
    };
    let after_dot = text.get(dot + 1..).unwrap_or_default();
    if after_dot.first().is_none_or(|c| !c.is_ascii_alphanumeric()) {
        return text.len();
    }
    after_dot
        .iter()
        .position(|&c| is_cjk_or_hangul(c))
        .map_or(text.len(), |off| dot + 1 + off)
}

/// 찾아 볼 실제 경로들(앞이 우선). URL·빈 경로·제어 문자는 링크가 아니다.
fn resolve(path: &str, cwd: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    if path.is_empty() || path.contains("://") || path.starts_with("mailto:") {
        return Vec::new();
    }
    if path.starts_with('/') {
        return vec![PathBuf::from(path)];
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.map(|h| h.join(rest)).into_iter().collect();
    }
    let Some(cwd) = cwd else {
        return Vec::new();
    };
    let mut out = vec![cwd.join(path)];
    // `git diff`의 `a/src/x.rs`·`b/src/x.rs`.
    if let Some(rest) = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")) {
        out.push(cwd.join(rest));
    }
    out
}

/// 디스크 확인 결과 한 칸 캐시: 포인터가 같은 글자 위에서 움직이는 동안 stat을 다시 하지 않는다.
// ponytail: 한 칸 캐시라 파일이 생기거나 지워진 직후 같은 글자열 위에서는 옛 결과를 본다. 포인터가 다른 글자로 가면 다시 본다.
#[derive(Debug, Default)]
pub struct Cache {
    last: Option<(String, Option<PathBuf>, Option<PathBuf>)>,
}

impl Cache {
    /// `row`의 `column` 칸을 덮는 파일 링크. 존재하는 일반 파일만(디렉터리 제외).
    // ponytail: 자동 줄바꿈으로 두 행에 걸친 경로는 포인터 쪽 행 조각만 본다. 긴 경로가 잘리면 WRAPLINE으로 잇는다.
    pub fn link_at(&mut self, row: &[Cell], column: u16, cwd: Option<&Path>) -> Option<PathLink> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        self.link_at_with_home(row, column, cwd, home.as_deref())
    }

    fn link_at_with_home(
        &mut self,
        row: &[Cell],
        column: u16,
        cwd: Option<&Path>,
        home: Option<&Path>,
    ) -> Option<PathLink> {
        let cand = candidate_at(row, column)?;
        let (path, line, col, chars) = split_location(&cand.chars);
        let cols_end = cand.starts.get(chars).copied().unwrap_or(cand.end);
        let start = *cand.starts.first()?;
        if !(start..cols_end).contains(&column) {
            return None;
        }
        let cwd_key = cwd.map(Path::to_owned);
        let found = match &self.last {
            Some((p, c, found)) if *p == path && *c == cwd_key => found.clone(),
            _ => {
                let found = resolve(&path, cwd, home).into_iter().find(|p| p.is_file());
                self.last = Some((path, cwd_key, found.clone()));
                found
            }
        }?;
        Some(PathLink {
            column: start,
            cols: cols_end - start,
            path: found,
            line,
            col,
        })
    }
}

#[cfg(test)]
mod tests;
