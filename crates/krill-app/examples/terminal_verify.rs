//! macOS 네이티브 Metal readback에서 캐시·비캐시 터미널을 픽셀 단위로 비교한다.
//! `cargo run --locked -p krill-app --example terminal_verify -- <비공개 출력 디렉터리>`
//! 설정 초기화·데몬·PTY는 사용하지 않는다. 시간은 강제 draw와 GPU readback이며 입력→표시 지연은 아니다.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, AsyncApp, Bounds, Context, Render, Window, WindowBounds, WindowHandle,
    WindowOptions, div, point, prelude::*, px, size,
};
use krill_app::terminal::{Highlight, Preedit, RowPlanCache, TerminalElement};
use krill_proto::screen::Link;
use krill_proto::theme::{DEFAULT, Theme};
use krill_term::grid::{Cell, Color, Flags, Screen};
use krill_term::ime::CellPoint;
use krill_term::select::{Point, Selection};
use sha2::{Digest as _, Sha256};

const FRAMES: usize = 12;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone)]
struct Frame {
    screen: Rc<Screen>,
    theme: Theme,
    rem: f32,
    overlay: bool,
}

struct Surface {
    frame: Frame,
    cache: RowPlanCache,
    cached: bool,
}

impl Render for Surface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut terminal = TerminalElement::new(self.frame.screen.clone(), self.frame.theme);
        if self.cached {
            terminal = terminal.with_row_cache(self.cache.clone());
        }
        if self.frame.overlay {
            terminal.selection = Some(Selection {
                anchor: Point { line: 1, column: 5 },
                head: Point {
                    line: 3,
                    column: 28,
                },
                block: false,
            });
            terminal.preedit = Some(Preedit {
                origin: CellPoint { line: 4, column: 8 },
                text: "한글👩\u{200d}💻".into(),
                columns: 6,
            });
            terminal.link = Some(Highlight {
                line: 2,
                column: 30,
                cols: 8,
            });
            terminal.highlight = Some(Highlight {
                line: 5,
                column: 9,
                cols: 6,
            });
        }
        div()
            .size_full()
            .bg(krill_app::ui::palette(cx).background)
            .child(terminal)
    }
}

fn row(serial: usize, cols: u16) -> Vec<Cell> {
    let mut cells = vec![Cell::default(); usize::from(cols)];
    for (index, cell) in cells.iter_mut().enumerate() {
        cell.ch = if index % 8 == 0 { ' ' } else { 'x' };
        cell.fg = Color::Indexed(u8::try_from(index % 16).unwrap_or(0));
    }
    for (cell, ch) in cells.iter_mut().zip(format!("{serial:08x}").chars()) {
        cell.ch = ch;
    }
    cells[12].ch = '한';
    cells[12].flags = Flags::WIDE | Flags::BOLD;
    cells[13].flags = Flags::SPACER;
    cells[14].ch = 'ᄀ';
    cells[14].zerowidth = vec!['ᅡ'];
    cells[16].ch = '👩';
    cells[16].zerowidth = vec!['\u{200d}', '💻'];
    cells[16].flags = Flags::WIDE;
    cells[17].flags = Flags::SPACER;
    cells[19].ch = '❤';
    cells[19].zerowidth = vec!['\u{fe0f}'];
    cells[20].ch = ' ';
    cells[22].flags = Flags::UNDERLINE | Flags::STRIKEOUT;
    cells[22].underline = Color::Indexed(3);
    cells[23].flags = Flags::DIM;
    cells[24].flags = Flags::HIDDEN;
    for cell in cells.iter_mut().skip(30).take(8) {
        cell.flags |= Flags::LINK;
    }
    cells
}

fn fixture(cols: u16, lines: u16, serial: usize) -> Screen {
    let mut screen = Screen::blank(cols, lines);
    screen.rows = (0..usize::from(lines))
        .map(|line| row(serial + line, cols))
        .collect();
    screen.links = (0..lines)
        .map(|line| {
            vec![Link {
                column: 30,
                cols: 8,
                uri: format!("https://example.com/{serial}/{line}"),
            }]
        })
        .collect();
    screen.cursor.line = 3;
    screen.cursor.column = 8;
    screen
}

