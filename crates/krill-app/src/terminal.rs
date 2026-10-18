//! 터미널 렌더러(MK-027). `krill_term::grid`가 만든 행 계획을 gpui로 칠한다.
//!
//! 클린룸: Zed `terminal`·`terminal_view`(GPL-3.0)는 보지 않았다. 근거는 Apache-2.0 gpui 소스와 MK-013 측정뿐이다.
//! gpui 의존(MK-002·MK-013 잠정 결정)은 이 파일에 가둔다. gpui를 버리면 이 파일만 다시 쓴다.
//!
//! - **레이어 2장**: 배경 사각형을 레이어 하나에, 글리프를 다른 레이어 하나에 넣는다. 레이어 안의 primitive는 레이어 순서를
//!   그대로 받아 `BoundsTree` 삽입이 없다(gpui `Scene::insert_primitive`). `ShapedLine::paint`는 호출마다 레이어를 만들어
//!   run이 많은 화면에서 삽입이 프레임의 87%였다(MK-013 결정 1).
//! - **글리프마다 색칠**: 색 없이 `layout_line`(셰이핑 캐시 키에도 색이 없다), 글리프마다 그 칸의 색으로 `paint_glyph`.
//! - **밑줄·취소선**(OWE-481): `TextRun`의 `underline`·`strikethrough`를 쓰지 않고(색이 run을 나눈다) 행 계획의 구간을
//!   글자 레이어에 두께 [`DECORATION`] 사각형으로 칠한다. 세로 위치는 기본 글꼴 기준이라 대체 글꼴 칸과도 이어진다.
//! - **vsync 합치기**: [`Terminal::set_screen`]은 화면을 바꾸고 `notify`만 한다. gpui는 디스플레이 링크 틱에서 더러운
//!   창을 한 번 그리므로, 틱 사이에 화면이 여러 번 와도 마지막 화면 하나만 그린다. 바뀐 게 없으면 그리지 않는다(유휴 프레임 0).
//! - **커서 모양**(OWE-490): 블록은 반투명 칸, 밑줄·막대는 [`CURSOR_THIN`] 두께 사각형. 프로그램이 모양을 지정하지
//!   않았으면 설정 기본 커서([`effective_cursor`], OWE-658). 깜박임은 셸이 타이머로 [`TerminalElement::cursor_hidden`]을 뒤집는다.
//! - **선택**(MK-033): 배경 레이어 맨 끝에 반투명 사각형을 칠해 글자는 그대로 보인다.
//! - **IME 조합 중 글자**(MK-028, OWE-615): 확정 에코 대기 글자(`pending`)와 조합 중 글자(`preedit`)를
//!   확정 셀과 같은 폭(`layout_line` + 셀 폭 강제)으로 그린다. 밑줄도 확정 셀과 같은 `underline_dy` 장식
//!   사각형이라 확정 전후 픽셀 위치가 같다. 그리드 커서 대신 마지막 글자 끝에 같은 모양 커서를 그린다.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, Context, Element, ElementId, Font, FontStyle, FontWeight, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Render, SharedString, Size, Style, TextRun,
    Window, fill, font, point, px, rems, size, underline_y_offset,
};
use krill_proto::screen::{CursorShape, CursorStyle};
use krill_proto::theme::{Rgb, Theme};
use krill_term::grid::{Screen, plan_row};
use krill_term::ime::CellPoint;
use krill_term::select::Selection;

pub const FONT_SIZE: Pixels = px(14.);
/// 커서 블록 불투명도(밑 글자가 보이게).
const CURSOR_ALPHA: f32 = 0.5;
/// 선택 영역 불투명도(테마에 선택 색이 없어 전경색을 덮는다).
const SELECTION_ALPHA: f32 = 0.3;
/// 밑줄·취소선 두께.
const DECORATION: Pixels = px(1.);
/// 밑줄·막대 커서 두께.
const CURSOR_THIN: Pixels = px(2.);
/// 취소선 높이: 기준선 위로 ascent의 이 비율(소문자 높이 가운데쯤, 추정).
const STRIKE_RISE: f32 = 0.3;

