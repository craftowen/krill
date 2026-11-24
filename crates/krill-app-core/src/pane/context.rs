//! MK-092: 스크롤 위치와 독립적으로 최근 출력 행을 가져온다. 기존 HISTORY와 콜드 중복 제거를 재사용한다.

use std::io::{self, Write};

use krill_term::select::{Point, Selection};

use super::{Cell, History, HistoryRows, Pane, Phase, Screen};

/// 앱에서 한 번에 복사할 최대 행 수. 히스토리는 작업이 끝나면 즉시 버린다.
const MAX_LINES: u16 = crate::commands::MAX_CONTEXT_LINES;

#[derive(Debug)]
pub(super) struct Copy {
    screen: Screen,
    lines: usize,
    history: History,
}

impl<W: Write> Pane<W> {
    /// 최근 `lines`행을 복사한다. 즉시 완성되면 문자열, 아니면 `HISTORY_ROWS`를 받을 때 `Events.clipboard`로 전달한다.
    /// 화면 아래 빈 행은 제외하고 자동 줄바꿈·한글·결합 문자는 기존 선택 복사와 같은 규칙을 따른다.
    /// 재접속·칸 닫기는 진행 중 복사를 취소한다. 스크롤 위치·선택·검색은 바꾸지 않는다.
    pub fn copy_context(&mut self, lines: u16) -> io::Result<Option<String>> {
        let mut screen = self.screen().clone();
        let used = screen
            .rows
            .iter()
            .rposition(|row| row.iter().any(|c| !c.is_blank()))
            .map_or(0, |i| i + 1);
        screen.rows.truncate(used);
        let lines = usize::from(lines.clamp(1, MAX_LINES));
        let mut history = if self.history.fixed {
            self.history.clone()
        } else {
            History::default()
        };
        history.pending = None;
        // 화면도 최신 행으로 넣어 둔다. 핫 기록이 비어 있어도 최신 콜드 페이지와 화면의 겹침을 제거한다.
        let mut rows: Vec<_> = screen.rows.iter().rev().cloned().collect();
        rows.append(&mut history.rows);
        history.rows = rows;
        history.hot = history.rows.len();
        history.offset = lines;
        self.context = Some(Copy {
            screen,
            lines,
            history,
        });
        self.advance_context()
    }

    pub(super) fn context_rows(&mut self, rows: &HistoryRows) -> bool {
        let Some(copy) = &mut self.context else {
            return false;
        };
        if copy.history.pending != Some(rows.req) {
            return false;
        }
        copy.history.rows(rows.clone());
        true
    }

    pub(super) fn advance_context(&mut self) -> io::Result<Option<String>> {
        let Some(copy) = &mut self.context else {
            return Ok(None);
        };
        let need = copy.lines;
        if copy.history.rows.len() >= need || copy.history.phase == Phase::Done {
            let Some(copy) = self.context.take() else {
                return Ok(None);
            };
            let rows: Vec<Vec<Cell>> = copy.history.rows.into_iter().take(need).rev().collect();
            let screen = Screen {
                rows,
                lines: u16::try_from(copy.lines).unwrap_or(MAX_LINES),
                ..copy.screen
            };
            let last = u16::try_from(screen.rows.len())
                .unwrap_or(MAX_LINES)
                .saturating_sub(1);
            let text = Selection {
                anchor: Point { line: 0, column: 0 },
                head: Point {
                    line: last,
                    column: screen.cols.saturating_sub(1),
                },
                block: false,
            }
            .text(&screen);
            return Ok(Some(text));
        }
        if self.out.is_none() {
            self.context = None;
            return Err(io::ErrorKind::NotConnected.into());
        }
        // 스크롤 요청과 같은 단조 증가 번호를 써서 늦은 답이 다른 작업으로 들어가지 않게 한다.
        copy.history.req = self.history.req;
        let request = copy.history.next_request(0);
        self.history.req = copy.history.req;
        if let Some(mut request) = request {
            if matches!(request.source, super::Source::Hot) {
                request.count = u16::try_from(need.saturating_sub(copy.history.rows.len()))
                    .unwrap_or(MAX_LINES)
                    .min(MAX_LINES);
            }
            if self.send(&request).is_err() {
                self.unlink();
                return Err(io::ErrorKind::BrokenPipe.into());
            }
        }
        Ok(None)
    }
}
