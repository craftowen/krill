//! 터미널 검색(MK-059). 인덱스/전체 결과 목록 없이 콜드 페이지를 하나씩 읽고 결과 행만 보관한다.
//!
//! 잠정 결정 의존: MK-024 세그먼트·콜드 재생(`Store::history`/`History::page`). 거부 시 이 모듈의
//! 콜드 순회만 되돌리면 핫+화면 검색과 와이어 포맷은 유지된다. 페이지의 근사/Gap 한계는 응답에 드러낸다.

use krill_proto::screen::{Cell, Flags, Seq};
use krill_proto::search::{Direction, Hit, SearchRequest, SearchResult, Snapshot, Source};

use crate::attach::Tap;

use crate::store::replay_gate as replay;

/// 요청 하나. 디스크 IO/재생 중에는 라이브 Term 잠금을 잡지 않는다.
pub fn search(tap: &Tap, req: &SearchRequest) -> SearchResult {
    search_with_options(
        tap,
        req,
        krill_proto::search::Options {
            case_sensitive: true,
            regex: false,
        },
    )
}

pub fn search_with_options(
    tap: &Tap,
    req: &SearchRequest,
    options: krill_proto::search::Options,
) -> SearchResult {
    search_cancellable(tap, req, options, &|| false)
}

pub(crate) fn search_cancellable(
    tap: &Tap,
    req: &SearchRequest,
    options: krill_proto::search::Options,
    cancelled: &dyn Fn() -> bool,
) -> SearchResult {
    search_admitted(tap, req, options, cancelled, None)
}

fn search_admitted(
    tap: &Tap,
    req: &SearchRequest,
    options: krill_proto::search::Options,
    cancelled: &dyn Fn() -> bool,
    admitted: Option<replay::Permit<'static>>,
) -> SearchResult {
    let (screen, revision, seq) = tap.capture();
    let mut reply = SearchResult {
        req: req.req,
        snapshot: Snapshot {
            revision,
            seq,
            ..Snapshot::default()
        },
        total: 0,
        hit: None,
        wrapped: false,
        stale: false,
        incomplete: false,
        approximate: false,
    };
    if !req.valid() {
        return reply;
    }
    let Ok(pattern) = options.compile(&req.query) else {
        return reply;
    };
    // guarded()가 live.seq를 먼저 올린 뒤 recorder.append()와 publish()를 부르므로 검색 시점 seq가 디스크에 기록될 때까지 기다린다.
    tap.wait(|p| p.end >= seq.0 || p.closed || cancelled());
    let target = req.after.and_then(|cursor| match req.direction {
        Direction::Next => cursor.index.checked_add(1),
        Direction::Previous => cursor.index.checked_sub(1),
    });
    let mut scan = Scan {
        pattern: &pattern,
        target,
        forward: req.direction == Direction::Next,
        total: 0,
        first: None,
        last: None,
        selected: None,
    };
    let history = tap.store().map(|store| store.history(tap.id()));
    let mut used_cold = false;
    let mut disk_end = 0;
    match history {
        Some(Ok(history)) if !history.is_empty() => {
            // Limit the paired cold engines only; input, live capture and hot/UI
            // reads never acquire this permit. Cancellation remains generation-based.
            if admitted.is_none() {
                let Some(permit) = replay::acquire(cancelled) else {
                    reply.incomplete = true;
                    return reply;
                };
                // A queued query may have crossed clear/privacy/output changes. Drop
                // its old grid/manifest and take one fresh snapshot while admitted.
                drop(scan);
                drop(screen);
                drop(history);
                return search_admitted(tap, req, options, cancelled, Some(permit));
            }
            reply.approximate = true;
            let mut pages = 0u32;
            let mut page_rows = 0usize;
            history.visit_until(seq.0, cancelled, |page, event| {
                let source = Source::Cold {
                    page: u32::try_from(page).unwrap_or(u32::MAX),
                };
                match event {
                    crate::store::HistoryEvent::Rows(rows) => {
                        scan.rows_at(source, &rows, page_rows);
                        page_rows = page_rows.saturating_add(rows.len());
                    }
                    crate::store::HistoryEvent::End(result) => {
                        pages = pages.saturating_add(1);
                        scan.finish_page(source, page_rows);
                        page_rows = 0;
                        match result {
                            Ok(data) => {
                                if !used_cold {
                                    reply.snapshot.oldest = Seq(data.start_seq);
                                    reply.incomplete |= data.start_seq != 0;
                                }
                                reply.incomplete |= !data.gaps.is_empty()
                                    || (used_cold && data.start_seq != disk_end);
                                disk_end = disk_end.max(data.end_seq);
                                used_cold = true;
                            }
                            Err(_) => reply.incomplete = true,
                        }
                    }
                }
            });
            reply.snapshot.pages = pages;
        }
        _ => reply.incomplete = true,
    }
    // Release the paired-engine admission before hot/screen matching.
    drop(admitted);
    if cancelled() {
        reply.incomplete = true;
        return reply;
    }
    reply.incomplete |= used_cold && disk_end < seq.0;
    if !used_cold {
        let (_, hot) = tap.history(0, crate::HOT_LINES);
        scan.rows(Source::Hot, &hot);
    }
    scan.rows(Source::Screen, &screen.rows);
    reply.total = scan.total;
    let after = req
        .after
        .filter(|cursor| cursor_valid(cursor, &reply.snapshot, scan.total));
    reply.stale = req.after.is_some() && after.is_none();
    let forward = req.direction == Direction::Next;
    reply.wrapped = after.is_some() && scan.selected.is_none();
    reply.hit = if after.is_some() {
        scan.selected
            .or(if forward { scan.first } else { scan.last })
    } else if forward {
        scan.first
    } else {
        scan.last
    };
    reply
}

