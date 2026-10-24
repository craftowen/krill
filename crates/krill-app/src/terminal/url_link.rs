//! 여러 화면 행에 걸친 평문 URL. 자동 줄바꿈은 WRAPLINE으로, TUI가 직접 나눈
//! 독립 URL은 같은 들여쓰기·오른쪽 끝·문자 스타일로 이어 붙인다.
//! UX 참고: Orca terminal link provider (Copyright (c) 2026 Lovecast Inc., MIT).
//! wrapped/hard-wrapped URL 및 전체 주소 복사 동작만 참고했으며 코드 차용 없음.

use krill_proto::screen::{Cell, Flags, Screen};

use super::{Highlight, path_link};

// 포인터 이동 때 검사할 행 수·셀 수를 제한한다.
const MAX_CELLS: usize = 16_384;
const MAX_ROWS: usize = 128;

struct Part<'a> {
    line: usize,
    column: usize,
    cells: &'a [Cell],
}

fn parse(parts: &[Part<'_>], line: u16, column: u16) -> Option<(Highlight, String)> {
    let mut cells = Vec::new();
    let mut hit = None;
    let mut row_offset = 0;
    for part in parts {
        if cells.len().checked_add(part.cells.len())? > MAX_CELLS {
            return None;
        }
        if part.line == usize::from(line) {
            let local = usize::from(column).checked_sub(part.column)?;
            if local >= part.cells.len() {
                return None;
            }
            row_offset = cells.len();
            hit = Some(u16::try_from(row_offset + local).ok()?);
        }
        cells.extend_from_slice(part.cells);
    }
    let link = path_link::url_at(&cells, hit?)?;
    let part = parts.iter().find(|p| p.line == usize::from(line))?;
    let start = usize::from(link.column).max(row_offset);
    let end =
        (usize::from(link.column) + usize::from(link.cols)).min(row_offset + part.cells.len());
    Some((
        Highlight {
            line,
            column: u16::try_from(part.column + start.checked_sub(row_offset)?).ok()?,
            cols: u16::try_from(end.checked_sub(start)?).ok()?,
        },
        link.uri,
    ))
}

fn wraps(screen: &Screen, line: usize) -> bool {
    screen.rows.get(line).is_some_and(|row| {
        row.get(usize::from(screen.cols).saturating_sub(1))
            .is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE))
    })
}

fn soft_parts(screen: &Screen, line: usize) -> Option<Vec<Part<'_>>> {
    let mut first = line;
    let mut last = line;
    while first > 0 && wraps(screen, first - 1) {
        first -= 1;
        if last - first >= MAX_ROWS {
            return None;
        }
    }
    while wraps(screen, last) {
        last += 1;
        if last - first >= MAX_ROWS {
            return None;
        }
        screen.rows.get(last)?;
    }
    (first..=last)
        .map(|line| {
            Some(Part {
                line,
                column: 0,
                cells: screen.rows.get(line)?,
            })
        })
        .collect()
}

/// 들여쓰기 뒤 공백 없는 URL 문자만 남긴다. 일반 문장·숨긴 글자·OSC 8은 합치지 않는다.
fn standalone(row: &[Cell]) -> Option<(usize, &[Cell])> {
    let start = row.iter().position(|c| c.ch != ' ')?;
    let end = row.iter().rposition(|c| c.ch != ' ')? + 1;
    let cells = row.get(start..end)?;
    cells
        .iter()
        .all(|c| {
            c.ch.is_ascii()
                && !c.ch.is_whitespace()
                && !c.ch.is_control()
                && !matches!(
                    c.ch,
                    '"' | '\'' | '`' | '<' | '>' | '{' | '}' | '|' | '\\' | '^'
                )
                && c.zerowidth.is_empty()
                && !c.flags.contains(Flags::HIDDEN)
                && !c.flags.contains(Flags::SPACER)
                && !c.flags.contains(Flags::LINK)
        })
        .then_some((start, cells))
}

fn starts_url(cells: &[Cell]) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        cells
            .iter()
            .take(scheme.len())
            .map(|c| c.ch)
            .eq(scheme.chars())
    })
}

fn hard_parts(screen: &Screen, first: usize, hit: usize) -> Option<Vec<Part<'_>>> {
    let (indent, head) = standalone(screen.rows.get(first)?)?;
    if !starts_url(head) {
        return None;
    }
    let mut parts = vec![Part {
        line: first,
        column: indent,
        cells: head,
    }];
    let mut total = head.len();
    loop {
        let previous = parts.last()?;
        // Claude의 출력처럼 좌우 여백을 남기고 직접 개행한 행도 허용한다.
        if previous.column + previous.cells.len() + 3 < usize::from(screen.cols) {
            break;
        }
        let line = previous.line + 1;
        let Some((column, cells)) = screen.rows.get(line).and_then(|row| standalone(row)) else {
            break;
        };
        if column != indent || starts_url(cells) {
            break;
        }
        let style = head.first()?;
        if cells.iter().any(|c| c.fg != style.fg || c.bg != style.bg) {
            break;
        }
        total = total.checked_add(cells.len())?;
        if total > MAX_CELLS || parts.len() >= MAX_ROWS {
            return None;
        }
        parts.push(Part {
            line,
            column,
            cells,
        });
    }
    (parts.len() > 1 && parts.last()?.line >= hit).then_some(parts)
}

pub(super) fn at(screen: &Screen, line: u16, column: u16) -> Option<(Highlight, String)> {
    let hit = usize::from(line);
    let soft = soft_parts(screen, hit)?;
    if soft.len() > 1 {
        return parse(&soft, line, column);
    }
    for first in (hit.saturating_sub(MAX_ROWS - 1)..=hit).rev() {
        if let Some(parts) = hard_parts(screen, first, hit)
            && let Some(link) = parse(&parts, line, column)
        {
            return Some(link);
        }
        if first < hit
            && screen
                .rows
                .get(first)
                .is_none_or(|row| standalone(row).is_none())
        {
            break;
        }
    }
    parse(&soft, line, column)
}

#[cfg(test)]
mod tests;
