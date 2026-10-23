//! 터미널 엔진 경계. `alacritty_terminal`(ARCHITECTURE §4, 0.26 버전 핀)은 이 모듈 밖으로 새지 않는다.
//!
//! 엔진을 바꾸거나 MK-006의 좁은 포크(잠정 결정)를 되돌리면 이 파일만 다시 쓴다.
//!
//! # 질의 응답 (MK-020, 응답 주체 표는 비공개 MK-008 스파이크 기록 §2)
//!
//! - alacritty가 내는 응답 이벤트를 질의 순서 그대로 모아 한 문자열로 돌려준다. 세 에이전트 모두 DA1을 질의 묶음의
//!   센티널로 쓰므로 순서가 바뀌면 안 된다(MK-008 결정 5).
//! - `PtyWrite`(DA1·DA2·DSR 5/6·kitty `CSI ?u`·DECRQM·`CSI 18t`)는 그대로, `ColorRequest`는 프로그램이 OSC로 바꾼
//!   색 → [`Appearance`] 테마 순서로, `TextAreaSizeRequest`(`CSI 14t`)는 셀 픽셀 크기를 받았을 때만 답한다.
//!   OSC 52 읽기는 답하지 않는다(보안). OSC 52 쓰기는 [`Feed::Live`]만 [`Engine::clipboard`]에 남겨 앱에 알린다(MK-033). 나머지(XTVERSION·XTGETTCAP·OSC 13~19·kitty 그래픽·OSC 99·1337·7501)는
//!   alacritty가 이벤트를 내지 않아 자연히 무응답이다.
//! - alacritty가 모르는 모드 2031(색 구성 알림)은 원본 바이트를 작은 vte 파서로 따로 훑어 추적한다.
//!   `CSI ?2031$p`에 alacritty가 내는 "미인식"(`0`) 응답을 그 질의 시점의 상태로 바꾸고, 테마가 바뀌면
//!   [`Engine::scheme_notice`]로 `CSI ?997;Nn`을 만든다(MK-008 결정 4). `CSI ?996n`(선택, 보낸 에이전트 없음)은 넣지 않았다.
//! - [`Feed::Replay`]로 넣은 바이트의 질의에는 아무것도 답하지 않는다.
//!
//! # 화면 부가 정보 (OWE-490)
//!
//! 창 제목(OSC 0·2, 제목 스택 포함)과 벨은 alacritty 이벤트로 받아 엔진이 들고, [`Engine::screen`]이 화면과 함께 뜬다.
//! 벨은 [`Feed::Live`]만 센다(재생한 바이트의 벨은 이미 울렸다). OSC 4로 바꾼 팔레트 0~255는 칸 색을 RGB로 풀어
//! 보낸다(앱은 테마로 색을 풀기 때문). OSC 10·11·12(기본 전경·배경·커서)는 아직 앱에 전하지 않는다.
//!
//! 셸이 알리는 현재 디렉터리(OSC 7 `file://host/path`, Windows Terminal식 OSC 9;9 `"C:\path"`)도 alacritty가
//! 버리므로 같은 곁 파서가 본다([`Engine::cwd`]). 프로세스 cwd를 읽지 못하는 Windows의 세션 cwd가 이것이다.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Row;
use alacritty_terminal::grid::{Charsets, Cursor as ACursor, Dimensions};
use alacritty_terminal::index::Line;
use alacritty_terminal::term::cell::{Cell as ACell, Flags as AFlags};
use alacritty_terminal::term::color::{COUNT, Colors};
use alacritty_terminal::term::{Config, Osc52, TermMode};
use alacritty_terminal::vte::ansi::{
    self, CharsetIndex, CursorShape as AShape, NamedColor, Processor, StdSyncHandler,
};
use alacritty_terminal::vte::{Params, Parser, Perform};
use krill_proto::clipboard::MAX_CLIPBOARD;
use krill_proto::holder::WinSize;
use krill_proto::screen::{
    Cell, Color, Cursor, CursorShape, CursorStyle, Flags, Link, MAX_COLS, MAX_LINES, MAX_TITLE,
    MAX_URI, MAX_ZEROWIDTH, Screen,
};
use krill_proto::theme::{Rgb, Theme};

use crate::appearance::Appearance;

/// 핫 스크롤백 행 수의 고정 상한이자 기본값(ARCHITECTURE §4, AGENTS 메모리 예산). 넘는 행은 세그먼트(MK-024)가 맡는다.
/// [`Engine::with_history`]로 더 작게 둘 수는 있어도 이보다 크게 둘 수는 없다.
pub const HOT_LINES: usize = 1_000;

/// 모드 2031(Contour 색 구성 알림).
const SCHEME_MODE: u16 = 2031;
/// alacritty 0.26이 `CSI ?2031$p`에 내는 응답(모르는 모드 = 0).
const SCHEME_RQM_UNKNOWN: &str = "\x1b[?2031;0$y";