/// 검색 커서가 새 스냅샷에서도 유효한지 판정한다. 출력 추가·세그먼트 봉인으로
/// revision·seq·pages가 증가해도 oldest가 같으면 순번을 유지하고, oldest가
/// 달라지거나 pages가 줄거나 index가 범위를 벗어나면 무효로 본다.
fn cursor_valid(
    cursor: &krill_proto::search::Cursor,
    snapshot: &krill_proto::search::Snapshot,
    total: u64,
) -> bool {
    cursor.snapshot.oldest == snapshot.oldest
        && cursor.snapshot.pages <= snapshot.pages
        && cursor.index < total
}

struct Scan<'a> {
    pattern: &'a krill_proto::search::Pattern,
    target: Option<u64>,
    forward: bool,
    total: u64,
    first: Option<Hit>,
    last: Option<Hit>,
    selected: Option<Hit>,
}

impl Scan<'_> {
    fn rows(&mut self, source: Source, rows: &[Vec<Cell>]) {
        self.rows_at(source, rows, 0);
        self.finish_page(source, rows.len());
    }

    fn finish_page(&mut self, source: Source, rows: usize) {
        for hit in [&mut self.first, &mut self.last, &mut self.selected]
            .into_iter()
            .flatten()
        {
            if hit.source == source {
                hit.total_rows = u32::try_from(rows).unwrap_or(u32::MAX);
            }
        }
    }

    fn rows_at(&mut self, source: Source, rows: &[Vec<Cell>], offset: usize) {
        let total_rows = u32::try_from(rows.len()).unwrap_or(u32::MAX);
        for (row, cells) in rows.iter().enumerate() {
            let (text, spans) = text_spans(cells);
            // 겹치지 않는 리터럴 일치. 바이트 위치를 셀 대응표로 하이라이트 범위로 바꾼다.
            let mut matches = self
                .pattern
                .find_iter(&text)
                .filter(|m| !m.is_empty())
                .peekable();
            while let Some(matched) = matches.next() {
                let byte = matched.start();
                let end = matched.end();
                let mut overlaps = spans
                    .iter()
                    .filter(|span| span.end > byte && span.start < end);
                let Some(first) = overlaps.next() else {
                    continue;
                };
                let last = overlaps.next_back().unwrap_or(first);
                let keep_first = self.forward && self.first.is_none();
                let keep_last = !self.forward && matches.peek().is_none();
                if !keep_first && self.target != Some(self.total) && !keep_last {
                    self.total = self.total.saturating_add(1);
                    continue;
                }
                let hit = Hit {
                    index: self.total,
                    source,
                    row: u32::try_from(offset.saturating_add(row)).unwrap_or(u32::MAX),
                    total_rows,
                    column: first.column,
                    cols: last.end_column.saturating_sub(first.column),
                    cells: cells.clone(),
                };
                if keep_first {
                    self.first = Some(hit.clone());
                }
                if self.target == Some(self.total) {
                    self.selected = Some(hit.clone());
                }
                if keep_last {
                    self.last = Some(hit);
                }
                self.total = self.total.saturating_add(1);
            }
        }
    }
}

struct Span {
    start: usize,
    end: usize,
    column: u16,
    end_column: u16,
}

/// 결합 문자는 기본 셀에, SPACER는 앞의 넓은 문자에 포함한다. 제어 시퀀스는 엔진에서 이미 제거됐다.
fn text_spans(cells: &[Cell]) -> (String, Vec<Span>) {
    let mut text = String::new();
    let mut spans = Vec::new();
    for (column, cell) in cells.iter().enumerate() {
        if cell.flags.contains(Flags::SPACER) {
            continue;
        }
        let Ok(column) = u16::try_from(column) else {
            break;
        };
        let start = text.len();
        text.push(cell.ch);
        text.extend(&cell.zerowidth);
        spans.push(Span {
            start,
            end: text.len(),
            column,
            end_column: column.saturating_add(if cell.flags.contains(Flags::WIDE) {
                2
            } else {
                1
            }),
        });
    }
    (text, spans)
}

#[cfg(test)]
mod tests;