/// 창의 `rem_size`(줌 단계)와 설정 글꼴 크기(`terminal_font_size`, 10..=24pt)를
/// 반영한 터미널 글자 크기. 바꾸면 칸 크기가 바뀌어 PTY 리사이즈(`grid`)까지 이어진다.
pub fn font_size(window: &Window, cx: &App) -> Pixels {
    let pt = f32::from(crate::settings::terminal_font_pt(cx));
    rems(pt / 16.).to_pixels(window.rem_size())
}

/// 칸 크기: 'm' 폭 × round(글꼴 × 1.25).
pub fn cell_size(window: &Window, cx: &App) -> Size<Pixels> {
    let font_size = font_size(window, cx);
    let text = window.text_system();
    let family = crate::settings::terminal_font_family(cx);
    let id = text.resolve_font(&term_font(&family, false, false));
    let width = text
        .advance(id, font_size, 'm')
        .map_or(font_size * 0.6, |s| s.width);
    size(width, (font_size * 1.25).round())
}

/// `area`에 들어가는 (열, 행). 최소 1(빈 영역·잴 수 없는 칸에서도 PTY가 0이 되지 않게).
pub fn grid(area: Size<Pixels>, cell: Size<Pixels>) -> (u16, u16) {
    let fit = |len: Pixels, unit: Pixels| {
        let n = (f32::from(len) / f32::from(unit)).floor();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "유한한 1 이상 값을 u16 최대로 자른 뒤 바꾼다"
        )]
        if n.is_finite() && n >= 1. {
            n.min(f32::from(u16::MAX)) as u16
        } else {
            1
        }
    };
    (fit(area.width, cell.width), fit(area.height, cell.height))
}

fn term_font(family: &SharedString, bold: bool, italic: bool) -> Font {
    let mut f = font(family.clone());
    if bold {
        f.weight = FontWeight::BOLD;
    }
    if italic {
        f.style = FontStyle::Italic;
    }
    f
}

fn text_run(family: &SharedString, len: usize, bold: bool, italic: bool) -> TextRun {
    TextRun {
        len,
        font: term_font(family, bold, italic),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// 프로그램이 DECSCUSR로 커서를 지정하지 않았으면(기본값 그대로) 설정 기본 커서를 쓴다(OWE-658).
// ponytail: 화면은 "미지정"과 "DECSCUSR 2(깜박이지 않는 블록)"를 구분하지 못해 둘 다 설정값을 따른다.
// 구분이 필요하면 krill-proto `CursorStyle`에 지정 여부를 싣는다(와이어 변경).
pub fn effective_cursor(program: CursorStyle, default: CursorStyle) -> CursorStyle {
    if program == CursorStyle::default() {
        default
    } else {
        program
    }
}

fn hsla(c: Rgb) -> Hsla {
    gpui::rgb((u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)).into()
}

/// 터미널 하나의 뷰. 화면은 데몬 델타를 적용한 결과를 통째로 받는다(앱은 Term이 없다, MK-010).
#[derive(Debug)]
pub struct Terminal {
    screen: Rc<Screen>,
    theme: Theme,
}

impl Terminal {
    pub fn new(screen: Screen, theme: Theme) -> Self {
        Self {
            screen: Rc::new(screen),
            theme,
        }
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// 화면을 바꾼다. 그리기는 다음 디스플레이 링크 틱에 한 번(vsync 합치기).
    pub fn set_screen(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = Rc::new(screen);
        cx.notify();
    }
}

impl Render for Terminal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut element = TerminalElement::new(self.screen.clone(), self.theme);
        element.opacity = crate::settings::terminal_opacity_alpha(cx);
        element.cursor = effective_cursor(
            self.screen.cursor_style,
            crate::settings::terminal_cursor(cx),
        );
        element
    }
}

/// 그린 자리와 칸 크기. 요소가 칠할 때 적고 셸이 읽는다(IME 후보창 위치).
pub type Painted = Rc<Cell<(Bounds<Pixels>, Size<Pixels>)>>;

/// 포인터를 실제로 그린 터미널 칸에 맞춘다. 패딩·빈 창·그리드 밖은 제외한다.
pub fn cell_at(
    painted: &Painted,
    screen: &Screen,
    position: gpui::Point<Pixels>,
) -> Option<(u16, u16)> {
    let (bounds, cell) = painted.get();
    if !bounds.contains(&position) || cell.width <= px(0.) || cell.height <= px(0.) {
        return None;
    }
    let x = f32::from(position.x - bounds.origin.x) / f32::from(cell.width);
    let y = f32::from(position.y - bounds.origin.y) / f32::from(cell.height);
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.
        || y < 0.
        || x >= f32::from(screen.cols)
        || y >= f32::from(screen.lines)
    {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "유한한 그리드 안 좌표임을 위에서 검사했다"
    )]
    Some((x as u16, y as u16))
}

