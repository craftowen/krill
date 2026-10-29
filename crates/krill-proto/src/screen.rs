//! 세션 화면·원본 출력 전송(MK-023, T2 #12·T3 #6). [`crate::ipc`] v1 프레임 위에서, 협상한 feature에
//! [`FEAT_SCREEN`]이 있을 때만 쓰는 kind다.
//!
//! # 두 번호: 원본 seq와 화면 revision
//!
//! - **원본 seq**([`Seq`]): 세션이 시작된 뒤 PTY에서 읽은 원본 출력의 바이트 오프셋. 세션마다 하나이고 모든
//!   클라이언트(앱·`krill attach`)와 디스크 세그먼트(MK-024 `start_seq`)가 같은 값을 쓴다. krilld가 재기동해도 세그먼트
//!   끝에서 이어진다. `[a, b)`는 원본 바이트 구간이다.
//! - **화면 revision**([`Revision`]): krilld가 세션 화면 상태에 매기는 판 번호. 엔진에 바이트를 넣거나, sync 마감이
//!   지나거나, 크기가 바뀔 때마다 오른다. krilld 세대([`ServerHello::instance_id`](crate::ipc::ServerHello)) 안에서만
//!   뜻이 있어 세대가 바뀌면 클라이언트는 스냅샷을 다시 받는다. 같은 seq에서도 화면이 바뀔 수 있어(sync 마감·리사이즈)
//!   seq로 화면을 셀 수 없다.
//! - [`Update`]는 둘을 함께 싣는다: "이 화면(revision)은 원본 `[0, seq)`를 엔진에 넣은 결과". DEC 2026 sync 버퍼 안의
//!   바이트는 seq에 들어가지만 아직 화면에 보이지 않을 수 있다.
//!
//! # 흐름
//!
//! 연결 하나 = 세션 하나의 스트림 하나. 앱은 보이는 세션마다 연결한다(숨긴 세션은 끊으면 앱에 남는 것이 없다).
//!
//! 1. Hello 협상 뒤 클라이언트가 [`Attach`]를 보낸다. 없는 세션이면 krilld가 [`Refused`]를 보내고 닫는다.
//! 2. krilld가 [`Attached`](지금 seq·revision)를 보낸다.
//! 3. 화면 스트림([`Stream::Screen`]): [`Update`]가 이어진다. 첫 Update는 키프레임, 즉 [`Op::Reset`]으로 시작하는
//!    보이는 화면 전체다(`known`이 같은 세대·같은 revision이면 생략). 그 뒤는 행 이동·행 교체 델타다.
//!    스크롤백은 보내지 않는다. 클라이언트가 [`HistoryRequest`]로 필요한 행만 받는다(지연 히스토리).
//! 4. 원본 스트림([`Stream::Raw`]): `from`부터 [`kind::OUTPUT`]이 이어진다(seq 이어받기). krilld 메모리 링에 없으면
//!    디스크 세그먼트에서 읽고, 거기에도 없으면(GC·토큰 버킷·크래시) [`GapMsg`]로 알리고 건너뛴다.
//! 5. 세션이 끝나면 남은 것을 다 보낸 뒤 [`Closed`].
//!
//! 화면 스트림 클라이언트는 언제든 [`kind::RESYNC`](body 없음, 스냅샷 다시 받기)와 [`HistoryRequest`]를 보낼 수 있다.
//! [`Screen::apply`]가 실패하면 RESYNC를 보낸다.
//!
//! # 화면 부가 정보 ([`FEAT_SCREEN_META`], OWE-490)
//!
//! 이 비트를 협상한 연결에만 krilld가 [`Op::Title`](창 제목, OSC 0·2)·[`Op::Bell`](BEL)·[`Op::CursorStyle`](DECSCUSR)·
//! [`Op::Links`](OSC 8 링크 URI)를 보낸다. 구 클라이언트는 이 variant를 디코드하지 못하므로 krilld가 걸러 낸다.
//! 줄바꿈 표시 [`Flags::WRAPLINE`]는 칸 비트라 feature 없이도 보낸다(모르는 비트는 무시).
//! 스냅샷에서는 기본값이 아닌 것만 보낸다(빈 제목·기본 커서 모양·링크 없는 행은 생략). 벨은 상태가 아니라 사건이라
//! 스냅샷에 넣지 않는다. OSC 4로 바꾼 팔레트는 krilld가 칸 색을 RGB로 풀어 보내므로 와이어에 따로 없다.
//!
//! # 느린 클라이언트
//!
//! krilld는 연결마다 "클라이언트가 가진 화면"과 지금 화면의 차이만 만든다(당김 방식). 연결별 큐는 프레임 하나이고,
//! 클라이언트가 늦으면 중간 화면이 합쳐질 뿐 쌓이지 않는다. 원본 스트림은 세션의 메모리 링이 큐이고, 링 밖으로
//! 밀려난 구간은 디스크에서, 디스크에도 없으면 [`GapMsg`](= Resync)다. PTY 리더는 어떤 클라이언트도 기다리지 않는다.
//!
//! # 상한(받는 쪽이 검사)
//!
//! - 화면 [`MAX_COLS`]×[`MAX_LINES`], 칸당 폭 0 문자 [`MAX_ZEROWIDTH`]개. 넘는 [`Op`]은 [`Screen::apply`]가 거부한다.
//!   krilld는 이 범위만 캡처한다.
//! - 메시지는 ipc [`MAX_FRAME`] 하나에 들어간다. 큰 화면은 Update 여러 개로 나눠 보내고(`done = false`), 마지막만
//!   `done = true`다([`encode_update`]).
//! - 압축은 하지 않는다(로컬 UDS, 크기 측정은 비공개 MK-023 스파이크 기록). 그래서 압축 해제 상한은 없다.
//!
//! # 호환
//!
//! [`crate::ipc`]의 규칙과 같다. 추가로 [`Op`]·[`Cell`]·[`Cursor`]처럼 목록 안에 들어가는 타입은 끝에 필드를 붙일 수도
//! 없다(뒤 원소가 밀린다). 새 정보는 새 `Op` variant나 새 kind로, feature 비트를 협상했을 때만 보낸다. [`Flags`] 비트
//! 값은 영구이고 받는 쪽은 모르는 비트를 무시한다. `screen/tests.rs`의 고정 바이트가 바뀌면 호환 위반이다.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::SessionId;
use crate::ipc::{self, Error, MAX_FRAME};
use crate::theme::Rgb;

