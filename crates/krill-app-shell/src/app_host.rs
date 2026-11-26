//! Krill 앱 호스트: GPUI 애플리케이션 초기화, 창 수명주기, 네이티브 메뉴, 단일 인스턴스 가드.

use crate::shell;
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
        ShowShortcuts,
        SendFeedback
    ]
);

pub(super) struct StartupBounds(pub(super) Option<krill_app_core::layout::SavedWindowBounds>);
impl gpui::Global for StartupBounds {}

#[derive(Default)]
pub(super) struct OpeningWindow {
    pub(super) pending: bool,
    pub(super) session: Option<krill_proto::SessionId>,
}
impl gpui::Global for OpeningWindow {}

pub fn run() -> std::process::ExitCode {
    // 옛 로그인 세션에 남은 앱은 창도 못 열고 holder도 못 띄운다(SIGTERM·종료 요청도 못 받는다). 끝나야 다시 열린다.
    #[cfg(target_os = "macos")]
    std::thread::Builder::new()
        .name("login-guard".to_owned())
        .spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
                if krill_proto::os::bootstrap_dead() && krill_proto::os::bootstrap_dead() {
                    #[expect(
                        clippy::exit,
                        reason = "macOS login-guard: 옛 로그인 세션에 남은 앱은 이벤트 루프·SIGTERM에 응답하지 않아 즉시 종료해야 한다"
                    )]
                    std::process::exit(0);
                }
            }
        })
        .ok();
    if let Some(code) = krill_app_core::updater::maybe_run_relauncher_cli() {
        return code;
    }
    if let Some(code) = krill_app_core::updater::maybe_run_update_cli() {
        return std::process::ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    // Windows는 X로 닫아도 트레이에 남고 재실행해도 OS가 기존 앱을 깨우지 않는다(on_reopen 미호출).
    // 같은 상태 폴더의 앱이 있으면 그 앱에 창 열기를 넘기고 끝낸다. 업데이트 health 확인 실행은
    // 넘기지 않는다(넘기면 새 버전이 바로 끝나 롤백된다).
    #[cfg(windows)]
    let mut instance = {
        let key =
            krill_app_core::platform::instance_key(krill_proto::daemon::state_dir().as_deref());
        let forward = std::env::var_os(krill_app_core::updater::READY_ENV).is_none();
        match krill_app_core::platform::instance::claim(&key, forward) {
            Some(instance) => instance,
            None => return std::process::ExitCode::SUCCESS,
        }
    };
    #[cfg(windows)]
    let opens = instance.take_opens();
    // 작업 스케줄러처럼 breakaway를 막는 Job에서 실행되면 세션 데몬을 Explorer가 Job 밖에서 띄우게 한다.
    #[cfg(windows)]
    krill_proto::daemon::set_job_escape(krill_app_core::platform::launch_outside_job);
    // OWE-576: gpui-component 아이콘 자산은 재열기·새 창에서도 같은 소스를 쓴다.
    let application =
        gpui_platform::application().with_assets(krill_app_core::ui::icons::AppAssets);
    application.on_reopen(|cx| request_open_from_menu(None, cx));
    application.run(move |cx: &mut App| {
        init(cx);
        #[cfg(windows)]
        if let Some(mut opens) = opens {
            cx.spawn(async move |cx| {
                while opens.recv().await.is_some() {
                    cx.update(|cx| request_open_from_menu(None, cx));
                }
            })
            .detach();
        }
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
                .and_then(|exe| krill_app_core::updater::current_app_bundle_in(None, exe));
            tracing::info!(
                version = krill_app_core::updater::effective_version(bundle.as_deref()),
                executable = redacted(executable),
                state_dir = redacted(krill_proto::daemon::state_dir()),
                state_override = krill_proto::daemon::state_dir_override().is_some(),
                "app starting"
            );
            let bounds = krill_app_core::layout::default_path()
                .as_deref()
                .and_then(krill_app_core::layout::Layout::peek_window_bounds);
            let paths = krill_proto::daemon::Paths::dev().ok();
            (bounds, paths)
        });
        // Discover onboarding environment in parallel without blocking First Paint (`new_window`).
        let env_discovery =
            cx.background_spawn(async { krill_app_core::onboarding::Env::discover_current() });
        cx.spawn(async move |cx| {
            let (bounds, paths) = startup.await;
            cx.update(|cx| {
                #[cfg(target_os = "macos")]
                krill_app_core::responsiveness::start(cx);
                cx.set_global(StartupBounds(bounds));
                krill_app_core::settings::init(cx);
                krill_app_core::errors::start(cx);
                krill_app_core::telemetry::start(cx);
                let Some(window) = new_window(cx) else {
                    cx.quit();
                    return;
                };
                if let Some(paths) = paths {
                    krill_app_core::menu_bar::init(
                        krill_app_core::sidebar::status_source(paths),
                        request_open_from_menu,
                        cx,
                    );
                }
                if let Some(state) = krill_proto::daemon::state_dir() {
                    let incoming_state = state.clone();
                    cx.background_spawn(async move {
                        let executable = krill_app_core::hosts::link::executable();
                        krill_app_core::hosts::link::stop_stale_connect_processes(
                            &incoming_state,
                            &executable,
                        );
                        // 부팅 직후엔 launchd가 느려 10초 시작 제한을 넘길 수 있다. 한 번 실패해도 두 번 더 시도하고
                        // 마지막 실패만 경고로 남긴다.
                        for attempt in 0..3 {
                            match krill_link_core::service::ensure_default_server(
                                &executable,
                                &incoming_state,
                            ) {
                                Ok(()) => break,
                                Err(error) if attempt == 2 => {
                                    tracing::warn!(%error, "failed to start incoming access");
                                }
                                Err(_) => std::thread::sleep(std::time::Duration::from_secs(30)),
                            }
                        }
                    })
                    .detach();
                    window
                        .update(cx, |shell, window, cx| {
                            shell.arm_update_ready(state.clone());
                            krill_app_core::star_nag::init(state.clone(), cx);
                            krill_app_core::updater::on_startup(state.clone(), window, cx);
                            krill_app_core::crash::offer_report(
                                state,
                                |p, cx| cx.reveal_path(p),
                                window,
                                cx,
                            );
                        })
                        .ok();
                    cx.spawn(async move |cx| {
                        let Some(env) = env_discovery.await else {
                            return;
                        };
                        cx.update(|cx| {
                            window
                                .update(cx, |_, _, cx| {
                                    krill_app_core::onboarding::offer(env.clone(), cx);
                                    krill_app_core::onboarding::spawn_quick_check(&env, cx);
                                    krill_app_core::accounts::installed::spawn_detect(
                                        env.shell, cx,
                                    );
                                })
                                .ok();
                        });
                    })
                    .detach();
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
    #[cfg(windows)]
    drop(instance);
    std::process::ExitCode::SUCCESS
}

/// 창이 남아 있으면 앞으로 가져오고, 마지막 창을 닫은 뒤에는 새 셸 창을 연다.
/// 설정 창만 열려 있어도 작업 창은 다시 만든다.
pub(super) fn reopen(cx: &mut App, open: impl FnOnce(&mut App)) {
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
    krill_app_core::menu_bar::sync_window_mode(cx);
    cx.activate(true);
}

/// 메뉴바·알림 클릭을 창 수명과 독립적으로 처리한다.
pub(super) fn open_from_menu(
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
pub(super) fn request_open_from_menu(session: Option<krill_proto::SessionId>, cx: &mut App) {
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
        krill_app_core::layout::default_path()
            .as_deref()
            .and_then(krill_app_core::layout::Layout::peek_window_bounds)
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

pub(super) fn new_window(cx: &mut App) -> Option<WindowHandle<shell::Shell>> {
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
        krill_app_core::menu_bar::sync_window_mode(cx);
        cx.activate(true);
        return Some(window);
    }
    let window = open_shell(cx, krill_app_core::layout::default_path())?;
    window
        .update(cx, |shell, _, cx| shell.connect_daemon(cx))
        .ok();
    Some(window)
}

/// 창 구성과 외부 IO를 분리하여 gpui 테스트가 데몬을 띄우지 않게 한다.
pub(super) fn open_shell(
    cx: &mut App,
    path: Option<PathBuf>,
) -> Option<WindowHandle<shell::Shell>> {
    let window_bounds = cx
        .try_global::<StartupBounds>()
        .and_then(|startup| startup.0)
        .map_or_else(
            || WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(760.)), cx)),
            |saved| on_screen(shell::gpui_window_bounds_from_saved(saved), cx),
        );
    let window_background = krill_app_core::settings::window_background_appearance(
        krill_app_core::settings::window_blur(cx),
    );
    let options = WindowOptions {
        window_bounds: Some(window_bounds),
        window_background,
        window_min_size: Some(size(
            px(krill_app_core::layout::MIN_WINDOW_WIDTH),
            px(krill_app_core::layout::MIN_WINDOW_HEIGHT),
        )),
        titlebar: Some(krill_app_core::ui::titlebar(
            krill_app_core::i18n::catalog::security::APP_MENU.get(),
        )),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| shell::Shell::new(path, cx));
        window.set_rem_size(px(16. * krill_app_core::settings::current(cx).zoom()));
        let focus = view.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        view
    });
    match opened {
        Ok(window) => {
            #[cfg(target_os = "macos")]
            window
                .update(cx, |_, window, cx| record_test_window_geometry(window, cx))
                .ok();
            krill_app_core::menu_bar::sync_window_mode(cx);
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

/// Test-only opt-in observation of AppKit/GPUI bounds; `CGWindow` includes compositor framing.
#[cfg(target_os = "macos")]
pub(crate) fn record_test_window_geometry(window: &gpui::Window, cx: &mut gpui::App) {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if std::env::var_os("KRILL_TEST_WINDOW_GEOMETRY").as_deref() != Some(std::ffi::OsStr::new("1"))
    {
        return;
    }
    let Some(state) = krill_proto::daemon::state_dir_override() else {
        return;
    };
    let state = std::path::PathBuf::from(state);
    if !state.is_absolute() {
        return;
    }
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bounds = window.bounds();
    let value = serde_json::json!({
        "pid": std::process::id(),
        "width": bounds.size.width.as_f32(),
        "height": bounds.size.height.as_f32(),
        "scale": window.scale_factor(),
        "window_count": cx.windows().len(),
        "windowed": !window.is_fullscreen() && !window.is_simple_fullscreen() && !window.is_maximized(),
    });
    cx.background_spawn(async move {
        let path = state.join(format!("memory-window-{sequence:020}.json"));
        let temporary = path.with_extension("tmp");
        let result = std::fs::write(&temporary, value.to_string())
            .and_then(|()| std::fs::rename(&temporary, &path));
        if let Err(error) = result {
            tracing::warn!(kind = ?error.kind(), "memory window probe write failed");
        }
    })
    .detach();
}

/// OWE-842: 저장된 위치에 드래그할 제목 영역이 남아 있지 않으면 저장 크기로
/// 주 화면 가운데에 연다(`Bounds::centered`가 화면 크기로 줄인다). 창 모드는 유지한다.
pub(super) fn on_screen(saved: WindowBounds, cx: &App) -> WindowBounds {
    let b = saved.get_bounds();
    if cx
        .displays()
        .iter()
        .any(|d| has_visible_title_area(b, d.bounds()))
    {
        return saved;
    }
    let b = Bounds::centered(None, b.size, cx);
    match saved {
        WindowBounds::Windowed(_) => WindowBounds::Windowed(b),
        WindowBounds::Maximized(_) => WindowBounds::Maximized(b),
        WindowBounds::Fullscreen(_) => WindowBounds::Fullscreen(b),
    }
}

/// A one-pixel intersection, or a visible bottom edge with the title bar above
/// every screen, cannot provide a usable window recovery target. Use logical
/// pixels so a display's scale factor does not change the required drag area.
pub(super) fn has_visible_title_area(
    window: Bounds<gpui::Pixels>,
    display: Bounds<gpui::Pixels>,
) -> bool {
    let title_height = krill_app_core::ui::TITLEBAR_HEIGHT
        .min(window.size.height)
        .min(display.size.height);
    let drag_width = px(240.).min(window.size.width).min(display.size.width);
    let left = window.origin.x.max(display.origin.x);
    let right = (window.origin.x + window.size.width).min(display.origin.x + display.size.width);
    right - left >= drag_width
        && window.origin.y >= display.origin.y
        && window.origin.y + title_height <= display.origin.y + display.size.height
}

pub(super) fn init(cx: &mut App) {
    gpui_component::init(cx);
    krill_app_core::settings::init_startup_language(cx);
    krill_app_core::ui::apply(gpui_component::ThemeMode::Dark, cx);
    shell::bind_keys(cx);
    krill_app_core::onboarding::init_actions(cx);
    // macOS stays in the menu bar/Dock. Windows starts with last-window quit;
    // menu_bar::init switches to explicit quit only after its tray icon succeeds.
    cx.set_quit_mode(QuitMode::Default);
    cx.on_app_quit(|cx| {
        let services = krill_app_core::hosts::link::begin_shutdown();
        cx.background_executor().spawn(async move {
            krill_app_core::hosts::link::shutdown_services(&services);
        })
    })
    .detach();
    cx.bind_keys([
        // OWE-864: macOS 표준 창·앱 동작. 터미널 문맥 바인딩과 겹치지 않는 전역 키만 쓴다.
        krill_app_core::keys::bind("cmd-m", MinimizeWindow, None),
        krill_app_core::keys::bind("ctrl-cmd-f", ToggleFullscreen, None),
    ]);
    cx.on_action(|_: &NewWindow, cx| request_open_from_menu(None, cx));
    #[cfg(target_os = "macos")]
    {
        cx.bind_keys([
            krill_app_core::keys::bind("cmd-h", HideApp, None),
            krill_app_core::keys::bind("cmd-alt-h", HideOthers, None),
        ]);
        cx.on_action(|_: &HideApp, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    }
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
        krill_app_core::settings::open_window_at_tab(
            krill_app_core::settings::AdminTab::Shortcuts,
            cx,
        );
    });
    cx.on_action(|_: &SendFeedback, cx| {
        krill_app_core::settings::open_window_at_tab(
            krill_app_core::settings::AdminTab::Feedback,
            cx,
        );
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
    krill_app_core::settings::init_actions(cx);
    set_menus(&krill_app_core::security::State::default(), cx);
    cx.observe_global::<krill_app_core::i18n::Locale>(|cx| cx.defer(refresh_menus))
        .detach();
    cx.observe_global::<krill_app_core::settings::Config>(|cx| {
        let show_mobile = krill_app_core::settings::current(cx).show_mobile_button;
        if cx.try_global::<MenuShowMobile>().map(|state| state.0) != Some(show_mobile) {
            cx.set_global(MenuShowMobile(show_mobile));
            cx.defer(refresh_menus);
        }
    })
    .detach();
}

#[derive(Default)]
pub(super) struct MenuSecurity(pub(super) bool);
impl gpui::Global for MenuSecurity {}

#[derive(Default)]
struct MenuShowMobile(bool);
impl gpui::Global for MenuShowMobile {}

pub(crate) fn set_menus(security: &krill_app_core::security::State, cx: &mut App) {
    cx.set_global(MenuSecurity(security.is_secure_keyboard_enabled()));
    refresh_menus(cx);
}

fn refresh_menus(cx: &mut App) {
    let secure = cx.try_global::<MenuSecurity>().is_some_and(|state| state.0);
    let show_mobile = krill_app_core::settings::current(cx).show_mobile_button;
    cx.set_global(MenuShowMobile(show_mobile));
    let mut app_items = vec![
        MenuItem::action(
            krill_app_core::i18n::catalog::general::SETTINGS_MENU.get(),
            krill_app_core::settings::OpenSettings,
        ),
        MenuItem::action(
            krill_app_core::i18n::catalog::onboarding::MENU_OPEN_CHECKS.get(),
            krill_app_core::onboarding::OpenChecks,
        ),
    ];
    // Secure Keyboard Entry는 macOS 전용이다(규칙 10).
    if cfg!(target_os = "macos") {
        app_items.push(
            MenuItem::action(
                krill_app_core::security::text::SECURE_KEYBOARD_MENU.get(),
                shell::security::ToggleSecureInput,
            )
            .checked(secure),
        );
    }
    #[cfg(target_os = "macos")]
    app_items.extend([
        MenuItem::separator(),
        // OWE-864: macOS 표준 가리기. `cx.hide` 계열이라 창이 없어도 동작한다.
        MenuItem::action(
            krill_app_core::i18n::catalog::general::HIDE_APP.get(),
            HideApp,
        ),
        MenuItem::action(
            krill_app_core::i18n::catalog::general::HIDE_OTHERS.get(),
            HideOthers,
        ),
        MenuItem::action(
            krill_app_core::i18n::catalog::general::SHOW_ALL.get(),
            ShowAll,
        ),
    ]);
    app_items.extend([
        MenuItem::separator(),
        MenuItem::action(
            krill_app_core::i18n::catalog::general::QUIT.get(),
            shell::Quit,
        ),
    ]);
    cx.set_menus([
        Menu::new(krill_app_core::i18n::catalog::security::APP_MENU.get()).items(app_items),
        Menu::new(krill_app_core::i18n::catalog::general::FILE.get()).items([
            MenuItem::action(
                krill_app_core::i18n::catalog::general::NEW_WINDOW.get(),
                NewWindow,
            ),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::NEW_TERMINAL.get(),
                shell::NewTab,
            ),
            MenuItem::separator(),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::CLOSE_PANE.get(),
                shell::ClosePane,
            ),
            // OWE-864: 탭 닫기(⇧⌘W). 칸 닫기(⌘W)와 다른 동작이라 별도 항목으로 둔다.
            MenuItem::action(
                krill_app_core::i18n::catalog::general::CLOSE_TAB.get(),
                shell::CloseTab,
            ),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::CLOSE_WINDOW.get(),
                CloseWindow,
            ),
        ]),
        // OWE-864: 편집 메뉴. 실행 취소·다시 실행·잘라내기는 포커스된 입력이 처리하고
        // 복사·붙여넣기·모두 선택은 셸(터미널 선택)이 처리한다. 둘 다 `os_action`이라
        // macOS가 네이티브 선택자·단축키 표기로 연결한다.
        Menu::new(krill_app_core::i18n::catalog::general::EDIT.get()).items([
            MenuItem::os_action(
                krill_app_core::i18n::catalog::general::UNDO.get(),
                gpui_component::input::Undo,
                gpui::OsAction::Undo,
            ),
            MenuItem::os_action(
                krill_app_core::i18n::catalog::general::REDO.get(),
                gpui_component::input::Redo,
                gpui::OsAction::Redo,
            ),
            MenuItem::separator(),
            MenuItem::os_action(
                krill_app_core::i18n::catalog::general::CUT.get(),
                gpui_component::input::Cut,
                gpui::OsAction::Cut,
            ),
            MenuItem::os_action(
                krill_app_core::i18n::catalog::general::COPY_ITEM.get(),
                shell::Copy,
                gpui::OsAction::Copy,
            ),
            MenuItem::action(
                krill_app_core::i18n::catalog::shell::COPY_CLEAN.get(),
                shell::CopyClean,
            ),
            MenuItem::os_action(
                krill_app_core::i18n::catalog::general::PASTE_ITEM.get(),
                shell::Paste,
                gpui::OsAction::Paste,
            ),
            MenuItem::os_action(
                krill_app_core::i18n::catalog::general::SELECT_ALL_ITEM.get(),
                shell::SelectAll,
                gpui::OsAction::SelectAll,
            ),
        ]),
        // OWE-864: 보기 메뉴. 전부 이미 동작하는 액션만 올린다.
        Menu::new(krill_app_core::i18n::catalog::general::VIEW.get()).items([
            MenuItem::action(
                krill_app_core::i18n::catalog::general::SIDEBAR_TOGGLE.get(),
                shell::ToggleSidebar,
            ),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::PANEL_TOGGLE.get(),
                shell::TogglePanel,
            ),
            MenuItem::action(
                krill_app_core::i18n::catalog::shell::TOGGLE_CHAT_VIEW.get(),
                shell::ToggleChatView,
            ),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::SHOW_MOBILE_BUTTON.get(),
                krill_app_core::settings::ToggleMobileButton,
            )
            .checked(show_mobile),
            MenuItem::separator(),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::ZOOM_IN_ITEM.get(),
                krill_app_core::settings::ZoomIn,
            ),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::ZOOM_OUT_ITEM.get(),
                krill_app_core::settings::ZoomOut,
            ),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::ZOOM_RESET_ITEM.get(),
                krill_app_core::settings::ZoomReset,
            ),
            MenuItem::separator(),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::FULLSCREEN_TOGGLE.get(),
                ToggleFullscreen,
            ),
        ]),
        // OWE-864: 윈도우 메뉴. 최소화·확대/축소·모두 앞으로 가져오기는 활성 창에 직접 건다.
        Menu::new(krill_app_core::i18n::catalog::general::WINDOW_MENU.get()).items([
            MenuItem::action(
                krill_app_core::i18n::catalog::general::MINIMIZE.get(),
                MinimizeWindow,
            ),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::ZOOM_WINDOW.get(),
                ZoomWindow,
            ),
            MenuItem::separator(),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::BRING_ALL_FRONT.get(),
                BringAllToFront,
            ),
        ]),
        // OWE-864: 도움말 메뉴. 실제 동작하는 항목만 둔다.
        Menu::new(krill_app_core::i18n::catalog::general::HELP.get()).items([
            MenuItem::action(
                krill_app_core::i18n::catalog::general::SHORTCUTS_HELP.get(),
                ShowShortcuts,
            ),
            MenuItem::action(
                krill_app_core::i18n::catalog::general::SEND_FEEDBACK.get(),
                SendFeedback,
            ),
        ]),
    ]);
}