/// Cmd+클릭 대상. OSC 8이 임의 앱 실행용 scheme을 열지 않도록 일반 링크만 허용한다.
pub fn link_target<'a>(
    screen: &'a Screen,
    painted: &Painted,
    position: gpui::Point<Pixels>,
) -> Option<&'a str> {
    let (column, line) = cell_at(painted, screen, position)?;
    let uri = screen.link_at(line, column)?;
    (["https://", "http://", "mailto:"]
        .iter()
        .any(|s| uri.starts_with(s))
        && !uri
            .chars()
            .any(|c| c.is_control() || krill_proto::screen::is_bidi_control(c)))
    .then_some(uri)
}

/// 로컬 파일 링크는 실행하지 않고 Finder에서 위치만 표시한다.
pub fn file_link_target(
    screen: &Screen,
    painted: &Painted,
    position: gpui::Point<Pixels>,
) -> Option<std::path::PathBuf> {
    let (column, line) = cell_at(painted, screen, position)?;
    let uri = screen.link_at(line, column)?;
    if !uri.starts_with("file://") {
        return None;
    }
    let path = gpui::http_client::Url::parse(uri)
        .ok()?
        .to_file_path()
        .ok()?;
    if uri
        .chars()
        .chain(path.to_string_lossy().chars())
        .any(|c| c.is_control() || krill_proto::screen::is_bidi_control(c))
    {
        return None;
    }
    Some(path)
}

/// ⌘+호버 시 유효한 OSC 8 링크 칸 범위를 밑줄·포인터 커서 대상으로 돌려준다(OWE-867).
pub fn osc_link_highlight(
    screen: &Screen,
    painted: &Painted,
    position: gpui::Point<Pixels>,
) -> Option<Highlight> {
    if link_target(screen, painted, position).is_none()
        && file_link_target(screen, painted, position).is_none()
    {
        return None;
    }
    let (column, line) = cell_at(painted, screen, position)?;
    let uri = screen.link_at(line, column)?;
    let mut start = column;
    while start > 0 && screen.link_at(line, start - 1) == Some(uri) {
        start -= 1;
    }
    let mut end = column.saturating_add(1);
    while end < screen.cols && screen.link_at(line, end) == Some(uri) {
        end = end.saturating_add(1);
    }
    Some(Highlight {
        line,
        column: start,
        cols: end.saturating_sub(start),
    })
}

/// ⌘+호버·⌘+클릭 대상인 평문 `http://`·`https://` URL과 밑줄 범위를 돌려준다(OWE-867).
pub fn url_link_target(
    screen: &Screen,
    painted: &Painted,
    position: gpui::Point<Pixels>,
) -> Option<(Highlight, String)> {
    let (column, line) = cell_at(painted, screen, position)?;
    if screen.link_at(line, column).is_some() {
        return None;
    }
    let row = screen.rows.get(usize::from(line))?;
    let u = path_link::url_at(row, column)?;
    Some((
        Highlight {
            line,
            column: u.column,
            cols: u.cols,
        },
        u.uri,
    ))
}

/// IME 조합 중 글자(커서 칸 `origin`부터 `columns`칸).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preedit {
    pub origin: CellPoint,
    pub text: String,
    pub columns: usize,
}

impl Preedit {
    /// 한 셀에 그릴 묶음들: (칸 오프셋, 묶음 글자, 묶음 칸 수). 폭 0 문자는 앞 글자와 같은 칸이다.
    pub(crate) fn groups(&self) -> Vec<(usize, String, usize)> {
        let overlay = krill_term::ime::Overlay {
            origin: self.origin,
            text: &self.text,
            columns: self.columns,
        };
        let mut groups: Vec<(usize, String, usize)> = Vec::new();
        for (offset, ch) in overlay.cells() {
            if groups.last().is_some_and(|g| g.0 == offset) {
                if let Some(g) = groups.last_mut() {
                    g.1.push(ch);
                }
            } else {
                groups.push((offset, ch.to_string(), 0));
            }
        }
        for i in 0..groups.len() {
            let next = if i + 1 < groups.len() {
                groups[i + 1].0
            } else {
                self.columns
            };
            groups[i].2 = next.saturating_sub(groups[i].0);
        }
        groups
    }
}