/// 이 모듈의 kind를 쓸 수 있다는 feature 비트.
pub const FEAT_SCREEN: u64 = 1;
/// Attach-local frozen history ranges with explicit invalidation and bounded decoded caches.
pub const FEAT_HISTORY_VIEW: u64 = 1 << 24;
mod history_view;
pub use history_view::{HistoryIdentity, HistoryViewRequest, HistoryViewRows, HistoryViewStatus};
/// 화면 부가 정보 [`Op`](제목·벨·커서 모양·링크 URI)를 받을 수 있다는 feature 비트(모듈 문서).
pub const FEAT_SCREEN_META: u64 = 1 << 9;
/// [`Op::Title`] 최대 바이트. krilld는 넘는 제목을 글자 경계에서 자른다.
pub const MAX_TITLE: usize = 1_024;

/// 창 제목(OSC 0·2)에서 제어 문자(C0·DEL·C1)와 양방향(Bidi) 방향 전환 문자를 걸러 내고 [`MAX_TITLE`] 바이트 이하로
/// 글자 경계에서 자른다(MK-076).
pub fn sanitize_title(title: &str) -> String {
    sanitize_title_max(title, MAX_TITLE)
}

/// [`sanitize_title`]과 같되 바이트 상한을 `max`로 지정한다.
pub fn sanitize_title_max(title: &str, max: usize) -> String {
    let mut out = String::with_capacity(title.len().min(max));
    for c in title.chars() {
        if c.is_control() || is_bidi_control(c) {
            continue;
        }
        if out.len().saturating_add(c.len_utf8()) > max {
            break;
        }
        out.push(c);
    }
    out
}

