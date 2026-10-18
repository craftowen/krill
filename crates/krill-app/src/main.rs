//! Krill 앱. 3열 셸 + 탭 + 분할 트리(MK-030). 시작하면 세션 데몬에 붙는다(없으면 띄운다, MK-025). 보이는 칸마다
//! krilld 세션 화면 스트림에 붙어(`krill_app::pane`, OWE-490) 터미널 렌더러(`krill_app::terminal`, MK-027)로 그린다.
//! panic은 크래시 로그로 남기고, 다음 실행이 진단 대화상자를 띄운다(MK-067). `--crash-test`는 그 경로를 검증하려고
//! 시작 직후 일부러 panic한다. 첫 실행이면 온보딩 대화상자를 띄운다(MK-073).

mod shell;

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, Menu, MenuItem, QuitMode, TitlebarOptions,
    WindowBounds, WindowHandle, WindowOptions, actions, px, size,
};
use std::path::PathBuf;

actions!(minikrill, [NewWindow, CloseWindow]);

fn main() -> std::process::ExitCode {
    // MK-170: 경로를 정하기 전에 옛 번들 id 데이터를 옮긴다(옛 holder가 살아 있으면 그대로 둔다).
    let migrated = krill_proto::daemon::migrate::run();
    krill_log::init(krill_log::Component::App).ok();
    tracing::info!(?migrated, "app starting");
    krill_proto::diag::install_panic_hook("krill-app");
    if std::env::args().any(|a| a == "--crash-test") {
        crash_test();
    }
    if let Some(code) = krill_app::updater::maybe_run_relauncher_cli() {
        return code;
    }
    if let Some(code) = krill_app::updater::maybe_run_update_cli() {
        return std::process::ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    if let Some(mode) =
        std::env::args().find_map(|a| a.strip_prefix("--popout-bench=").map(str::to_owned))
    {
        return shell::popout::bench::run(mode);
    }
    if let Some(dir) =
        std::env::args().find_map(|a| a.strip_prefix("--files-bench=").map(PathBuf::from))
    {
        return shell::files::bench::run(&dir);
    }
    // OWE-576: gpui-component 아이콘 자산은 재열기·새 창에서도 같은 소스를 쓴다.
    let application = gpui_platform::application().with_assets(gpui_kit_assets::Assets);
    application.on_reopen(|cx| {
        reopen(cx, |cx| {
            let _ = new_window(cx);
        });
    });
    application.run(|cx: &mut App| {
        init(cx);
        krill_app::settings::init(cx);
        let Some(window) = new_window(cx) else {
            cx.quit();
            return;
        };
        if let Ok(paths) = krill_proto::daemon::Paths::dev() {
            krill_app::menu_bar::init(
                krill_app::sidebar::status_source(paths),
                |session, cx| {
                    open_from_menu(session, cx, |cx| {
                        let _ = new_window(cx);
                    });
                },
                cx,
            );
        }
        if let Some(state) = krill_proto::daemon::state_dir() {
            window
                .update(cx, |shell, window, cx| {
                    shell.arm_update_ready(state.clone());
                    krill_app::updater::on_startup(state.clone(), window, cx);
                    krill_app::crash::offer_report(state, |p, cx| cx.reveal_path(p), window, cx);
                    if let Some(env) = krill_app::onboarding::Env::current() {
                        krill_app::onboarding::offer(env.clone(), window, cx);
                        // 시작 시 빠른 점검(bin/state/위치만, 백그라운드). UI 스레드를 막지 않는다.
                        krill_app::onboarding::spawn_quick_check(&env, cx);
                        // OWE-706: 설치된 프로바이더만 계정 화면·상태바에 보인다.
                        krill_app::accounts::installed::spawn_detect(env.shell.clone(), cx);
                    }
                })
                .ok();
        }
    });
    std::process::ExitCode::SUCCESS
}

/// 창이 남아 있으면 앞으로 가져오고, 마지막 창을 닫은 뒤에는 새 셸 창을 연다.
/// 설정 창만 열려 있어도 작업 창은 다시 만든다.
fn reopen(cx: &mut App, open: impl FnOnce(&mut App)) {
    if let Some(window) = cx
        .windows()
        .into_iter()
        .find(|w| w.downcast::<shell::Shell>().is_some())
    {
        window
            .update(cx, |_, window, _| window.activate_window())
            .ok();
    } else {
        open(cx);
    }
    krill_app::menu_bar::sync_window_mode(cx);
    cx.activate(true);
}

/// 메뉴바·알림 클릭을 창 수명과 독립적으로 처리한다.
fn open_from_menu(
    session: Option<krill_proto::SessionId>,
    cx: &mut App,
    open: impl FnOnce(&mut App),
) {
    reopen(cx, open);
    if let Some(session) = session
        && let Some(window) = cx
            .windows()
            .into_iter()
            .find_map(|w| w.downcast::<shell::Shell>())
    {
        window
            .update(cx, |shell, _, cx| shell.open_session(session, cx))
            .ok();
    }
}

fn new_window(cx: &mut App) -> Option<WindowHandle<shell::Shell>> {
    // 단일 메인 창 정책: 이미 열린 Shell 창이 있으면 활성화하고, 모든 창이 닫혔을 때만
    // 기본 레이아웃 저장 경로로 메인 창을 다시 열어 탭·분할 변경이 유실되지 않게 한다(OWE-859).
    if let Some(window) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<shell::Shell>())
    {
        window
            .update(cx, |_, window, _| window.activate_window())
            .ok();
        krill_app::menu_bar::sync_window_mode(cx);
        cx.activate(true);
        return Some(window);
    }
    let window = open_shell(cx, krill_app::layout::default_path())?;
    window
        .update(cx, |shell, _, cx| shell.connect_daemon(cx))
        .ok();
    Some(window)
}