/// 엔진에 넣는 바이트의 출처.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Feed {
    /// PTY에서 지금 읽은 바이트. 질의에 답한다.
    Live,
    /// 이미 처리했던 바이트를 다시 넣는다(키프레임 복원·세그먼트 재구성). 그때 응답은 이미 나갔으니 답하지 않는다.
    Replay,
}

/// alacritty 이벤트를 발생 순서대로 엔진에 넘긴다.
#[derive(Clone)]
struct Events(Sender<Event>);

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        // 받는 쪽(Engine)이 Term보다 먼저 사라지지 않으므로 실패하지 않는다.
        self.0.send(event).ok();
    }
}

/// alacritty가 버리는 것을 원본 바이트에서 따로 본다: 모드 2031 상태와 셸의 cwd 보고.
#[derive(Default)]
struct RawScan {
    on: bool,
    /// alacritty가 아직 답하지 않은 `CSI ?2031$p`마다 질의 시점의 상태. DEC 2026 sync 안의 질의는 alacritty가
    /// 구간이 끝날 때 답하므로 큐로 둔다.
    asked: VecDeque<bool>,
    /// 마지막으로 보고된 절대 경로 cwd.
    cwd: Option<PathBuf>,
}

impl Perform for RawScan {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let mut modes = params.iter().map(|p| p.first().copied());
        match (action, intermediates) {
            ('h' | 'l', [b'?']) if modes.any(|m| m == Some(SCHEME_MODE)) => self.on = action == 'h',
            // alacritty처럼 첫 번째 인자만 본다.
            ('p', [b'?', b'$']) if modes.next() == Some(Some(SCHEME_MODE)) => {
                self.asked.push_back(self.on);
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if byte == b'c' && intermediates.is_empty() {
            self.on = false; // RIS
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let cwd = match params {
            [b"7", rest @ ..] => file_url_path(&rest.join(&b';')),
            [b"9", b"9", rest @ ..] => {
                let raw = rest.join(&b';');
                let path = raw
                    .strip_prefix(b"\"")
                    .and_then(|p| p.strip_suffix(b"\""))
                    .unwrap_or(&raw);
                std::str::from_utf8(path).ok().map(PathBuf::from)
            }
            _ => None,
        };
        // 원격 셸(ssh)이 보낸 경로처럼 이 OS에서 절대 경로가 아니면 버린다.
        // ponytail: 호스트 이름은 대조하지 않는다. 원격 경로가 우연히 로컬 절대 경로 꼴이면(macOS·Linux 간) 받아들인다.
        if let Some(cwd) = cwd.filter(|p| p.is_absolute()) {
            self.cwd = Some(cwd);
        }
    }
}

/// `file://host/path`의 경로(퍼센트 디코딩). Windows는 `/C:/x`를 `C:/x`로 둔다.
fn file_url_path(url: &[u8]) -> Option<PathBuf> {
    let rest = url.strip_prefix(b"file://")?;
    let path = rest.get(rest.iter().position(|&b| b == b'/')?..)?;
    let mut out = Vec::with_capacity(path.len());
    let mut bytes = path.iter();
    while let Some(&b) = bytes.next() {
        if b == b'%' {
            let hex = [*bytes.next()?, *bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(b);
        }
    }
    let path = String::from_utf8(out).ok()?;
    #[cfg(windows)]
    let path = match path.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() => path.get(1..)?.to_owned(),
        _ => path,
    };
    Some(PathBuf::from(path))
}

/// 엔진이 실제로 쓰는 최소 열 수. 1열은 alacritty 0.26 `shrink_columns`가 끝나지 않는다:
/// 너비가 1이면 앞에 남은 wide 문자 하나가 매 반복에서 잘린 쪽 맨 앞으로 돌아가 같은 상태가
/// 반복된다(OWE-495). wide 문자는 2칸이 필요하므로 하한을 2로 둔다(포크 없이 우회).
/// [`Engine::new`]·[`Engine::resize`]가 모두 [`Dims`]를 거치므로 재생 경로도 함께 막힌다.
pub(crate) const MIN_COLS: u16 = 2;

/// 창 크기를 엔진·PTY가 감당하는 범위로 자른다(K-01). 상한은 와이어 상한
///([`MAX_COLS`]×[`MAX_LINES`], CLI·attach 검증과 같은 값)이다. 상한이 없으면
///세션 안 `stty`나 daemon `Create`의 거대 winsize가 재기동 때 그대로 Term에 들어가
///수백 MB를 할당하고 abort·jetsam 재기동 루프가 된다. 모든 엔진 진입은 [`Dims`]를
///거치고, PTY 크기(세션 생성·재기동·리사이즈)는 호출자가 이 함수로 자른다.
pub(crate) fn clamp_size(size: WinSize) -> WinSize {
    WinSize {
        cols: size.cols.clamp(MIN_COLS, MAX_COLS),
        rows: size.rows.clamp(1, MAX_LINES),
    }
}

struct Dims(WinSize);

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(clamp_size(self.0).rows)
    }

