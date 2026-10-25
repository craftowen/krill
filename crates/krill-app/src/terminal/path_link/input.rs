//! 파일 링크의 제한된 화면 스냅샷. 파일 확인은 호출자가 백그라운드에서 수행한다.

use krill_proto::screen::Screen;

use super::{
    Cache, Cell, Flags, Path, PathLink, bounded_candidates, collect_remote_queries,
    is_bidi_control, lookup_remote,
};

const MAX_ROWS: usize = 32;
const MAX_CELLS: usize = 16_384;
const MAX_GROUPS: usize = 16;
const MAX_CANDIDATES: usize = 64;
const MAX_FILE_CHECKS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    line: u16,
    cells: Vec<Cell>,
    // 파일 후보에서는 OSC 8 구간을 제외하므로 URI 자체를 복제할 필요는 없다.
    links: Vec<(u16, u16)>,
}

/// 호버·클릭·메뉴가 공유하는 화면 좌표가 있는 파일 후보 입력.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    cols: u16,
    line: u16,
    column: u16,
    rows: Vec<Row>,
    soft_first: usize,
    soft_last: usize,
}

fn wraps(screen: &Screen, line: usize) -> bool {
    screen
        .rows
        .get(line)
        .and_then(|row| row.get(usize::from(screen.cols).checked_sub(1)?))
        .is_some_and(|c| c.flags.contains(Flags::WRAPLINE))
}

fn bounds(row: &[Cell]) -> Option<(usize, usize)> {
    let visible =
        |c: &Cell| c.ch != ' ' || !c.zerowidth.is_empty() || c.flags.contains(Flags::SPACER);
    Some((
        row.iter().position(visible)?,
        row.iter().rposition(visible)? + 1,
    ))
}

fn hard_join(screen: &Screen, line: usize) -> bool {
    let Some(a) = screen.rows.get(line) else {
        return false;
    };
    let Some(b) = screen.rows.get(line + 1) else {
        return false;
    };
    let (Some((ai, end)), Some((bi, _))) = (bounds(a), bounds(b)) else {
        return false;
    };
    if ai != bi
        || end + 3 < usize::from(screen.cols)
        || screen.links.get(line).is_some_and(|l| !l.is_empty())
        || screen.links.get(line + 1).is_some_and(|l| !l.is_empty())
    {
        return false;
    }
    let Some(style) = a.get(ai) else {
        return false;
    };
    a.iter().chain(b).filter(|c| c.ch != ' ').all(|c| {
        !c.flags.contains(Flags::HIDDEN)
            && !c.flags.contains(Flags::LINK)
            && !c.ch.is_control()
            && !is_bidi_control(c.ch)
            && c.zerowidth
                .iter()
                .all(|c| !c.is_control() && !is_bidi_control(*c))
            && c.fg == style.fg
            && c.bg == style.bg
    })
}

struct Flat {
    cells: Vec<Cell>,
    points: Vec<Option<(u16, u16)>>,
    hit: u16,
}

