//! 같은 입력을 이전/변경 바이너리에 재생하는 수동 행 계획 벤치. 화면 생성·정확성 검사는 시간 밖이다.

use super::*;
use std::hint::black_box;
use std::time::{Duration, Instant};

const COLS: u16 = 160;
const LINES: u16 = 48;
const FRAMES: usize = 48;
const ROUNDS: usize = 9;

#[derive(Debug, Clone, Copy)]
enum Workload {
    SameScreen,
    Metadata,
    SingleCell,
    ScrollUp,
    ScrollDown,
    FullRedraw,
    CommonPrefix,
    Theme,
}

impl Workload {
    fn name(self) -> &'static str {
        match self {
            Self::SameScreen => "same_screen",
            Self::Metadata => "metadata",
            Self::SingleCell => "single_cell",
            Self::ScrollUp => "scroll_up_one",
            Self::ScrollDown => "scroll_down_one",
            Self::FullRedraw => "full_redraw",
            Self::CommonPrefix => "full_common_prefix",
            Self::Theme => "theme_change",
        }
    }
}

fn row(serial: usize, late_identity: bool) -> Vec<Cell> {
    let mut cells: Vec<_> = (0..COLS)
        .map(|column| Cell {
            ch: if column % 8 == 0 { ' ' } else { 'x' },
            fg: Color::Indexed(u8::try_from(column % 16).unwrap()),
            flags: if column % 13 == 0 {
                Flags::UNDERLINE | Flags::BOLD
            } else {
                Flags::default()
            },
            ..Cell::default()
        })
        .collect();
    let identity = format!("{serial:08x}");
    let offset = if late_identity {
        cells.len() - identity.len()
    } else {
        0
    };
    for (cell, ch) in cells.iter_mut().skip(offset).zip(identity.chars()) {
        cell.ch = ch;
    }
    cells
}

fn frames(workload: Workload) -> Vec<(Rc<Screen>, Theme)> {
    let late_identity = matches!(workload, Workload::CommonPrefix);
    let mut screen = Screen::blank(COLS, LINES);
    screen.rows = (0..usize::from(LINES))
        .map(|line| row(line, late_identity))
        .collect();
    let mut output = vec![(Rc::new(screen), DEFAULT)];
    for frame in 1..=FRAMES {
        let (previous, _) = output.last().unwrap();
        if matches!(workload, Workload::SameScreen) {
            output.push((Rc::clone(previous), DEFAULT));
            continue;
        }
        let mut screen = previous.as_ref().clone();
        let serial = (frame + 1) * usize::from(LINES);
        let mut theme = DEFAULT;
        match workload {
            Workload::SameScreen => unreachable!(),
            Workload::Metadata => {
                screen.cursor.column = u16::try_from(frame % usize::from(COLS)).unwrap();
                screen.title = format!("frame-{frame}");
            }
            Workload::SingleCell => {
                screen.rows.last_mut().unwrap().last_mut().unwrap().ch =
                    if frame % 2 == 0 { '/' } else { '|' };
            }
            Workload::ScrollUp => {
                screen.rows.rotate_left(1);
                *screen.rows.last_mut().unwrap() = row(serial, false);
            }
            Workload::ScrollDown => {
                screen.rows.rotate_right(1);
                screen.rows[0] = row(serial, false);
            }
            Workload::FullRedraw | Workload::CommonPrefix => {
                screen.rows = (serial..serial + usize::from(LINES))
                    .map(|line| row(line, late_identity))
                    .collect();
            }
            Workload::Theme => {
                if frame % 2 == 1 {
                    theme.foreground = Rgb { r: 1, g: 2, b: 3 };
                }
            }
        }
        output.push((Rc::new(screen), theme));
    }
    output
}

fn verify_frames(frames: &[(Rc<Screen>, Theme)]) -> usize {
    let cache = RowPlanCache::default();
    let mut reused = 0;
    for (index, (screen, theme)) in frames.iter().enumerate() {
        // 포인터 관찰은 별도 검증 단계에서만 한다. 측정 중 이전 계획을 붙잡아 해제 비용을 숨기지 않는다.
        let previous = cache.0.borrow().plans.clone();
        let plans = cache.plans(screen, theme);
        assert_eq!(plans.len(), screen.rows.len());
        for (row, plan) in screen.rows.iter().zip(plans.iter()) {
            assert_eq!(plan.as_ref(), &plan_row(row, theme));
        }
        if index == FRAMES {
            reused = plans
                .iter()
                .filter(|plan| previous.iter().any(|old| Rc::ptr_eq(plan, old)))
                .count();
        }
    }
    reused
}

fn replay(frames: &[(Rc<Screen>, Theme)]) -> Duration {
    let cache = RowPlanCache::default();
    cache.plans(&frames[0].0, &frames[0].1);
    let start = Instant::now();
    for (screen, theme) in frames.iter().skip(1) {
        black_box(cache.plans(black_box(screen), black_box(theme)));
    }
    start.elapsed()
}

/// native paint・GPU·앱 전체가 아니라 행 계획 준비 비용만 비교한다.
#[test]
#[ignore = "수동: 같은 테스트 실행 파일을 baseline/candidate 순서를 바꿔 실행"]
fn row_plan_workload_benchmark() {
    let variant = std::env::var("KRILL_ROW_CACHE_BENCH_VARIANT").unwrap_or_else(|_| "unset".into());
    for workload in [
        Workload::SameScreen,
        Workload::Metadata,
        Workload::SingleCell,
        Workload::ScrollUp,
        Workload::ScrollDown,
        Workload::FullRedraw,
        Workload::CommonPrefix,
        Workload::Theme,
    ] {
        let input = frames(workload);
        let reused = verify_frames(&input);
        for _ in 0..3 {
            black_box(replay(&input));
        }
        let mut samples: Vec<_> = (0..ROUNDS)
            .map(|_| replay(&input).as_nanos() / u128::try_from(FRAMES).unwrap())
            .collect();
        samples.sort_unstable();
        println!(
            "row-cache-matrix variant={variant} workload={} cols={COLS} rows={LINES} frames={FRAMES} rounds={ROUNDS} warmup=3 min_ns={} median_ns={} max_ns={} reused_last_rows={reused}",
            workload.name(),
            samples[0],
            samples[ROUNDS / 2],
            samples[ROUNDS - 1],
        );
    }
}
