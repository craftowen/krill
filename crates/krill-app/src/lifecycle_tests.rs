use super::*;

#[gpui::test]
fn app_menu_is_available_without_windows(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        init(cx);
        let menus = cx
            .get_menus()
            .expect("app menu must exist even with no windows");
        assert!(menus.iter().any(|m| m.name.as_ref() == "Krill"));
        assert!(menus.iter().any(|m| m.name.as_ref() == "파일"));
        let labels: Vec<_> = menus
            .iter()
            .flat_map(|menu| &menu.items)
            .filter_map(|item| {
                if let gpui::OwnedMenuItem::Action { name, .. } = item {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect();
        for expected in ["설정…", "Krill 종료", "새 창", "창 닫기"] {
            assert!(labels.contains(&expected), "missing {expected}");
        }
        assert!(cx.is_action_available(&NewWindow));
        assert!(cx.is_action_available(&shell::Quit));
    });
}

#[gpui::test]
fn closing_last_window_then_reopening_restores_layout(cx: &mut gpui::TestAppContext) {
    let path = std::env::temp_dir().join(format!("owe-583-reopen-{}.bin", std::process::id()));
    let mut layout = krill_app::layout::Layout::default();
    layout.set_session(layout.active_tab().focused, krill_proto::SessionId(583));
    layout.save(&path).unwrap();
    cx.update(|cx| {
        init(cx);
        let window = open_shell(cx, Some(path.clone())).unwrap();
        reopen(cx, |_| panic!("existing shell should be activated"));
        assert_eq!(cx.windows().len(), 1);
        window
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(cx.windows().is_empty());
        reopen(cx, |cx| {
            open_shell(cx, Some(path.clone())).unwrap();
        });
        assert_eq!(cx.windows().len(), 1);
        // 창을 다시 열어도 저장된 세션 연결은 지워지지 않는다.
        assert_eq!(krill_app::layout::Layout::load(&path).unwrap(), layout);
        let window = cx.windows()[0];
        window
            .update(cx, |_, window, cx| {
                window.dispatch_action(Box::new(shell::NewTab), cx);
            })
            .unwrap();
    });
    cx.run_until_parked();
    // 편집 중에는 디바운스 저장한다. 정상 창 종료는 마지막 대기분까지 저장해야 한다.
    cx.update(|cx| {
        cx.windows()[0]
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
    });
    cx.run_until_parked();
    let restored = krill_app::layout::Layout::load(&path).unwrap();
    assert_eq!(restored.tabs().len(), 2);
    assert_eq!(restored.sessions(), [krill_proto::SessionId(583)]);
    std::fs::remove_file(path).unwrap();
}

#[gpui::test]
fn settings_only_does_not_prevent_reopening_shell(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        init(cx);
        cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| gpui::Empty))
            .unwrap();
        reopen(cx, |cx| {
            open_shell(cx, None).unwrap();
        });
        assert_eq!(cx.windows().len(), 2);
    });
}

#[gpui::test]
fn close_window_action_keeps_app_available_for_reopen(cx: &mut gpui::TestAppContext) {
    let window = cx.update(|cx| {
        init(cx);
        let window = open_shell(cx, None).unwrap();
        window
            .update(cx, |_, window, _| window.activate_window())
            .unwrap();
        window.into()
    });
    cx.run_until_parked();
    cx.dispatch_action(window, CloseWindow);
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(cx.windows().is_empty());
        assert!(cx.is_action_available(&NewWindow));
        reopen(cx, |cx| {
            open_shell(cx, None).unwrap();
        });
        assert_eq!(cx.windows().len(), 1);
    });
}

#[gpui::test]
fn secure_keyboard_toggle_preserves_app_and_file_menus(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        init(cx);
        let window = open_shell(cx, None).unwrap();
        window
            .update(cx, |s, window, cx| {
                s.on_toggle_secure_input(&shell::security::ToggleSecureInput, window, cx);
            })
            .unwrap();
        let menus = cx.get_menus().expect("menus must exist after toggle");
        assert!(menus.iter().any(|m| m.name.as_ref() == "Krill"));
        assert!(menus.iter().any(|m| m.name.as_ref() == "파일"));
        let items: Vec<_> = menus
            .iter()
            .flat_map(|menu| &menu.items)
            .filter_map(|item| {
                if let gpui::OwnedMenuItem::Action { name, checked, .. } = item {
                    Some((name.as_str(), *checked))
                } else {
                    None
                }
            })
            .collect();
        for expected in [
            ("설정…", false),
            (krill_app::security::text::SECURE_KEYBOARD_MENU.get(), true),
            ("Krill 종료", false),
            ("새 창", false),
            ("새 터미널", false),
            ("칸 닫기", false),
            ("창 닫기", false),
        ] {
            assert!(
                items.contains(&expected),
                "missing {expected:?} in {items:?}"
            );
        }
    });
}