/// 제목·링크 위장에 쓰이는 유니코드 양방향(Bidi) 제어 문자.
pub const fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}
/// [`Link::uri`] 최대 바이트. krilld는 넘는 링크를 보내지 않는다(칸의 [`Flags::LINK`]는 남는다).
pub const MAX_URI: usize = 2_048;
/// 전송하는 화면 최대 열 수.
pub const MAX_COLS: u16 = 2_048;
/// 전송하는 화면 최대 행 수.
pub const MAX_LINES: u16 = 1_024;
/// 칸 하나에 붙는 폭 0 문자 최대 개수. krilld는 넘는 것을 버린다.
pub const MAX_ZEROWIDTH: usize = 8;
/// [`HistoryRequest::count`] 상한.
pub const MAX_HISTORY_ROWS: u16 = 1_000;
/// [`kind::OUTPUT`] 한 프레임의 최대 원본 바이트.
pub const MAX_OUTPUT: usize = 64 * 1_024;

/// 이 모듈의 kind. 값은 영구다.
pub mod kind {
    /// 클라이언트 → krilld: [`super::Attach`].
    pub const ATTACH: u8 = 0x10;
    /// 클라이언트 → krilld: 화면 스냅샷 다시 받기(body 없음).
    pub const RESYNC: u8 = 0x11;
    /// 클라이언트 → krilld: [`super::HistoryRequest`].
    pub const HISTORY: u8 = 0x12;
    /// Negotiated frozen range request.
    pub const HISTORY_VIEW: u8 = 0x18;
    /// krilld → 클라이언트: [`super::Attached`].
    pub const ATTACHED: u8 = 0x20;
    /// krilld → 클라이언트: [`super::Update`].
    pub const UPDATE: u8 = 0x21;
    /// krilld → 클라이언트: 원본 바이트 `[u64 LE seq][바이트]`(postcard 아님, [`super::encode_output`]).
    pub const OUTPUT: u8 = 0x22;
    /// krilld → 클라이언트: [`super::GapMsg`].
    pub const GAP: u8 = 0x23;
    /// krilld → 클라이언트: [`super::Closed`].
    pub const CLOSED: u8 = 0x24;
    /// krilld → 클라이언트: [`super::Refused`].
    pub const REFUSED: u8 = 0x25;
    /// krilld → 클라이언트: [`super::HistoryRows`].
    pub const HISTORY_ROWS: u8 = 0x26;
    /// Negotiated frozen range reply.
    pub const HISTORY_VIEW_ROWS: u8 = 0x2d;
}

/// 원본 바이트 오프셋(모듈 문서 "두 번호").
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Seq(pub u64);

/// 화면 판 번호(모듈 문서 "두 번호").
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Revision(pub u64);

/// 칸 색. `Default`는 테마의 기본 전경/배경이다(어느 쪽인지는 쓰는 자리가 정한다).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Color {
    #[default]
    Default,
    /// 256색 팔레트 번호. 0~15는 테마, 16~255는 표준 6×6×6 큐브·회색 단계.
    Indexed(u8),
    Rgb(Rgb),
}

/// 칸 속성 비트. 값은 와이어 포맷이라 영구다(모르는 비트는 무시).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Flags(pub u16);

impl Flags {
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(1 << 1);
    pub const INVERSE: Self = Self(1 << 2);
    /// 2칸 글자의 첫 칸. 다음 칸은 `SPACER`다.
    pub const WIDE: Self = Self(1 << 3);
    /// 2칸 글자의 둘째 칸, 또는 줄 끝에서 다음 줄로 넘어간 2칸 글자의 자리. 아무것도 그리지 않는다.
    pub const SPACER: Self = Self(1 << 4);
    /// SGR 4(이중·곡선 밑줄도 지금은 한 줄로 그린다).
    pub const UNDERLINE: Self = Self(1 << 5);
    /// SGR 9.
    pub const STRIKEOUT: Self = Self(1 << 6);
    /// SGR 2. 글자색을 배경 쪽으로 1/3 섞는다.
    pub const DIM: Self = Self(1 << 7);
    /// SGR 8. 배경만 그린다.
    pub const HIDDEN: Self = Self(1 << 8);
    /// OSC 8 링크 칸. 밑줄을 긋는다. URI는 [`Screen::links`]([`FEAT_SCREEN_META`]).
    pub const LINK: Self = Self(1 << 9);
    /// 행의 마지막 칸에만: 이 행이 자동 줄바꿈으로 다음 행에 이어진다(복사할 때 줄을 잇는다).
    pub const WRAPLINE: Self = Self(1 << 10);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Flags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Flags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// 화면 한 칸.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cell {
    pub ch: char,
    /// 앞 글자에 붙는 폭 0 문자(결합 자모·ZWJ·이모지 변형 선택자).
    pub zerowidth: Vec<char>,
    pub fg: Color,
    pub bg: Color,
    /// 밑줄 색(SGR 58). `Default`면 글자색을 따른다.
    pub underline: Color,
    pub flags: Flags,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            zerowidth: Vec::new(),
            fg: Color::Default,
            bg: Color::Default,
            underline: Color::Default,
            flags: Flags::default(),
        }
    }
}