fn frames(workload: &str) -> Result<Vec<Frame>> {
    let mut previous = fixture(96, 24, 0);
    (0..FRAMES)
        .map(|frame| {
            let mut screen = previous.clone();
            let mut theme = DEFAULT;
            let mut rem = 16.;
            let serial = 100 + frame * 100;
            match workload {
                "scroll_up" | "multiple_rows" | "mixed_updates" => {
                    let shift = if workload == "multiple_rows" { 3 } else { 1 };
                    screen.rows.rotate_left(shift);
                    let length = screen.rows.len();
                    for (index, cells) in screen.rows.iter_mut().enumerate().skip(length - shift) {
                        *cells = row(serial + index, screen.cols);
                    }
                    if workload == "mixed_updates" {
                        screen.rows[0][0].ch = 'A';
                        screen.rows[1][0].ch = 'B';
                    }
                }
                "scroll_down" => {
                    screen.rows.rotate_right(1);
                    screen.rows[0] = row(serial, screen.cols);
                }
                "single_cell" => screen.rows[3][40].ch = if frame % 2 == 0 { '/' } else { '|' },
                "full_redraw" => screen = fixture(96, 24, serial),
                "theme" => {
                    if frame % 2 == 0 {
                        theme.foreground = DEFAULT.background;
                        theme.background = DEFAULT.foreground;
                    }
                }
                "resize" => {
                    let (cols, lines) = [(72, 16), (96, 24), (120, 32)][frame % 3];
                    screen = fixture(cols, lines, serial);
                    rem = if frame % 2 == 0 { 16. } else { 18. };
                }
                "overlays" => {
                    screen.cursor.column = u16::try_from(8 + frame).unwrap_or(8);
                    screen.links[2][0].uri = format!("https://example.com/changed/{frame}");
                }
                _ => return Err(format!("unknown workload: {workload}").into()),
            }
            previous = screen.clone();
            Ok(Frame {
                screen: Rc::new(screen),
                theme,
                rem,
                overlay: workload == "overlays" && frame % 2 == 0,
            })
        })
        .collect()
}

struct Rendered {
    bytes: Vec<u8>,
    draw_us: f64,
    readback_us: f64,
    dimensions: (u32, u32),
}

fn draw(
    handle: WindowHandle<Surface>,
    frame: &Frame,
    cached: bool,
    save: Option<&Path>,
    cx: &mut AsyncApp,
) -> Result<Rendered> {
    handle.update(cx, |view, _, cx| {
        view.frame = frame.clone();
        view.cached = cached;
        cx.notify();
    })?;
    cx.update_window(handle.into(), |_, window, cx| {
        window.set_rem_size(px(frame.rem));
        window.refresh();
        let started = Instant::now();
        window.draw(cx).clear(cx);
        let draw_us = started.elapsed().as_secs_f64() * 1_000_000.;
        let started = Instant::now();
        let image = window.render_to_image()?;
        let readback_us = started.elapsed().as_secs_f64() * 1_000_000.;
        let dimensions = (image.width(), image.height());
        if let Some(path) = save {
            image.save(path)?;
        }
        Ok(Rendered {
            bytes: image.into_raw(),
            draw_us,
            readback_us,
            dimensions,
        })
    })?
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn main() -> ExitCode {
    let Some(output) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: terminal_verify <private-output-directory>");
        return ExitCode::FAILURE;
    };
    if std::env::args_os()
        .nth(2)
        .is_some_and(|arg| arg == "--native-worker")
    {
        return native(output);
    }
    // AppKit의 terminate는 main의 반환값과 관계없이 0으로 종료할 수 있다.
    // GUI를 띄우지 않는 부모가 작업 완료 증거까지 확인하여 실패를 성공으로 바꾸지 않게 한다.
    match supervise(&output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("TERMINAL_NATIVE_SUPERVISOR_ERROR {error}");
            ExitCode::FAILURE
        }
    }
}

fn supervise(output: &Path) -> Result<()> {
    std::fs::create_dir_all(output)?;
    let _owner = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join(".verification-owner"))?;
    if output.join("results.json").exists() {
        return Err("output already contains results; use a fresh directory".into());
    }
    let mut worker = Command::new(std::env::current_exe()?)
        .arg(output)
        .arg("--native-worker")
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Some(status) = worker.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            worker.kill()?;
            worker.wait()?;
            return Err("native worker timed out".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        return Err(format!("native worker failed: {status}").into());
    }
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(output.join("results.json"))?)?;
    let expected = [
        "scroll_up",
        "scroll_down",
        "multiple_rows",
        "mixed_updates",
        "single_cell",
        "full_redraw",
        "theme",
        "resize",
        "overlays",
    ];
    let names: HashSet<_> = rows
        .iter()
        .filter_map(|row| row["workload"].as_str())
        .collect();
    if rows.len() != expected.len()
        || names.len() != expected.len()
        || !expected.iter().all(|name| names.contains(name))
    {
        return Err("missing or duplicate completed workload".into());
    }
    for row in &rows {
        if row["pixel_pairs"].as_u64() != Some(u64::try_from(FRAMES)?)
            || row["measured_frames"].as_u64() != Some(u64::try_from(FRAMES - 2)?)
            || row["first_image_sha256"]
                .as_str()
                .is_none_or(|hash| hash.len() != 64)
            || row["last_image_sha256"]
                .as_str()
                .is_none_or(|hash| hash.len() != 64)
            || row["first_image_sha256"] == row["last_image_sha256"]
        {
            return Err("incomplete or inactive workload result".into());
        }
    }
    println!("TERMINAL_NATIVE_VERIFIED pixel_pairs=108 workloads=9");
    Ok(())
}