#[gpui::test]
fn menu_notification_reopens_saved_shell_and_selects_requested_session(
    cx: &mut gpui::TestAppContext,
) {
    use krill_proto::SessionId;
    let path = std::env::temp_dir().join(format!("owe-452-reopen-{}.bin", std::process::id()));
    let mut layout = krill_app::layout::Layout::default();
    layout.set_session(layout.active_tab().focused, SessionId(4521));
    layout.new_tab();
    layout.set_session(layout.active_tab().focused, SessionId(4522));
    layout.activate(0);
    layout.save(&path).unwrap();
    cx.update(|cx| {
        init(cx);
        let window = open_shell(cx, Some(path.clone())).unwrap();
        window
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(cx.windows().is_empty());
        open_from_menu(Some(SessionId(4522)), cx, |cx| {
            open_shell(cx, Some(path.clone())).unwrap();
        });
        open_from_menu(None, cx, |_| {
            panic!("메뉴 클릭은 기존 창을 활성화해야 한다")
        });
        assert_eq!(cx.windows().len(), 1);
    });
    cx.run_until_parked();
    // 디바운스 저장은 백그라운드 작업자라 디스크 읽기 전 플러시한다.
    cx.update(|cx| {
        let shell_window = cx
            .windows()
            .into_iter()
            .find_map(|w| w.downcast::<shell::Shell>())
            .expect("shell window must exist");
        shell_window
            .update(cx, |shell, _, _| shell.flush_layout())
            .unwrap();
    });
    let selected = krill_app::layout::Layout::load(&path).unwrap();
    assert_eq!(
        selected.session(selected.active_tab().focused),
        Some(SessionId(4522))
    );
    assert_eq!(
        selected.tabs().len(),
        2,
        "기존 세션에 중복 탭을 만들지 않는다"
    );
    std::fs::remove_file(path).unwrap();
}

#[gpui::test]
fn language_switch_rebuilds_native_menus_and_keeps_the_security_check(
    cx: &mut gpui::TestAppContext,
) {
    use krill_app::i18n::{Language, catalog};
    let dir = std::env::temp_dir().join(format!("mk-menu-i18n-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    cx.update(|cx| {
        init(cx);
        krill_app::settings::init_at(Some(dir.clone()), cx);
        cx.set_global(MenuSecurity(true));
    });
    for language in [Language::En, Language::Ko] {
        cx.update(|cx| krill_app::settings::set_language(language, cx));
        cx.run_until_parked();
        cx.update(|cx| {
            let menus = cx.get_menus().unwrap();
            assert!(
                menus
                    .iter()
                    .any(|menu| menu.name.as_ref() == catalog::general::FILE.get())
            );
            let items: Vec<_> = menus.iter().flat_map(|menu| &menu.items).collect();
            assert!(items.iter().any(|item| matches!(item,
                gpui::OwnedMenuItem::Action { name, checked: true, .. }
                if name.as_str() == catalog::security::SECURE_KEYBOARD_MENU.get()
            )));
            for key in [
                catalog::general::SETTINGS_MENU,
                catalog::general::QUIT,
                catalog::general::NEW_WINDOW,
            ] {
                assert!(items.iter().any(|item| matches!(item,
                    gpui::OwnedMenuItem::Action { name, .. } if name.as_str() == key.get()
                )));
            }
        });
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// OWE-859: `⌘N`은 `shell::NewWorktree`에만 바인딩되어 `NewWindow`와 충돌하지 않고,
/// `NewWindow` 액션은 이미 열린 메인 `Shell` 창을 재사용해 레이아웃 저장 경로 유실을 막는다.
#[gpui::test]
fn cmd_n_belongs_to_new_worktree_and_new_window_reuses_persisted_shell(
    cx: &mut gpui::TestAppContext,
) {
    let path = std::env::temp_dir().join(format!("owe-859-layout-{}.bin", std::process::id()));
    std::fs::remove_file(&path).ok();
    cx.update(|cx| {
        init(cx);
        let typed = [gpui::Keystroke::parse("cmd-n").unwrap()];
        let actions: Vec<_> = cx
            .all_bindings_for_input(&typed)
            .into_iter()
            .map(|b| b.action().name())
            .collect();
        assert_eq!(actions, ["shell::NewWorktree"]);

        let first = open_shell(cx, Some(path.clone())).unwrap();
        let second = new_window(cx).unwrap();
        assert_eq!(
            first, second,
            "NewWindow reuses existing persisted Shell window"
        );
        assert_eq!(cx.windows().len(), 1);
        second
            .update(cx, |_, window, cx| {
                window.dispatch_action(Box::new(shell::NewTab), cx);
            })
            .unwrap();
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let shell_window = cx
            .windows()
            .into_iter()
            .find_map(|w| w.downcast::<shell::Shell>())
            .unwrap();
        shell_window
            .update(cx, |shell, _, _| shell.flush_layout())
            .unwrap();
    });
    let restored = krill_app::layout::Layout::load(&path).unwrap();
    assert_eq!(restored.tabs().len(), 2);
    std::fs::remove_file(path).ok();
}
