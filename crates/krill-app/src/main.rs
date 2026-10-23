//! Krill 앱. 3열 셸 + 탭 + 분할 트리(MK-030). 시작하면 세션 데몬에 붙는다(없으면 띄운다, MK-025). 보이는 칸마다
//! krilld 세션 화면 스트림에 붙어(`krill_app::pane`, OWE-490) 터미널 렌더러(`krill_app::terminal`, MK-027)로 그린다.
//! panic은 크래시 로그로 남기고, 다음 실행이 진단 대화상자를 띄운다(MK-067). `--crash-test`는 그 경로를 검증하려고
//! 시작 직후 일부러 panic한다. 첫 실행이면 온보딩 대화상자를 띄운다(MK-073).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod shell;

use gpui::{
    App, AppContext as _, Bounds, Menu, MenuItem, QuitMode, WindowBounds, WindowHandle,
    WindowOptions, actions, px, size,
};
use std::path::PathBuf;

actions!(
    krill,
    [
        NewWindow,
        CloseWindow,
        HideApp,
        HideOthers,
        ShowAll,
        MinimizeWindow,
        ZoomWindow,
        ToggleFullscreen,
        BringAllToFront,
        ShowShortcuts
    ]
);

struct StartupBounds(Option<krill_app::layout::SavedWindowBounds>);
impl gpui::Global for StartupBounds {}

#[derive(Default)]
struct OpeningWindow {
    pending: bool,
    session: Option<krill_proto::SessionId>,
}
impl gpui::Global for OpeningWindow {}

