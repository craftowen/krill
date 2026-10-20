use super::*;
use krill_proto::theme::{DEFAULT, Rgb};
use krill_term::grid::{Cell, Color, Flags};

#[test]
fn reuses_unchanged_rows_and_rebuilds_changed_content_and_theme() {
    let cache = RowPlanCache::default();
    let mut screen = Screen::blank(6, 3);
    for row in &mut screen.rows {
        row.resize(6, Cell::default());
    }
    screen.rows[0][0].ch = '한';
    screen.rows[0][0].flags = Flags::WIDE | Flags::BOLD;
    screen.rows[0][1].flags = Flags::SPACER;
    screen.rows[1][0].ch = 'x';
    screen.rows[1][0].flags = Flags::UNDERLINE;
    let screen = Rc::new(screen);
    let initial = cache.plans(&screen, &DEFAULT);
    assert!(Rc::ptr_eq(&initial, &cache.plans(&screen, &DEFAULT)));

    // 출력 내용은 그대로인 커서/제목 갱신에서도 행 계획을 새로 만들지 않는다.
    let mut moved = (*screen).clone();
    moved.cursor.column = 3;
    moved.title = "new title".into();
    let moved = Rc::new(moved);
    let cursor_only = cache.plans(&moved, &DEFAULT);
    for (old, next) in initial.iter().zip(cursor_only.iter()) {
        assert!(Rc::ptr_eq(old, next));
    }

    // 색·장식만 바뀌어도 해당 행만 무효화한다.
    let mut changed = (*moved).clone();
    changed.rows[1][0].fg = Color::Indexed(3);
    changed.rows[1][0].flags |= Flags::STRIKEOUT;
    let changed = Rc::new(changed);
    let output = cache.plans(&changed, &DEFAULT);
    assert!(Rc::ptr_eq(&cursor_only[0], &output[0]));
    assert!(!Rc::ptr_eq(&cursor_only[1], &output[1]));
    assert!(Rc::ptr_eq(&cursor_only[2], &output[2]));
    for (row, plan) in changed.rows.iter().zip(output.iter()) {
        assert_eq!(plan.as_ref(), &plan_row(row, &DEFAULT));
    }

    let mut light = DEFAULT;
    light.foreground = Rgb { r: 1, g: 2, b: 3 };
    let themed = cache.plans(&changed, &light);
    for ((old, next), row) in output.iter().zip(themed.iter()).zip(&changed.rows) {
        assert!(!Rc::ptr_eq(old, next));
        assert_eq!(next.as_ref(), &plan_row(row, &light));
    }
}

#[test]
fn scroll_resize_and_replacement_do_not_keep_old_rows_or_screens() {
    let cache = RowPlanCache::default();
    let mut old = Screen::blank(4, 3);
    for (row, ch) in old.rows.iter_mut().zip(['a', 'b', 'c']) {
        row.resize(4, Cell::default());
        row[0].ch = ch;
    }
    let old = Rc::new(old);
    let weak = Rc::downgrade(&old);
    cache.plans(&old, &DEFAULT);
    let mut scrolled = (*old).clone();
    scrolled.rows.rotate_left(1);
    scrolled.rows.pop();
    scrolled.lines = 2;
    let next = Rc::new(scrolled);
    let plans = cache.plans(&next, &DEFAULT);
    assert_eq!(plans.len(), 2);
    for (row, plan) in next.rows.iter().zip(plans.iter()) {
        assert_eq!(plan.as_ref(), &plan_row(row, &DEFAULT));
    }
    drop(old);
    assert!(
        weak.upgrade().is_none(),
        "직전보다 오래된 화면은 보관하지 않는다"
    );
    let empty = Rc::new(Screen::default());
    assert!(cache.plans(&empty, &DEFAULT).is_empty());
}

/// 앱 전체 CPU가 아니라 동일 화면의 행 계획 준비 비용만 비교하는 수동 벤치다.
#[test]
#[ignore = "수동 행 렌더 계획 벤치: --ignored --nocapture로 실행"]
fn row_plan_cache_benchmark() {
    use std::hint::black_box;
    use std::time::Instant;
    const FRAMES: usize = 500;
    let mut screen = Screen::blank(160, 48);
    for (line, row) in screen.rows.iter_mut().enumerate() {
        row.resize(160, Cell::default());
        for (column, cell) in row.iter_mut().enumerate() {
            cell.ch = if column % 8 == 0 { ' ' } else { 'x' };
            cell.fg = Color::Indexed(u8::try_from((line + column) % 16).unwrap());
            if column % 13 == 0 {
                cell.flags = Flags::UNDERLINE | Flags::BOLD;
            }
        }
    }
    let screen = Rc::new(screen);
    let start = Instant::now();
    for _ in 0..FRAMES {
        let plans: Vec<_> = screen.rows.iter().map(|r| plan_row(r, &DEFAULT)).collect();
        black_box(plans);
    }
    let uncached = start.elapsed();
    let cache = RowPlanCache::default();
    cache.plans(&screen, &DEFAULT);
    let start = Instant::now();
    for _ in 0..FRAMES {
        black_box(cache.plans(black_box(&screen), black_box(&DEFAULT)));
    }
    let cached = start.elapsed();
    println!(
        "row-plan-only cols=160 rows=48 frames={FRAMES} uncached_us={} cached_same_screen_us={}",
        uncached.as_micros(),
        cached.as_micros()
    );
}
