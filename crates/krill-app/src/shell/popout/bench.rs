//! 실제 Metal 창의 MK-098 재현 측정. 데몬/실제 에이전트 없이 고정된 10세션 화면을 쓴다.
//! `KRILL_STATE_DIR` 격리 + onboarding.done 생성 후 krill-app --popout-bench=terminal|dashboard|both.
//! `--popout-bench=accounts`는 같은 합성 계정으로 실제 상태바 팝업을 수동 확인한다.
//! `KRILL_BENCH_REMOTE_HOSTS=5`로 합성 원격 기기가 많은 팝업도 재현한다.
//! `--popout-bench=chat`은 데몬 없이 실제 채팅 화면의 레이아웃과 상호작용을 확인한다.

mod chat;

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

struct AccountsPreview;
impl sidebar::Source for AccountsPreview {
    fn fetch(&self) -> io::Result<sidebar::Snapshot> {
        let repo = std::path::PathBuf::from("/tmp/krill-ui-preview");
        let tree = repo.join("account-popup");
        Ok(sidebar::Snapshot {
            sessions: vec![sidebar::Session {
                id: SessionId(51),
                status: AgentStatus {
                    state: AgentState::Done,
                    agent: "claude".into(),
                    ..AgentStatus::UNKNOWN
                },
                worktree: Some(tree.clone()),
                cwd: Some(tree.clone()),
                account: Some(krill_proto::control::SessionAccount {
                    session: SessionId(51),
                    account_id: "layout-main".into(),
                    label: "Synthetic main".into(),
                }),
            }],
            trees: [(repo, vec![tree])].into(),
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

pub(super) fn account_popup_snapshot() -> AccountsSnapshot {
    use krill_proto::usage::{Snapshot, Window};
    let now = krill_app::accounts::display::now();
    let account = |id: &str, email: &str, active: bool, usage| AccountInfo {
        id: id.into(),
        provider: ProviderKind::Claude,
        label: if active {
            "Synthetic main"
        } else {
            "Synthetic other"
        }
        .into(),
        email: Some(email.into()),
        profile_dir: "/tmp/krill-popup-layout-fixture".into(),
        active,
        host_default: false,
        stale: false,
        usage,
    };
    AccountsSnapshot {
        accounts: vec![
            account(
                "layout-main",
                "long.account.identity.for.the.primary.team.workspace@example.test",
                true,
                Snapshot {
                    five_hour: Some(Window {
                        percent: 23,
                        resets_at: Some((now + 12_660).to_string()),
                    }),
                    seven_day: Some(Window {
                        percent: 76,
                        resets_at: Some((now + 432_000).to_string()),
                    }),
                },
            ),
            account(
                "layout-other",
                "another.long.account.identity.for.a.secondary.workspace@example.test",
                false,
                Snapshot {
                    five_hour: Some(Window {
                        percent: 99,
                        resets_at: Some(now.saturating_sub(60).to_string()),
                    }),
                    seven_day: Some(Window {
                        percent: 47,
                        resets_at: None,
                    }),
                },
            ),
        ],
        ..Default::default()
    }
}

pub(crate) fn run(mode: String) -> ExitCode {
    if !matches!(
        mode.as_str(),
        "terminal" | "dashboard" | "both" | "accounts" | "chat"
    ) || std::env::var_os("KRILL_STATE_DIR").is_none()
    {
        eprintln!(
            "BENCH_ERROR use KRILL_STATE_DIR and --popout-bench=terminal|dashboard|both|accounts|chat"
        );
        return ExitCode::FAILURE;
    }
    let failed = Arc::new(AtomicBool::new(true));
    let result = Arc::clone(&failed);
    gpui_platform::application().with_assets(krill_app::ui::icons::AppAssets).run(move |cx| {
        crate::init(cx);
        if matches!(mode.as_str(), "accounts" | "chat") {
            let state = std::env::var_os("KRILL_STATE_DIR").map(std::path::PathBuf::from);
            // Shell::new schedules a local snapshot read. Inject an empty home
            // before construction so even that background read stays synthetic.
            krill_app::orca_import::set_env_paths(
                state.as_ref().map(|dir| dir.join("synthetic-home")),
                state.as_ref().map(|dir| dir.join("synthetic-orca")),
                state,
                None,
                cx,
            );
        }
        let Some(main) = crate::open_shell(cx, None) else { cx.quit(); return };
        if mode == "chat" {
            if main.update(cx, chat::install).is_err() {
                cx.quit();
                return;
            }
            result.store(false, Ordering::Relaxed);
            println!("CHAT_FIXTURE pid={} · synthetic transcript, no agent or daemon", std::process::id());
            return;
        }
        if mode == "accounts" {
            cx.set_global(krill_app::accounts::installed::Installed(Some(vec![ProviderKind::Claude])));
            main.update(cx, |shell, window, cx| {
                shell.link = Link::Down;
                shell.local_generation = shell.local_generation.wrapping_add(1);
                shell.accounts_poll = None;
                shell.usage_refresh = None;
                shell.orch_poll = None;
                // A settings change must not replace the fixed preview snapshot.
                #[expect(clippy::used_underscore_binding, reason = "replace the retained settings observer only for the synthetic diagnostic")]
                {
                    shell._config_observer = cx.observe_global::<krill_app::settings::Config>(|_, cx| cx.notify());
                }
                shell.accounts_snapshot = account_popup_snapshot().into();
                if let Ok(count) = std::env::var("KRILL_BENCH_REMOTE_HOSTS")
                    && let Ok(count) = count.parse::<usize>()
                {
                    for i in 0..count.min(32) {
                        let id = format!("popup-preview-{i}");
                        shell.host_statuses.push((krill_app::hosts::RemoteHostSpec {
                            id: id.clone(), label: format!("Synthetic remote {i}"), address: id.clone(),
                            port: None, identity_file: None, remote_socket: None,
                        }, krill_app::hosts::HostStatus::Connected));
                        shell.remote_accounts.insert(krill_app::hosts::HostId::Remote(id), shell.accounts_snapshot.clone());
                    }
                }
                shell.notice = Some("Synthetic account popup preview".into());
                let accounts = shell.accounts_snapshot.clone();
                shell.sidebar.update(cx, |s, cx| {
                    s.set_session_account_features(krill_app::hosts::HostId::Local, krill_proto::usage::FEAT_USAGE | krill_proto::usage::FEAT_SESSION_ACCOUNT_SWITCH | krill_proto::usage::FEAT_PROVIDER_DETAILS, cx);
                    s.sync_session_accounts(&krill_app::hosts::HostId::Local, accounts, cx);
                    s.connect_source(Arc::new(AccountsPreview), cx);
                    s.toggle_card_sessions(std::path::Path::new("/tmp/krill-ui-preview/account-popup"), cx);
                });
                if let Ok(width) = std::env::var("KRILL_BENCH_WIDTH")
                    && let Ok(width) = width.parse::<f32>()
                {
                    let height = std::env::var("KRILL_BENCH_HEIGHT")
                        .ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(760.);
                    window.resize(gpui::size(px(width.clamp(320., 1600.)), px(height.clamp(320., 1200.))));
                }
                if let Ok(zoom) = std::env::var("KRILL_BENCH_ZOOM")
                    && let Ok(zoom) = zoom.parse::<f32>()
                {
                    window.set_rem_size(px(16. * zoom.clamp(0.75, 2.)));
                }
                cx.notify();
            }).ok();
            if let Ok(view) = std::env::var("KRILL_BENCH_ACCOUNTS_VIEW") {
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(Duration::from_millis(750)).await;
                    cx.update_window(main.into(), |_, window, cx| {
                        window.draw(cx).clear(cx);
                        let position = if view == "popup" {
                            gpui::point(px(80.), window.viewport_size().height - px(12.))
                        } else {
                            gpui::point(window.viewport_size().width - px(100.), window.viewport_size().height - px(65.))
                        };
                        eprintln!("ACCOUNTS_NATIVE_INPUT viewport={:?} position={position:?}", window.viewport_size());
                        window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                            position, pressed_button: None, modifiers: gpui::Modifiers::default(),
                        }), cx);
                        if view == "popup" {
                            window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                                position, button: gpui::MouseButton::Left, modifiers: gpui::Modifiers::default(), click_count: 1, first_mouse: false,
                            }), cx);
                            window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                                position, button: gpui::MouseButton::Left, modifiers: gpui::Modifiers::default(), click_count: 1,
                            }), cx);
                        }
                        // Let the native frame callback draw and present the input result.
                        // A manual draw here would consume invalidation without presenting.
                        window.refresh();
                    }).ok();
                    eprintln!("ACCOUNTS_NATIVE_INPUT_DISPATCHED");
                }).detach();
            }
            result.store(false, Ordering::Relaxed);
            println!("ACCOUNTS_FIXTURE pid={} · click the Claude status-bar pill to open the production popup", std::process::id());
            return;
        }
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
                    if let Some(d) = existing_dashboard(cx) {
                        d.update(cx, |d, cx| d.sidebar.update(cx, |s, cx| s.connect_source(Arc::new(Fixed), cx)));
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
                let cpu_before = bg.spawn(async { cpu_seconds().map_err(|e| e.to_string()) }).await?;
                bg.timer(Duration::from_secs(10)).await;
                let cpu_delta = bg.spawn(async { cpu_seconds().map_err(|e| e.to_string()) }).await? - cpu_before;
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