/// 검색 일치 범위(MK-059). 배경 위·글자 아래에 반투명으로 칠한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Highlight {
    pub line: u16,
    pub column: u16,
    pub cols: u16,
}

/// 화면 하나를 그리는 요소. 크기는 칸 × (열, 행).
#[derive(Debug)]
pub struct TerminalElement {
    pub screen: Rc<Screen>,
    pub theme: Theme,
    pub preedit: Option<Preedit>,
    /// 확정 뒤 에코 대기 중인 낙관적 글자(OWE-615 원인 1). 화면에 아직 없어도 그린다.
    pub pending: Option<Preedit>,
    /// 그린 자리와 칸 크기를 적어 둔다(IME 후보창 위치).
    pub painted: Option<Painted>,
    /// 마우스 선택(MK-033). 배경 위·글자 아래에 반투명으로 칠한다.
    pub selection: Option<Selection>,
    /// 검색 일치 하이라이트(MK-059).
    pub highlight: Option<Highlight>,
    /// ⌘ 아래 파일 경로 링크(OWE-663). 글자색 밑줄을 긋는다.
    pub link: Option<Highlight>,
    /// 터미널 배경 알파(`terminal_opacity_pct / 100`, 글자·장식은 그대로).
    /// 배경 quad에만 곱한다. 기본 1.0(불투명).
    pub opacity: f32,
    /// 그릴 커서 모양(OWE-658). 기본은 화면이 가진 프로그램 지정값, 셸이 [`effective_cursor`]로 덮는다.
    pub cursor: CursorStyle,
    /// 깜박임의 꺼진 순간(OWE-658). 셸 타이머가 뒤집는다.
    pub cursor_hidden: bool,
}

impl TerminalElement {
    pub fn new(screen: Rc<Screen>, theme: Theme) -> Self {
        Self {
            cursor: screen.cursor_style,
            screen,
            theme,
            preedit: None,
            pending: None,
            painted: None,
            selection: None,
            highlight: None,
            link: None,
            opacity: 1.,
            cursor_hidden: false,
        }
    }

