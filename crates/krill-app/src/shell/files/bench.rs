//! OWE-665: 실제 Metal 창에서 파일 탭 메모리(`phys_footprint`)를 잰다. 클릭 없이 탭을 열고 돌린다(데몬 없음).
//! `KRILL_STATE_DIR` 격리 + onboarding.done 생성 후 `krill-app --files-bench=<폴더>` — 폴더의 파일을 이름순으로 탭에 연다.
//! 단계마다 `OWE665 phase=… t_ms=… bytes=… peak_bytes=…`를 찍는다. 순간값은 바깥에서 `footprint --sample`로 함께 본다.

use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use super::*;

/// 앱 예산(AGENTS 2). peak가 넘으면 실패로 끝난다.
const LIMIT: u64 = 120 << 20;
/// 탭 전환 횟수(탭 수 × ROUNDS)와 간격 — 60Hz 화면처럼 프레임을 연달아 그린다.
const ROUNDS: usize = 10;
const FRAME: Duration = Duration::from_millis(16);

/// (`phys_footprint`, `phys_footprint_peak`) 바이트.
fn footprint() -> Result<(u64, u64), String> {
    let out = Command::new("/usr/bin/footprint")
        .args(["-f", "bytes", "-p", &std::process::id().to_string()])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("footprint exited with {}", out.status));
    }
    parse_footprint(&String::from_utf8_lossy(&out.stdout))
}

fn parse_footprint(text: &str) -> Result<(u64, u64), String> {
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.trim_start().strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.replace(',', "").parse::<u64>().ok())
            .ok_or_else(|| format!("missing {name}"))
    };
    Ok((field("phys_footprint:")?, field("phys_footprint_peak:")?))
}

const fn within_budget(worst_bytes: u64) -> bool {
    worst_bytes <= LIMIT
}

/// macOS `cx.quit()`(`[NSApp terminate:nil]`)은 `AppKit` 안에서 `exit(0)`을 직접 불러 `application().run`이
/// 반환하지 않으므로, 실패 시에는 종료 코드 1을 직접 전달한다(OWE-779).
fn finish(failed: bool, cx: &mut gpui::App) {
    if failed {
        #[expect(
            clippy::exit,
            reason = "macOS NSApplication terminate:가 exit(0)으로 끝내 run()이 반환하지 않으므로 벤치 실패 코드를 직접 전달한다"
        )]
        std::process::exit(1);
    }
    cx.quit();
}

pub(crate) fn run(dir: &std::path::Path) -> ExitCode {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect(),
        Err(e) => {
            eprintln!("BENCH_ERROR {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    };
    files.sort();
    if files.is_empty() || krill_proto::daemon::state_dir_override().is_none() {
        eprintln!("BENCH_ERROR use KRILL_STATE_DIR and --files-bench=<non-empty folder>");
        return ExitCode::FAILURE;
    }
    let failed = Arc::new(AtomicBool::new(true));
    let result = Arc::clone(&failed);
    gpui_platform::application()
        .with_assets(gpui_kit_assets::Assets)
        .run(move |cx| {
            crate::init(cx);
            let Some(main) = crate::open_shell(cx, None) else {
                finish(true, cx);
                return;
            };
            cx.spawn(async move |cx| {
                let started = Instant::now();
                let measured = async {
                    let bg = cx.background_executor().clone();
                    let draw = |cx: &mut gpui::AsyncApp| {
                        cx.update_window(main.into(), |_, w, cx| w.draw(cx).clear(cx))
                    };
                    let mut worst = 0;
                    let mut mark = async |phase: &str| {
                        let (now, peak) = bg.spawn(async { footprint() }).await?;
                        worst = worst.max(peak);
                        println!(
                            "OWE665 phase={phase} t_ms={} bytes={now} peak_bytes={peak}",
                            started.elapsed().as_millis()
                        );
                        Ok::<_, Box<dyn std::error::Error>>(())
                    };
                    bg.timer(Duration::from_secs(1)).await;
                    draw(cx)?;
                    bg.timer(Duration::from_secs(2)).await;
                    mark("start").await?;
                    for path in &files {
                        let path = path.clone();
                        main.update(cx, |s, _, cx| s.open_file(path, cx))?;
                        draw(cx)?;
                        bg.timer(Duration::from_millis(300)).await;
                    }
                    mark("opened").await?;
                    let tabs = main.update(cx, |s, _, _| s.layout.tabs().len())?;
                    for _ in 0..ROUNDS {
                        for i in 0..tabs {
                            main.update(cx, |s, _, cx| s.change(false, cx, |l| l.activate(i)))?;
                            draw(cx)?;
                            bg.timer(FRAME).await;
                        }
                    }
                    mark("switched").await?;
                    bg.timer(Duration::from_secs(15)).await;
                    mark("idle").await?;
                    Ok::<_, Box<dyn std::error::Error>>(within_budget(worst))
                }
                .await;
                let is_failed = match measured {
                    Ok(pass) => !pass,
                    Err(error) => {
                        eprintln!("BENCH_ERROR {error}");
                        true
                    }
                };
                result.store(is_failed, Ordering::Relaxed);
                cx.update(|cx| finish(is_failed, cx));
            })
            .detach();
        });
    if failed.load(Ordering::Relaxed) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_rejects_peak_above_120_mib() {
        assert!(within_budget(LIMIT));
        assert!(within_budget(123_077_760));
        assert!(!within_budget(LIMIT + 1));
        assert!(!within_budget(126_895_232));
    }

    #[test]
    fn parse_footprint_extracts_current_and_peak_bytes() {
        let sample = "\
            phys_footprint: 97256576 bytes\n\
            phys_footprint_peak: 126895232 bytes\n";
        assert_eq!(parse_footprint(sample), Ok((97_256_576, 126_895_232)));
        assert!(parse_footprint("phys_footprint: 100 bytes\n").is_err());
    }
}