impl Cell {
    /// 아무것도 그리지 않는 기본 칸(행 끝에서 잘라 낸다).
    pub fn is_blank(&self) -> bool {
        self.ch == ' '
            && self.zerowidth.is_empty()
            && self.fg == Color::Default
            && self.bg == Color::Default
            && self.underline == Color::Default
            && self.flags.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cursor {
    pub line: u16,
    pub column: u16,
    pub visible: bool,
}

/// 커서 모양(DECSCUSR). variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    /// 세로 막대.
    Bar,
}

/// [`Op::CursorStyle`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blink: bool,
}

/// 한 행 안의 OSC 8 링크 구간: `column`부터 `cols`칸.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Link {
    pub column: u16,
    pub cols: u16,
    pub uri: String,
}

/// 보이는 화면. 행은 끝의 기본 칸을 잘라내도 된다(`cols`보다 짧을 수 있다). `rows.len() == lines`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Screen {
    pub cols: u16,
    pub lines: u16,
    pub rows: Vec<Vec<Cell>>,
    pub cursor: Cursor,
    /// 창 제목(OSC 0·2). 비어 있으면 프로그램이 정하지 않았다.
    pub title: String,
    /// 받은 벨 수(이 화면을 만든 쪽 기준으로 센다, 넘치면 0으로 돈다). 늘었으면 벨이 울린 것이다.
    pub bells: u64,
    pub cursor_style: CursorStyle,
    /// A program explicitly selected the cursor, including steady block.
    pub cursor_explicit: bool,
    /// 행마다 OSC 8 링크 구간(열 오름차순). 비어 있거나 `rows`보다 짧으면 링크 없음.
    pub links: Vec<Vec<Link>>,
}

/// 화면 연산. 받은 순서대로 적용한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    /// 화면을 `cols`×`lines` 빈 화면(커서 (0,0) 보임)으로. 스냅샷(키프레임)의 첫 연산이다.
    Reset {
        cols: u16,
        lines: u16,
    },
    /// 화면 전체 행 이동. `n > 0`이면 내용이 위로 n행(위 n행 버림, 아래 n행 빈 행), `n < 0`이면 아래로.
    Scroll(i16),
    /// 행 `line`을 통째로 바꾼다. 끝의 기본 칸은 잘려 있다.
    Line {
        line: u16,
        cells: Vec<Cell>,
    },
    Cursor(Cursor),
    /// 창 제목([`MAX_TITLE`] 이하). 빈 문자열은 제목 없음. 이하 [`FEAT_SCREEN_META`].
    Title(String),
    /// 벨이 울렸다.
    Bell,
    CursorStyle(CursorStyle),
    /// 행 `line`의 링크 구간을 통째로 바꾼다. [`Op::Line`]은 그 행의 링크를 지우므로 링크가 있는 행은 `Line` 뒤에 온다.
    Links {
        line: u16,
        links: Vec<Link>,
    },
}

impl Op {
    /// [`FEAT_SCREEN_META`]를 협상해야 보낼 수 있는 연산.
    pub fn is_meta(&self) -> bool {
        matches!(
            self,
            Self::Title(_) | Self::Bell | Self::CursorStyle(_) | Self::Links { .. }
        )
    }
}