/// 창 구성과 외부 IO를 분리하여 gpui 테스트가 데몬을 띄우지 않게 한다.
fn open_shell(cx: &mut App, path: Option<PathBuf>) -> Option<WindowHandle<shell::Shell>> {
    let bounds = Bounds::centered(None, size(px(1200.), px(760.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(krill_app::i18n::catalog::security::APP_MENU.get().into()),
            appears_transparent: true,
            traffic_light_position: Some(krill_app::ui::TRAFFIC_LIGHT_INSET),
        }),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| shell::Shell::new(path, cx));
        window.set_rem_size(px(16. * krill_app::settings::current(cx).zoom()));
        let focus = view.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        view
    });
    match opened {
        Ok(window) => {
            krill_app::menu_bar::sync_window_mode(cx);
            cx.activate(true);
            Some(window)
        }
        Err(err) => {
            tracing::error!("window creation failed");
            eprintln!("window creation failed: {err:#}");
            None
        }
    }
}

#[expect(
    clippy::panic,
    reason = "MK-067: 크래시 로그·진단 대화상자 e2e용 강제 panic"
)]
fn crash_test() {
    panic!("MK-067 crash test (--crash-test)");
}

fn init(cx: &mut App) {
    gpui_component::init(cx);
    krill_app::ui::apply(gpui_component::ThemeMode::Dark, cx);
    shell::bind_keys(cx);
    krill_app::link_ui::bind_keys(cx);
    krill_app::onboarding::init_actions(cx);
    cx.set_quit_mode(QuitMode::Explicit);
    cx.on_app_quit(|cx| {
        let services = krill_app::hosts::link::begin_shutdown();
        cx.background_executor().spawn(async move {
            for service in services {
                service.shutdown();
            }
        })
    })
    .detach();
    cx.bind_keys([KeyBinding::new("cmd-alt-w", CloseWindow, None)]);
    cx.on_action(|_: &NewWindow, cx| {
        let _ = new_window(cx);
    });
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(window) = cx.active_window() {
            // 액션 전달 중에는 현재 창이 빌려져 있으므로 전달이 끝난 뒤 닫는다.
            cx.defer(move |cx| {
                window
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            });
        }
    });
    set_menus(&krill_app::security::State::default(), cx);
    cx.observe_global::<krill_app::i18n::Locale>(|cx| cx.defer(refresh_menus))
        .detach();
}

#[derive(Default)]
struct MenuSecurity(bool);
impl gpui::Global for MenuSecurity {}

pub(crate) fn set_menus(security: &krill_app::security::State, cx: &mut App) {
    cx.set_global(MenuSecurity(security.is_secure_keyboard_enabled()));
    refresh_menus(cx);
}

fn refresh_menus(cx: &mut App) {
    let secure = cx.try_global::<MenuSecurity>().is_some_and(|state| state.0);
    cx.set_menus([
        Menu::new(krill_app::i18n::catalog::security::APP_MENU.get()).items([
            MenuItem::action(
                krill_app::i18n::catalog::general::SETTINGS_MENU.get(),
                krill_app::settings::OpenSettings,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::onboarding::MENU_OPEN_CHECKS.get(),
                krill_app::onboarding::OpenChecks,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::onboarding::MENU_RERUN.get(),
                krill_app::onboarding::RerunOnboarding,
            ),
            MenuItem::action(
                krill_app::security::text::SECURE_KEYBOARD_MENU.get(),
                shell::security::ToggleSecureInput,
            )
            .checked(secure),
            MenuItem::separator(),
            MenuItem::action(krill_app::i18n::catalog::general::QUIT.get(), shell::Quit),
        ]),
        Menu::new(krill_app::i18n::catalog::general::FILE.get()).items([
            MenuItem::action(
                krill_app::i18n::catalog::general::NEW_WINDOW.get(),
                NewWindow,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::general::NEW_TERMINAL.get(),
                shell::NewTab,
            ),
            MenuItem::separator(),
            MenuItem::action(
                krill_app::i18n::catalog::general::CLOSE_PANE.get(),
                shell::ClosePane,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::general::CLOSE_WINDOW.get(),
                CloseWindow,
            ),
        ]),
        Menu::new(krill_app::i18n::catalog::general::VIEW.get()).items([
            MenuItem::action(
                krill_app::i18n::catalog::settings::SC_ZOOM_IN.get(),
                krill_app::settings::ZoomIn,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::settings::SC_ZOOM_OUT.get(),
                krill_app::settings::ZoomOut,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::settings::SC_ZOOM_RESET.get(),
                krill_app::settings::ZoomReset,
            ),
            MenuItem::separator(),
            MenuItem::action(
                krill_app::i18n::catalog::settings::SC_SIDEBAR.get(),
                shell::ToggleSidebar,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::settings::SC_PANEL.get(),
                shell::TogglePanel,
            ),
        ]),
    ]);
}

#[cfg(test)]
mod lifecycle_tests;