    fn columns(&self) -> usize {
        usize::from(clamp_size(self.0).cols)
    }
}

/// 세션 하나의 화면 상태: Term + vte 파서(DEC 2026 sync 버퍼 포함) + 질의 응답 상태.
pub(crate) struct Engine {
    term: Term<Events>,
    parser: Processor<StdSyncHandler>,
    events: Receiver<Event>,
    scan: Parser,
    scheme: RawScan,
    cli_boundary: crate::cli::Boundary,
    /// 강등이 풀렸을 때의 핫 스크롤백 행 수(≤ [`HOT_LINES`]).
    hot: usize,
    /// 창 제목(OSC 0·2). [`MAX_TITLE`]에서 자른다.
    title: String,
    /// 살아 있는 바이트([`Feed::Live`])로 울린 벨 수.
    bells: u64,
    /// 살아 있는 바이트로 받은 OSC 52 쓰기 수와 마지막 값(MK-033).
    clips: u64,
    clip: String,
}

impl Engine {
    pub(crate) fn new(size: WinSize) -> Self {
        Self::with_history(size, HOT_LINES)
    }

    /// 스크롤백 상한을 `history`행으로 둔 엔진. [`HOT_LINES`]를 넘으면 [`HOT_LINES`]로 자른다(메모리 예산).
    pub(crate) fn with_history(size: WinSize, history: usize) -> Self {
        let hot = history.min(HOT_LINES);
        let (tx, events) = mpsc::channel();
        Self {
            term: Term::new(config(hot), &Dims(size), Events(tx)),
            parser: Processor::new(),
            events,
            scan: Parser::new(),
            scheme: RawScan::default(),
            cli_boundary: crate::cli::Boundary::default(),
            hot,
            title: String::new(),
            bells: 0,
            clips: 0,
            clip: String::new(),
        }
    }

    /// 바이트를 처리하고, 그 안의 질의에 대한 응답(PTY에 쓸 바이트)을 질의 순서대로 돌려준다.
    pub(crate) fn feed(&mut self, bytes: &[u8], feed: Feed, look: &Appearance) -> String {
        self.cli_boundary.advance(bytes);
        // RawScan only observes escape sequences; printable ASCII and C0 controls
        // have no side effects while its parser has no pending sequence/codepoint.
        if !self.scan.mk_is_ground() || !bytes.is_ascii() || bytes.contains(&0x1b) {
            self.scan.advance(&mut self.scheme, bytes);
        }
        self.parser.advance(&mut self.term, bytes);
        self.replies(feed, look)
    }

    /// 열린 sync(DEC 2026) 구간이 강제로 반영돼야 하는 시각. 없으면 `None`.
    pub(crate) fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// 종료 신호(ESU) 없이 마감이 지난 sync 버퍼를 화면에 반영한다. 버퍼 안 질의의 응답을 돌려준다.
    pub(crate) fn end_sync(&mut self, feed: Feed, look: &Appearance) -> String {
        self.parser.stop_sync(&mut self.term);
        self.replies(feed, look)
    }

    /// 숨긴 세션 강등(MK-035): 핫 스크롤백을 비워 메모리를 돌려주고 새 행을 쌓지 않는다. 화면·모드·제목·파서 꼬리는
    /// 그대로다. 파서가 바닥 상태(sync 버퍼·미완성 제어열·UTF-8 꼬리 없음)일 때만 vte 파서를 새로 만들어 큰 sync 뒤
    /// 남은 버퍼 용량도 돌려준다. 버린 행은 세그먼트(MK-024) 콜드 페이지에 있다.
    pub(crate) fn demote(&mut self, look: &Appearance) {
        self.set_history(0, look);
        // `set_options`만으로는 alacritty가 캐시 1,000행을 남긴다(비공개 MK-009 스파이크 기록 원인 3).
        // ponytail: 대체 화면 중에는 기본 화면 grid가 비활성이라 공개 API로 truncate할 수 없어 캐시가 최대 1,000행
        // 남는다. 대체 화면을 오래 쓰는 에이전트가 문제가 되면 좁은 포크(MK-006)로 비활성 grid도 자른다.
        self.term.grid_mut().truncate();
        if self.capture_rewind() == 0 {
            self.parser = Processor::new();
        }
    }