/// 적용할 수 없는 연산(범위·상한 밖). 클라이언트는 RESYNC를 보낸다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyError(pub &'static str);

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad screen op: {}", self.0)
    }
}

impl std::error::Error for ApplyError {}

impl Screen {
    /// 빈 화면.
    pub fn blank(cols: u16, lines: u16) -> Self {
        Self {
            cols,
            lines,
            rows: vec![Vec::new(); usize::from(lines)],
            cursor: Cursor {
                visible: true,
                ..Cursor::default()
            },
            links: vec![Vec::new(); usize::from(lines)],
            ..Self::default()
        }
    }

    /// (`line`, `column`) 칸의 OSC 8 링크 URI.
    pub fn link_at(&self, line: u16, column: u16) -> Option<&str> {
        self.links
            .get(usize::from(line))?
            .iter()
            .find(|l| (l.column..l.column.saturating_add(l.cols)).contains(&column))
            .map(|l| l.uri.as_str())
    }

    /// 커서 칸 폭(2칸 글자 위면 2).
    pub fn cursor_width(&self) -> u16 {
        let c = self.cursor;
        let wide = self
            .rows
            .get(usize::from(c.line))
            .and_then(|r| r.get(usize::from(c.column)))
            .is_some_and(|cell| cell.flags.contains(Flags::WIDE));
        if wide { 2 } else { 1 }
    }

    /// 연산을 차례로 적용한다. krilld가 보낸 값을 믿지 않는다: 범위·상한 밖이면 거기서 멈추고 `Err`(panic 없음).
    pub fn apply(&mut self, ops: Vec<Op>) -> Result<(), ApplyError> {
        for op in ops {
            match op {
                Op::Reset { cols, lines } => {
                    if !(1..=MAX_COLS).contains(&cols) || !(1..=MAX_LINES).contains(&lines) {
                        return Err(ApplyError("size"));
                    }
                    *self = Self::blank(cols, lines);
                }
                Op::Scroll(n) => {
                    let k = usize::from(n.unsigned_abs());
                    if k >= self.rows.len() {
                        return Err(ApplyError("scroll"));
                    }
                    self.links.resize(self.rows.len(), Vec::new());
                    if n > 0 {
                        self.rows.rotate_left(k);
                        self.rows.iter_mut().rev().take(k).for_each(Vec::clear);
                        self.links.rotate_left(k);
                        self.links.iter_mut().rev().take(k).for_each(Vec::clear);
                    } else {
                        self.rows.rotate_right(k);
                        self.rows.iter_mut().take(k).for_each(Vec::clear);
                        self.links.rotate_right(k);
                        self.links.iter_mut().take(k).for_each(Vec::clear);
                    }
                }
                Op::Line { line, cells } => {
                    if cells.len() > usize::from(self.cols) {
                        return Err(ApplyError("line too long"));
                    }
                    if cells.iter().any(|c| c.zerowidth.len() > MAX_ZEROWIDTH) {
                        return Err(ApplyError("zerowidth"));
                    }
                    *self
                        .rows
                        .get_mut(usize::from(line))
                        .ok_or(ApplyError("line"))? = cells;
                    if let Some(l) = self.links.get_mut(usize::from(line)) {
                        l.clear();
                    }
                }
                Op::Cursor(c) => {
                    if c.line >= self.lines || c.column >= self.cols {
                        return Err(ApplyError("cursor"));
                    }
                    self.cursor = c;
                }
                Op::Title(t) => {
                    if t.len() > MAX_TITLE {
                        return Err(ApplyError("title"));
                    }
                    self.title = sanitize_title(&t);
                }
                Op::Bell => self.bells = self.bells.wrapping_add(1),
                Op::CursorStyle(s) => self.cursor_style = s,
                Op::Links { line, links } => {
                    let ok = links.iter().all(|l| {
                        l.uri.len() <= MAX_URI
                            && l.cols > 0
                            && l.column
                                .checked_add(l.cols)
                                .is_some_and(|end| end <= self.cols)
                    });
                    if !ok || links.len() > usize::from(self.cols) {
                        return Err(ApplyError("links"));
                    }
                    if usize::from(line) >= self.rows.len() {
                        return Err(ApplyError("line"));
                    }
                    self.links.resize(self.rows.len(), Vec::new());
                    if let Some(l) = self.links.get_mut(usize::from(line)) {
                        *l = links;
                    }
                }
            }
        }
        Ok(())
    }
}

