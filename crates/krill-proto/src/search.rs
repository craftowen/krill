//! 데몬 측 터미널 검색(MK-059). `FEAT_SCREEN` + `FEAT_SEARCH`를 협상한 화면 Attach 연결에서만 쓴다.
//!
//! 대소문자를 구분하는 리터럴을 화면 행 단위로 검색한다(정규식·자동 줄바꿈 경계 결합 없음).
//! 콜드 페이지(오래된 순) → 현재 화면 순서, 디스크가 없으면 핫 스크롤백 → 현재 화면 순서다.
//! 콜드에 이미 있는 핫 행을 중복 검색하지 않는다. 결과 한 건의 셀과 하이라이트 범위만 보내므로
//! 클라이언트는 전체 이력을 보관할 필요가 없다. Cold 행은 `HistoryRequest`로 주변을 가져올 수 있다:
//! `skip = total_rows - row - 1`. Hot도 같은 식이다. Screen의 row는 보이는 화면 행이다.
//!
//! after는 같은 query로 받은 index와 snapshot이다. oldest가 같고 pages가 줄지 않으면 출력 추가·
//! 세그먼트 봉인으로 revision·seq·pages가 증가해도 index를 유지한다. oldest가 달라지거나 pages가
//! 줄거나 index가 범위를 벗어나면 탐색을 처음부터 시작하고 stale=true로 알린다. 검색은 캡처한 seq까지만 재생해 출력 중에도 결과를 반환한다.
//! 다음은 끝에서 처음으로, 이전은 처음에서 끝으로 순환하며 wrapped로 알린다.
//! 빈 query와 `MAX_QUERY` 초과는 거부한다. 요청·응답은 고정 v1 DTO이며 필드 추가는 새 kind/feature로 한다.

use serde::{Deserialize, Serialize};

use crate::ipc::{self, Error};
use crate::screen::{Cell, Msg, Revision, Seq};

pub const FEAT_SEARCH: u64 = 1 << 8;
/// 검색어의 최대 UTF-8 바이트 수(설계 상한).
pub const MAX_QUERY: usize = 1_024;

pub mod kind {
    pub const SEARCH: u8 = 0x15;
    pub const SEARCH_RESULT: u8 = 0x2a;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Next,
    Previous,
}

/// 결과 순번을 해석할 수 있는 이력 판. 재접속 때 after는 버린다(세션·데몬 세대는 연결에 귀속).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// 검색이 캡처한 화면 판. 출력 추가만으로 커서를 무효화하지 않는다.
    pub revision: Revision,
    /// 검색이 캡처한 원본 끝(콜드 재생 상한).
    pub seq: Seq,
    /// 가장 오래된 검색 페이지의 원본 시작. GC로 바뀌면 커서를 초기화한다.
    pub oldest: Seq,
    /// 검색한 페이지 수. 페이지 추가·손실로 바뀌면 커서를 초기화한다.
    pub pages: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub snapshot: Snapshot,
    /// 전체 검색 결과에서 오래된 순으로 매긴 0 기반 순번.
    pub index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub req: u32,
    pub query: String,
    pub direction: Direction,
    pub after: Option<Cursor>,
}

impl SearchRequest {
    pub fn valid(&self) -> bool {
        !self.query.is_empty() && self.query.len() <= MAX_QUERY
    }
}

impl Msg for SearchRequest {
    const KIND: u8 = kind::SEARCH;

    fn decode(kind: u8, body: &[u8]) -> Result<Self, Error> {
        let request: Self = ipc::decode(Self::KIND, kind, body)?;
        if !request.valid() {
            return Err(Error::TooLong(request.query.len()));
        }
        Ok(request)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Cold { page: u32 },
    Hot,
    Screen,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub index: u64,
    pub source: Source,
    /// 출처에서 위부터 0 기반 행 번호.
    pub row: u32,
    pub total_rows: u32,
    /// 터미널 셀 좌표(UTF-8 바이트나 문자 인덱스가 아님). 넓은 문자·결합 문자도 전체 칸을 칠한다.
    pub column: u16,
    pub cols: u16,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "서로 독립인 와이어 상태: 순환·오래됨·이력 손실·재생 근사"
)]
pub struct SearchResult {
    pub req: u32,
    pub snapshot: Snapshot,
    pub total: u64,
    pub hit: Option<Hit>,
    pub wrapped: bool,
    pub stale: bool,
    /// GC·Gap·읽기 실패·기록 없음으로 전체 과거를 검색하지 못했다.
    pub incomplete: bool,
    /// MK-024 콜드 재생의 세그먼트 시작 상태 근사를 썼다.
    pub approximate: bool,
}

impl Msg for SearchResult {
    const KIND: u8 = kind::SEARCH_RESULT;
}

#[cfg(test)]
mod tests;
