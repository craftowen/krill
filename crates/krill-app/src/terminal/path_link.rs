//! 터미널 출력 속 파일 경로 링크(OWE-663 수용 기준 4, MK-119). 컴파일러·grep·git이 찍는
//! `path`, `path:line`, `path:line:col`을 ⌘호버 밑줄·⌘클릭 대상으로 본다.
//!
//! 동작(존재하는 파일만, URL 제외, 상대 경로는 세션 cwd 기준, git `a/`·`b/` 접두사)은 Orca의 터미널 파일 링크
//! (`.ref/orca/src/renderer/src/components/terminal-pane/terminal-file-link-*.ts`, MIT)를 참고했다. 코드 차용 없음.
//!
//! 비용: 화면 기반 조회는 제한된 이웃 행을 모은다. 공백을 포함한 경로 후보는 최대 64개이며 디스크 확인은 호출자의 백그라운드 작업에서 한다.

use std::path::{Path, PathBuf};

use krill_proto::screen::{Cell, Flags, is_bidi_control};

mod input;
pub use input::Input;

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
    if row
        .iter()
        .try_fold(0usize, |count, cell| {
            count
                .checked_add(1 + cell.zerowidth.len())
                .filter(|count| *count <= 16_384)
        })
        .is_none()
    {
        return Vec::new();
    }
    let mut chars: Vec<(u16, char, u16)> = Vec::with_capacity(row.len());
    for (col, cell) in (0u16..).zip(row) {
        if cell.flags.contains(Flags::SPACER) {
            if col > 0
                && row
                    .get(usize::from(col - 1))
                    .is_some_and(|c| c.flags.contains(Flags::WIDE))
                && let Some(last) = chars.iter_mut().rev().find(|t| t.0 == col - 1 && t.2 > 0)
            {
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
        for &ch in cell
            .zerowidth
            .iter()
            .filter(|_| !cell.flags.contains(Flags::HIDDEN))
        {
            chars.push((col, ch, 0));
        }
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
        let end_col = token
            .iter()
            .map(|&(col, _, w)| col.saturating_add(w))
            .max()
            .unwrap_or(last_col.saturating_add(last_w));
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

fn candidate_at(chars: &[(u16, char, u16)], column: u16) -> Option<Candidate> {
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
    candidate(chars.get(first..last)?)
}

fn candidate(token: &[(u16, char, u16)]) -> Option<Candidate> {
    let end = token.iter().map(|&(c, _, w)| c.saturating_add(w)).max()?;
    Some(Candidate {
        chars: token.iter().map(|t| t.1).collect(),
        starts: token.iter().map(|t| t.0).collect(),
        end,
    })
}

/// 따옴표·마크다운 안 경로를 먼저 보고, 공백 경계 조합을 실제 파일 존재 여부로 구분한다.
fn candidates_at(row: &[Cell], column: u16) -> Vec<Candidate> {
    const MAX_CANDIDATES: usize = 64;
    let chars = row_chars(row);
    let Some(hit) = chars
        .iter()
        .position(|&(c, _, w)| (c..c.saturating_add(w)).contains(&column))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (open, close) in [
        ('"', '"'),
        ('\'', '\''),
        ('`', '`'),
        ('[', ']'),
        ('(', ')'),
        ('“', '”'),
        ('‘', '’'),
    ] {
        let Some(start) = chars
            .get(..hit)
            .and_then(|s| s.iter().rposition(|t| t.1 == open))
        else {
            continue;
        };
        let Some(end) = chars
            .get(hit + 1..)
            .and_then(|s| s.iter().position(|t| t.1 == close))
            .map(|i| hit + 1 + i)
        else {
            continue;
        };
        let token = &chars[start + 1..end];
        if !token
            .iter()
            .any(|t| t.1.is_control() || is_bidi_control(t.1))
            && let Some(cand) = candidate(token)
        {
            out.push(cand);
        }
    }
    let hard_break = |c: char| breaks(c) && c != ' ';
    let first = chars[..hit]
        .iter()
        .rposition(|t| hard_break(t.1))
        .map_or(0, |i| i + 1);
    let last = chars[hit..]
        .iter()
        .position(|t| hard_break(t.1))
        .map_or(chars.len(), |i| hit + i);
    let mut starts: Vec<_> = (first..=hit)
        .filter(|&start| {
            !chars[start].1.is_whitespace() && (start == first || chars[start - 1].1 == ' ')
        })
        .collect();
    starts.sort_by_key(|&start| {
        (
            !matches!(chars[start].1, '/' | '~'),
            std::cmp::Reverse(start),
        )
    });
    starts.truncate(MAX_CANDIDATES);
    let ends: Vec<_> = (hit + 1..=last)
        .rev()
        .filter(|&end| !chars[end - 1].1.is_whitespace() && (end == last || chars[end].1 == ' '))
        .take(MAX_CANDIDATES)
        .collect();
    let mut ranges: Vec<_> = starts
        .into_iter()
        .flat_map(|start| ends.iter().map(move |&end| (start, end)))
        .collect();
    // 절대·홈 경로를 우선하고 같은 시작점에서는 가장 긴 실재 경로를 고른다.
    ranges.sort_by_key(|&(start, end)| {
        (
            !matches!(chars[start].1, '/' | '~'),
            std::cmp::Reverse(end - start),
        )
    });
    out.extend(
        ranges
            .into_iter()
            .take(MAX_CANDIDATES)
            .filter_map(|(start, end)| candidate(&chars[start..end])),
    );
    out.truncate(MAX_CANDIDATES - 1);
    // 컴파일러 형식은 문장부호를 뺀 기존 후보도 남긴다.
    if let Some(cand) = candidate_at(&chars, column) {
        out.push(cand);
    }
    out.dedup();
    out
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
pub(super) fn valid_raw_candidate(path: &str) -> bool {
    !path.is_empty()
        && !path.contains("://")
        && !path.starts_with("mailto:")
        && !path.chars().any(|c| c.is_control() || is_bidi_control(c))
}

fn resolve(path: &str, cwd: Option<&Path>, home: Option<&Path>) -> Vec<PathBuf> {
    if !valid_raw_candidate(path) {
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
    last: Option<Cached>,
    remote_probes: std::collections::HashMap<(Option<PathBuf>, String), Option<PathBuf>>,
}

#[derive(Debug)]
struct Cached {
    candidates: Vec<Candidate>,
    column: u16,
    cwd: Option<PathBuf>,
    home: Option<PathBuf>,
    found: Option<PathLink>,
}

impl Cache {
    /// `row`의 `column` 칸을 덮는 파일 링크. 존재하는 일반 파일만(디렉터리 제외).
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
        self.lookup(candidates_at(row, column), column, cwd, home, &mut 256)
    }

    fn bounded(
        &mut self,
        row: &[Cell],
        column: u16,
        cwd: Option<&Path>,
        cap: usize,
        budget: &mut usize,
        crossing: Option<(u16, u16)>,
    ) -> Option<PathLink> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut candidates = candidates_at(row, column);
        if let Some((first_end, last_start)) = crossing {
            candidates.retain(|candidate| {
                candidate
                    .starts
                    .first()
                    .is_some_and(|&start| start < first_end)
                    && candidate.end > last_start
            });
        }
        candidates.truncate(cap);
        self.lookup(candidates, column, cwd, home.as_deref(), budget)
    }

    fn lookup(
        &mut self,
        candidates: Vec<Candidate>,
        column: u16,
        cwd: Option<&Path>,
        home: Option<&Path>,
        budget: &mut usize,
    ) -> Option<PathLink> {
        if self.last.as_ref().is_none_or(|last| {
            last.candidates != candidates
                || last.cwd.as_deref() != cwd
                || last.home.as_deref() != home
                || (last.column != column
                    && last.found.as_ref().is_none_or(|link| {
                        !(link.column..link.column.saturating_add(link.cols)).contains(&column)
                    }))
        }) {
            let mut is_file = |path: &PathBuf| {
                if *budget == 0 {
                    return false;
                }
                *budget -= 1;
                path.is_file()
            };
            let found = candidates.iter().find_map(|cand| {
                let literal: String = cand.chars.iter().collect();
                let exact = resolve(&literal, cwd, home).into_iter().find(&mut is_file);
                let (path, line, col, used) = if let Some(path) = exact {
                    (path, None, None, cand.chars.len())
                } else {
                    let (path, line, col, used) = split_location(&cand.chars);
                    let found = resolve(&path, cwd, home).into_iter().find(&mut is_file)?;
                    (found, line, col, used)
                };
                let end = cand.starts.get(used).copied().map_or(cand.end, |next| {
                    let last = cand
                        .starts
                        .get(used.saturating_sub(1))
                        .copied()
                        .unwrap_or(next);
                    next.max(last.saturating_add(1))
                });
                let start = *cand.starts.first()?;
                if !(start..end).contains(&column) {
                    return None;
                }
                Some(PathLink {
                    column: start,
                    cols: end - start,
                    path,
                    line,
                    col,
                })
            });
            self.last = Some(Cached {
                candidates,
                column,
                cwd: cwd.map(Path::to_owned),
                home: home.map(Path::to_owned),
                found,
            });
        }
        self.last
            .as_ref()?
            .found
            .clone()
            .filter(|link| (link.column..link.column.saturating_add(link.cols)).contains(&column))
    }

    pub(super) fn probe_remote(
        &mut self,
        target: &krill_hosts::HostTarget,
        cwd: Option<&Path>,
        query_paths: &[PathBuf],
    ) -> std::collections::HashMap<String, PathBuf> {
        let cwd_key = cwd.map(Path::to_owned);
        let mut missing = Vec::new();
        for raw in query_paths {
            if let Some(raw_str) = raw.to_str()
                && !self
                    .remote_probes
                    .contains_key(&(cwd_key.clone(), raw_str.to_owned()))
            {
                missing.push(raw.clone());
            }
        }
        if !missing.is_empty() {
            let probed = crate::remote_fs::probe_paths(target, cwd, &missing);
            for (raw, found) in missing.into_iter().zip(probed) {
                if let Some(raw_str) = raw.to_str() {
                    self.remote_probes
                        .insert((cwd_key.clone(), raw_str.to_owned()), found);
                }
            }
        }
        let mut resolved = std::collections::HashMap::new();
        for raw in query_paths {
            if let Some(raw_str) = raw.to_str()
                && let Some(Some(found_path)) = self
                    .remote_probes
                    .get(&(cwd_key.clone(), raw_str.to_owned()))
            {
                resolved.insert(raw_str.to_owned(), found_path.clone());
            }
        }
        resolved
    }
}

fn bounded_candidates(
    row: &[Cell],
    column: u16,
    cap: usize,
    crossing: Option<(u16, u16)>,
) -> Vec<Candidate> {
    let mut candidates = candidates_at(row, column);
    if let Some((first_end, last_start)) = crossing {
        candidates.retain(|candidate| {
            candidate
                .starts
                .first()
                .is_some_and(|&start| start < first_end)
                && candidate.end > last_start
        });
    }
    candidates.truncate(cap);
    candidates
}

fn collect_remote_queries(
    candidates: &[Candidate],
    out: &mut Vec<PathBuf>,
    seen: &mut std::collections::HashSet<String>,
    max_queries: usize,
) {
    for cand in candidates {
        if out.len() >= max_queries {
            break;
        }
        let literal: String = cand.chars.iter().collect();
        if valid_raw_candidate(&literal) && seen.insert(literal.clone()) {
            out.push(PathBuf::from(&literal));
            if out.len() >= max_queries {
                break;
            }
        }
        let (split, _, _, _) = split_location(&cand.chars);
        if split != literal && valid_raw_candidate(&split) && seen.insert(split.clone()) {
            out.push(PathBuf::from(split));
        }
    }
}

fn lookup_remote(
    candidates: &[Candidate],
    column: u16,
    resolved: &std::collections::HashMap<String, PathBuf>,
) -> Option<PathLink> {
    candidates.iter().find_map(|cand| {
        let literal: String = cand.chars.iter().collect();
        let (path, line, col, used) = if let Some(path) = resolved.get(&literal) {
            (path.clone(), None, None, cand.chars.len())
        } else {
            let (raw, line, col, used) = split_location(&cand.chars);
            let found = resolved.get(&raw)?.clone();
            (found, line, col, used)
        };
        let end = cand.starts.get(used).copied().map_or(cand.end, |next| {
            let last = cand
                .starts
                .get(used.saturating_sub(1))
                .copied()
                .unwrap_or(next);
            next.max(last.saturating_add(1))
        });
        let start = *cand.starts.first()?;
        if !(start..end).contains(&column) {
            return None;
        }
        Some(PathLink {
            column: start,
            cols: end - start,
            path,
            line,
            col,
        })
    })
}

#[cfg(test)]
mod tests;