/// 화면 스트림을 따라가는 클라이언트 상태. 보이는 화면 하나만 든다(스크롤백 없음).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    pub screen: Screen,
    /// 마지막으로 끝까지 받은 revision. 스냅샷 전이거나 조각을 받는 중이면 `None`.
    pub revision: Option<Revision>,
    /// `revision` 화면이 반영한 원본 끝.
    pub seq: Seq,
}

impl View {
    /// Update 하나를 적용한다. 실패하면 화면을 믿을 수 없으니 호출자가 RESYNC를 보낸다.
    pub fn apply(&mut self, u: Update) -> Result<(), ApplyError> {
        self.revision = None;
        self.screen.apply(u.ops)?;
        if let Some(explicit) = u.cursor_explicit {
            self.screen.cursor_explicit = explicit;
        }
        if u.done {
            self.revision = Some(u.revision);
            self.seq = u.seq;
        }
        Ok(())
    }

    /// 재접속 때 [`Stream::Screen`]에 실을 값. `instance`는 이 화면을 받은 krilld의 `instance_id`.
    /// 이 View를 계속 보관해 델타를 적용할 때만 보낸다. 새 View로 시작하면 `known = None`이다.
    pub fn known(&self, instance: u64) -> Option<Known> {
        self.revision.map(|revision| Known { instance, revision })
    }
}

/// 클라이언트가 가진 화면의 출처. 같은 세대·같은 revision이면 krilld는 스냅샷을 생략한다.
/// revision만 저장하면 안 된다. 해당 화면 전체를 보관해 다음 델타의 기준으로 써야 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known {
    pub instance: u64,
    pub revision: Revision,
}

/// 구독할 스트림. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stream {
    /// 보이는 화면(앱).
    Screen { known: Option<Known> },
    /// 원본 바이트(`krill attach`, 해시 검사). `from`부터.
    Raw { from: Seq },
}

/// [`kind::ATTACH`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attach {
    pub session: SessionId,
    pub stream: Stream,
}

/// [`kind::ATTACHED`]: 붙은 시점의 세션 위치.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attached {
    pub seq: Seq,
    pub revision: Revision,
}

/// [`kind::UPDATE`]: 한 revision의 연산 조각.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub revision: Revision,
    pub seq: Seq,
    /// 이 revision의 마지막 조각.
    pub done: bool,
    pub ops: Vec<Op>,
    /// Optional postfix; skipped in the base representation for N-1 compatibility.
    #[serde(default, skip)]
    pub cursor_explicit: Option<bool>,
}

/// [`kind::GAP`]: 원본 `[start, start + len)`을 보낼 수 없다. `len == 0`은 크래시로 길이를 모르는 구간(seq는 이어진다).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapMsg {
    pub start: Seq,
    pub len: u64,
}

/// [`kind::CLOSED`]: 세션이 끝났고 `seq`까지 다 보냈다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closed {
    pub seq: Seq,
}

/// [`kind::REFUSED`]. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refused {
    NoSession,
    /// 지금 연결에서 받을 수 없는 요청(원본 스트림에 RESYNC·히스토리 등).
    BadRequest,
}

/// 스크롤백 출처.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// krilld 라이브 Term의 핫 스크롤백(최대 1,000행).
    Hot,
    /// 디스크 세그먼트 하나를 재생한 콜드 페이지(0이 가장 오래됨). 세그먼트 시작 상태는 MK-024 근사를 따른다.
    Cold { page: u32 },
}

/// [`kind::HISTORY`]: 출처의 행 중 아래(최신)에서 `skip`행 위부터 위로 `count`행.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRequest {
    pub req: u32,
    pub source: Source,
    pub skip: u32,
    /// [`MAX_HISTORY_ROWS`] 이하. 프레임에 다 안 들어가면 krilld가 덜 보낸다.
    pub count: u16,
}