    /// 강등을 푼다: 핫 스크롤백을 다시 만들 때의 상한(≤ [`HOT_LINES`])까지 쌓는다(이전 행은 콜드 페이지로).
    pub(crate) fn promote(&mut self, look: &Appearance) {
        self.set_history(self.hot, look);
    }

    fn set_history(&mut self, history: usize, look: &Appearance) {
        self.term.set_options(config(history));
        // `set_options`가 제목 이벤트를 다시 낸다. 질의 응답은 아니니 재생처럼 비운다.
        self.replies(Feed::Replay, look);
    }

    /// 테마가 바뀌었을 때 PTY에 쓸 알림. 모드 2031이 꺼져 있으면 `None`.
    pub(crate) fn scheme_notice(&self, theme: &Theme) -> Option<String> {
        let n = if is_dark(theme.background) { 1 } else { 2 };
        self.scheme.on.then(|| format!("\x1b[?997;{n}n"))
    }

    fn replies(&mut self, feed: Feed, look: &Appearance) -> String {
        let mut out = String::new();
        while let Ok(event) = self.events.try_recv() {
            let reply = match event {
                Event::PtyWrite(text) if text == SCHEME_RQM_UNKNOWN => {
                    let on = self.scheme.asked.pop_front().unwrap_or(self.scheme.on);
                    format!("\x1b[?{SCHEME_MODE};{}$y", if on { 1 } else { 2 })
                }
                Event::PtyWrite(text) => text,
                Event::ColorRequest(index, fmt) => {
                    // 프로그램이 OSC 4·10·11·12로 바꾼 색이 먼저다(alacritty 앱과 같은 규칙).
                    let set = (index < COUNT).then(|| self.term.colors()[index]).flatten();
                    let Some(c) = set.map(from_ansi).or_else(|| color(&look.theme(), index)) else {
                        continue;
                    };
                    fmt(to_ansi(c))
                }
                Event::TextAreaSizeRequest(fmt) => {
                    let Some(cell) = look.cell_size() else {
                        continue;
                    };
                    let lines = u16::try_from(self.term.screen_lines()).unwrap_or(u16::MAX);
                    let cols = u16::try_from(self.term.columns()).unwrap_or(u16::MAX);
                    // alacritty 포매터는 u16 곱셈을 한다. 넘치면 panic(overflow-checks)이니 답하지 않는다.
                    if lines.checked_mul(cell.height).is_none()
                        || cols.checked_mul(cell.width).is_none()
                    {
                        continue;
                    }
                    fmt(WindowSize {
                        num_lines: lines,
                        num_cols: cols,
                        cell_width: cell.width,
                        cell_height: cell.height,
                    })
                }
                Event::Title(t) => {
                    self.title = clip(&t, MAX_TITLE);
                    continue;
                }
                Event::ResetTitle => {
                    self.title.clear();
                    continue;
                }
                Event::Bell => {
                    if feed == Feed::Live {
                        self.bells = self.bells.wrapping_add(1);
                    }
                    continue;
                }
                // OSC 52 쓰기: 재생한 바이트는 이미 한 번 썼다. 너무 크면 버린다(잘라 넣으면 다른 값).
                Event::ClipboardStore(_, text) => {
                    if feed == Feed::Live && text.len() <= MAX_CLIPBOARD {
                        self.clips = self.clips.saturating_add(1);
                        self.clip = text;
                    }
                    continue;
                }
                // OSC 52 읽기는 보안상 답하지 않는다(MK-008 19번, `Osc52::OnlyCopy`라 이벤트도 오지 않는다).
                _ => continue,
            };
            if feed == Feed::Live {
                out.push_str(&reply);
            }
        }
        out
    }

