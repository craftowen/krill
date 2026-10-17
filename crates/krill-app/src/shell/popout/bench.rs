//! 실제 Metal 창의 MK-098 재현 측정. 데몬/실제 에이전트 없이 고정된 10세션 화면을 쓴다.
//! `KRILL_STATE_DIR` 격리 + onboarding.done 생성 후 krill-app --popout-bench=terminal|dashboard|both.

use std::{
    io,
    process::{Command, ExitCode},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::Task;
use krill_proto::{
    control::{AgentState, AgentStatus, Cold},
    screen::{Cell, Op},
};

use super::*;

struct Fixed;
impl sidebar::Source for Fixed {
    fn fetch(&self) -> io::Result<sidebar::Snapshot> {
        Ok(sidebar::Snapshot {
            sessions: (0..10)
                .map(|id| sidebar::Session {
                    id: SessionId(id),
                    status: AgentStatus {
                        state: AgentState::Done,
                        agent: "bench".into(),
                        ..AgentStatus::UNKNOWN
                    },
                    worktree: None,
                    cwd: None,
                    account: None,
                })
                .collect(),
            ..Default::default()
        })
    }
}

fn screen(shell: &mut Shell, cx: &mut Context<Shell>) {
    let pane = shell.layout.active_tab().focused;
    let session = shell.layout.session(pane).unwrap_or(SessionId(0));
    shell.layout.set_session(pane, session);
    let mut live = super::super::Live::new(Task::ready(()));
    let ops = std::iter::once(Op::Reset {
        cols: 80,
        lines: 24,
    })
    .chain((0..24).map(|line| {
        Op::Line {
            line,
            cells: format!("Krill MK-098 · \u{d55c}\u{ae00}🙂 · idle row {line}")
                .chars()
                .map(|ch| Cell {
                    ch,
                    ..Cell::default()
                })
                .collect(),
        }
    }))
    .collect();
    if let Err(error) = live.pane.show_cold(
        session,
        Cold {
            screen: ops,
            history: Vec::new(),
        },
    ) {
        eprintln!("BENCH_ERROR screen: {error}");
    }
    shell.live.insert(pane, live);
    cx.notify();
}

fn footprint() -> Result<u64, Box<dyn std::error::Error>> {
    let out = Command::new("/usr/bin/footprint")
        .args(["-f", "bytes", "-p", &std::process::id().to_string()])
        .output()?;
    if !out.status.success() {
        return Err("footprint failed".into());
    }
    let text = String::from_utf8(out.stdout)?;
    let value = text
        .lines()
        .find(|l| l.trim_start().starts_with("phys_footprint:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .ok_or("missing phys_footprint")?;
    Ok(value.replace(',', "").parse()?)
}

fn cpu_seconds() -> Result<f64, Box<dyn std::error::Error>> {
    let out = Command::new("/bin/ps")
        .args(["-p", &std::process::id().to_string(), "-o", "time="])
        .output()?;
    if !out.status.success() {
        return Err("ps failed".into());
    }
    let text = String::from_utf8(out.stdout)?;
    let (minutes, seconds) = text.trim().split_once(':').ok_or("invalid ps time")?;
    Ok(minutes.parse::<f64>()? * 60. + seconds.parse::<f64>()?)
}

pub(crate) fn run(mode: String) -> ExitCode {
    if !matches!(mode.as_str(), "terminal" | "dashboard" | "both")
        || std::env::var_os("KRILL_STATE_DIR").is_none()
    {
        eprintln!("BENCH_ERROR use KRILL_STATE_DIR and --popout-bench=terminal|dashboard|both");
        return ExitCode::FAILURE;
    }
    let failed = Arc::new(AtomicBool::new(true));
    let result = Arc::clone(&failed);
    gpui_platform::application().with_assets(gpui_kit_assets::Assets).run(move |cx| {
        crate::init(cx);
        let Some(main) = crate::open_shell(cx, None) else { cx.quit(); return };
        main.update(cx, |shell, _, cx| {
            shell.link = Link::Up;
            screen(shell, cx);
            shell.sidebar.update(cx, |s, cx| s.connect_source(Arc::new(Fixed), cx));
        }).ok();
        cx.spawn(async move |cx| {
            let measured = async {
                let bg = cx.background_executor().clone();
                bg.timer(Duration::from_secs(1)).await;
                // 잠금 화면에서도 실제 Metal drawable을 확보한다(비테스트 바이너리라 이미지 캡처 없음).
                for _ in 0..5 {
                    cx.update_window(main.into(), |_, w, cx| {
                        w.draw(cx).clear(cx);
                    })?;
                }
                let before = bg.spawn(async { footprint().map_err(|e| e.to_string()) }).await?;
                main.update(cx, |s, _, cx| {
                    if mode != "dashboard" { s.float_terminal(SessionId(0), cx); }
                    if mode != "terminal" { s.dashboard(cx); }
                })?;
                cx.update(|cx| {
                    if let Some(f) = existing::<Floating>(cx) {
                        f.update(cx, |f, _, cx| f.shell.update(cx, screen)).ok();
                    }
                    if let Some(d) = existing::<Dashboard>(cx) {
                        d.update(cx, |d, _, cx| d.sidebar.update(cx, |s, cx| s.connect_source(Arc::new(Fixed), cx))).ok();
                    }
                });
                bg.timer(Duration::from_secs(1)).await;
                let windows = cx.update(|cx| cx.windows());
                for window in windows {
                    for _ in 0..5 {
                        cx.update_window(window, |_, w, cx| {
                            w.draw(cx).clear(cx);
                        })?;
                    }
                }
                bg.timer(Duration::from_secs(2)).await;
                let after = bg.spawn(async { footprint().map_err(|e| e.to_string()) }).await?;
                let started = Instant::now();
                let cpu_before = cpu_seconds()?;
                bg.timer(Duration::from_secs(10)).await;
                let cpu_delta = cpu_seconds()? - cpu_before;
                let cpu_pct = cpu_delta / started.elapsed().as_secs_f64() * 100.;
                let delta = after.saturating_sub(before);
                println!("MK098 mode={mode} baseline_bytes={before} popout_bytes={after} delta_bytes={delta} idle_cpu_seconds={cpu_delta:.2} idle_cpu_pct={cpu_pct:.1} sample_seconds={:.3}", started.elapsed().as_secs_f64());
                // ps의 0.01초 해상도를 포함해 소수 첫째 자리 CPU 사용률로 비교한다.
                Ok::<_, Box<dyn std::error::Error>>(delta <= 40_000_000 && cpu_pct < 0.05)
            }.await;
            match measured {
                Ok(pass) => result.store(!pass, Ordering::Relaxed),
                Err(error) => eprintln!("BENCH_ERROR {error}"),
            }
            cx.update(|cx| cx.quit());
        }).detach();
    });
    if failed.load(Ordering::Relaxed) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