/// [`kind::HISTORY_ROWS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRows {
    pub req: u32,
    /// 그 출처의 전체 행 수(핫 스크롤백 행 수 또는 그 페이지의 행 수).
    pub total: u32,
    /// 디스크에 있는 콜드 페이지 수.
    pub pages: u32,
    pub skip: u32,
    /// 위(오래된 행)부터. 마지막 행이 아래에서 `skip`행 위다. 요청보다 적을 수 있다.
    pub rows: Vec<Vec<Cell>>,
}

impl HistoryRows {
    /// 프레임 하나에 들어가도록 위(오래된) 행부터 버린다. 남은 행은 여전히 아래에서 `skip`행 위에서 끝난다.
    pub fn fit(&mut self) {
        let mut used = 0usize;
        let keep = self
            .rows
            .iter()
            .rev()
            .take_while(|row| {
                let n = postcard::experimental::serialized_size(row).unwrap_or(usize::MAX);
                used = used.saturating_add(n);
                used <= OPS_BUDGET
            })
            .count();
        let drop = self.rows.len().saturating_sub(keep);
        self.rows.drain(..drop);
    }
}

/// postcard DTO 메시지와 그 kind.
pub trait Msg: Serialize + DeserializeOwned {
    const KIND: u8;

    /// 프레임 전체를 만든다.
    fn encode(&self) -> Result<Vec<u8>, Error> {
        ipc::encode(Self::KIND, self)
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, Error> {
        ipc::decode(Self::KIND, kind, body)
    }
}

impl Msg for Attach {
    const KIND: u8 = kind::ATTACH;
}
impl Msg for HistoryRequest {
    const KIND: u8 = kind::HISTORY;
}
impl Msg for Attached {
    const KIND: u8 = kind::ATTACHED;
}
impl Msg for Update {
    const KIND: u8 = kind::UPDATE;

    fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut body = postcard::to_stdvec(self).map_err(Error::Codec)?;
        if let Some(explicit) = self.cursor_explicit {
            body.extend(postcard::to_stdvec(&explicit).map_err(Error::Codec)?);
        }
        ipc::encode_frame(Self::KIND, &body)
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, Error> {
        if kind != Self::KIND {
            return Err(Error::UnexpectedKind {
                want: Self::KIND,
                got: kind,
            });
        }
        let (mut update, rest) = postcard::take_from_bytes::<Self>(body).map_err(Error::Codec)?;
        if !rest.is_empty() {
            let (explicit, _future) =
                postcard::take_from_bytes::<bool>(rest).map_err(Error::Codec)?;
            update.cursor_explicit = Some(explicit);
        }
        Ok(update)
    }
}
impl Msg for GapMsg {
    const KIND: u8 = kind::GAP;
}
impl Msg for Closed {
    const KIND: u8 = kind::CLOSED;
}
impl Msg for Refused {
    const KIND: u8 = kind::REFUSED;
}
impl Msg for HistoryRows {
    const KIND: u8 = kind::HISTORY_ROWS;
}

/// [`kind::OUTPUT`] 프레임. `bytes`는 [`MAX_OUTPUT`] 이하여야 한다.
pub fn encode_output(seq: Seq, bytes: &[u8]) -> Result<Vec<u8>, Error> {
    if bytes.len() > MAX_OUTPUT {
        return Err(Error::TooLong(bytes.len()));
    }
    let mut body = Vec::with_capacity(bytes.len().saturating_add(8));
    body.extend_from_slice(&seq.0.to_le_bytes());
    body.extend_from_slice(bytes);
    ipc::encode_frame(kind::OUTPUT, &body)
}

/// [`kind::OUTPUT`] body를 (첫 바이트의 seq, 원본)으로.
pub fn decode_output(kind: u8, body: &[u8]) -> Result<(Seq, &[u8]), Error> {
    if kind != kind::OUTPUT {
        return Err(Error::UnexpectedKind {
            want: kind::OUTPUT,
            got: kind,
        });
    }
    let (head, bytes) = body.split_first_chunk::<8>().ok_or(Error::BadLength(
        u64::try_from(body.len()).unwrap_or(u64::MAX),
    ))?;
    Ok((Seq(u64::from_le_bytes(*head)), bytes))
}

/// 프레임 하나에 담는 연산 바이트 예산. 나머지는 Update 머리(revision·seq varint 최대 20B, done, 개수)와 프레임 머리.
pub(crate) const OPS_BUDGET: usize = MAX_FRAME - 64;

/// `Op::Links` 목록을 프레임 예산에 들어가는 앞부분만 남긴다. 한 행의 링크 수 상한(`MAX_COLS`) ×
/// URI 상한(`MAX_URI`) ≈ 4MiB라 예산(~1MiB)을 넘을 수 있다(PH-03). 예산 안이면 그대로 돌려준다.
/// 목록 인코딩 = 개수 varint + 항목 이어붙이기라 항목별 크기를 재서 앞에서부터 담고, 끝에서 측정으로
/// 확인한다(개수 varint 증가분 +2B 여유). 빈 목록도 예산을 넘으면(있을 수 없음) `BadLength`.
fn fit_links(line: u16, links: Vec<Link>) -> Result<Vec<Link>, Error> {
    let mut kept: Vec<Link> = Vec::new();
    let mut used = postcard::experimental::serialized_size(&Op::Links {
        line,
        links: Vec::new(),
    })
    .map_err(Error::Codec)?;
    for link in links {
        let cost = postcard::experimental::serialized_size(&link).map_err(Error::Codec)?;
        let next = used.saturating_add(cost).saturating_add(2);
        if next > OPS_BUDGET {
            break;
        }
        used = next;
        kept.push(link);
    }
    loop {
        let n = postcard::experimental::serialized_size(&Op::Links {
            line,
            links: kept.clone(),
        })
        .map_err(Error::Codec)?;
        if n <= OPS_BUDGET {
            return Ok(kept);
        }
        if kept.pop().is_none() {
            return Err(Error::BadLength(u64::try_from(n).unwrap_or(u64::MAX)));
        }
    }
}

/// 한 revision의 연산을 프레임 하나 이상으로 나눈다. 마지막 조각만 `done = true`. 연산이 없어도 프레임 하나를 만든다.
/// `Op::Links` 하나가 예산을 넘으면 앞부분 링크만 남기고 자른다(칸의 `LINK` 표시는 그대로라 화면은
/// 그려진다, [`fit_links`]). 그 외 연산이 예산을 넘으면 `BadLength`.
pub fn encode_update(revision: Revision, seq: Seq, ops: Vec<Op>) -> Result<Vec<Vec<u8>>, Error> {
    encode_update_with_cursor(revision, seq, ops, None)
}

/// The final chunk carries the exact program cursor selection state. Old decoders
/// consume the unchanged base Update and ignore this optional postfix.
pub fn encode_update_with_cursor(
    revision: Revision,
    seq: Seq,
    ops: Vec<Op>,
    cursor_explicit: Option<bool>,
) -> Result<Vec<Vec<u8>>, Error> {
    let mut parts: Vec<Vec<Op>> = vec![Vec::new()];
    let mut used = 0usize;
    for op in ops {
        let op = match op {
            Op::Links { line, links } => Op::Links {
                line,
                links: fit_links(line, links)?,
            },
            op => op,
        };
        let n = postcard::experimental::serialized_size(&op).map_err(Error::Codec)?;
        if n > OPS_BUDGET {
            return Err(Error::BadLength(u64::try_from(n).unwrap_or(u64::MAX)));
        }
        let fits = used.checked_add(n).is_some_and(|t| t <= OPS_BUDGET);
        match parts.last_mut() {
            Some(last) if fits || last.is_empty() => {
                last.push(op);
                used = used.saturating_add(n);
            }
            _ => {
                parts.push(vec![op]);
                used = n;
            }
        }
    }
    let count = parts.len();
    parts
        .into_iter()
        .enumerate()
        .map(|(i, ops)| {
            Update {
                revision,
                seq,
                done: i.checked_add(1) == Some(count),
                ops,
                cursor_explicit: (i.checked_add(1) == Some(count))
                    .then_some(cursor_explicit)
                    .flatten(),
            }
            .encode()
        })
        .collect()
}

#[cfg(test)]
mod tests;
