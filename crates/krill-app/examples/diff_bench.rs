//! MK-054 native Metal offscreen scroll benchmark. Runs on a locked display too.
//! Reproduce: `cargo run --locked --release -p krill-app --example diff_bench`
use std::fmt::Write as _;
use std::process::Command;
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Context, Entity, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size,
};
use gpui_component::ActiveTheme as _;
use gpui_component::diff::{Diff, DiffFile, DiffLinePosition, DiffSide, DiffState};

struct Surface {
    state: Entity<DiffState>,
}
impl Render for Surface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(Diff::new(&self.state).size_full())
    }
}

fn footprint() -> Result<u64, Box<dyn std::error::Error>> {
    let output = Command::new("/usr/bin/footprint")
        .args(["-f", "bytes", "-p", &std::process::id().to_string()])
        .output()?;
    if !output.status.success() {
        return Err("footprint failed".into());
    }
    let report = String::from_utf8(output.stdout)?;
    let line = report
        .lines()
        .find(|l| l.trim_start().starts_with("phys_footprint:"))
        .ok_or("missing phys_footprint")?;
    Ok(line
        .split_whitespace()
        .nth(1)
        .ok_or("missing footprint value")?
        .replace(',', "")
        .parse()?)
}

fn fixture() -> Result<Vec<DiffFile>, Box<dyn std::error::Error>> {
    // 2500 removed + 2500 added rows: strings, comments, identifiers, numbers and UTF-8.
    let mut source = String::from(
        "diff --git a/review.rs b/review.rs\n--- a/review.rs\n+++ b/review.rs\n@@ -1,2500 +1,2500 @@\n",
    );
    for i in 0..2500 {
        writeln!(source, "-let old_{i} = \"한글🙂\"; // previous value")?;
    }
    for i in 0..2500 {
        writeln!(source, "+let value_{i} = {i}; // reviewed value 한글🙂")?;
    }
    Ok(DiffFile::parse(&source)?)
}

fn main() {
    gpui_platform::application().run(|cx| {
        gpui_component::init(cx);
        let options = WindowOptions { window_bounds: Some(WindowBounds::Windowed(Bounds::new(gpui::point(px(0.), px(0.)), size(px(1000.), px(700.))))), ..Default::default() };
        let opened = cx.open_window(options, |_, cx| cx.new(|cx| Surface { state: cx.new(|cx| DiffState::new([], cx).with_inline_unit(None)) }));
        let Ok(win) = opened else { eprintln!("BENCH_ERROR open window"); cx.quit(); return; };
        cx.spawn(async move |cx| {
            let result = async {
                let state = win.update(cx, |view, _, _| view.state.clone())?;
                // Warm renderer and IOSurface allocation before the baseline.
                for _ in 0..30 { cx.update_window(win.into(), |_, window, cx| { window.draw(cx).clear(cx); window.render_to_image() })??; }
                let before = footprint()?;
                let files = fixture()?;
                state.update(cx, |state, cx| state.set_files(files, cx));
                // Draw starts background syntax preparation; allow it to finish outside measured frames.
                cx.update_window(win.into(), |_, window, cx| window.draw(cx).clear(cx))?;
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let mut times = Vec::new();
                for frame in 0..360usize {
                    let (side, line) = if frame % 2 == 0 { (DiffSide::Original, 1 + (frame * 53) % 2450) } else { (DiffSide::Modified, 1 + (frame * 53) % 2450) };
                    let started = Instant::now();
                    state.update(cx, |state, cx| state.scroll_to_line(DiffLinePosition::new("review.rs", side, line), cx));
                    cx.update_window(win.into(), |_, window, cx| {
                        window.draw(cx).clear(cx);
                        let image = window.render_to_image()?;
                        drop(image);
                        Ok::<_, Box<dyn std::error::Error>>(())
                    })??;
                    if frame >= 60 { times.push(started.elapsed().as_secs_f64() * 1000.); }
                    // Yield so background presentation and window events are serviced.
                    cx.background_executor().timer(Duration::from_millis(1)).await;
                }
                let after = footprint()?;
                cx.update_window(win.into(), |_, window, _| -> Result<(), Box<dyn std::error::Error>> { let image = window.render_to_image()?; println!("MK054 image={}x{} visible={}", image.width(), image.height(), window.is_visible()); image.save("target/mk054-diff.png")?; Ok(()) })??;
                times.sort_by(f64::total_cmp);
                let sum: f64 = times.iter().sum();
                println!("MK054 rows=5000 samples={} mean_ms={:.3} p95_ms={:.3} p99_ms={:.3} max_ms={:.3} baseline_bytes={before} diff_bytes={after} delta_bytes={} mode=draw+render_to_image", times.len(), sum / 300., times[284], times[296], times[299], after.saturating_sub(before));
                Ok::<_, Box<dyn std::error::Error>>(())
            }.await;
            if let Err(e) = result { eprintln!("BENCH_ERROR {e:#}"); }
            cx.update(|cx| cx.quit());
        }).detach();
    });
}