impl Input {
    /// 디스크를 읽지 않고 관련 행만 모은다. 필수 자동 줄바꿈이 불완전하면 실패한다.
    pub fn at(screen: &Screen, line: u16, column: u16) -> Option<Self> {
        if line >= screen.lines || column >= screen.cols || screen.link_at(line, column).is_some() {
            return None;
        }
        let hit = usize::from(line);
        let mut first = hit;
        let mut last = hit;
        while first > 0 && wraps(screen, first - 1) {
            first -= 1;
            if last - first >= MAX_ROWS {
                return None;
            }
        }
        while wraps(screen, last) {
            last += 1;
            screen.rows.get(last)?;
            if last - first >= MAX_ROWS {
                return None;
            }
        }
        let soft_first = first;
        let soft_last = last;
        while first > 0 && (wraps(screen, first - 1) || hard_join(screen, first - 1)) {
            first -= 1;
            if last - first >= MAX_ROWS {
                return None;
            }
        }
        while wraps(screen, last) || hard_join(screen, last) {
            last += 1;
            screen.rows.get(last)?;
            if last - first >= MAX_ROWS {
                return None;
            }
        }
        if last - first >= MAX_ROWS
            || (last - first + 1).checked_mul(usize::from(screen.cols))? > MAX_CELLS
        {
            return None;
        }
        let rows = (first..=last)
            .map(|index| {
                Some(Row {
                    line: u16::try_from(index).ok()?,
                    cells: screen.rows.get(index)?.clone(),
                    links: screen
                        .links
                        .get(index)
                        .into_iter()
                        .flatten()
                        .map(|link| (link.column, link.cols))
                        .collect(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            cols: screen.cols,
            line,
            column,
            rows,
            soft_first: soft_first - first,
            soft_last: soft_last - first,
        })
    }

    /// 현재 입력과 비교해 이웃 행·줄바꿈·링크 메타데이터 변경도 확인한다.
    pub fn matches(&self, screen: &Screen) -> bool {
        Self::at(screen, self.line, self.column).as_ref() == Some(self)
    }

    fn wrapped(&self, index: usize) -> bool {
        self.rows
            .get(index)
            .and_then(|r| r.cells.get(usize::from(self.cols).checked_sub(1)?))
            .is_some_and(|c| c.flags.contains(Flags::WRAPLINE))
    }

    fn flat(&self, first: usize, last: usize, spaces: bool) -> Option<Flat> {
        let mut cells = Vec::new();
        let mut points = Vec::new();
        let mut hit = None;
        for index in first..=last {
            let row = self.rows.get(index)?;
            let (indent, trimmed) = bounds(&row.cells).unwrap_or((0, 0));
            let hard_before = index > first && !self.wrapped(index - 1);
            let hard_after = index < last && !self.wrapped(index);
            if hard_before && spaces {
                if cells.len() >= MAX_CELLS {
                    return None;
                }
                cells.push(Cell::default());
                points.push(None);
            }
            let start = if hard_before { indent } else { 0 };
            let end = if hard_after { trimmed } else { row.cells.len() };
            for column in start..end {
                if cells.len() >= MAX_CELLS {
                    return None;
                }
                let mut cell = row.cells.get(column)?.clone();
                // wide 문자를 다음 행으로 넘기는 끝의 빈 자리는 텍스트·클릭 영역에서 제외한다.
                if cell.flags.contains(Flags::SPACER)
                    && column
                        .checked_sub(1)
                        .and_then(|c| row.cells.get(c))
                        .is_none_or(|c| !c.flags.contains(Flags::WIDE))
                {
                    continue;
                }
                let column = u16::try_from(column).ok()?;
                if cell.flags.contains(Flags::LINK)
                    || cell.flags.contains(Flags::HIDDEN)
                    || row
                        .links
                        .iter()
                        .any(|&(start, cols)| (start..start.saturating_add(cols)).contains(&column))
                {
                    // 공백 파일명·따옴표 후보도 이 구간을 가로질러 합칠 수 없어야 한다.
                    cell.ch = '\0';
                    cell.zerowidth.clear();
                    cell.flags = Flags::default();
                }
                if (row.line, column) == (self.line, self.column) {
                    hit = Some(u16::try_from(cells.len()).ok()?);
                }
                cells.push(cell);
                points.push(Some((row.line, column)));
            }
        }
        Some(Flat {
            cells,
            points,
            hit: hit?,
        })
    }

    fn project(&self, flat: &Flat, mut link: PathLink) -> Option<PathLink> {
        let end = usize::from(link.column) + usize::from(link.cols);
        let points = flat.points.get(usize::from(link.column)..end)?;
        let mut columns = points
            .iter()
            .flatten()
            .filter(|p| p.0 == self.line)
            .map(|p| p.1);
        let first = columns.next()?;
        let last = columns.next_back().unwrap_or(first);
        if !(first..=last).contains(&self.column) {
            return None;
        }
        link.column = first;
        link.cols = last - first + 1;
        Some(link)
    }

    fn crossing(&self, flat: &Flat, first: usize, last: usize) -> Option<(u16, u16)> {
        let first_line = self.rows.get(first)?.line;
        let last_line = self.rows.get(last)?.line;
        let first_end = flat
            .points
            .iter()
            .rposition(|p| p.is_some_and(|p| p.0 == first_line))?
            + 1;
        let last_start = flat
            .points
            .iter()
            .position(|p| p.is_some_and(|p| p.0 == last_line))?;
        Some((
            u16::try_from(first_end).ok()?,
            u16::try_from(last_start).ok()?,
        ))
    }

    /// 전체 경로를 해석하고 포인터 행의 범위로 투영한다. 백그라운드에서 호출한다.
    pub fn resolve(&self, cache: &mut Cache, cwd: Option<&Path>) -> Option<PathLink> {
        let mut groups = Vec::new();
        for first in 0..=self.soft_first {
            if first > 0 && self.wrapped(first - 1) {
                continue;
            }
            for last in self.soft_last..self.rows.len() {
                if self.wrapped(last) {
                    continue;
                }
                groups.push((first, last));
            }
        }
        groups.sort_by_key(|&(a, b)| std::cmp::Reverse(b - a));
        groups.retain(|g| *g != (self.soft_first, self.soft_last));
        groups.truncate(MAX_GROUPS - 1);
        groups.push((self.soft_first, self.soft_last));
        let variants: usize = groups
            .iter()
            .map(|&(a, b)| {
                if (a..b).any(|i| !self.wrapped(i)) {
                    2
                } else {
                    1
                }
            })
            .sum();
        let cap = MAX_CANDIDATES / variants;
        let mut budget = MAX_FILE_CHECKS;
        let mut best: Option<(usize, PathLink)> = None;
        for (first, last) in groups {
            let span = last - first;
            if best.as_ref().is_some_and(|(old_span, _)| *old_span > span) {
                break;
            }
            let hard = (first..last).any(|i| !self.wrapped(i));
            let mut found = None;
            for spaces in [false, true].into_iter().take(if hard { 2 } else { 1 }) {
                let Some(flat) = self.flat(first, last, spaces) else {
                    continue;
                };
                let crossing = if hard {
                    self.crossing(&flat, first, last)
                } else {
                    None
                };
                let link = cache
                    .bounded(&flat.cells, flat.hit, cwd, cap, &mut budget, crossing)
                    .filter(|link| {
                        crossing.is_none_or(|(first_end, last_start)| {
                            link.column < first_end
                                && link.column.saturating_add(link.cols) > last_start
                        })
                    })
                    .and_then(|link| self.project(&flat, link));
                if let Some(link) = link {
                    if found.as_ref().is_some_and(|old: &PathLink| {
                        old.path != link.path || old.line != link.line || old.col != link.col
                    }) {
                        return None;
                    }
                    found = Some(link);
                }
            }
            if let Some(link) = found {
                if best.as_ref().is_some_and(|(_, old)| {
                    old.path != link.path || old.line != link.line || old.col != link.col
                }) {
                    return None;
                }
                best = Some((span, link));
            }
        }
        best.map(|(_, link)| link)
    }

    /// 원격 호스트에서 후보 경로들을 단일 배치 RPC로 검사하고 포인터 행의 범위로 투영한다.
    pub fn resolve_remote(
        &self,
        target: &krill_hosts::HostTarget,
        cwd: Option<&Path>,
    ) -> Option<PathLink> {
        let mut groups = Vec::new();
        for first in 0..=self.soft_first {
            if first > 0 && self.wrapped(first - 1) {
                continue;
            }
            for last in self.soft_last..self.rows.len() {
                if self.wrapped(last) {
                    continue;
                }
                groups.push((first, last));
            }
        }
        groups.sort_by_key(|&(a, b)| std::cmp::Reverse(b - a));
        groups.retain(|g| *g != (self.soft_first, self.soft_last));
        groups.truncate(MAX_GROUPS - 1);
        groups.push((self.soft_first, self.soft_last));
        let variants: usize = groups
            .iter()
            .map(|&(a, b)| {
                if (a..b).any(|i| !self.wrapped(i)) {
                    2
                } else {
                    1
                }
            })
            .sum();
        let cap = (MAX_CANDIDATES / variants.max(1)).max(1);

        let mut query_paths = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // 소프트 줄바꿈 그룹(`soft_first..=soft_last`)의 후보를 먼저 담아 64개 한도 안에서 최우선 검사한다.
        if let Some(flat) = self.flat(self.soft_first, self.soft_last, false) {
            let cands = bounded_candidates(&flat.cells, flat.hit, MAX_CANDIDATES, None);
            collect_remote_queries(
                &cands,
                &mut query_paths,
                &mut seen,
                krill_proto::remote_fs::MAX_PROBE_PATHS,
            );
        }
        for &(first, last) in &groups {
            if (first, last) == (self.soft_first, self.soft_last) {
                continue;
            }
            let hard = (first..last).any(|i| !self.wrapped(i));
            for spaces in [false, true].into_iter().take(if hard { 2 } else { 1 }) {
                let Some(flat) = self.flat(first, last, spaces) else {
                    continue;
                };
                let crossing = if hard {
                    self.crossing(&flat, first, last)
                } else {
                    None
                };
                let cands = bounded_candidates(&flat.cells, flat.hit, cap, crossing);
                collect_remote_queries(
                    &cands,
                    &mut query_paths,
                    &mut seen,
                    krill_proto::remote_fs::MAX_PROBE_PATHS,
                );
            }
        }
        if query_paths.is_empty() {
            return None;
        }
        let probed = crate::remote_fs::probe_paths(target, cwd, &query_paths);
        let mut resolved = std::collections::HashMap::new();
        for (raw, found) in query_paths.into_iter().zip(probed) {
            if let (Some(raw_str), Some(found_path)) = (raw.to_str(), found) {
                resolved.insert(raw_str.to_owned(), found_path);
            }
        }
        if resolved.is_empty() {
            return None;
        }

        let mut best: Option<(usize, PathLink)> = None;
        for (first, last) in groups {
            let span = last - first;
            if best.as_ref().is_some_and(|(old_span, _)| *old_span > span) {
                break;
            }
            let hard = (first..last).any(|i| !self.wrapped(i));
            let mut found = None;
            for spaces in [false, true].into_iter().take(if hard { 2 } else { 1 }) {
                let Some(flat) = self.flat(first, last, spaces) else {
                    continue;
                };
                let crossing = if hard {
                    self.crossing(&flat, first, last)
                } else {
                    None
                };
                let cands = bounded_candidates(&flat.cells, flat.hit, cap, crossing);
                let link = lookup_remote(&cands, flat.hit, &resolved)
                    .filter(|link| {
                        crossing.is_none_or(|(first_end, last_start)| {
                            link.column < first_end
                                && link.column.saturating_add(link.cols) > last_start
                        })
                    })
                    .and_then(|link| self.project(&flat, link));
                if let Some(link) = link {
                    if found.as_ref().is_some_and(|old: &PathLink| {
                        old.path != link.path || old.line != link.line || old.col != link.col
                    }) {
                        return None;
                    }
                    found = Some(link);
                }
            }
            if let Some(link) = found {
                if best.as_ref().is_some_and(|(_, old)| {
                    old.path != link.path || old.line != link.line || old.col != link.col
                }) {
                    return None;
                }
                best = Some((span, link));
            }
        }
        best.map(|(_, link)| link)
    }
}

#[cfg(test)]
mod tests;
