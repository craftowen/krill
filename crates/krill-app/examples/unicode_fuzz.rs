//! MK-056: 실제 PTY → krilld 파서 → 화면 → `TerminalElement` → macOS CoreText 셰이핑.
//! `cargo run --locked -p krill-app --example unicode_fuzz -- 3600`
//! gpui 잠정 결정 의존은 이 테스트 모듈에만 있다. 제품 코드는 바꾸지 않는다.

#[cfg(target_os = "macos")]
#[path = "../../krill-term/tests/support/unicode_fuzz.rs"]
mod corpus;

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("MK-056 requires the macOS native text system".into())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::fs::File;
    use std::io::Write;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use gpui::{TestAppContext, TestDispatcher, point, px, size};
    use krill_app::terminal::{FONT_SIZE, Terminal, TerminalElement};
    use krill_proto::SessionId;
    use krill_proto::holder::WinSize;
    use krill_proto::screen::Screen;
    use krill_proto::theme::DEFAULT;
    use krilld::{Appearance, Session};
    use rustix::termios::{OptionalActions, Winsize, tcgetattr, tcsetattr};

    use super::corpus::{Generator, SEED};

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let seconds = std::env::args()
            .nth(1)
            .map_or(Ok(3600), |s| s.parse::<u64>())?;
        if seconds == 0 {
            return Err("duration must be positive".into());
        }
        // MacPlatform의 main-thread 요구와 실제 font-kit/CoreText 구현을 만족한다.
        let platform = gpui_platform::current_platform(true);
        let mut context = TestAppContext::build_with_text_system(
            TestDispatcher::new(0),
            Some("unicode_fuzz"),
            platform.text_system(),
        );
        // 위에서 선택한 macOS 텍스트 시스템이 한글·이모지를 셰이핑하는지 먼저 확인한다.
        let probe = gpui::WindowTextSystem::new(Arc::clone(context.text_system())).layout_line(
            "한🙂",
            FONT_SIZE,
            &[gpui::TextRun {
                len: "한🙂".len(),
                font: gpui::font("Menlo"),
                color: gpui::Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            Some(px(18.)),
        );
        if !probe.runs.iter().any(|r| !r.glyphs.is_empty()) {
            return Err("native text system produced no glyphs".into());
        }
        let (_, cx) = context.add_window_view(|_, _| Terminal::new(Screen::blank(80, 8), DEFAULT));
        let ws = Winsize {
            ws_row: 8,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = rustix_openpty::openpty(None, Some(&ws))?;
        let mut attributes = tcgetattr(&pty.user)?;
        attributes.make_raw();
        tcsetattr(&pty.user, OptionalActions::Now, &attributes)?;
        let session = Session::start(
            SessionId(56),
            pty.controller,
            WinSize { cols: 80, rows: 8 },
            Arc::new(Appearance::load(PathBuf::from("/nonexistent/mk056/theme"))),
            None,
        )?;
        let tap = session.tap();
        let mut output = File::from(pty.user);
        let mut rng = Generator::new(SEED);
        let started = Instant::now();
        let mut progress = started;
        let mut iterations = 0u64;
        let mut total_bytes = 0u64;
        eprintln!(
            "MK-056 start seed={SEED:#x} requested_seconds={seconds} native_glyphs={}",
            probe.runs.iter().map(|r| r.glyphs.len()).sum::<usize>()
        );
        while started.elapsed() < Duration::from_secs(seconds) {
            let bytes = rng.case(iterations);
            let result = catch_unwind(AssertUnwindSafe(
                || -> Result<(), Box<dyn std::error::Error>> {
                    // RIS로 미완성 CSI/OSC를 취소하고 스택을 초기화한다. 매 case의 독립 재현이 가능하다.
                    output.write_all(b"\x18\x1bc")?;
                    total_bytes += 3;
                    let mut rest = bytes.as_slice();
                    while !rest.is_empty() {
                        let n = (1 + rng.next(16)).min(rest.len());
                        output.write_all(&rest[..n])?;
                        total_bytes += u64::try_from(n)?;
                        rest = &rest[n..];
                        wait_output(|| (tap.pulse().end, tap.pulse().closed), total_bytes)?;
                    }
                    // 열린 DCS/OSC/DEC 2026도 닫아 최종 화면을 칠한다.
                    output.write_all(b"\x18\x1b[?2026l")?;
                    total_bytes += 9;
                    wait_output(|| (tap.pulse().end, tap.pulse().closed), total_bytes)?;
                    if !session.gaps().is_empty() {
                        return Err(format!("daemon panic gaps: {:?}", session.gaps()).into());
                    }
                    let (screen, _, _) = tap.capture();
                    let (cell, ()) =
                        cx.draw(point(px(0.), px(0.)), size(px(800.), px(240.)), |_, _| {
                            TerminalElement::new(Rc::new(screen), DEFAULT)
                        });
                    if cell.width <= px(0.) || cell.height <= px(0.) {
                        return Err("invalid cell dimensions".into());
                    }
                    Ok(())
                },
            ));
            match result {
                Ok(Ok(())) => {}
                failure => {
                    let path = PathBuf::from("target/mk056-crash.bin");
                    std::fs::write(&path, &bytes)?;
                    eprintln!(
                        "MK-056 FAIL case={iterations} seed={SEED:#x} input={}",
                        path.display()
                    );
                    return match failure {
                        Ok(Err(error)) => Err(error),
                        _ => Err("renderer panic".into()),
                    };
                }
            }
            iterations += 1;
            if progress.elapsed() >= Duration::from_secs(60) {
                eprintln!(
                    "MK-056 progress elapsed={:.3}s cases={iterations} bytes={total_bytes} panics=0",
                    started.elapsed().as_secs_f64()
                );
                progress = Instant::now();
            }
        }
        drop(output);
        session.wait()?;
        eprintln!(
            "MK-056 PASS elapsed={:.3}s cases={iterations} bytes={total_bytes} panics=0 seed={SEED:#x}",
            started.elapsed().as_secs_f64()
        );
        Ok(())
    }

    fn wait_output(
        pulse: impl Fn() -> (u64, bool),
        end: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while pulse().0 < end {
            if pulse().1 || Instant::now() >= deadline {
                return Err("PTY output did not reach parser within 5 seconds".into());
            }
            std::thread::sleep(Duration::from_micros(100));
        }
        Ok(())
    }
}