    /// 엔진 모드를 프레임워크·엔진 중립 입력 모드로 바꾼다(MK-029).
    pub(crate) fn input_modes(&self) -> krill_proto::term_modes::Modes {
        use krill_proto::term_modes::Modes;
        let mode = self.term.mode();
        let mut out = Modes::default();
        for (flag, bit) in [
            (TermMode::DISAMBIGUATE_ESC_CODES, 1),
            (TermMode::REPORT_EVENT_TYPES, 2),
            (TermMode::REPORT_ALTERNATE_KEYS, 4),
            (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
            (TermMode::REPORT_ASSOCIATED_TEXT, 16),
        ] {
            if mode.contains(flag) {
                out.kitty |= bit;
            }
        }
        for (flag, bit) in [
            (TermMode::APP_CURSOR, Modes::APP_CURSOR),
            (TermMode::MOUSE_REPORT_CLICK, Modes::MOUSE_CLICK),
            (TermMode::MOUSE_DRAG, Modes::MOUSE_DRAG),
            (TermMode::MOUSE_MOTION, Modes::MOUSE_MOTION),
            (TermMode::SGR_MOUSE, Modes::SGR_MOUSE),
            (TermMode::BRACKETED_PASTE, Modes::BRACKETED_PASTE),
            (TermMode::ALT_SCREEN, Modes::ALT_SCREEN),
            (TermMode::ALTERNATE_SCROLL, Modes::ALTERNATE_SCROLL),
        ] {
            if mode.contains(flag) {
                out.flags |= bit;
            }
        }
        out
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// 셸이 마지막으로 알린 현재 디렉터리(OSC 7·OSC 9;9). 엔진을 새로 만들면 다음 보고까지 없다.
    pub(crate) fn cwd(&self) -> Option<&Path> {
        self.scheme.cwd.as_deref()
    }

    /// OSC 52 쓰기 수와 마지막 값.
    pub(crate) fn clipboard(&self) -> (u64, &str) {
        (self.clips, &self.clip)
    }

    pub(crate) fn resize(&mut self, size: WinSize) {
        self.term.resize(Dims(size));
    }

    /// Rebase the harvesting replay view onto the authoritative raw replay state.
    ///
    /// Both engines must have consumed identical bytes in identical feed boundaries
    /// since their last parser initialization. The VTE/RawScan/CLI parsers (including
    /// partial UTF-8, CSI and DEC 2026 buffers) stay in place: they depend on the raw
    /// stream, not on removed history. Only terminal state can diverge after harvest.
    /// Each grid is bounded by the ordinary screen and [`HOT_LINES`] limits.
    pub(crate) fn rebase_replay(&mut self, source: &Self) {
        // Resize first so Term's private damage bookkeeping matches the new dimensions.
        self.resize(WinSize {
            cols: u16::try_from(source.term.columns()).unwrap_or(u16::MAX),
            rows: u16::try_from(source.term.screen_lines()).unwrap_or(u16::MAX),
        });
        self.term.grid_mut().clone_from(source.term.grid());
        self.term
            .inactive_grid
            .clone_from(&source.term.inactive_grid);
        self.term.vi_mode_cursor = source.term.vi_mode_cursor;
        self.term.selection.clone_from(&source.term.selection);
        self.term.is_focused = source.term.is_focused;
        self.term.active_charset = source.term.active_charset;
        self.term.tabs.tabs.clone_from(&source.term.tabs.tabs);
        self.term.mode = source.term.mode;
        self.term
            .scroll_region
            .clone_from(&source.term.scroll_region);
        self.term.colors = source.term.colors;
        self.term.cursor_style = source.term.cursor_style;
        self.term.title.clone_from(&source.term.title);
        self.term.title_stack.clone_from(&source.term.title_stack);
        self.term
            .keyboard_mode_stack
            .clone_from(&source.term.keyboard_mode_stack);
        self.term
            .inactive_keyboard_mode_stack
            .clone_from(&source.term.inactive_keyboard_mode_stack);
        self.title.clone_from(&source.title);
        self.bells = source.bells;
        self.clips = source.clips;
        self.clip.clone_from(&source.clip);
        self.scheme.on = source.scheme.on;
        self.scheme.asked.clone_from(&source.scheme.asked);
        self.scheme.cwd.clone_from(&source.scheme.cwd);
    }

    /// 스크롤백 행 수.
    pub(crate) fn history_len(&self) -> usize {
        self.term.grid().history_size()
    }

    /// 핫 스크롤백을 비운다. 보이는 화면·커서·모드는 그대로 둔다(OWE-873b).
    pub(crate) fn clear_history(&mut self) {
        self.term.grid_mut().clear_history();
    }

    /// Orca/xterm clear처럼 현재 커서 행만 맨 위에 남긴다. PTY나 모드에는 쓰지 않는다.
    pub(crate) fn clear_viewport(&mut self) {
        let row = self.term.grid()[self.term.grid().cursor.point.line].clone();
        ansi::Handler::clear_screen(&mut self.term, ansi::ClearMode::All);
        self.term.grid_mut()[Line(0)] = row;
        self.term.grid_mut().cursor.point.line = Line(0);
        self.clear_history();
    }

    /// 스크롤백을 오래된 행부터 꺼내고 비운다. 콜드 페이지 재생이 메모리를 일정하게 두려고 쓴다.
    pub(crate) fn take_history(&mut self) -> Vec<Vec<Cell>> {
        let n = self.history_len();
        let (_, rows) = self.history_rows(0, n);
        self.clear_history();
        rows
    }

    /// 핫 스크롤백 행 수와, 아래(화면 바로 위)에서 `skip`행 위부터 위로 최대 `count`행(오래된 행부터).
    pub(crate) fn history_rows(&self, skip: usize, count: usize) -> (usize, Vec<Vec<Cell>>) {
        let grid = self.term.grid();
        let total = grid.history_size();
        let top = skip.saturating_add(count).min(total);
        let rows = (skip.saturating_add(1)..=top)
            .rev()
            .filter_map(|k| i32::try_from(k).ok())
            .map(|k| capture_row(&grid[Line(-k)], self.term.colors()).0)
            .collect();
        (total, rows)
    }

    /// 보이는 화면(화면 전송 키프레임, MK-023). [`MAX_COLS`]×[`MAX_LINES`]까지만 담는다.
    pub(crate) fn screen(&self) -> Screen {
        let grid = self.term.grid();
        let lines = u16::try_from(grid.screen_lines())
            .unwrap_or(u16::MAX)
            .min(MAX_LINES);
        let cols = u16::try_from(grid.columns())
            .unwrap_or(u16::MAX)
            .min(MAX_COLS);
        let point = grid.cursor.point;
        let line = u16::try_from(point.line.0).unwrap_or(0);
        let column = u16::try_from(point.column.0).unwrap_or(0);
        let (rows, links) = (0..i32::from(lines))
            .map(|l| capture_row(&grid[Line(l)], self.term.colors()))
            .unzip();
        let style = self.term.cursor_style();
        Screen {
            cols,
            lines,
            rows,
            cursor: Cursor {
                line: line.min(lines.saturating_sub(1)),
                column: column.min(cols.saturating_sub(1)),
                visible: self.term.mode().contains(TermMode::SHOW_CURSOR),
            },
            title: self.title.clone(),
            bells: self.bells,
            cursor_style: CursorStyle {
                shape: match style.shape {
                    AShape::Underline => CursorShape::Underline,
                    AShape::Beam => CursorShape::Bar,
                    AShape::Block | AShape::HollowBlock | AShape::Hidden => CursorShape::Block,
                },
                blink: style.blinking,
            },
            links,
        }
    }

    /// CLI 화면 표현. 모드는 엔진 경계 안에서 ANSI로 바꿔 전달한다.
    pub(crate) fn capture_cli(
        &self,
        format: krill_proto::control::CaptureFormat,
    ) -> std::io::Result<Vec<u8>> {
        crate::cli::capture(
            &self.screen(),
            &host_modes(*self.term.mode()),
            format,
            &cell(&self.term.grid().cursor.template, self.term.colors()),
        )
    }

    /// 화면에 아직 반영되지 않은 sync 버퍼 또는 미완성 제어·UTF-8 꼬리.
    pub(crate) fn capture_rewind(&self) -> usize {
        let sync = self.parser.sync_bytes_count();
        if sync > 0 {
            sync
        } else {
            self.cli_boundary.pending
        }
    }

    /// ANSI 캡처가 표현하지 못하는 엔진 상태가 있으면 콜드 키프레임을 생략한다.
    /// 생략한 세그먼트는 앞 세그먼트의 원본으로 워밍업한다.
    pub(crate) fn cold_keyframe(&self) -> std::io::Result<Option<Vec<u8>>> {
        let grid = self.term.grid();
        let captured_modes = TermMode::APP_CURSOR
            | TermMode::APP_KEYPAD
            | TermMode::SHOW_CURSOR
            | TermMode::LINE_WRAP
            | TermMode::MOUSE_MODE
            | TermMode::FOCUS_IN_OUT
            | TermMode::UTF8_MOUSE
            | TermMode::SGR_MOUSE
            | TermMode::BRACKETED_PASTE
            | TermMode::KITTY_KEYBOARD_PROTOCOL;
        let extra_modes = (*self.term.mode() ^ TermMode::default()) & !captured_modes;
        let rows = i32::try_from(grid.screen_lines()).unwrap_or(i32::MAX);
        let unsupported_cell = |cell: &ACell| {
            cell.hyperlink().is_some()
                || cell
                    .flags
                    .intersects(AFlags::WRAPLINE | (AFlags::ALL_UNDERLINES & !AFlags::UNDERLINE))
        };
        if self.capture_rewind() != 0
            || !extra_modes.is_empty()
            || self.term.scroll_region != (Line(0)..Line(rows))
            || grid.cursor.input_needs_wrap
            || grid.cursor.charsets != Charsets::default()
            || self.term.active_charset != CharsetIndex::default()
            || grid.saved_cursor != ACursor::default()
            || self.term.inactive_grid.saved_cursor != ACursor::default()
            || self.term.inactive_grid.cursor != ACursor::default()
            || self
                .term
                .inactive_grid
                .display_iter()
                .any(|cell| cell.cell != &ACell::default())
            || !self.term.keyboard_mode_stack.is_empty()
            || !self.term.title_stack.is_empty()
            || !self.title.is_empty()
            || self.scheme.on
            || self
                .term
                .tabs
                .tabs
                .iter()
                .enumerate()
                .any(|(i, tab)| *tab != (i % 8 == 0))
            || (0..COUNT).any(|i| self.term.colors()[i].is_some())
            || unsupported_cell(&grid.cursor.template)
            || grid.display_iter().any(|cell| unsupported_cell(cell.cell))
        {
            return Ok(None);
        }
        self.capture_cli(krill_proto::control::CaptureFormat::Ansi)
            .map(Some)
    }

    /// Plain non-empty tail lines, with bounded output and no Cell clones.
    pub(crate) fn screen_tail(&self, count: usize, chars: usize) -> Vec<String> {
        let grid = self.term.grid();
        let mut lines = Vec::new();
        for row in (0..grid.screen_lines()).rev() {
            let Ok(row) = i32::try_from(row) else {
                continue;
            };
            let cells = &grid[Line(row)];
            if !(0..grid.columns()).any(|col| {
                let cell = &cells[alacritty_terminal::index::Column(col)];
                !cell.flags.contains(AFlags::WIDE_CHAR_SPACER) && !cell.c.is_whitespace()
            }) {
                continue;
            }
            let text: String = (0..grid.columns())
                .map(|col| &cells[alacritty_terminal::index::Column(col)])
                .filter(|cell| !cell.flags.contains(AFlags::WIDE_CHAR_SPACER))
                .take(chars)
                .map(|cell| cell.c)
                .collect();
            lines.push(text);
            if lines.len() >= count {
                break;
            }
        }
        lines.reverse();
        lines
    }

    /// 보이는 화면을 줄마다 평문으로(진단·테스트용, 색·속성 없음).
    pub(crate) fn screen_text(&self) -> String {
        let grid = self.term.grid();
        let mut out = String::new();
        let mut line = Line(0);
        for cell in grid.display_iter() {
            if cell.point.line != line {
                out.push('\n');
                line = cell.point.line;
            }
            if !cell.flags.contains(AFlags::WIDE_CHAR_SPACER) {
                out.push(cell.c);
            }
        }
        out
    }
}

/// 엔진 설정. `history`만 바뀐다(kitty 설정이 바뀌면 alacritty가 키보드 모드 스택을 지우므로 고정).
fn config(history: usize) -> Config {
    Config {
        scrolling_history: history,
        // 에이전트 TUI가 쓴다(MK-029). MK-006도 이 설정으로 검증했다.
        kitty_keyboard: true,
        // 쓰기만 받고 읽기(질의)는 막는다(MK-033, MK-008 19번). alacritty 기본값과 같지만 보안 결정이라 적어 둔다.
        osc52: Osc52::OnlyCopy,
        ..Config::default()
    }
}

/// 터미널 입력 모드는 엔진 경계 안에서 호스트 ANSI로 바꾼다.
fn host_modes(mode: TermMode) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (flag, number) in [
        (TermMode::APP_CURSOR, 1),
        (TermMode::LINE_WRAP, 7),
        (TermMode::SHOW_CURSOR, 25),
        (TermMode::MOUSE_REPORT_CLICK, 1000),
        (TermMode::MOUSE_DRAG, 1002),
        (TermMode::MOUSE_MOTION, 1003),
        (TermMode::FOCUS_IN_OUT, 1004),
        (TermMode::UTF8_MOUSE, 1005),
        (TermMode::SGR_MOUSE, 1006),
        (TermMode::BRACKETED_PASTE, 2004),
    ] {
        write!(
            out,
            "\x1b[?{number}{}",
            if mode.contains(flag) { 'h' } else { 'l' }
        )
        .ok();
    }
    out.push_str(if mode.contains(TermMode::APP_KEYPAD) {
        "\x1b="
    } else {
        "\x1b>"
    });
    let mut kitty = 0;
    for (flag, bit) in [
        (TermMode::DISAMBIGUATE_ESC_CODES, 1),
        (TermMode::REPORT_EVENT_TYPES, 2),
        (TermMode::REPORT_ALTERNATE_KEYS, 4),
        (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
        (TermMode::REPORT_ASSOCIATED_TEXT, 16),
    ] {
        if mode.contains(flag) {
            kitty |= bit;
        }
    }
    write!(out, "\x1b[={kitty}u").ok();
    out
}

/// alacritty 행 → 전송 행과 그 행의 OSC 8 링크 구간. 앞 [`MAX_COLS`]칸만, 끝의 기본 칸은 잘라 낸다.
/// `colors`는 프로그램이 OSC 4로 바꾼 팔레트(바뀐 번호는 RGB로 풀어 보낸다).
fn capture_row(row: &Row<ACell>, colors: &Colors) -> (Vec<Cell>, Vec<Link>) {
    let mut links: Vec<Link> = Vec::new();
    let mut out: Vec<Cell> = Vec::new();
    for (column, c) in (0..MAX_COLS).zip(row) {
        out.push(cell(c, colors));
        let Some(h) = c.hyperlink() else { continue };
        let uri = h.uri();
        if uri.len() > MAX_URI {
            continue;
        }
        match links.last_mut() {
            Some(l) if l.column.saturating_add(l.cols) == column && l.uri == uri => l.cols += 1,
            _ => links.push(Link {
                column,
                cols: 1,
                uri: uri.to_owned(),
            }),
        }
    }
    let end = out.iter().rposition(|c| !c.is_blank()).map_or(0, |i| i + 1);
    out.truncate(end);
    (out, links)
}

/// 제어 문자와 양방향 방향 전환 문자를 걸러 내고 `max`바이트 이하가 되게 글자 경계에서 자른다(MK-076).
fn clip(s: &str, max: usize) -> String {
    krill_proto::screen::sanitize_title_max(s, max)
}

fn cell(c: &ACell, colors: &Colors) -> Cell {
    let f = c.flags;
    let mut flags = Flags::default();
    for (from, to) in [
        (AFlags::BOLD, Flags::BOLD),
        (AFlags::ITALIC, Flags::ITALIC),
        (AFlags::INVERSE, Flags::INVERSE),
        (AFlags::WIDE_CHAR, Flags::WIDE),
        (AFlags::WIDE_CHAR_SPACER, Flags::SPACER),
        (AFlags::LEADING_WIDE_CHAR_SPACER, Flags::SPACER),
        (AFlags::STRIKEOUT, Flags::STRIKEOUT),
        (AFlags::DIM, Flags::DIM),
        (AFlags::HIDDEN, Flags::HIDDEN),
        (AFlags::WRAPLINE, Flags::WRAPLINE),
    ] {
        if f.contains(from) {
            flags |= to;
        }
    }
    if f.intersects(AFlags::ALL_UNDERLINES) {
        flags |= Flags::UNDERLINE;
    }
    if c.hyperlink().is_some() {
        flags |= Flags::LINK;
    }
    let zerowidth = c
        .zerowidth()
        .map(|z| z.iter().take(MAX_ZEROWIDTH).copied().collect())
        .unwrap_or_default();
    Cell {
        ch: c.c,
        zerowidth,
        fg: wire_color(c.fg, colors),
        bg: wire_color(c.bg, colors),
        underline: c
            .underline_color()
            .map_or(Color::Default, |u| wire_color(u, colors)),
        flags,
    }
}

/// alacritty 색 → 전송 색. 이름 색 0~15(흐린 변형 포함)는 팔레트 번호, 기본 전경·배경 등은 `Default`.
/// 프로그램이 OSC 4로 바꾼 번호는 그 RGB로 보낸다.
fn wire_color(c: ansi::Color, colors: &Colors) -> Color {
    let i = match c {
        ansi::Color::Spec(c) => return Color::Rgb(from_ansi(c)),
        ansi::Color::Indexed(i) => i,
        ansi::Color::Named(n) => {
            let n = n as usize;
            let dim = NamedColor::DimBlack as usize;
            let i = if (dim..dim + 8).contains(&n) {
                n - dim
            } else {
                n
            };
            match u8::try_from(i).ok().filter(|i| *i < 16) {
                Some(i) => i,
                None => return Color::Default,
            }
        }
    };
    colors[usize::from(i)].map_or(Color::Indexed(i), |rgb| Color::Rgb(from_ansi(rgb)))
}

/// alacritty 색 번호 → 테마 색. 0~15 ANSI, 16~231 6×6×6 큐브, 232~255 회색, 256 전경, 257 배경, 258 커서.
fn color(theme: &Theme, index: usize) -> Option<Rgb> {
    let level = |v: usize| u8::try_from(if v == 0 { 0 } else { 55 + v * 40 }).ok();
    match index {
        0..=15 => theme.ansi.get(index).copied(),
        16..=231 => {
            let i = index - 16;
            Some(Rgb {
                r: level(i / 36)?,
                g: level(i / 6 % 6)?,
                b: level(i % 6)?,
            })
        }
        232..=255 => {
            let v = u8::try_from(8 + (index - 232) * 10).ok()?;
            Some(Rgb { r: v, g: v, b: v })
        }
        256 => Some(theme.foreground),
        257 => Some(theme.background),
        258 => Some(theme.cursor),
        _ => None,
    }
}

/// 배경 밝기(ITU-R BT.601 가중치)로 어두운 테마인지 정한다.
fn is_dark(bg: Rgb) -> bool {
    u32::from(bg.r) * 299 + u32::from(bg.g) * 587 + u32::from(bg.b) * 114 < 128_000
}

fn to_ansi(c: Rgb) -> ansi::Rgb {
    ansi::Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

fn from_ansi(c: ansi::Rgb) -> Rgb {
    Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

#[cfg(test)]
mod tests;
