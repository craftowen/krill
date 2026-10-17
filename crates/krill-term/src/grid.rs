//! 그리드 렌더 모델(MK-027): UI가 받은 화면(보이는 영역만)을 행마다 "배경 칸 + 셰이핑 단위"로 바꾼다.
//!
//! 화면 타입(`Screen`·`Cell`·`Color`·`Flags`)은 화면 전송 와이어 타입 그대로다(`krill_proto::screen`, MK-023).
//! 프레임워크와 무관한 부분만 여기 둔다. 실제로 그리는 쪽(`krill-app` `terminal`)은 이 결과를 그대로 칠한다.
//! 방식은 MK-013 결정 1(Apache-2.0 gpui 소스와 측정만 근거, Zed 터미널 코드는 보지 않음):
//!
//! 1. **배경 병합**: 기본 배경이 아닌 이어진 같은 색 칸은 사각형 하나([`BgSpan`]).
//! 2. **run 배치**: 같은 폭 종류(1칸/2칸)로 이어진 칸을 셰이핑 한 번([`Segment`])으로 묶는다. 셰이핑 run은 글꼴(굵게·기울임)로만
//!    나누고 색은 넣지 않는다 — 색이 run을 나누면 CoreText run이 칸 수만큼 생겨 truecolor 화면에서 프레임 p99 67.8ms였다.
//!    색은 글리프마다 [`Segment::color_at`]으로 칠한다.
//! 3. **셀 폭 강제**: 2칸 묶음은 칸 폭 × 2, 나머지는 칸 폭으로 글리프 간격을 고정한다(`force_width`).
//! 4. **밑줄·취소선**(OWE-481): 셰이핑에 넣지 않고 배경처럼 같은 색 이웃 칸을 구간 하나([`Span`])로 합친다. 그리는 쪽은
//!    글자 레이어에 사각형으로 칠한다(레이어 2장 유지). 흐림(DIM)은 글자색을 배경 쪽으로 섞고, 숨김(HIDDEN)은 글자와
//!    밑줄·취소선을 그리지 않는다(배경은 그린다).
//! 5. **이모지·ZWJ·VS16**(MK-058): [`crate::width`] 묶음은 셰이핑 한 번으로 합친다. ZWJ 묶음은 왼쪽 2칸에 그리고
//!    나머지는 빈칸으로 둔다(뒤 텍스트를 당기지 않는다). VS16 1칸 묶음은 오른쪽이 빈칸일 때만 2칸으로 넘쳐 그린다.
//!    엔진 셀·커서·주소는 그대로다(지휘자 결정 A').

pub use krill_proto::screen::{Cell, Color, Cursor, Flags, Screen};
use krill_proto::theme::{Rgb, Theme};

/// 같은 색 이웃 칸을 합친 가로 구간(배경 사각형·밑줄·취소선).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub column: u16,
    pub cols: u16,
    pub color: Rgb,
}

/// 셰이핑 run: `len` 바이트를 같은 글꼴로.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub len: usize,
    pub bold: bool,
    pub italic: bool,
}

/// 셰이핑 한 번으로 그리는 이어진 칸들.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// 첫 칸 열.
    pub column: u16,
    /// 2칸 글자 묶음이면 참. 글리프 간격(`force_width`)이 칸 폭 × 2가 된다.
    /// ZWJ 묶음(MK-058)은 첫 칸이 1칸이어도 참이 된다(왼쪽 2칸에 그린다).
    pub wide: bool,
    /// 1칸 VS16 묶음을 오른쪽 빈칸에 넘쳐 그려도 되면 참(MK-058). 그리는 쪽이 2칸 간격으로 칠한다.
    pub overflow: bool,
    pub text: String,
    pub runs: Vec<Run>,
    /// 칸마다 (`text` 안 시작 바이트, 전경색). 시작 바이트 오름차순.
    pub colors: Vec<(usize, Rgb)>,
}

impl Segment {
    /// `text`의 바이트 `index`(글리프의 원문 위치)가 속한 칸의 전경색.
    pub fn color_at(&self, index: usize) -> Option<Rgb> {
        let i = self.colors.partition_point(|&(start, _)| start <= index);
        self.colors.get(i.checked_sub(1)?).map(|&(_, c)| c)
    }
}

/// 한 행을 그리는 데 필요한 것: 배경 사각형(먼저) + 셰이핑 단위·밑줄·취소선(나중).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowPlan {
    /// 기본 배경이 아닌 칸만.
    pub bg: Vec<Span>,
    pub segments: Vec<Segment>,
    pub underlines: Vec<Span>,
    pub strikes: Vec<Span>,
}

/// 칸 색을 테마로 풀어 RGB로.
pub fn resolve(theme: &Theme, color: Color, foreground: bool) -> Rgb {
    match color {
        Color::Default if foreground => theme.foreground,
        Color::Default => theme.background,
        Color::Rgb(rgb) => rgb,
        Color::Indexed(i @ 0..=15) => theme.ansi[usize::from(i)],
        Color::Indexed(i @ 16..=231) => {
            let i = i - 16;
            let level = |x: u8| if x == 0 { 0 } else { 55 + 40 * x };
            Rgb {
                r: level(i / 36),
                g: level(i / 6 % 6),
                b: level(i % 6),
            }
        }
        Color::Indexed(i) => {
            let g = 8 + 10 * (i - 232);
            Rgb { r: g, g, b: g }
        }
    }
}

