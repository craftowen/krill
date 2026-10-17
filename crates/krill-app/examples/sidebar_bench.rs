//! Native macOS MK-046 benchmark: 500 rows, paced scrolling, stable polling idle.
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, Subscription, Window, WindowBounds,
    WindowOptions, canvas, div, prelude::*, px, size,
};
use krill_app::sidebar::{Session, Sidebar, Snapshot, Source};
use krill_proto::{SessionId, control::AgentStatus};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

struct Memory(Snapshot);
impl Source for Memory {
    fn fetch(&self) -> io::Result<Snapshot> {
        Ok(self.0.clone())
    }
}

type Samples = Rc<RefCell<Vec<(Instant, Duration)>>>;
struct Bench {
    sidebar: Entity<Sidebar>,
    renders: Rc<Cell<usize>>,
    measuring: Rc<Cell<bool>>,
    samples: Samples,
    _subscription: Subscription,
}
impl Render for Bench {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let start = Instant::now();
        self.renders.set(self.renders.get() + 1);
        let samples = self.samples.clone();
        let measuring = self.measuring.clone();
        div()
            .relative()
            .size_full()
            .font_family("Menlo")
            .text_sm()
            .child(self.sidebar.clone())
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, (), _, _| {
                        if measuring.get() {
                            samples.borrow_mut().push((start, start.elapsed()));
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

fn main() -> ExitCode {
    let failed = Arc::new(AtomicBool::new(false));
    let result = failed.clone();
    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        let trees: Vec<_> = (0..250).map(|i| PathBuf::from(format!("/repo/branch-{i:03}"))).collect();
        let sessions = trees.iter().take(249).enumerate().filter_map(|(i, p)| Some(Session {
            id: SessionId(u64::try_from(i).ok()?), status: AgentStatus::UNKNOWN, worktree: Some(p.clone()), cwd: Some(p.clone()), account: None,
        })).collect();
        let source: Arc<dyn Source> = Arc::new(Memory(Snapshot { sessions, trees: BTreeMap::from([(PathBuf::from("/repo"), trees)]) }));
        let renders = Rc::new(Cell::new(0));
        let samples: Samples = Rc::default();
        let measuring = Rc::new(Cell::new(false));
        let opened = cx.open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(320.), px(760.)), cx))),
            ..WindowOptions::default()
        }, |_, cx| cx.new(|cx| {
            let sidebar = cx.new(|cx| { let mut sidebar = Sidebar::new(None); sidebar.connect_source(source, cx); sidebar });
            let subscription = cx.observe(&sidebar, |_, _, cx| cx.notify());
            Bench { sidebar, renders: renders.clone(), samples: samples.clone(), measuring: measuring.clone(), _subscription: subscription }
        }));
        let Ok(handle) = opened else { result.store(true, Ordering::Relaxed); cx.quit(); return; };
        cx.activate(true);
        cx.spawn(async move |cx| {
            let bg = cx.background_executor().clone();
            bg.timer(Duration::from_secs(1)).await;
            let sidebar = handle.update(cx, |bench, _, _| bench.sidebar.clone()).ok();
            let Some(sidebar) = sidebar else { result.store(true, Ordering::Relaxed); cx.update(|cx| cx.quit()); return; };
            let mut count = 0;
            let started = Instant::now();
            let period = Duration::from_nanos(16_666_667);
            measuring.set(true);
            for i in 0..240u32 {
                handle.update(cx, |_, _, cx| sidebar.update(cx, |s, cx| { s.scroll_to_row((usize::try_from(i).unwrap_or(0) * 7) % 500, cx); count = s.row_count(); })).ok();
                let deadline = started + period.saturating_mul(i + 1);
                bg.timer(deadline.saturating_duration_since(Instant::now())).await;
            }
            let elapsed = started.elapsed();
            bg.timer(Duration::from_millis(500)).await;
            measuring.set(false);
            let before = renders.get();
            bg.timer(Duration::from_secs(2)).await;
            let idle = renders.get().saturating_sub(before);
            let samples = samples.borrow();
            let mut intervals: Vec<_> = samples.windows(2).map(|pair| pair[1].0.duration_since(pair[0].0)).collect();
            let mut durations: Vec<_> = samples.iter().map(|(_, d)| *d).collect();
            durations.sort_unstable(); intervals.sort_unstable();
            let p99 = durations.get(durations.len() * 99 / 100).copied().unwrap_or(Duration::MAX);
            println!("MK-046 native rows={count} frames={} elapsed_ms={} p50_render_us={} p99_render_us={} max_render_us={} p50_frame_interval_us={} p99_frame_interval_us={} idle_renders={idle}", samples.len(), elapsed.as_millis(), durations.get(durations.len()/2).map_or(0, Duration::as_micros), p99.as_micros(), durations.last().map_or(0, Duration::as_micros), intervals.get(intervals.len()/2).map_or(0, Duration::as_micros), intervals.get(intervals.len()*99/100).map_or(0, Duration::as_micros));
            result.store(count != 500 || p99 > period || idle != 0 || samples.len() < 200, Ordering::Relaxed);
            cx.update(|cx| cx.quit());
        }).detach();
    });
    if failed.load(Ordering::Relaxed) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