fn main() -> std::process::ExitCode {
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
    application.on_reopen(|cx| request_open_from_menu(None, cx));
    application.run(|cx: &mut App| {
        init(cx);
        cx.set_global(OpeningWindow {
            pending: true,
            session: None,
        });
        // Resolve startup files and helper paths while the application event loop stays responsive.
        let startup = cx.background_spawn(async {
            krill_log::init(krill_log::Component::App).ok();
            krill_log::forward_log_warnings();
            // Which build is this (installed app vs. a test/nightly binary) and which state it shares.
            let redacted = |path: Option<PathBuf>| {
                path.map(|p| krill_proto::diag::redact(&p.to_string_lossy()))
            };
            let executable = std::env::current_exe().ok();
            let bundle = executable
                .as_deref()
                .and_then(|exe| krill_app::updater::current_app_bundle_in(None, exe));
            tracing::info!(
                version = krill_app::updater::effective_version(bundle.as_deref()),
                executable = redacted(executable),
                state_dir = redacted(krill_proto::daemon::state_dir()),
                state_override = krill_proto::daemon::state_dir_override().is_some(),
                "app starting"
            );
            let bounds = krill_app::layout::default_path()
                .as_deref()
                .and_then(krill_app::layout::Layout::peek_window_bounds);
            let paths = krill_proto::daemon::Paths::dev().ok();
            let env = krill_app::onboarding::Env::discover_current();
            (bounds, paths, env)
        });
        cx.spawn(async move |cx| {
            let (bounds, paths, env) = startup.await;
            cx.update(|cx| {
                #[cfg(target_os = "macos")]
                krill_app::responsiveness::start(cx);
                cx.set_global(StartupBounds(bounds));
                krill_app::settings::init(cx);
                let Some(window) = new_window(cx) else {
                    cx.quit();
                    return;
                };
                if let Some(paths) = paths {
                    krill_app::menu_bar::init(
                        krill_app::sidebar::status_source(paths),
                        request_open_from_menu,
                        cx,
                    );
                }
                if let Some(state) = krill_proto::daemon::state_dir() {
                    let incoming_state = state.clone();
                    cx.background_spawn(async move {
                        let executable = krill_app::hosts::link::executable();
                        if let Err(error) =
                            krill_link::service::ensure_default_server(&executable, &incoming_state)
                        {
                            tracing::warn!(%error, "failed to start incoming access");
                        }
                    })
                    .detach();
                    window
                        .update(cx, |shell, window, cx| {
                            shell.arm_update_ready(state.clone());
                            krill_app::star_nag::init(state.clone(), cx);
                            krill_app::updater::on_startup(state.clone(), window, cx);
                            krill_app::crash::offer_report(
                                state,
                                |p, cx| cx.reveal_path(p),
                                window,
                                cx,
                            );
                            if let Some(env) = env {
                                krill_app::onboarding::offer(env.clone(), cx);
                                krill_app::onboarding::spawn_quick_check(&env, cx);
                                krill_app::accounts::installed::spawn_detect(env.shell, cx);
                            }
                        })
                        .ok();
                }
                let opening = cx.global_mut::<OpeningWindow>();
                opening.pending = false;
                let session = opening.session.take();
                if session.is_some() {
                    open_from_menu(session, cx, |_| {});
                }
            });
        })
        .detach();
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

/// Reopening after the last window closes loads its latest bounds without blocking menus.
fn request_open_from_menu(session: Option<krill_proto::SessionId>, cx: &mut App) {
    if cx
        .windows()
        .iter()
        .any(|window| window.downcast::<shell::Shell>().is_some())
    {
        open_from_menu(session, cx, |_| {});
        return;
    }
    if !cx.has_global::<OpeningWindow>() {
        cx.set_global(OpeningWindow::default());
    }
    let opening = cx.global_mut::<OpeningWindow>();
    if session.is_some() {
        opening.session = session;
    }
    if opening.pending {
        return;
    }
    opening.pending = true;
    let work = cx.background_spawn(async {
        krill_app::layout::default_path()
            .as_deref()
            .and_then(krill_app::layout::Layout::peek_window_bounds)
    });
    cx.spawn(async move |cx| {
        let bounds = work.await;
        cx.update(|cx| {
            cx.set_global(StartupBounds(bounds));
            let opening = cx.global_mut::<OpeningWindow>();
            opening.pending = false;
            let session = opening.session.take();
            open_from_menu(session, cx, |cx| {
                let _ = new_window(cx);
            });
        });
    })
    .detach();
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
    let window_bounds = cx
        .try_global::<StartupBounds>()
        .and_then(|startup| startup.0)
        .map_or_else(
            || WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(760.)), cx)),
            |saved| on_screen(shell::gpui_window_bounds_from_saved(saved), cx),
        );
    let options = WindowOptions {
        window_bounds: Some(window_bounds),
        titlebar: Some(krill_app::ui::titlebar(
            krill_app::i18n::catalog::security::APP_MENU.get(),
        )),
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

/// OWE-842: 저장된 위치가 지금 연결된 어느 화면과도 겹치지 않으면(모니터를 뗀 경우) 저장 크기로
/// 주 화면 가운데에 연다(`Bounds::centered`가 화면 크기로 줄인다). 창 모드는 유지한다.
fn on_screen(saved: WindowBounds, cx: &App) -> WindowBounds {
    let b = saved.get_bounds();
    if cx.displays().iter().any(|d| d.bounds().intersects(&b)) {
        return saved;
    }
    let b = Bounds::centered(None, b.size, cx);
    match saved {
        WindowBounds::Windowed(_) => WindowBounds::Windowed(b),
        WindowBounds::Maximized(_) => WindowBounds::Maximized(b),
        WindowBounds::Fullscreen(_) => WindowBounds::Fullscreen(b),
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
    krill_app::onboarding::init_actions(cx);
    // macOS stays in the menu bar/Dock. Windows starts with last-window quit;
    // menu_bar::init switches to explicit quit only after its tray icon succeeds.
    cx.set_quit_mode(QuitMode::Default);
    cx.on_app_quit(|cx| {
        cx.background_executor().spawn(async move {
            for service in krill_app::hosts::link::begin_shutdown() {
                service.shutdown();
            }
        })
    })
    .detach();
    cx.bind_keys([
        // OWE-859: ⌘N은 메인 창 안 새 워크트리(Orca와 같음), 새 창은 ⇧⌘N(메뉴 표기도 여기서 나온다).
        krill_app::keys::bind("cmd-shift-n", NewWindow, None),
        krill_app::keys::bind("cmd-alt-w", CloseWindow, None),
        // OWE-864: macOS 표준 창·앱 동작. 터미널 문맥 바인딩과 겹치지 않는 전역 키만 쓴다.
        krill_app::keys::bind("cmd-h", HideApp, None),
        krill_app::keys::bind("cmd-alt-h", HideOthers, None),
        krill_app::keys::bind("cmd-m", MinimizeWindow, None),
        krill_app::keys::bind("ctrl-cmd-f", ToggleFullscreen, None),
    ]);
    cx.on_action(|_: &NewWindow, cx| request_open_from_menu(None, cx));
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &MinimizeWindow, cx| {
        if let Some(window) = cx.active_window() {
            window
                .update(cx, |_, window, _| window.minimize_window())
                .ok();
        }
    });
    cx.on_action(|_: &ZoomWindow, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.zoom_window()).ok();
        }
    });
    cx.on_action(|_: &ToggleFullscreen, cx| {
        if let Some(window) = cx.active_window() {
            window
                .update(cx, |_, window, _| window.toggle_fullscreen())
                .ok();
        }
    });
    cx.on_action(|_: &BringAllToFront, cx| {
        for window in cx.windows() {
            window
                .update(cx, |_, window, _| window.activate_window())
                .ok();
        }
    });
    cx.on_action(|_: &ShowShortcuts, cx| {
        krill_app::settings::open_window_at_tab(krill_app::settings::AdminTab::Shortcuts, cx);
    });
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(window) = cx
            .active_window()
            .and_then(|window| window.downcast::<shell::Shell>())
        {
            // 액션 전달 중에는 현재 창이 빌려져 있으므로 전달이 끝난 뒤 닫는다.
            cx.defer(move |cx| {
                window.update(cx, shell::Shell::request_close).ok();
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
    let mut app_items = vec![
        MenuItem::action(
            krill_app::i18n::catalog::general::SETTINGS_MENU.get(),
            krill_app::settings::OpenSettings,
        ),
        MenuItem::action(
            krill_app::i18n::catalog::onboarding::MENU_OPEN_CHECKS.get(),
            krill_app::onboarding::OpenChecks,
        ),
    ];
    // Secure Keyboard Entry는 macOS 전용이다(규칙 10).
    if cfg!(target_os = "macos") {
        app_items.push(
            MenuItem::action(
                krill_app::security::text::SECURE_KEYBOARD_MENU.get(),
                shell::security::ToggleSecureInput,
            )
            .checked(secure),
        );
    }
    app_items.extend([
        MenuItem::separator(),
        // OWE-864: macOS 표준 가리기. `cx.hide` 계열이라 창이 없어도 동작한다.
        MenuItem::action(krill_app::i18n::catalog::general::HIDE_APP.get(), HideApp),
        MenuItem::action(
            krill_app::i18n::catalog::general::HIDE_OTHERS.get(),
            HideOthers,
        ),
        MenuItem::action(krill_app::i18n::catalog::general::SHOW_ALL.get(), ShowAll),
        MenuItem::separator(),
        MenuItem::action(krill_app::i18n::catalog::general::QUIT.get(), shell::Quit),
    ]);
    cx.set_menus([
        Menu::new(krill_app::i18n::catalog::security::APP_MENU.get()).items(app_items),
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
            // OWE-864: 탭 닫기(⇧⌘W). 칸 닫기(⌘W)와 다른 동작이라 별도 항목으로 둔다.
            MenuItem::action(
                krill_app::i18n::catalog::general::CLOSE_TAB.get(),
                shell::CloseTab,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::general::CLOSE_WINDOW.get(),
                CloseWindow,
            ),
        ]),
        // OWE-864: 편집 메뉴. 실행 취소·다시 실행·잘라내기는 포커스된 입력이 처리하고
        // 복사·붙여넣기·모두 선택은 셸(터미널 선택)이 처리한다. 둘 다 `os_action`이라
        // macOS가 네이티브 선택자·단축키 표기로 연결한다.
        Menu::new(krill_app::i18n::catalog::general::EDIT.get()).items([
            MenuItem::os_action(
                krill_app::i18n::catalog::general::UNDO.get(),
                gpui_component::input::Undo,
                gpui::OsAction::Undo,
            ),
            MenuItem::os_action(
                krill_app::i18n::catalog::general::REDO.get(),
                gpui_component::input::Redo,
                gpui::OsAction::Redo,
            ),
            MenuItem::separator(),
            MenuItem::os_action(
                krill_app::i18n::catalog::general::CUT.get(),
                gpui_component::input::Cut,
                gpui::OsAction::Cut,
            ),
            MenuItem::os_action(
                krill_app::i18n::catalog::general::COPY_ITEM.get(),
                shell::Copy,
                gpui::OsAction::Copy,
            ),
            MenuItem::os_action(
                krill_app::i18n::catalog::general::PASTE_ITEM.get(),
                shell::Paste,
                gpui::OsAction::Paste,
            ),
            MenuItem::os_action(
                krill_app::i18n::catalog::general::SELECT_ALL_ITEM.get(),
                shell::SelectAll,
                gpui::OsAction::SelectAll,
            ),
        ]),
        // OWE-864: 보기 메뉴. 전부 이미 동작하는 액션만 올린다.
        Menu::new(krill_app::i18n::catalog::general::VIEW.get()).items([
            MenuItem::action(
                krill_app::i18n::catalog::general::SIDEBAR_TOGGLE.get(),
                shell::ToggleSidebar,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::general::PANEL_TOGGLE.get(),
                shell::TogglePanel,
            ),
            MenuItem::separator(),
            MenuItem::action(
                krill_app::i18n::catalog::general::ZOOM_IN_ITEM.get(),
                krill_app::settings::ZoomIn,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::general::ZOOM_OUT_ITEM.get(),
                krill_app::settings::ZoomOut,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::general::ZOOM_RESET_ITEM.get(),
                krill_app::settings::ZoomReset,
            ),
            MenuItem::separator(),
            MenuItem::action(
                krill_app::i18n::catalog::general::FULLSCREEN_TOGGLE.get(),
                ToggleFullscreen,
            ),
        ]),
        // OWE-864: 윈도우 메뉴. 최소화·확대/축소·모두 앞으로 가져오기는 활성 창에 직접 건다.
        Menu::new(krill_app::i18n::catalog::general::WINDOW_MENU.get()).items([
            MenuItem::action(
                krill_app::i18n::catalog::general::MINIMIZE.get(),
                MinimizeWindow,
            ),
            MenuItem::action(
                krill_app::i18n::catalog::general::ZOOM_WINDOW.get(),
                ZoomWindow,
            ),
            MenuItem::separator(),
            MenuItem::action(
                krill_app::i18n::catalog::general::BRING_ALL_FRONT.get(),
                BringAllToFront,
            ),
        ]),
        // OWE-864: 도움말 메뉴. 단축키 탭으로 바로 가는 실제 항목만 둔다.
        Menu::new(krill_app::i18n::catalog::general::HELP.get()).items([MenuItem::action(
            krill_app::i18n::catalog::general::SHORTCUTS_HELP.get(),
            ShowShortcuts,
        )]),
    ]);
}

#[cfg(test)]
mod lifecycle_tests;