/// (글자색, 배경) RGB. `INVERSE`면 바꾸고, `DIM`이면 바꾼 뒤의 글자색을 배경 쪽으로 섞는다.
fn colors(theme: &Theme, cell: &Cell) -> (Rgb, Rgb) {
    let fg = resolve(theme, cell.fg, true);
    let bg = resolve(theme, cell.bg, false);
    let (fg, bg) = if cell.flags.contains(Flags::INVERSE) {
        (bg, fg)
    } else {
        (fg, bg)
    };
    if !cell.flags.contains(Flags::DIM) {
        return (fg, bg);
    }
    let mix = |f: u8, b: u8| u8::try_from((2 * u16::from(f) + u16::from(b)) / 3).unwrap_or(f);
    let dim = Rgb {
        r: mix(fg.r, bg.r),
        g: mix(fg.g, bg.g),
        b: mix(fg.b, bg.b),
    };
    (dim, bg)
}

/// 구간 끝에 바로 붙는 같은 색 칸이면 늘리고, 아니면 새 구간.
fn extend(spans: &mut Vec<Span>, column: u16, cols: u16, color: Rgb) {
    match spans.last_mut() {
        Some(span) if span.column.saturating_add(span.cols) == column && span.color == color => {
            span.cols = span.cols.saturating_add(cols);
        }
        _ => spans.push(Span {
            column,
            cols,
            color,
        }),
    }
}

/// 한 행의 그리기 계획.
///
/// [`crate::width::clusters`] 묶음마다 셰이핑 단위를 정한다. 묶음이 한 칸이면 예전처럼 같은 폭 이웃과
/// 합친다(다만 폭 0 문자가 붙은 칸은 혼자 묶는다, MK-002). ZWJ로 이어진 여러 칸 묶음과 넘침(VS16) 묶음은
/// 혼자 묶음이다: ZWJ 묶음은 왼쪽 2칸에 그리고 나머지는 빈칸으로 두며, 넘침 묶음은 오른쪽 빈칸에 넘쳐 그린다.
/// 꼬리 칸의 배경·밑줄·취소선은 그대로 둔다(엔진 열과 같다). 칸마다 글자색·글꼴도 묶음에 적어 둔다.
pub fn plan_row(row: &[Cell], theme: &Theme) -> RowPlan {
    let mut plan = RowPlan::default();
    let mut seg: Option<Segment> = None;
    // 다음 칸이 이어 붙을 열(직전 칸의 끝).
    let mut next = 0;
    for cluster in crate::width::clusters(row) {
        let begin = usize::from(cluster.start);
        let end = usize::from(cluster.end).min(row.len());
        // 묶음 안의 실제 칸 수(자리 칸 제외). 2칸 글자 하나는 한 칸이다.
        let cells = row[begin..end]
            .iter()
            .filter(|c| !c.flags.contains(Flags::SPACER))
            .count();
        // 여러 칸 묶음·넘침 묶음은 혼자 묶는다.
        let standalone = cells > 1 || cluster.overflow;
        if standalone {
            flush(&mut plan, seg.take());
            seg = Some(Segment {
                column: cluster.start,
                wide: true,
                overflow: cluster.overflow,
                text: String::new(),
                runs: Vec::new(),
                colors: Vec::new(),
            });
        }
        for (column, cell) in (cluster.start..).zip(row[begin..end].iter()) {
            if cell.flags.contains(Flags::SPACER) {
                continue;
            }
            let wide = cell.flags.contains(Flags::WIDE);
            let width: u16 = if wide { 2 } else { 1 };
            let (fg, bg) = colors(theme, cell);

            if bg != theme.background {
                extend(&mut plan.bg, column, width, bg);
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);
            if !hidden {
                if cell.flags.contains(Flags::UNDERLINE) || cell.flags.contains(Flags::LINK) {
                    let color = match cell.underline {
                        Color::Default => fg,
                        c => resolve(theme, c, true),
                    };
                    extend(&mut plan.underlines, column, width, color);
                }
                if cell.flags.contains(Flags::STRIKEOUT) {
                    extend(&mut plan.strikes, column, width, fg);
                }
            }

            // 폭 0 문자가 붙은 칸은 앞 칸과 한 클러스터로 셰이핑될 수 있어(MK-002) 혼자 묶는다.
            // 숨긴 칸은 공백으로 넣어 묶음을 끊지 않는다. 묶음 여러 칸·넘침은 위에서 이미 따로 뺐다.
            let alone = !standalone && !hidden && !cell.zerowidth.is_empty();
            let joins = !standalone
                && !alone
                && seg
                    .as_ref()
                    .is_some_and(|s| !s.overflow && s.wide == wide && next == column);
            if !joins && !standalone {
                flush(&mut plan, seg.take());
                seg = Some(Segment {
                    column,
                    wide,
                    overflow: false,
                    text: String::new(),
                    runs: Vec::new(),
                    colors: Vec::new(),
                });
            }
            let Some(s) = seg.as_mut() else { continue };
            let start = s.text.len();
            if hidden {
                s.text.push(' ');
            } else {
                s.text.push(cell.ch);
                s.text.extend(&cell.zerowidth);
            }
            s.colors.push((start, fg));
            let (bold, italic) = (
                cell.flags.contains(Flags::BOLD),
                cell.flags.contains(Flags::ITALIC),
            );
            let len = s.text.len() - start;
            match s.runs.last_mut() {
                Some(run) if run.bold == bold && run.italic == italic => run.len += len,
                _ => s.runs.push(Run { len, bold, italic }),
            }
            next = column.saturating_add(width);
            if alone {
                flush(&mut plan, seg.take());
            }
        }
        if standalone {
            flush(&mut plan, seg.take());
            next = cluster.end;
        }
    }
    flush(&mut plan, seg);
    plan
}

/// 공백뿐인 묶음은 그릴 글리프가 없어 버린다.
fn flush(plan: &mut RowPlan, seg: Option<Segment>) {
    if let Some(s) = seg
        && !s.text.trim().is_empty()
    {
        plan.segments.push(s);
    }
}

#[cfg(test)]
mod tests;