    /// 배경 알파를 곱한 터미널 배경색.
    fn background(&self) -> Hsla {
        hsla(self.theme.background).opacity(self.opacity.clamp(0., 1.))
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = Size<Pixels>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Size<Pixels>) {
        let cell = cell_size(window, cx);
        let mut style = Style::default();
        style.size.width = (cell.width * f32::from(self.screen.cols)).into();
        style.size.height = (cell.height * f32::from(self.screen.lines)).into();
        (window.request_layout(style, [], cx), cell)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _cell: &mut Size<Pixels>,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        cell: &mut Size<Pixels>,
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let cell = *cell;
        if let Some(p) = &self.painted {
            p.set((bounds, cell));
        }
        let theme = &self.theme;
        let family = crate::settings::terminal_font_family(cx);
        let background = self.background();
        window.paint_quad(fill(bounds, background));
        let plans: Vec<_> = self
            .screen
            .rows
            .iter()
            .map(|r| plan_row(r, theme))
            .collect();
        let x0 = bounds.origin.x;
        let col_x = |col: u16| x0 + cell.width * f32::from(col);
        let line_y = |line: u16| bounds.origin.y + cell.height * f32::from(line);

        window.paint_layer(bounds, |window| {
            for (line, plan) in (0u16..).zip(&plans) {
                for span in &plan.bg {
                    let origin = point(col_x(span.column), line_y(line));
                    let extent = size(cell.width * f32::from(span.cols), cell.height);
                    window.paint_quad(fill(Bounds::new(origin, extent), hsla(span.color)));
                }
            }
            if let Some(sel) = &self.selection {
                for line in 0..self.screen.lines {
                    if let Some((column, cols)) = sel.span(line, self.screen.cols) {
                        let origin = point(col_x(column), line_y(line));
                        let extent = size(cell.width * f32::from(cols), cell.height);
                        let color = hsla(theme.foreground).opacity(SELECTION_ALPHA);
                        window.paint_quad(fill(Bounds::new(origin, extent), color));
                    }
                }
            }
            if let Some(h) = self.highlight
                && h.line < self.screen.lines
                && h.cols > 0
            {
                let origin = point(col_x(h.column), line_y(h.line));
                let extent = size(cell.width * f32::from(h.cols), cell.height);
                let color = hsla(theme.ansi[3]).opacity(SELECTION_ALPHA);
                window.paint_quad(fill(Bounds::new(origin, extent), color));
            }
        });

        // 밑줄·취소선 세로 위치(칸 위에서부터). 기본 글꼴 한 글자 레이아웃의 ascent·descent로(프레임 캐시에 걸린다).
        let font_size = font_size(window, cx);
        let probe = window.text_system().layout_line(
            "m",
            font_size,
            &[text_run(&family, 1, false, false)],
            None,
        );
        let underline_dy = underline_y_offset(cell.height, probe.ascent, probe.descent);
        let strike_dy =
            (cell.height - probe.ascent - probe.descent) / 2. + probe.ascent * (1. - STRIKE_RISE);

        window.paint_layer(bounds, |window| {
            for (line, plan) in (0u16..).zip(&plans) {
                for (spans, dy) in [(&plan.underlines, underline_dy), (&plan.strikes, strike_dy)] {
                    for span in spans {
                        let origin = point(col_x(span.column), line_y(line) + dy);
                        let extent = size(cell.width * f32::from(span.cols), DECORATION);
                        window.paint_quad(fill(Bounds::new(origin, extent), hsla(span.color)));
                    }
                }
                if let Some(link) = self.link.filter(|l| l.line == line) {
                    let origin = point(col_x(link.column), line_y(line) + underline_dy);
                    let extent = size(cell.width * f32::from(link.cols), DECORATION);
                    window.paint_quad(fill(Bounds::new(origin, extent), hsla(theme.foreground)));
                }
                for seg in &plan.segments {
                    let runs: Vec<TextRun> = seg
                        .runs
                        .iter()
                        .map(|r| text_run(&family, r.len, r.bold, r.italic))
                        .collect();
                    // MK-058: ZWJ 묶음·넘침(VS16) 묶음은 2칸 간격으로 칠한다(엔진 열은 그대로).
                    let force = cell.width * if seg.wide || seg.overflow { 2. } else { 1. };
                    let layout =
                        window
                            .text_system()
                            .layout_line(&seg.text, font_size, &runs, Some(force));
                    // 줄 높이 안에서 세로 가운데(gpui ShapedLine::paint와 같은 계산).
                    let baseline = line_y(line)
                        + (cell.height - layout.ascent - layout.descent) / 2.
                        + layout.ascent;
                    let x = col_x(seg.column);
                    for run in &layout.runs {
                        for g in &run.glyphs {
                            let origin = point(x + g.position.x, baseline + g.position.y);
                            // 글리프 하나를 못 그려도(아틀라스 실패) 화면 나머지는 그린다.
                            let painted = if g.is_emoji {
                                window.paint_emoji(origin, run.font_id, g.id, font_size)
                            } else {
                                let color = seg.color_at(g.index).unwrap_or(theme.foreground);
                                window.paint_glyph(
                                    origin,
                                    run.font_id,
                                    g.id,
                                    font_size,
                                    hsla(color),
                                )
                            };
                            painted.ok();
                        }
                    }
                }
            }
        });

        // OWE-615 원인 1·3: 확정 에코 대기 글자와 조합 중 글자를 확정 셀과 같은 폭으로 그린다.
        // `pending`은 밑줄 없이(확정된 글자), `preedit`는 밑줄 장식으로. 커서는 마지막 글자 끝에.
        if self.pending.is_some() || self.preedit.is_some() {
            let paint_run = |run: &Preedit, underline: bool, window: &mut Window| {
                let cols = |v: usize| cell.width * f32::from(u16::try_from(v).unwrap_or(u16::MAX));
                let row = point(
                    x0 + cell.width
                        * f32::from(u16::try_from(run.origin.column).unwrap_or(u16::MAX)),
                    bounds.origin.y
                        + cell.height
                            * f32::from(u16::try_from(run.origin.line).unwrap_or(u16::MAX)),
                );
                window.paint_quad(fill(
                    Bounds::new(row, size(cols(run.columns), cell.height)),
                    background,
                ));
                for (offset, text, group_cols) in run.groups() {
                    // 확정 셀과 같은 강제 폭(2칸 글자는 2칸, 아니면 1칸)으로 같은 자리에 그린다.
                    let force = cell.width
                        * f32::from(u16::try_from(group_cols.max(1)).unwrap_or(u16::MAX));
                    let x =
                        row.x + cell.width * f32::from(u16::try_from(offset).unwrap_or(u16::MAX));
                    let layout = window.text_system().layout_line(
                        &text,
                        font_size,
                        &[text_run(&family, text.len(), false, false)],
                        Some(force),
                    );
                    // 줄 높이 안에서 세로 가운데(확정 셀과 같은 계산).
                    let baseline =
                        row.y + (cell.height - layout.ascent - layout.descent) / 2. + layout.ascent;
                    for layout_run in &layout.runs {
                        for g in &layout_run.glyphs {
                            let origin = point(x + g.position.x, baseline + g.position.y);
                            let painted = if g.is_emoji {
                                window.paint_emoji(origin, layout_run.font_id, g.id, font_size)
                            } else {
                                window.paint_glyph(
                                    origin,
                                    layout_run.font_id,
                                    g.id,
                                    font_size,
                                    hsla(theme.foreground),
                                )
                            };
                            // 글리프 하나를 못 그려도(아틀라스 실패) 화면 나머지는 그린다.
                            painted.ok();
                        }
                    }
                    if underline && group_cols > 0 {
                        let origin = point(x, row.y + underline_dy);
                        let extent = size(cols(group_cols), DECORATION);
                        window
                            .paint_quad(fill(Bounds::new(origin, extent), hsla(theme.foreground)));
                    }
                }
            };
            if let Some(p) = &self.pending {
                paint_run(p, false, window);
            }
            if let Some(p) = &self.preedit {
                paint_run(p, true, window);
            }
            // 마지막 글자 끝(조합 중이면 preedit 끝, 아니면 확정 대기 끝)에 그리드와 같은 모양 커서.
            let (caret_column, caret_line) = match (&self.preedit, &self.pending) {
                (Some(p), _) | (None, Some(p)) => (p.origin.column + p.columns, p.origin.line),
                (None, None) => return,
            };
            let caret = |v: usize| f32::from(u16::try_from(v).unwrap_or(u16::MAX));
            let origin = point(
                x0 + cell.width * caret(caret_column),
                bounds.origin.y + cell.height * caret(caret_line),
            );
            let (origin, extent, alpha) = match self.cursor.shape {
                CursorShape::Block => (origin, size(cell.width, cell.height), CURSOR_ALPHA),
                CursorShape::Underline => (
                    point(origin.x, origin.y + cell.height - CURSOR_THIN),
                    size(cell.width, CURSOR_THIN),
                    1.,
                ),
                CursorShape::Bar => (origin, size(CURSOR_THIN, cell.height), 1.),
            };
            window.paint_quad(fill(
                Bounds::new(origin, extent),
                hsla(theme.cursor).opacity(alpha),
            ));
            return;
        }
        let cur = self.screen.cursor;
        if cur.visible && !self.cursor_hidden {
            let origin = point(col_x(cur.column), line_y(cur.line));
            let width = cell.width * f32::from(self.screen.cursor_width());
            let (origin, extent, alpha) = match self.cursor.shape {
                CursorShape::Block => (origin, size(width, cell.height), CURSOR_ALPHA),
                CursorShape::Underline => (
                    point(origin.x, origin.y + cell.height - CURSOR_THIN),
                    size(width, CURSOR_THIN),
                    1.,
                ),
                CursorShape::Bar => (origin, size(CURSOR_THIN, cell.height), 1.),
            };
            window.paint_quad(fill(
                Bounds::new(origin, extent),
                hsla(theme.cursor).opacity(alpha),
            ));
        }
    }
}

pub mod path_link;

#[cfg(test)]
mod tests;