fn native(output: PathBuf) -> ExitCode {
    if let Err(error) = std::fs::create_dir_all(&output) {
        eprintln!("cannot create output directory: {error}");
        return ExitCode::FAILURE;
    }
    let failed = Arc::new(AtomicBool::new(true));
    let status = failed.clone();
    let inject_mismatch = std::env::var_os("KRILL_TERMINAL_VERIFY_INJECT_MISMATCH").is_some();
    gpui_platform::application().run(move |cx| {
        gpui_component::init(cx);
        let opened = cx.open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(16.), px(40.)), size(px(1200.), px(760.))))),
            focus: false,
            ..WindowOptions::default()
        }, |window, cx| {
            window.set_window_title("Krill terminal verification — isolated fixture");
            cx.new(|_| Surface { frame: Frame { screen: Rc::new(fixture(96, 24, 0)), theme: DEFAULT, rem: 16., overlay: false }, cache: RowPlanCache::default(), cached: true })
        });
        let Ok(handle) = opened else { eprintln!("cannot open verification window"); cx.quit(); return; };
        cx.spawn(async move |cx| {
            let result = async {
                let mut results = Vec::new();
                for workload in ["scroll_up", "scroll_down", "multiple_rows", "mixed_updates", "single_cell", "full_redraw", "theme", "resize", "overlays"] {
                    let input = frames(workload)?;
                    handle.update(cx, |view, _, _| view.cache = RowPlanCache::default())?;
                    let mut cached_draw = Vec::new();
                    let mut direct_draw = Vec::new();
                    let mut readbacks = Vec::new();
                    let mut first_hash = String::new();
                    let mut last_hash = String::new();
                    for (index, frame) in input.iter().enumerate() {
                        let cached_path = output.join(format!("{workload}-cached.png"));
                        let direct_path = output.join(format!("{workload}-direct.png"));
                        let save = index == 0 || index == FRAMES - 1;
                        let cached = draw(handle, frame, true, save.then_some(cached_path.as_path()), cx)?;
                        // macOS가 autorelease 자원과 실제 창의 예약된 표시를 처리할 기회를 준다.
                        cx.background_executor().timer(Duration::from_millis(1)).await;
                        let mut direct = draw(handle, frame, false, save.then_some(direct_path.as_path()), cx)?;
                        if index == 0 {
                            let mut colors = HashSet::new();
                            for pixel in cached.bytes.as_chunks::<4>().0 {
                                colors.insert(pixel);
                                if colors.len() >= 32 { break; }
                            }
                            if colors.len() < 32 { return Err("renderer produced an empty or trivial image".into()); }
                            first_hash = format!("{:x}", Sha256::digest(&cached.bytes));
                            if inject_mismatch && let Some(byte) = direct.bytes.first_mut() { *byte ^= 1; }
                        }
                        if cached.dimensions != direct.dimensions || cached.bytes != direct.bytes {
                            return Err(format!("pixel mismatch: {workload} frame {index}").into());
                        }
                        if index >= 2 {
                            cached_draw.push(cached.draw_us);
                            direct_draw.push(direct.draw_us);
                            readbacks.push(cached.readback_us);
                        }
                        if index == FRAMES - 1 { last_hash = format!("{:x}", Sha256::digest(&cached.bytes)); }
                        cx.background_executor().timer(Duration::from_millis(2)).await;
                    }
                    if first_hash == last_hash { return Err(format!("workload did not change the rendered image: {workload}").into()); }
                    let item = serde_json::json!({"workload":workload,"pixel_pairs":FRAMES,"measured_frames":cached_draw.len(),"cached_draw_median_us":median(&mut cached_draw),"direct_draw_median_us":median(&mut direct_draw),"cached_readback_median_us":median(&mut readbacks),"first_image_sha256":first_hash,"last_image_sha256":last_hash});
                    println!("TERMINAL_NATIVE {item}");
                    results.push(item);
                }
                std::fs::write(output.join("results.json"), serde_json::to_vec_pretty(&results)?)?;
                Ok::<_, Box<dyn std::error::Error>>(())
            }.await;
            match result { Ok(()) => status.store(false, Ordering::Relaxed), Err(error) => eprintln!("TERMINAL_NATIVE_ERROR {error}") }
            cx.update(|cx| cx.quit());
        }).detach();
    });
    if failed.load(Ordering::Relaxed) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
