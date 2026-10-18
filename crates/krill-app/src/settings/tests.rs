use std::fs;

use gpui::{Action as _, TestAppContext};
use gpui_component::ActiveTheme as _;

use super::*;

actions!(test, [Probe]);

#[gpui::test]
fn account_login_and_status_badges_are_visible_without_launching_cli(cx: &mut TestAppContext) {
    use krill_proto::usage::{
        AccountInfo, AccountStatus, AccountStatusInfo, ProviderKind, Snapshot,
    };
    let d = dir("owe700-703-controls");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    vcx.run_until_parked();
    view.update(vcx, |v, cx| {
        v.tab = AdminTab::Accounts;
        v.accounts_snapshot.accounts = vec![AccountInfo {
            id: "claude-login-test".into(),
            provider: ProviderKind::Claude,
            label: "Work".into(),
            email: None,
            profile_dir: d.join("profile").display().to_string(),
            active: true,
            host_default: false,
            stale: false,
            usage: Snapshot::default(),
        }];
        v.accounts_snapshot.statuses = vec![AccountStatusInfo {
            account_id: "claude-login-test".into(),
            status: AccountStatus::NeedsLogin,
            updated_at: Some(crate::accounts::display::now()),
            detail: None,
        }];
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("admin-login-account-claude-login-test")
            .is_some()
    );
    assert!(
        vcx.debug_bounds("admin-account-status-claude-login-test")
            .is_some()
    );
    fs::remove_dir_all(d).unwrap();
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mk-settings-{}-{name}", std::process::id()));
    fs::remove_dir_all(&d).ok();
    fs::create_dir_all(&d).unwrap();
    d
}

/// 테스트용 앱 기본 바인딩(셸처럼 문맥이 있는 바인딩 하나).
fn defaults() -> Vec<KeyBinding> {
    vec![KeyBinding::new("cmd-j", Probe, Some("Shell"))]
}

fn bound(cx: &App, key: &str) -> Option<&'static str> {
    let typed = [Keystroke::parse(key).unwrap()];
    cx.all_bindings_for_input(&typed)
        .last()
        .map(|b| b.action().name())
}

fn init(cx: &mut TestAppContext, d: &Path) {
    let d = d.to_path_buf();
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.bind_keys(defaults());
        init_at(Some(d), cx);
    });
}

#[test]
fn settings_parse_defaults_and_zoom() {
    let def = Settings::default();
    assert_eq!(Settings::parse("").unwrap(), def);
    assert!(def.notifications);
    assert!(def.webhooks.is_empty());
    assert_eq!(def.keep_awake, KeepAwake::Agent);
    let s = Settings::parse(
        "theme = \"dark\"\nzoom = 1.25\nnotifications = false\nkeep_awake = \"on\"",
    )
    .unwrap();
    assert_eq!(s.theme, ThemeChoice::Dark);
    assert!(!s.notifications);
    assert_eq!(s.keep_awake, KeepAwake::On);
    assert_eq!(Settings::parse(&s.to_toml()).unwrap(), s);
    let alias = Settings::parse("awake = \"off\"").unwrap();
    assert_eq!(alias.keep_awake, KeepAwake::Off);
    assert!(Settings::parse("future_setting = 1").is_ok());
    assert!(Settings::parse("theme = \"blue\"").is_err());
    assert!(Settings::parse("keep_awake = \"sometimes\"").is_err());
    let z = |zoom| Settings {
        zoom,
        ..Settings::default()
    };
    assert!((z(9.0).zoom() - ZOOM_MAX).abs() < f32::EPSILON);
    assert!((z(f32::NAN).zoom() - 1.0).abs() < f32::EPSILON);
    assert!((z(1.0).zoom_step(1) - 1.1).abs() < 1e-6);
    assert!((z(ZOOM_MIN).zoom_step(-1) - ZOOM_MIN).abs() < f32::EPSILON);
}

#[gpui::test]
fn webhook_settings_roundtrip_survive_ui_edits_and_redact_parse_errors(cx: &mut TestAppContext) {
    let d = dir("webhooks");
    let body = "[[webhooks]]\nprovider = \"telegram\"\nurl = \"https://api.telegram.org/botSECRET/sendMessage\"\nchat_id = \"PRIVATE\"\n";
    fs::write(d.join(SETTINGS_FILE), body).unwrap();
    init(cx, &d);
    cx.update(|cx| {
        assert_eq!(current(cx).webhooks.len(), 1);
        assert_eq!(
            Settings::parse(&current(cx).to_toml()).unwrap(),
            current(cx)
        );
        assert!(!format!("{:?}", current(cx)).contains("SECRET"));
        edit(cx, |s| s.zoom = 1.3);
        assert_eq!(current(cx).webhooks.len(), 1);
    });
    let saved = fs::read_to_string(d.join(SETTINGS_FILE)).unwrap();
    assert_eq!(
        fs::metadata(d.join(SETTINGS_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(Settings::parse(&saved).unwrap().webhooks.len(), 1);
    fs::write(
        d.join(SETTINGS_FILE),
        body.replace("telegram", "SECRET_INVALID"),
    )
    .unwrap();
    cx.update(|cx| {
        reload(cx);
        assert_eq!(current(cx).webhooks.len(), 1);
        let error = cx.global::<Config>().error.as_ref().unwrap();
        assert!(!error.contains("SECRET"));
        assert!(!error.contains("PRIVATE"));
    });
    fs::remove_dir_all(d).ok();
}

#[gpui::test]
fn key_override_and_unbind(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let keys = Keys::parse(
            "[bindings]\n\"cmd-k\" = \"test::Probe\"\n\"cmd-j\" = \"\"\n\"cmd-=\" = \"\"",
        )
        .unwrap();
        let mut all = own_bindings();
        all.extend(defaults());
        let b = bindings(all, &keys, cx).unwrap();
        let on = |key: &str| {
            let typed = [Keystroke::parse(key).unwrap()];
            b.iter()
                .filter(|b| b.match_keystrokes(&typed) == Some(false))
                .collect::<Vec<_>>()
        };
        // 재정의는 같은 액션의 기본 문맥(Shell)을 따른다.
        let k = on("cmd-k");
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].action().name(), Probe.name());
        assert!(k[0].predicate().is_some());
        // 빈 이름은 그 키의 기본 바인딩을 뺀다.
        assert!(on("cmd-j").is_empty());
        assert!(on("cmd-=").is_empty());
        assert_eq!(on("cmd--").len(), 1);

        let bad = Keys::parse("[bindings]\n\"cmd-k\" = \"shell::Nope\"").unwrap();
        assert!(bindings(own_bindings(), &bad, cx).is_err());
        let bad = Keys::parse("[bindings]\n\"cmd-k\" = 3").unwrap();
        assert!(bindings(own_bindings(), &bad, cx).is_err());
    });
}

#[gpui::test]
fn file_changes_apply_without_restart(cx: &mut TestAppContext) {
    let d = dir("reload");
    init(cx, &d);
    cx.update(|cx| {
        assert_eq!(bound(cx, "cmd-="), Some(ZoomIn.name()));
        assert_eq!(bound(cx, "cmd-j"), Some(Probe.name()));
        assert_eq!(cx.global::<Config>().error, None);
    });

    fs::write(d.join(SETTINGS_FILE), "theme = \"dark\"\nui_zoom = 1.5").unwrap();
    fs::write(
        d.join(KEYS_FILE),
        "[bindings]\n\"cmd-alt-9\" = \"settings::ZoomIn\"\n\"cmd-=\" = \"\"",
    )
    .unwrap();
    // 폴링 태스크가 부르는 것과 같은 경로.
    cx.update(reload_if_changed);
    cx.update(|cx| {
        assert_eq!(cx.theme().mode, ThemeMode::Dark);
        assert!((cx.global::<Config>().settings.zoom() - 1.5).abs() < f32::EPSILON);
        assert_eq!(bound(cx, "cmd-alt-9"), Some(ZoomIn.name()));
        assert_eq!(bound(cx, "cmd-="), None);
        // 재정의하지 않은 기존 바인딩(셸·gpui-component)은 그대로 남는다.
        assert_eq!(bound(cx, "cmd-j"), Some(Probe.name()));
    });

    // 틀린 파일은 직전 설정을 유지하고 오류를 남긴다.
    fs::write(d.join(SETTINGS_FILE), "theme = 1").unwrap();
    fs::write(d.join(KEYS_FILE), "[bindings").unwrap();
    cx.update(reload);
    cx.update(|cx| {
        assert_eq!(cx.theme().mode, ThemeMode::Dark);
        assert_eq!(bound(cx, "cmd-alt-9"), Some(ZoomIn.name()));
        assert!(cx.global::<Config>().error.is_some());
    });

    // 사용자가 파일을 복구한 뒤 설정 창 버튼으로 저장한다.
    fs::write(d.join(SETTINGS_FILE), "theme = \"dark\"").unwrap();
    cx.update(|cx| edit(cx, |s| s.theme = ThemeChoice::Light));
    cx.update(|cx| assert_eq!(cx.theme().mode, ThemeMode::Light));
    let saved = Settings::parse(&fs::read_to_string(d.join(SETTINGS_FILE)).unwrap()).unwrap();
    assert_eq!(saved.theme, ThemeChoice::Light);
    fs::remove_dir_all(&d).ok();
}

#[gpui::test]
fn legacy_zoom_migrates_to_terminal_font_size_and_resets_ui_zoom(cx: &mut TestAppContext) {
    let d = dir("owe843-legacy-zoom");
    let path = d.join(SETTINGS_FILE);
    // 사용자가 터미널 글꼴을 줄이려다 ⌘-를 눌러 기존 zoom = 0.8로 저장된 상태.
    fs::write(&path, "# keep comment\ntheme = \"dark\"\nzoom = 0.8\n").unwrap();
    init(cx, &d);
    cx.update(|cx| {
        assert_eq!(bound(cx, "cmd-="), Some(ZoomIn.name()));
        assert_eq!(bound(cx, "cmd-+"), Some(ZoomIn.name()));
        assert_eq!(bound(cx, "cmd--"), Some(ZoomOut.name()));
        assert_eq!(bound(cx, "cmd-0"), Some(ZoomReset.name()));
        // 13 * 0.8 = 10.4 -> 10pt로 이관되고 UI 배율은 1.0으로 초기화된다.
        assert_eq!(terminal_font_pt(cx), 10);
        assert!((current(cx).zoom() - 1.0).abs() < f32::EPSILON);
    });
    let on_disk = fs::read_to_string(&path).unwrap();
    assert!(on_disk.contains("# keep comment"), "{on_disk}");
    assert!(on_disk.contains("ui_zoom = 1.0"), "{on_disk}");
    assert!(on_disk.contains("terminal_font_size = 10"), "{on_disk}");
    // 재로드해도 중복 이관되지 않는다.
    cx.update(reload);
    cx.update(|cx| {
        assert_eq!(terminal_font_pt(cx), 10);
        assert!((current(cx).zoom() - 1.0).abs() < f32::EPSILON);
    });
    fs::remove_dir_all(&d).ok();
}

#[gpui::test]
fn zoom_action_sets_window_rem(cx: &mut TestAppContext) {
    let d = dir("zoom");
    init(cx, &d);
    cx.update(|cx| {
        cx.on_action(|_: &ZoomIn, cx| edit(cx, |s| s.zoom = s.zoom_step(1)));
    });
    let (_, vcx) = cx.add_window_view(|_, _| gpui::Empty);
    // 키 → 전역 액션 경로(창이 빌려진 상태에서 처리된다).
    vcx.simulate_keystrokes("cmd-=");
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        assert_eq!(window.rem_size(), px(BASE_REM * 1.1));
        assert_eq!(cx.theme().font_size, px(13. * 1.1));
        assert_eq!(cx.theme().mono_font_size, px(12. * 1.1));
    });
    fs::remove_dir_all(&d).ok();
}

#[gpui::test]
fn regression_settings_edit_preserves_corrupt_file(cx: &mut TestAppContext) {
    let d = dir("corrupt-write");
    let path = d.join(SETTINGS_FILE);
    fs::write(&path, "theme = \"dark\"").unwrap();
    init(cx, &d);
    // 편집기가 초기 로드 뒤 파일을 손상시켜도 디스크를 다시 확인한다.
    for body in [b"theme = 1".as_slice(), b"theme = [", &[0xff]] {
        fs::write(&path, body).unwrap();
        cx.update(|cx| edit(cx, |s| s.zoom = 1.5));
        assert_eq!(fs::read(&path).unwrap(), body);
        cx.update(|cx| {
            assert!(cx.global::<Config>().error.is_some());
            assert_eq!(
                cx.global::<Config>().settings,
                Settings::parse("theme = \"dark\"").unwrap()
            );
        });
    }
    fs::remove_dir_all(d).unwrap();
}

#[gpui::test]
fn regression_settings_edit_replaces_inode(cx: &mut TestAppContext) {
    let d = dir("atomic-write");
    let path = d.join(SETTINGS_FILE);
    fs::write(&path, "theme = \"dark\"").unwrap();
    let old = d.join("old-settings");
    fs::hard_link(&path, &old).unwrap();
    init(cx, &d);
    cx.update(|cx| edit(cx, |s| s.theme = ThemeChoice::Light));
    assert_eq!(fs::read_to_string(old).unwrap(), "theme = \"dark\"");
    assert_eq!(
        Settings::parse(&fs::read_to_string(path).unwrap())
            .unwrap()
            .theme,
        ThemeChoice::Light
    );
    fs::remove_dir_all(d).unwrap();
}

#[gpui::test]
fn regression_settings_edit_applies_on_disk_value(cx: &mut TestAppContext) {
    let d = dir("external-edit");
    let path = d.join(SETTINGS_FILE);
    fs::write(&path, "theme = \"dark\"\nui_zoom = 1.0").unwrap();
    init(cx, &d);
    // 1초 폴링 전에 외부 편집기로 바꾼 유효한 최신 값을 edit()이 바탕으로 삼는다.
    fs::write(&path, "theme = \"light\"\nui_zoom = 1.5").unwrap();
    cx.update(|cx| edit(cx, |s| s.zoom = s.zoom_step(1)));
    cx.update(|cx| {
        assert_eq!(cx.global::<Config>().settings.theme, ThemeChoice::Light);
        assert!((cx.global::<Config>().settings.zoom() - 1.6).abs() < f32::EPSILON);
    });
    let saved = Settings::parse(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved.theme, ThemeChoice::Light);
    assert!((saved.zoom() - 1.6).abs() < f32::EPSILON);
    fs::remove_dir_all(d).unwrap();
}

#[gpui::test]
fn quick_commands_reload_and_settings_edit_preserve_them(cx: &mut TestAppContext) {
    let d = dir("commands");
    init(cx, &d);
    let body = r#"copy_context_lines = 3
[[quick_commands]]
name = "상태"
text = "git status"
[[quick_commands]]
name = "검토"
text = "변경 사항을 검토해줘"
"#;
    fs::write(d.join(SETTINGS_FILE), body).unwrap();
    cx.update(reload_if_changed);
    cx.update(|cx| {
        let s = current(cx);
        assert_eq!(s.quick_commands.len(), 2);
        assert_eq!(s.quick_commands[1].text, "변경 사항을 검토해줘");
        assert_eq!(s.copy_context_lines, 3);
        edit(cx, |s| s.zoom = 1.2);
    });
    let saved = Settings::parse(&fs::read_to_string(d.join(SETTINGS_FILE)).unwrap()).unwrap();
    assert_eq!(saved.quick_commands.len(), 2);
    assert_eq!(saved.copy_context_lines, 3);
    fs::write(d.join(SETTINGS_FILE), "[[quick_commands]]\nname = 3").unwrap();
    cx.update(reload);
    cx.update(|cx| assert_eq!(current(cx).quick_commands, saved.quick_commands));
    fs::remove_dir_all(d).unwrap();
}

#[gpui::test]
fn account_rule_methods_upsert_persist_and_clear(cx: &mut TestAppContext) {
    use krill_proto::usage::{AutoSwitchMetric, AutoSwitchRule, ProviderKind};

    let d = dir("account-rule-methods");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    let rule = AutoSwitchRule {
        provider: ProviderKind::Claude,
        from_account_id: "claude-work".into(),
        enabled: true,
        threshold_percent: 70,
        metric: AutoSwitchMetric::FiveHour,
        target_account_id: Some("claude-backup".into()),
    };
    let other = AutoSwitchRule {
        provider: ProviderKind::Codex,
        ..rule.clone()
    };
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.account_error = Some("previous error".into());
            v.set_account_rule(rule.clone(), cx);
            assert!(v.account_error.is_none());
            assert_eq!(v.accounts().auto_switch.rules, vec![rule.clone()]);
            v.set_account_rule(other.clone(), cx);
            let mut updated = rule.clone();
            updated.enabled = false;
            updated.threshold_percent = 85;
            updated.metric = AutoSwitchMetric::SevenDay;
            v.set_account_rule(updated.clone(), cx);
            let rules = &v.accounts().auto_switch.rules;
            assert_eq!(rules.len(), 2);
            assert!(rules.contains(&updated) && rules.contains(&other));
            let persisted = crate::accounts::refresh_local(&d, Some(&d));
            assert_eq!(persisted.auto_switch.rules.len(), 2);
            assert!(persisted.auto_switch.rules.contains(&updated));
            assert!(persisted.auto_switch.rules.contains(&other));
            v.clear_account_rule(rule.provider, &rule.from_account_id, cx);
            assert_eq!(v.accounts().auto_switch.rules, vec![other.clone()]);
            v.clear_account_rule(rule.provider, &rule.from_account_id, cx);
            assert_eq!(v.accounts().auto_switch.rules, vec![other.clone()]);
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert_eq!(
            view.read(cx).accounts().auto_switch.rules,
            vec![other.clone()]
        );
    });
    assert_eq!(
        crate::accounts::refresh_local(&d, Some(&d))
            .auto_switch
            .rules,
        vec![other]
    );
    fs::remove_dir_all(d).unwrap();
}

#[gpui::test]
fn account_rule_controls_toggle_threshold_metric_target_and_reset(cx: &mut TestAppContext) {
    use krill_proto::usage::{AutoSwitchMetric, ProviderKind, UsageRequest};

    let d = dir("account-rule-controls");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Accounts, cx);
            for (provider, label) in [
                (ProviderKind::Claude, "Work"),
                (ProviderKind::Claude, "Backup"),
                (ProviderKind::Codex, "Other Provider"),
            ] {
                v.apply_account_request(
                    UsageRequest::AddAccount {
                        provider,
                        label: label.into(),
                        email: None,
                        profile_dir: d.join(provider.as_str()).join(label).display().to_string(),
                    },
                    cx,
                );
            }
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-rule-clear-claude-work").is_none());
    assert!(
        vcx.debug_bounds("admin-rule-target-claude-work-claude-work")
            .is_none()
    );
    assert!(
        vcx.debug_bounds("admin-rule-target-claude-work-codex-other-provider")
            .is_none()
    );
    let click = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };
    // OWE-719: 계정별 규칙은 프로바이더 섹션 하단 "고급" 하나 아래에 접혀 있다(Codex 섹션과 별개).
    assert!(vcx.debug_bounds("admin-rule-toggle-claude-work").is_none());
    assert!(vcx.debug_bounds("admin-rule-current-claude-work").is_none());
    assert!(vcx.debug_bounds("admin-accounts-advanced-codex").is_some());
    click(vcx, "admin-accounts-advanced-claude");
    assert!(vcx.debug_bounds("admin-rule-current-claude-work").is_some());
    assert!(
        vcx.debug_bounds("admin-rule-current-claude-backup")
            .is_some()
    );
    assert!(
        vcx.debug_bounds("admin-rule-current-codex-other-provider")
            .is_none()
    );
    // 계정 카드 프리셋은 전역과 같은 공유 목록이다(60%는 없음).
    for pct in krill_proto::usage::AUTO_SWITCH_PRESETS {
        let card: &'static str = format!("admin-rule-threshold-claude-work-{pct}").leak();
        let global: &'static str = format!("admin-threshold-{pct}").leak();
        assert!(vcx.debug_bounds(card).is_some(), "{card}");
        assert!(vcx.debug_bounds(global).is_some(), "{global}");
    }
    assert!(
        vcx.debug_bounds("admin-rule-threshold-claude-work-60")
            .is_none()
    );
    click(vcx, "admin-rule-toggle-claude-work");
    vcx.update(|_, cx| {
        assert!(
            view.read(cx)
                .accounts()
                .auto_switch
                .rule_for(ProviderKind::Claude, "claude-work")
                .unwrap()
                .enabled
        );
    });
    for (selector, expected) in [
        ("admin-rule-threshold-claude-work-70", 70),
        ("admin-rule-threshold-claude-work-85", 85),
    ] {
        click(vcx, selector);
        vcx.update(|_, cx| {
            assert_eq!(
                view.read(cx)
                    .accounts()
                    .auto_switch
                    .rule_for(ProviderKind::Claude, "claude-work")
                    .unwrap()
                    .threshold_percent,
                expected
            );
        });
    }
    for (selector, expected) in [
        (
            "admin-rule-metric-claude-work-5h",
            AutoSwitchMetric::FiveHour,
        ),
        (
            "admin-rule-metric-claude-work-7d",
            AutoSwitchMetric::SevenDay,
        ),
        ("admin-rule-metric-claude-work-max", AutoSwitchMetric::Max),
    ] {
        click(vcx, selector);
        vcx.update(|_, cx| {
            assert_eq!(
                view.read(cx)
                    .accounts()
                    .auto_switch
                    .rule_for(ProviderKind::Claude, "claude-work")
                    .unwrap()
                    .metric,
                expected
            );
        });
    }
    click(vcx, "admin-rule-target-claude-work-claude-backup");
    vcx.update(|_, cx| {
        assert_eq!(
            view.read(cx)
                .accounts()
                .auto_switch
                .rule_for(ProviderKind::Claude, "claude-work")
                .unwrap()
                .target_account_id
                .as_deref(),
            Some("claude-backup")
        );
    });
    click(vcx, "admin-rule-target-auto-claude-work");
    vcx.update(|_, cx| {
        assert!(
            view.read(cx)
                .accounts()
                .auto_switch
                .rule_for(ProviderKind::Claude, "claude-work")
                .unwrap()
                .target_account_id
                .is_none()
        );
    });
    click(vcx, "admin-rule-toggle-claude-work");
    vcx.update(|_, cx| {
        let rule = view
            .read(cx)
            .accounts()
            .auto_switch
            .rule_for(ProviderKind::Claude, "claude-work")
            .unwrap();
        assert!(!rule.enabled);
        assert_eq!(rule.threshold_percent, 85);
        assert_eq!(rule.metric, AutoSwitchMetric::Max);
    });
    click(vcx, "admin-rule-clear-claude-work");
    vcx.update(|_, cx| {
        assert!(
            view.read(cx)
                .accounts()
                .auto_switch
                .rule_for(ProviderKind::Claude, "claude-work")
                .is_none()
        );
    });
    assert!(vcx.debug_bounds("admin-rule-clear-claude-work").is_none());
    assert!(
        crate::accounts::refresh_local(&d, Some(&d))
            .auto_switch
            .rules
            .is_empty()
    );
    fs::remove_dir_all(d).unwrap();
}

#[gpui::test]
fn admin_center_tabs_accounts_auto_switch_and_ssh_hosts(cx: &mut TestAppContext) {
    use krill_proto::usage::{AutoSwitchPolicy, ProviderKind, UsageRequest};

    let d = dir("admin-center");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));

    // Cycle through all 7 Admin tabs and verify rendering succeeds without panic.
    for tab in AdminTab::ALL {
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| v.set_tab(tab, cx));
            assert_eq!(view.read(cx).tab(), tab);
            window.draw(cx).clear(cx);
        });
    }

    // Add a Claude account, switch to it, and configure Auto-Switch policy.
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Accounts, cx);
            v.apply_account_request(
                UsageRequest::AddAccount {
                    provider: ProviderKind::Claude,
                    label: "Backup Max".into(),
                    email: Some("backup@example.com".into()),
                    profile_dir: d.join("claude-backup").display().to_string(),
                },
                cx,
            );
            v.apply_account_request(
                UsageRequest::SwitchAccount {
                    provider: ProviderKind::Claude,
                    account_id: "claude-backup-max".into(),
                },
                cx,
            );
            v.apply_account_request(
                UsageRequest::SetAutoSwitch(AutoSwitchPolicy {
                    enabled: true,
                    threshold_percent: 80,
                    switch_on_rate_limit: true,
                    rules: Vec::new(),
                }),
                cx,
            );
        });
        window.draw(cx).clear(cx);
    });

    vcx.update(|_, cx| {
        let snap = view.read(cx).accounts();
        assert!(snap.auto_switch.enabled);
        assert_eq!(snap.auto_switch.threshold_percent, 80);
        let active = snap
            .accounts
            .iter()
            .find(|a| a.provider == ProviderKind::Claude && a.active)
            .unwrap();
        assert_eq!(active.id, "claude-backup-max");
    });

    // Add, connect, and remove SSH host in SSH tab, and verify window reuse via open_window_at_tab.
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Ssh, cx);
            v.ssh_hosts = crate::ssh_hosts::add_host(
                Some(&d),
                "mac-node-1",
                "dev@192.168.1.11",
                Some(22),
                None,
            )
            .unwrap();
        });
        window.draw(cx).clear(cx);
        assert_eq!(view.read(cx).ssh_hosts().len(), 1);
        let host = view.read(cx).ssh_hosts()[0].clone();
        view.update(cx, |v, cx| v.connect_ssh_host(host.clone(), cx));
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-link-pairing").is_some());
    vcx.update(|_, cx| {
        let pending = take_pending_ssh_connect(cx).unwrap();
        assert_eq!(pending.label, "mac-node-1");
        let id = view.read(cx).ssh_hosts()[0].id.clone();
        view.update(cx, |v, cx| v.remove_ssh_host(&id, cx));
        assert!(view.read(cx).ssh_hosts().is_empty());

        // Calling open_window_at_tab reuses the existing SettingsView window instead of opening a second one.
        let before_windows = cx.windows().len();
        open_window_at_tab(AdminTab::Accounts, cx);
        assert_eq!(cx.windows().len(), before_windows);
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert_eq!(view.read(cx).tab(), AdminTab::Accounts);
    });

    fs::remove_dir_all(d).ok();
}

#[gpui::test]
fn m8_orchestration_and_checkpoint_badges_round_trip(_cx: &mut TestAppContext) {
    use krill_proto::orchestration::{OrchestrationStore, WorktreeStore, install_skill};

    let d = dir("m8-orch-badges");
    let sidebar_file = d.join("sidebar.bin");
    let wt = d.join("wt-feature");
    let wt_key = wt.to_string_lossy().into_owned();

    WorktreeStore::mutate(&d, |store| {
        store.unread.insert(wt_key.clone(), true);
        store
            .statuses
            .insert(wt_key.clone(), "in-review".to_owned());
        store.prs.insert(wt_key.clone(), 128);
        store.issues.insert(wt_key.clone(), "OWE-626".to_owned());
        store.add_reference(&wt_key, "https://github.com/org/repo/pull/128")?;
        store.add_reference(&wt_key, "https://linear.app/issue/OWE-626")?;
        Ok(())
    })
    .unwrap();

    assert_eq!(
        crate::sidebar::external::checkpoint_badge_for(Some(&sidebar_file), &wt).as_deref(),
        Some("● REVIEW #128 OWE-626 🔗2")
    );
    assert_eq!(
        crate::sidebar::external::checkpoint_badge_for(Some(&sidebar_file), &d.join("wt-empty")),
        None
    );

    OrchestrationStore::mutate(&d, |orch| {
        let run = orch.create_run("M8 Release", Some(7))?;
        let task = orch.create_task(&run.id, "Sidebar badges", "verify", Vec::new())?;
        orch.start_worker(
            &run.id,
            Some(&task.id),
            "worker-ui",
            "claude",
            Some(42),
            "/tmp",
            "Verify sidebar badges",
        )?;
        Ok(())
    })
    .unwrap();

    assert_eq!(
        crate::sidebar::external::session_orch_badge(Some(&sidebar_file), 42).as_deref(),
        Some("⚡ worker-ui")
    );
    assert_eq!(
        crate::sidebar::external::session_orch_badge(Some(&sidebar_file), 7).as_deref(),
        Some(crate::i18n::catalog::sidebar::COORD_BADGE.get())
    );
    assert_eq!(
        crate::sidebar::external::session_orch_badge(Some(&sidebar_file), 999),
        None
    );

    let skills_dir = d.join("skills");
    for name in [
        "minikrill",
        "orchestration",
        "worktree-checkpoints",
        "computer-use",
    ] {
        let installed = install_skill(name, &skills_dir).unwrap();
        assert!(installed.is_file());
        let body = fs::read_to_string(&installed).unwrap();
        assert!(body.contains(name));
    }

    fs::remove_dir_all(d).ok();
}

#[gpui::test]
fn orca_admin_layout_and_antigravity_provider_assets(cx: &mut TestAppContext) {
    use krill_proto::usage::{ProviderKind, UsageRequest};

    for provider in [
        ProviderKind::Claude,
        ProviderKind::Codex,
        ProviderKind::Antigravity,
        ProviderKind::OpenCode,
    ] {
        assert!(
            crate::ui::icons::provider_svg_bytes(provider).starts_with(b"<svg"),
            "missing svg for {provider:?}"
        );
    }
    for agent_id in ["claude", "codex", "agy", "cursor", "opencode"] {
        assert!(
            crate::ui::icons::agent_svg_bytes(agent_id).starts_with(b"<svg"),
            "missing svg for {agent_id}"
        );
    }
    assert!(crate::ui::icons::KRILL_SVG.starts_with(b"<svg"));
    cx.update(|cx| {
        assert!(
            cx.svg_renderer()
                .parse_svg(crate::ui::icons::KRILL_SVG)
                .is_ok(),
            "KRILL_SVG must parse in gpui svg_renderer"
        );
    });

    let d = dir("orca-admin-agy");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));

    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Accounts, cx);
            v.apply_account_request(
                UsageRequest::AddAccount {
                    provider: ProviderKind::Antigravity,
                    label: "Google Workspace".into(),
                    email: Some("dev@google.com".into()),
                    profile_dir: d.join("agy-profile").display().to_string(),
                },
                cx,
            );
            v.apply_account_request(
                UsageRequest::SwitchAccount {
                    provider: ProviderKind::Antigravity,
                    account_id: "agy-google-workspace".into(),
                },
                cx,
            );
        });
        window.draw(cx).clear(cx);
    });

    vcx.update(|window, cx| {
        let snap = view.read(cx).accounts();
        let active = snap
            .accounts
            .iter()
            .find(|a| a.provider == ProviderKind::Antigravity && a.active)
            .unwrap();
        assert_eq!(active.id, "agy-google-workspace");
        assert_eq!(active.email.as_deref(), Some("dev@google.com"));

        edit(cx, |s| s.default_agent = "agy".into());
        view.update(cx, |v, cx| v.set_tab(AdminTab::Agents, cx));
        window.draw(cx).clear(cx);
        assert_eq!(current(cx).default_agent, "agy");
    });

    let agy_hooks = d.join(".gemini").join("config").join("hooks.json");
    let installed_path = install_agy_hook(&agy_hooks).unwrap();
    assert_eq!(installed_path, agy_hooks);
    let hooks_val: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&agy_hooks).unwrap()).unwrap();
    // CLI(`krill hook install agy`)와 같은 모양: `minikrill` 묶음 키, `hook agy` 포함 명령.
    for event in [
        "PreInvocation",
        "PostInvocation",
        "Stop",
        "PreToolUse",
        "PostToolUse",
    ] {
        let entries = hooks_val["minikrill"][event]
            .as_array()
            .unwrap_or_else(|| panic!("missing hook array for {event}"));
        assert!(!entries.is_empty(), "empty hook array for {event}");
        assert!(
            entries.iter().all(|e| e.to_string().contains("hook agy")),
            "missing hook agy command for {event}"
        );
    }

    // 설정 검색(OWE-656): ⌘F로 포커스, 입력어로 내비를 거르고, 결과 클릭 시 그 탭으로, Esc로 지운다.
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-search-pill").is_some());
    vcx.simulate_keystrokes("cmd-f");
    vcx.simulate_input("ssh");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-tab-ssh").is_some());
    assert!(vcx.debug_bounds("admin-tab-general").is_none());
    assert!(vcx.debug_bounds("admin-tab-agents").is_none());
    let ssh = vcx.debug_bounds("admin-tab-ssh").unwrap();
    vcx.simulate_click(ssh.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|_, cx| assert_eq!(view.read(cx).tab(), AdminTab::Ssh));
    vcx.simulate_keystrokes("cmd-f");
    vcx.simulate_input("zzz-no-such-setting");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-search-empty").is_some());
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-tab-general").is_some());
    assert!(vcx.debug_bounds("admin-search-empty").is_none());

    fs::remove_dir_all(d).ok();
}

/// 시스템 기본값 카드 제목에 `{dir} ({env})` 같은 템플릿 원문이 새지 않는다.
#[test]
fn account_heading_formats_system_default_without_raw_placeholders() {
    use krill_proto::usage::{AccountInfo, ProviderKind};
    let mut acc = AccountInfo {
        id: "claude-host".into(),
        provider: ProviderKind::Claude,
        label: "Host (.claude)".into(),
        email: None,
        profile_dir: "/h/.claude".into(),
        active: true,
        host_default: true,
        stale: false,
        usage: krill_proto::usage::Snapshot::default(),
    };
    let (title, detail) = account_heading(&acc);
    assert_eq!(title, text::SYSTEM_DEFAULT.get());
    assert!(
        detail.contains("Claude Code") && !detail.contains('{'),
        "{detail}"
    );
    acc.email = Some("me@example.com".into());
    let (title, _) = account_heading(&acc);
    assert!(
        title.ends_with("me@example.com") && !title.contains('{'),
        "{title}"
    );
    acc.host_default = false;
    assert_eq!(
        account_heading(&acc),
        ("me@example.com".into(), "/h/.claude".into())
    );
    acc.email = Some(String::new());
    assert_eq!(account_heading(&acc).0, "Host (.claude)");
}

/// Orca 패리티: `+ 계정 추가`는 카드 목록 위 서브헤더에, 시스템 기본값은 맨 위 한 장(제거 버튼 없음), 저장된 계정은 휴지통 제거 버튼을 가진다.
#[gpui::test]
fn accounts_tab_puts_add_button_above_cards_and_host_default_first(cx: &mut TestAppContext) {
    use krill_proto::usage::{ProviderKind, UsageRequest};

    let d = dir("accounts-layout");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    vcx.run_until_parked();
    let bounds = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
        vcx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"))
    };
    let tab = bounds(vcx, "admin-tab-accounts");
    vcx.simulate_click(tab.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    // OWE-708: 추가한 계정이 없으면 점선 빈 상태 안내가 보인다.
    assert!(vcx.debug_bounds("admin-accounts-empty-claude").is_some());
    vcx.update(|_, cx| {
        assert_eq!(view.read(cx).tab, AdminTab::Accounts);
        view.update(cx, |v, cx| {
            v.apply_account_request(
                UsageRequest::AddAccount {
                    provider: ProviderKind::Claude,
                    label: "Work".into(),
                    email: None,
                    profile_dir: d.join("claude-work").display().to_string(),
                },
                cx,
            );
        });
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("admin-remove-account-claude-host")
            .is_none()
    );
    assert!(vcx.debug_bounds("admin-accounts-empty-claude").is_none());
    assert!(vcx.debug_bounds("admin-accounts-empty-codex").is_some());
    let add = bounds(vcx, "admin-add-account-claude");
    let host = bounds(vcx, "admin-account-row-claude-host");
    let remove = bounds(vcx, "admin-account-row-claude-work");
    assert!(add.origin.y < host.origin.y, "{add:?} {host:?}");
    assert!(host.origin.y < remove.origin.y, "{host:?} {remove:?}");
    fs::remove_dir_all(d).unwrap();
}

/// OWE-696·697: Orca 감지 계정은 전환·제거 버튼 없이 "Krill에서 실행 불가(Orca 관리)"로 보이고,
/// 데몬에 닿지 못한 화면은 "데몬 연결 필요" 배너와 함께 계정 변경을 거부한다(로컬 파일 쓰기 없음).
#[gpui::test]
fn orca_accounts_hide_actions_and_offline_view_is_read_only(cx: &mut TestAppContext) {
    use krill_proto::usage::{AccountInfo, ProviderKind, UsageRequest};

    let d = dir("owe696-orca-offline");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    vcx.run_until_parked();
    let tab = vcx.debug_bounds("admin-tab-accounts").unwrap();
    vcx.simulate_click(tab.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let orca = AccountInfo {
        id: "claude-orca-96f8c826".into(),
        provider: ProviderKind::Claude,
        label: "o@example.com".into(),
        email: Some("o@example.com".into()),
        profile_dir: String::new(),
        active: false,
        host_default: false,
        stale: false,
        usage: krill_proto::usage::Snapshot::default(),
    };
    assert_eq!(
        account_heading(&orca).1,
        text::ACCOUNT_ORCA_MANAGED.get(),
        "orca card must say it can't run in Krill"
    );
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.accounts_snapshot.accounts.push(orca.clone());
            v.accounts_offline = true;
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-accounts-daemon-required").is_some());
    assert!(
        vcx.debug_bounds("admin-switch-account-claude-orca-96f8c826")
            .is_none()
    );
    assert!(
        vcx.debug_bounds("admin-remove-account-claude-orca-96f8c826")
            .is_none()
    );
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.apply_account_request(
                UsageRequest::AddAccount {
                    provider: ProviderKind::Claude,
                    label: "Work".into(),
                    email: None,
                    profile_dir: d.join("work").display().to_string(),
                },
                cx,
            );
            assert_eq!(
                v.account_error.as_deref(),
                Some(text::ACCOUNTS_DAEMON_REQUIRED.get())
            );
        });
    });
    assert!(
        !crate::accounts::config_path(&d, Some(&d)).exists(),
        "offline view must not write accounts.toml"
    );
    fs::remove_dir_all(d).ok();
}

/// OWE-697(AS-06 잔여): `SettingsView::new`는 계정·호스트 파일을 UI 스레드에서 읽지 않는다.
#[gpui::test]
fn settings_view_new_defers_account_scan(cx: &mut TestAppContext) {
    use krill_proto::usage::{ProviderKind, UsageRequest};
    let d = dir("owe697-new-defers");
    init(cx, &d);
    crate::accounts::request_local(
        &d,
        Some(&d),
        &UsageRequest::AddAccount {
            provider: ProviderKind::Claude,
            label: "Seeded".into(),
            email: None,
            profile_dir: d.join("seeded").display().to_string(),
        },
    )
    .unwrap();
    let view = cx.new(SettingsView::new);
    view.update(cx, |v, _| {
        assert!(
            v.accounts().accounts.is_empty(),
            "constructor must not scan accounts synchronously"
        );
    });
    cx.run_until_parked();
    view.update(cx, |v, _| {
        assert!(v.accounts().accounts.iter().any(|a| a.label == "Seeded"));
        assert!(
            !v.accounts_offline,
            "temp state dir never probes the daemon"
        );
    });
    fs::remove_dir_all(d).ok();
}

/// 2탭 사용량 문자열: `resets_at`은 상대 시각으로 보이고, 빈·0·음수·파싱 실패는 숨긴다.
#[test]
fn account_usage_format_shows_relative_reset_and_hides_invalid() {
    use krill_proto::usage::{Snapshot, Window};
    let win = |percent, resets_at: Option<&str>| Window {
        percent,
        resets_at: resets_at.map(str::to_owned),
    };
    let snap = Snapshot {
        five_hour: Some(win(42, Some("4102444800"))),
        seven_day: Some(win(7, None)),
    };
    let rendered = format_account_usage(&snap);
    assert!(
        rendered.contains("42%") && rendered.contains("7%"),
        "{rendered}"
    );
    assert!(
        rendered.contains("후 리셋") && !rendered.contains("4102444800"),
        "{rendered}"
    );
    let stale = format_account_usage(&Snapshot {
        five_hour: Some(win(90, Some("1000000000"))),
        seven_day: None,
    });
    assert!(stale.contains("갱신 필요"), "{stale}");
    for bad in ["", "0", "  ", "-5", "내일", "2026-13-01T00:00:00Z"] {
        let snap = Snapshot {
            five_hour: Some(win(90, Some(bad))),
            seven_day: None,
        };
        let rendered = format_account_usage(&snap);
        assert!(
            !rendered.contains("리셋") && !rendered.contains("갱신"),
            "{bad:?} must hide reset: {rendered}"
        );
    }
    assert_eq!(
        format_account_usage(&Snapshot::default()),
        text::ACCOUNT_USAGE_EMPTY.get()
    );
}

/// OWE-652: 리셋 분류 — 미래(epoch·ISO·오프셋·소수점)·과거·파싱 실패.
#[test]
fn classify_reset_handles_future_past_and_parse_failure() {
    use super::{ResetDue, classify_reset};
    const NOW: u64 = 1_800_000_000;
    assert_eq!(classify_reset("1780003600", 1_780_003_600), ResetDue::Stale);
    assert_eq!(classify_reset("1800003600", NOW), ResetDue::Future(3_600));
    assert_eq!(classify_reset("1800000000", NOW), ResetDue::Stale);
    // 같은 순간을 Z·+09:00·소수점으로 적어도 같은 남은 초가 나온다.
    for ts in [
        "2030-01-01T00:00:00Z",
        "2030-01-01T09:00:00+09:00",
        "2030-01-01T00:00:00.428638+00:00",
    ] {
        assert_eq!(
            classify_reset(ts, NOW),
            ResetDue::Future(93_456_000),
            "{ts}"
        );
    }
    // 이미 지난 리셋(증적의 ISO 원문 모양)은 Stale이다.
    for ts in [
        "2026-10-16T10:30:00.000Z",
        "2026-10-05T22:59:59.914509+00:00",
        "2026-10-03T05:30:00.428638+00:00",
        "1000000000",
    ] {
        assert_eq!(classify_reset(ts, 1_900_000_000), ResetDue::Stale, "{ts}");
    }
    for bad in [
        "",
        "  ",
        "0",
        "-5",
        "내일",
        "not-a-date",
        "2026-13-01T00:00:00Z",
        "2026-02-30T00:00:00Z",
        "2026-10-16T25:00:00Z",
        "2026-10-16 10:30:00",
        "2030-01-01",
    ] {
        assert_eq!(classify_reset(bad, NOW), ResetDue::Unknown, "{bad:?}");
    }
}

#[test]
fn reset_parser_preserves_seconds_and_matches_daemon() {
    for timestamp in [
        "2030-01-01T00:00:59Z",
        "2030-01-01T09:00:59+09:00",
        "2030-01-01T00:00:59,125Z",
    ] {
        let epoch = krill_proto::time::parse_reset_epoch(timestamp).unwrap();
        let now = u64::try_from(epoch - 1).unwrap();
        assert_eq!(classify_reset(timestamp, now), ResetDue::Future(1));
    }
}

/// OWE-652: 상대 꼬리표 — 분·시·일 단위와 곧 리셋·갱신 필요.
#[test]
fn relative_reset_suffix_formats_countdown_and_stale() {
    use super::relative_reset_suffix;
    const NOW: u64 = 1_800_000_000;
    let suffix = |ts: &str, now: u64| relative_reset_suffix(Some(ts), now);
    assert_eq!(suffix("1800000030", NOW).as_deref(), Some("곧 리셋"));
    assert_eq!(suffix("1800000300", NOW).as_deref(), Some("5분 후 리셋"));
    assert_eq!(
        suffix("1800006000", NOW).as_deref(),
        Some("1시간 40분 후 리셋")
    );
    assert_eq!(
        suffix("1800090000", NOW).as_deref(),
        Some("1일 1시간 후 리셋")
    );
    assert_eq!(
        suffix("2030-01-01T00:00:00Z", NOW)
            .as_deref()
            .map(|s| s.contains("후 리셋")),
        Some(true)
    );
    assert_eq!(suffix("1799999999", NOW).as_deref(), Some("갱신 필요"));
    assert_eq!(suffix("내일", NOW), None);
    assert_eq!(relative_reset_suffix(None, NOW), None);
}

/// 7탭 기록 즉시 삭제: 데몬 경로가 없으면 IPC를 시도하지 않고 오류로 보고한다.
#[test]
fn history_clear_without_daemon_paths_errors() {
    assert!(request_history_clear(None).is_err());
}

/// 7탭 기록 즉시 삭제 버튼: 임시 상태 디렉터리에서는 실제 데몬을 건드리지 않고
/// 미연결 상태를 표시한다.
#[gpui::test]
fn resources_tab_history_clear_reports_unavailable_in_test_dir(cx: &mut TestAppContext) {
    let d = dir("history-clear");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Resources, cx));
    });
    vcx.run_until_parked();
    let clear = vcx
        .debug_bounds("admin-history-clear")
        .expect("missing admin-history-clear");
    // OWE-651: 파괴적 동작은 첫 클릭에 확인 상태로만 바뀌고 실행하지 않는다.
    vcx.simulate_click(clear.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-history-status").is_none());
    vcx.update(|_, cx| assert_eq!(view.read(cx).armed.as_deref(), Some("admin-history-clear")));
    vcx.simulate_click(clear.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-history-status").is_some());
    vcx.update(|_, cx| {
        assert_eq!(
            view.read(cx).history_status.as_deref(),
            Some(text::RECORDING_DELETE_UNAVAILABLE.get())
        );
    });
    fs::remove_dir_all(d).unwrap();
}

/// 7탭 기록 일시중지: 데몬 경로가 없으면 전송 실패로 보고한다.
#[test]
fn history_pause_without_daemon_paths_fails() {
    assert!(matches!(
        request_history_pause(None, true),
        Err(PauseError::Failed(_))
    ));
}

/// 7탭 기록 일시중지 토글: 임시 상태 디렉터리에서는 데몬에 보내지 않고 미연결을
/// 표시하며, 저장된 값은 그대로(원복) 둔다.
#[gpui::test]
fn resources_tab_recording_toggle_reverts_without_daemon(cx: &mut TestAppContext) {
    let d = dir("recording-pause");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Resources, cx));
    });
    vcx.run_until_parked();
    let paused = vcx
        .debug_bounds("admin-recording-paused")
        .expect("missing admin-recording-paused");
    vcx.simulate_click(paused.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-recording-status").is_some());
    vcx.update(|_, cx| {
        assert_eq!(
            view.read(cx).recording_status.as_deref(),
            Some(text::RECORDING_DELETE_UNAVAILABLE.get())
        );
        assert!(!current(cx).recording_paused);
    });
    fs::remove_dir_all(d).unwrap();
}

// 설치 테스트마다 별도 영수증을 사용하며 환경변수는 변경하지 않는다.
const CLAUDE_HOOK_COMMAND: &str =
    "if [ -n \"${MK_PANE_ID:-}\" ]; then exec 'krill' hook claude; else printf '{}\\n'; fi";
const AGY_HOOK_BASE_COMMAND: &str =
    "if [ -n \"${MK_PANE_ID:-}\" ]; then exec 'krill' hook agy; else printf '{}\\n'; fi";

fn install_claude_hook(path: &Path) -> std::io::Result<PathBuf> {
    install_hook_file_at(
        path,
        krill_proto::hook_install::TargetAgent::Claude,
        &path.parent().unwrap().join(".hook-state"),
    )
}

fn install_agy_hook(path: &Path) -> std::io::Result<PathBuf> {
    install_hook_file_at(
        path,
        krill_proto::hook_install::TargetAgent::Other(krill_proto::agent::hooks::Agent::Agy),
        &path.parent().unwrap().join(".hook-state"),
    )
}

/// 훅 설치 데이터 손실 방지: 손상·비UTF8·예상 밖 타입이면 오류 + 원본 바이트 그대로,
/// 백업·임시 파일도 남기지 않는다. 실제 사용자 경로를 쓰지 않는다(임시 디렉터리만).
#[test]
fn hook_install_preserves_unparseable_and_wrong_typed_files() {
    let root = std::env::temp_dir().join(format!("mk-hook-safe-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    let claude = root.join("settings.json");
    let agy = root.join("hooks.json");
    // (이름, 바이트, claude 실패?, agy 실패?)
    let cases: &[(&str, &[u8], bool, bool)] = &[
        ("corrupt", b"{not json", true, true),
        ("non-utf8", &[0xff, 0xfe, b'{', b'}'], true, true),
        ("top-array", b"[1, 2]", true, true),
        ("hooks-array", br#"{"hooks": []}"#, true, false),
        (
            "event-object",
            br#"{"hooks": {"PreToolUse": {}}}"#,
            true,
            false,
        ),
        ("bundle-string", br#"{"minikrill": "nope"}"#, false, true),
    ];
    for (name, bytes, claude_fails, agy_fails) in cases {
        for (path, install, fails) in [
            (
                &claude,
                install_claude_hook as fn(&Path) -> std::io::Result<PathBuf>,
                *claude_fails,
            ),
            (
                &agy,
                install_agy_hook as fn(&Path) -> std::io::Result<PathBuf>,
                *agy_fails,
            ),
        ] {
            // 성공 케이스는 매번 깨끗한 영수증 상태에서 시작한다.
            fs::remove_dir_all(root.join(".hook-state")).ok();
            fs::write(path, bytes).unwrap();
            let stem = path.file_name().unwrap().to_string_lossy().into_owned();
            for stale in fs::read_dir(&root)
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.path()))
            {
                if stale
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(stem.as_str()))
                    && stale != *path
                {
                    fs::remove_file(stale).ok();
                }
            }
            if fails {
                let before = fs::read(path).unwrap();
                let err = install(path).unwrap_err();
                assert!(!err.to_string().is_empty(), "{name}");
                assert_eq!(fs::read(path).unwrap(), before, "{name} bytes changed");
                let leftovers: Vec<_> = fs::read_dir(&root)
                    .unwrap()
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with(stem.as_str()))
                            && *p != *path
                    })
                    .collect();
                assert!(leftovers.is_empty(), "{name}: {leftovers:?}");
            } else {
                install(path).unwrap();
            }
        }
    }
    fs::remove_dir_all(&root).ok();
}

/// GUI 훅 설치가 CLI(`krill hook install`)와 같은 모양인지: 사용자 항목 보존, 항목 구조·
/// 명령 문자열·멱등성·백업을 순서无关 값 비교로 고정한다.
#[test]
fn gui_hook_install_matches_cli_shapes_and_preserves_user_hooks() {
    use serde_json::json;
    let root = std::env::temp_dir().join(format!("mk-hook-shape-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();

    // Claude: 사용자 테마·기존 훅을 살리고 4개 이벤트에 matcher 없는 항목을 추가한다.
    let claude_path = root.join("settings.json");
    let claude_before = r#"{"theme": "dark", "hooks": {"PreToolUse": [{"command": "user-hook"}]}}"#;
    fs::write(&claude_path, claude_before).unwrap();
    install_claude_hook(&claude_path).unwrap();
    let claude_out = fs::read_to_string(&claude_path).unwrap();
    let ours = json!({
        "hooks": [{"type": "command", "name": "minikrill", "command": CLAUDE_HOOK_COMMAND}]
    });
    let expected = json!({
        "theme": "dark",
        "hooks": {
            "PreToolUse": [{"command": "user-hook"}, ours],
            "PostToolUse": [ours],
            "Stop": [ours],
            "SessionStart": [ours],
        },
    });
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&claude_out).unwrap(),
        expected
    );
    // 같은 입력으로 공용 머지를 직접 부르면 바이트 단위로 같다.
    assert_eq!(
        claude_out,
        krill_proto::agent::hooks::install_claude_json(Some(claude_before), CLAUDE_HOOK_COMMAND)
            .unwrap()
    );
    // 백업에는 설치 전 원본이 그대로 있다.
    let backup = fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.to_string_lossy().contains(".minikrill-bak-"))
        .expect("missing hook backup");
    assert_eq!(fs::read_to_string(backup).unwrap(), claude_before);
    // 멱등: 다시 설치해도 바이트가 같고 새 백업이 생기지 않는다.
    let backups_before = fs::read_dir(&root).unwrap().count();
    install_claude_hook(&claude_path).unwrap();
    assert_eq!(fs::read_to_string(&claude_path).unwrap(), claude_out);
    assert_eq!(fs::read_dir(&root).unwrap().count(), backups_before);

    // Antigravity: `minikrill` 묶음 키에 `hook agy <event>` 완성 명령 + timeout 10.
    let agy_path = root.join("ahy-hooks.json");
    let agy_before = r#"{"user": 1}"#;
    fs::write(&agy_path, agy_before).unwrap();
    install_agy_hook(&agy_path).unwrap();
    let agy_out = fs::read_to_string(&agy_path).unwrap();
    let mut bundle = serde_json::Map::new();
    for event in [
        "PreInvocation",
        "PostInvocation",
        "Stop",
        "PreToolUse",
        "PostToolUse",
    ] {
        let ev_cmd = krill_proto::agent::hooks::agy_event_command(AGY_HOOK_BASE_COMMAND, event);
        assert!(
            ev_cmd.contains("hook agy"),
            "agy base command lost for {event}"
        );
        let entry = if matches!(event, "PreToolUse" | "PostToolUse") {
            json!({
                "matcher": "*",
                "hooks": [{"type": "command", "command": ev_cmd, "timeout": 10}]
            })
        } else {
            json!({"type": "command", "command": ev_cmd, "timeout": 10})
        };
        bundle.insert(event.to_owned(), serde_json::Value::Array(vec![entry]));
    }
    let expected_agy = json!({"user": 1, "minikrill": bundle});
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&agy_out).unwrap(),
        expected_agy
    );
    assert_eq!(
        agy_out,
        krill_proto::agent::hooks::install_agent_json(
            Some(agy_before),
            krill_proto::agent::hooks::Agent::Agy,
            AGY_HOOK_BASE_COMMAND,
        )
        .unwrap()
    );
    fs::remove_dir_all(&root).ok();
}

/// 훅 설치가 심볼릭을 따라가지 않는다: 링크 경로로는 오류, 원본은 그대로 둔다.
#[test]
fn hook_install_refuses_symlinked_config() {
    let root = std::env::temp_dir().join(format!("mk-hook-link-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    let target = root.join("real.json");
    let link = root.join("settings.json");
    fs::write(&target, r#"{"user": true}"#).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let err = install_claude_hook(&link).unwrap_err();
    assert!(err.to_string().contains("symlink"), "{err}");
    assert_eq!(fs::read_to_string(&target).unwrap(), r#"{"user": true}"#);
    fs::remove_dir_all(&root).ok();
}

/// OWE-651 1번: 행 안의 줄바꿈 가능한 텍스트 열이 폭 0으로 무너져 한 글자씩 세로로 쌓이면 안 된다
/// (실제 설정 창 크기 1100×740, 계정이 없는 빈 상태).
#[gpui::test]
fn accounts_empty_state_subheader_keeps_its_width(cx: &mut TestAppContext) {
    let d = dir("accounts-subheader-width");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1100.), px(740.)));
    vcx.update(|_, cx| view.update(cx, |v, cx| v.set_tab(AdminTab::Accounts, cx)));
    vcx.run_until_parked();
    let sub = vcx
        .debug_bounds("admin-accounts-subheader-claude")
        .expect("missing accounts subheader");
    assert!(sub.size.width > px(300.), "{sub:?}");
    assert!(sub.size.height < px(60.), "{sub:?}");
    fs::remove_dir_all(d).ok();
}

/// OWE-651 3번: 줌 "기본값" 버튼 이름이 현재 값과 같은 `100%`로 보여 중복 표시되지 않는다.
#[test]
fn zoom_reset_label_is_not_a_percentage() {
    for language in [Language::Ko, Language::En] {
        let label = text::RESET.in_language(language);
        assert!(!label.contains('%'), "{label}");
    }
}

/// OWE-651 6번: 개발용 샘플 버튼은 제품 화면에 없다. 실제 동작 버튼은 남는다.
#[gpui::test]
fn admin_tabs_hide_developer_sample_buttons(cx: &mut TestAppContext) {
    let d = dir("no-sample-buttons");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    for (tab, kept) in [
        (AdminTab::Ssh, "admin-ssh-import"),
        (AdminTab::Workflows, "admin-orch-reset"),
    ] {
        vcx.update(|_, cx| view.update(cx, |v, cx| v.set_tab(tab, cx)));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds(kept).is_some(), "missing {kept}");
        for gone in [
            "admin-ssh-add-local",
            "admin-ssh-add-dev",
            "admin-orch-create-run",
            "admin-qc-add-sample",
        ] {
            assert!(vcx.debug_bounds(gone).is_none(), "{gone} still rendered");
        }
    }
    fs::remove_dir_all(d).ok();
}

/// OWE-651 2번: 웹훅 비우기는 위험 버튼이며 두 번째 클릭에만 실행된다. 탭을 바꾸면 확인 상태가 풀린다.
#[gpui::test]
fn webhook_clear_requires_confirmation(cx: &mut TestAppContext) {
    let d = dir("webhook-clear-confirm");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(3000.)));
    vcx.update(|_, cx| {
        edit(cx, |s| {
            s.webhooks.push(crate::webhook::Endpoint::Ntfy {
                url: "http://127.0.0.1:9/hook".into(),
            });
        });
        view.update(cx, |v, cx| v.set_tab(AdminTab::Workflows, cx));
    });
    vcx.run_until_parked();
    let click = |vcx: &mut gpui::VisualTestContext| {
        let b = vcx
            .debug_bounds("admin-webhook-clear")
            .expect("missing admin-webhook-clear");
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };
    click(vcx);
    vcx.update(|_, cx| {
        assert_eq!(current(cx).webhooks.len(), 1);
        assert_eq!(view.read(cx).armed.as_deref(), Some("admin-webhook-clear"));
        view.update(cx, |v, cx| v.set_tab(AdminTab::Workflows, cx));
        assert_eq!(view.read(cx).armed, None);
    });
    vcx.run_until_parked();
    click(vcx);
    click(vcx);
    vcx.update(|_, cx| assert!(current(cx).webhooks.is_empty()));
    fs::remove_dir_all(d).ok();
}

/// OWE-651 5·7번: 단축키는 내부 액션 ID 대신 설명을 보이고, 탭 아이콘·배지·그룹명이 의미에 맞다.
#[test]
fn shortcuts_and_sidebar_use_readable_labels() {
    use gpui_kit_assets::IconName;
    for &(action, desc) in shortcuts::ACTIONS {
        assert!(action.contains("::"));
        for language in [Language::Ko, Language::En] {
            let label = desc.in_language(language);
            assert!(!label.is_empty() && !label.contains("::"), "{label}");
        }
    }
    assert!(matches!(AdminTab::Shortcuts.icon(), IconName::Keyboard));
    assert!(matches!(AdminTab::Ssh.icon(), IconName::Server));
    assert_eq!(AdminTab::Accounts.badge(), None);
    assert_eq!(AdminTab::Ssh.badge(), None);
    assert_ne!(text::GROUP_SETUP.in_language(Language::Ko), "설정 시작");
}

/// OWE-651 7번: 섹션 제목은 카드 행이 아니라 별도 머리글로 그린다.
#[gpui::test]
fn section_titles_render_as_headers(cx: &mut TestAppContext) {
    let d = dir("section-titles");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(3000.)));
    for (tab, id) in [
        (AdminTab::Resources, "admin-section-resources"),
        (AdminTab::Workflows, "admin-section-workflows"),
    ] {
        vcx.update(|_, cx| view.update(cx, |v, cx| v.set_tab(tab, cx)));
        vcx.run_until_parked();
        assert!(vcx.debug_bounds(id).is_some(), "missing {id}");
    }
    fs::remove_dir_all(d).ok();
}

/// OWE-651 8번: 시스템 제목 표시줄은 투명하게 두고 테마 색 제목 띠를 창 맨 위에 그린다.
#[gpui::test]
fn settings_window_draws_themed_titlebar(cx: &mut TestAppContext) {
    let d = dir("themed-titlebar");
    init(cx, &d);
    cx.update(|cx| {
        let titlebar = window_options(cx).titlebar.expect("titlebar options");
        assert!(titlebar.appears_transparent);
        assert!(titlebar.traffic_light_position.is_some());
    });
    let (_view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.run_until_parked();
    let bar = vcx
        .debug_bounds("admin-titlebar")
        .expect("missing admin-titlebar");
    assert_eq!(bar.origin.y, px(0.));
    assert_eq!(bar.size.height, crate::ui::TITLEBAR_HEIGHT);
    fs::remove_dir_all(d).ok();
}

#[test]
fn agent_card_save_writes_args_env_and_rejects_bad_lines() {
    let d = dir("owe657");
    let toml_path = d.join(krill_proto::agent::FILE_NAME);
    fs::write(&toml_path, "permission = \"yolo\"\n").unwrap();
    save_agent_profile(&d, "codex", "--model  o3 ", "A=1\n\nB = x=y\n").unwrap();
    let a =
        krill_proto::agent::Agents::load(Some(&fs::read_to_string(&toml_path).unwrap())).unwrap();
    let codex = &a.profiles["codex"];
    assert_eq!(a.permission, krill_proto::agent::Permission::Yolo);
    assert_eq!(codex.launch, ["codex", "--model", "o3"]);
    assert_eq!(
        codex.yolo_args,
        ["--dangerously-bypass-approvals-and-sandbox"]
    );
    assert_eq!(codex.env["B"], "x=y");
    let before = fs::read_to_string(&toml_path).unwrap();
    for (bad, line) in [("A=1\nNOEQ", '2'), ("=v", '1')] {
        let err = save_agent_profile(&d, "codex", "", bad).unwrap_err();
        assert!(err.contains(line), "{err}");
    }
    assert_eq!(
        fs::read_to_string(&toml_path).unwrap(),
        before,
        "잘못된 줄은 쓰지 않는다"
    );
    fs::remove_dir_all(&d).ok();
}

#[gpui::test]
fn agent_card_shows_row_error_and_saves_valid_env(cx: &mut TestAppContext) {
    let d = dir("owe657-ui");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Agents, cx));
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();
    let set_env = |vcx: &mut gpui::VisualTestContext, value: &'static str| {
        vcx.update(|window, cx| {
            let env = view.read(cx).agent_edits["codex"].env.clone();
            env.update(cx, |s, cx| s.set_value(value, window, cx));
        });
    };
    let click_save = |vcx: &mut gpui::VisualTestContext| {
        vcx.run_until_parked();
        let save = vcx.debug_bounds("admin-agent-save-codex").unwrap();
        vcx.simulate_click(save.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };
    set_env(vcx, "GOOD=1\nNOEQ");
    click_save(vcx);
    vcx.update(|_, cx| {
        let (msg, is_err) = view.read(cx).agent_edits["codex"].status.clone().unwrap();
        assert!(is_err && msg.contains('2'), "{msg}");
    });
    assert!(vcx.debug_bounds("admin-agent-status-codex").is_some());
    assert!(!d.join(krill_proto::agent::FILE_NAME).exists());

    set_env(vcx, "GOOD=1");
    click_save(vcx);
    let text = fs::read_to_string(d.join(krill_proto::agent::FILE_NAME)).unwrap();
    let a = krill_proto::agent::Agents::load(Some(&text)).unwrap();
    assert_eq!(a.profiles["codex"].env["GOOD"], "1");
    assert!(vcx.debug_bounds("admin-agent-open-toml-codex").is_some());
    fs::remove_dir_all(&d).ok();
}

/// GUI도 CLI와 같은 hooks/install.lock과 영수증을 사용해야 한다.
#[test]
fn gui_hook_install_shares_cli_install_lock_and_receipt() {
    use krill_proto::hook_install::{Action, Plan, Receipt, TargetAgent};
    let root = dir("shared-hook-lock");
    let state = root.join("state");
    let hooks = state.join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(hooks.join("install.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let path = root.join("settings.json");
    let before = r#"{"theme":"dark"}"#;
    fs::write(&path, before).unwrap();
    let install = || install_hook_file_at(&path, TargetAgent::Claude, &state);
    let err = install().unwrap_err();
    assert!(err.to_string().contains("another hook operation"), "{err}");
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    assert!(!hooks.join("claude.json").exists());
    drop(lock);
    install().unwrap();
    let receipt: Receipt =
        serde_json::from_slice(&fs::read(hooks.join("claude.json")).unwrap()).unwrap();
    assert!(receipt.complete);
    assert_eq!(receipt.agent, "claude");
    // CLI가 사용하는 Plan으로 GUI 영수증을 읽어 제거할 수 있다.
    Plan::prepare(
        Action::Uninstall,
        TargetAgent::Claude,
        &path,
        &state.canonicalize().unwrap(),
        Path::new("krill"),
    )
    .unwrap()
    .unwrap()
    .apply()
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    fs::remove_dir_all(root).unwrap();
}

#[derive(Default)]
struct RuleTransportReply {
    result: Option<Result<krill_proto::usage::AccountsSnapshot, String>>,
    waker: Option<std::task::Waker>,
}

/// 소켓·실시간 대기 없이 요청 완료 시점과 타임아웃 오류를 주입한다.
fn fake_rule_transport(
    reply: std::sync::Arc<std::sync::Mutex<RuleTransportReply>>,
) -> impl std::future::Future<Output = Result<krill_proto::usage::AccountsSnapshot, String>> + Send
{
    std::future::poll_fn(move |cx| {
        let mut reply = reply.lock().unwrap();
        if let Some(result) = reply.result.take() {
            std::task::Poll::Ready(result)
        } else {
            reply.waker = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    })
}

/// 반환된 스냅샷을 실제로 반영하는지 파일 폴링 없이 검증한다.
#[gpui::test]
fn account_rule_transport_applies_in_memory_success_snapshot(cx: &mut TestAppContext) {
    use krill_proto::usage::{AccountsSnapshot, AutoSwitchMetric, AutoSwitchRule, ProviderKind};
    let view = cx.new(SettingsView::new);
    let mut snapshot = AccountsSnapshot::default();
    snapshot.auto_switch.rules.push(AutoSwitchRule {
        provider: ProviderKind::Claude,
        from_account_id: "in-memory".into(),
        enabled: true,
        threshold_percent: 85,
        metric: AutoSwitchMetric::SevenDay,
        target_account_id: None,
    });
    let expected = snapshot.auto_switch.rules.clone();
    view.update(cx, |v, cx| {
        v.spawn_account_rule_request(async move { Ok(snapshot) }, cx);
        assert!(v.account_rule_pending);
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let v = view.read(cx);
        assert!(!v.account_rule_pending);
        assert!(v.account_error.is_none());
        assert_eq!(v.accounts().auto_switch.rules, expected);
    });
}

#[gpui::test]
fn account_rule_transport_success_releases_pending_and_ignores_duplicates(cx: &mut TestAppContext) {
    exercise_account_rule_transport(cx, false);
}

#[gpui::test]
fn account_rule_transport_timeout_preserves_snapshot_and_allows_retry(cx: &mut TestAppContext) {
    exercise_account_rule_transport(cx, true);
}

fn exercise_account_rule_transport(cx: &mut TestAppContext, timeout: bool) {
    use krill_proto::usage::{AutoSwitchMetric, AutoSwitchRule, ProviderKind, UsageRequest};
    let d = dir(if timeout {
        "rule-transport-timeout"
    } else {
        "rule-transport-success"
    });
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    let rule = AutoSwitchRule {
        provider: ProviderKind::Claude,
        from_account_id: "claude-work".into(),
        enabled: true,
        threshold_percent: 70,
        metric: AutoSwitchMetric::Max,
        target_account_id: None,
    };
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Accounts, cx);
            v.apply_account_request(
                UsageRequest::AddAccount {
                    provider: ProviderKind::Claude,
                    label: "Work".into(),
                    email: None,
                    profile_dir: d.join("profile").display().to_string(),
                },
                cx,
            );
            v.set_account_rule(rule.clone(), cx);
            v.advanced_open.insert(ProviderKind::Claude);
        });
    });
    vcx.run_until_parked();
    let reply = std::sync::Arc::new(std::sync::Mutex::new(RuleTransportReply::default()));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.account_error = Some("old error".into());
            v.spawn_account_rule_request(fake_rule_transport(reply.clone()), cx);
            assert!(v.account_rule_pending);
            assert!(v.account_error.is_none());
            v.spawn_account_rule_request(async { panic!("duplicate transport executed") }, cx);
            let mut changed = rule.clone();
            changed.threshold_percent = 95;
            v.set_account_rule(changed, cx);
            v.clear_account_rule(rule.provider, &rule.from_account_id, cx);
        });
    });
    vcx.run_until_parked();
    for selector in [
        "admin-rule-toggle-claude-work",
        "admin-rule-threshold-claude-work-95",
        "admin-rule-metric-claude-work-5h",
        "admin-rule-target-auto-claude-work",
        "admin-rule-clear-claude-work",
    ] {
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let v = view.read(cx);
            assert!(v.account_rule_pending);
            assert_eq!(v.accounts().auto_switch.rules, vec![rule.clone()]);
        });
    }
    let snapshot = vcx.update(|_, cx| view.read(cx).accounts_snapshot.clone());
    let waker = {
        let mut reply = reply.lock().unwrap();
        reply.result = Some(if timeout {
            Err(std::io::Error::from(std::io::ErrorKind::TimedOut).to_string())
        } else {
            Ok(snapshot)
        });
        reply.waker.take().unwrap()
    };
    waker.wake();
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            assert!(!v.account_rule_pending);
            assert_eq!(v.accounts().auto_switch.rules, vec![rule.clone()]);
            assert_eq!(v.account_error.is_some(), timeout);
            if timeout {
                assert!(v.account_error.as_ref().unwrap().contains("timed out"));
            }
            // 오류 뒤에도 다음 요청을 보낼 수 있고 성공하면 이전 오류를 지운다.
            let snapshot = v.accounts_snapshot.clone();
            v.spawn_account_rule_request(async move { Ok(snapshot) }, cx);
            assert!(v.account_rule_pending);
            assert!(v.account_error.is_none());
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert!(!view.read(cx).account_rule_pending);
        assert!(view.read(cx).account_error.is_none());
    });
    // 완료 후 버튼이 다시 동작한다.
    let bounds = vcx.debug_bounds("admin-rule-clear-claude-work").unwrap();
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|_, cx| assert!(view.read(cx).accounts().auto_switch.rules.is_empty()));
    fs::remove_dir_all(d).unwrap();
}

/// OWE-655: 네 옵션의 자름·기본값·저장 왕복과 동면 표식 파일 동기화.
#[gpui::test]
fn owe655_terminal_options_and_hibernation_flag_sync(cx: &mut TestAppContext) {
    let d = dir("owe655-options");
    init(cx, &d);
    cx.update(|cx| {
        assert_eq!(terminal_font_pt(cx), 13);
        assert_eq!(terminal_opacity_pct(cx), 100);
        assert!((terminal_opacity_alpha(cx) - 1.).abs() < f32::EPSILON);
        assert!(notification_sound(cx));
        assert!(!current(cx).hibernation_enabled);
    });
    // 기본값은 꺼짐(opt-in)이라 초기화가 표식을 만들지 않는다.
    assert!(!d.join(HIBERNATION_FLAG).exists());
    cx.update(|cx| edit(cx, |s| s.hibernation_enabled = true));
    assert!(d.join(HIBERNATION_FLAG).is_file());
    // 파싱 자름: 범위를 벗어나면 경계로 붙는다.
    assert_eq!(
        Settings::parse("terminal_font_size = 5\n")
            .unwrap()
            .terminal_font_size(),
        10
    );
    assert_eq!(
        Settings::parse("terminal_font_size = 99\n")
            .unwrap()
            .terminal_font_size(),
        24
    );
    assert_eq!(
        Settings::parse("terminal_opacity_pct = 10\n")
            .unwrap()
            .terminal_opacity_pct(),
        50
    );
    assert_eq!(
        Settings::parse("terminal_opacity_pct = 200\n")
            .unwrap()
            .terminal_opacity_pct(),
        100
    );
    // UI 편집 경로(`edit`)로 끄면 표식이 지워지고 파일에 남는다.
    cx.update(|cx| {
        edit(cx, |s| {
            s.hibernation_enabled = false;
            s.terminal_font_size = 20;
            s.terminal_opacity_pct = 75;
            s.notification_sound = false;
        });
        assert_eq!(terminal_font_pt(cx), 20);
        assert_eq!(terminal_opacity_pct(cx), 75);
        assert!((terminal_opacity_alpha(cx) - 0.75).abs() < 1e-6);
        assert!(!notification_sound(cx));
    });
    assert!(!d.join(HIBERNATION_FLAG).exists());
    let saved = Settings::parse(&fs::read_to_string(d.join(SETTINGS_FILE)).unwrap()).unwrap();
    assert!(!saved.hibernation_enabled);
    assert_eq!(saved.terminal_font_size(), 20);
    assert_eq!(saved.terminal_opacity_pct(), 75);
    assert!(!saved.notification_sound);
    // 외부 편집으로 다시 켜면 폴링 리로드가 표식을 되살린다.
    fs::write(d.join(SETTINGS_FILE), "hibernation_enabled = true\n").unwrap();
    cx.update(reload);
    cx.update(|cx| assert!(current(cx).hibernation_enabled));
    assert!(d.join(HIBERNATION_FLAG).is_file());
    // 사람이 직접 만든 표식은 설정 파일이 끔이어도 리로드가 지우지 않고 켜짐으로 읽는다.
    fs::write(d.join(SETTINGS_FILE), "hibernation_enabled = false\n").unwrap();
    cx.update(reload);
    cx.update(|cx| assert!(current(cx).hibernation_enabled));
    assert!(d.join(HIBERNATION_FLAG).is_file());
    fs::remove_dir_all(d).ok();
}

/// OWE-655: 일반 탭의 네 컨트롤과 에이전트 탭 동면 토글이 그려지고 클릭에 반응한다.
#[gpui::test]
fn owe655_settings_controls_toggle_and_persist(cx: &mut TestAppContext) {
    let d = dir("owe655-controls");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1100.), px(2400.)));
    let click = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };
    // 글꼴 크기·투명도는 터미널 탭(OWE-658), 사운드는 일반 탭.
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Terminal, cx));
        window.draw(cx).clear(cx);
    });
    for selector in [
        "font-dec",
        "font-inc",
        "font-reset",
        "editor-font-dec",
        "editor-font-inc",
        "editor-font-reset",
        "opacity-dec",
        "opacity-inc",
        "opacity-reset",
    ] {
        assert!(vcx.debug_bounds(selector).is_some(), "missing {selector}");
    }
    assert_eq!(text::RESET.in_language(Language::Ko), "기본값");
    assert_eq!(
        vcx.debug_bounds("font-reset").unwrap().size,
        vcx.debug_bounds("opacity-reset").unwrap().size,
        "font-reset must render the same '기본값' label as opacity-reset"
    );
    click(vcx, "font-inc");
    click(vcx, "opacity-dec");
    vcx.update(|_, cx| assert_eq!(current(cx).terminal_font_size(), 14));
    click(vcx, "font-reset");
    vcx.update(|_, cx| assert_eq!(current(cx).terminal_font_size(), 13));
    // OWE-843: 에디터 글꼴 행도 같은 탭에서 조절·영속된다.
    click(vcx, "editor-font-inc");
    vcx.update(|_, cx| assert_eq!(current(cx).editor_font_size(), 13));
    click(vcx, "editor-font-reset");
    vcx.update(|_, cx| assert_eq!(current(cx).editor_font_size(), 12));
    assert_eq!(
        Settings::parse(&fs::read_to_string(d.join(SETTINGS_FILE)).unwrap())
            .unwrap()
            .terminal_font_size(),
        13
    );
    click(vcx, "font-inc");
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::General, cx));
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("sound-on").is_some());
    click(vcx, "sound-off");
    vcx.update(|_, cx| {
        assert_eq!(current(cx).terminal_font_size(), 14);
        assert_eq!(current(cx).terminal_opacity_pct(), 95);
        assert!(!current(cx).notification_sound);
    });
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Agents, cx));
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("hib-on").is_some());
    assert!(vcx.debug_bounds("hib-off").is_some());
    click(vcx, "hib-off");
    vcx.update(|_, cx| assert!(!current(cx).hibernation_enabled));
    assert!(!d.join(HIBERNATION_FLAG).exists());
    click(vcx, "hib-on");
    vcx.update(|_, cx| assert!(current(cx).hibernation_enabled));
    assert!(d.join(HIBERNATION_FLAG).is_file());
    fs::remove_dir_all(d).ok();
}

/// OWE-660: 기록은 기존 키를 끄고 새 키를 쓰며, 초기화는 그 액션 항목만 지운다. 주석·다른 항목은 남는다.
#[test]
fn owe660_keys_toml_record_and_reset_round_trip() {
    let original = "# 내 키맵\n[bindings]\n# 메모\n\"cmd-k\" = \"shell::Jump\" # 유지\n\n";
    let defaults = vec![
        ("settings::ZoomIn", "cmd-=".to_owned()),
        ("settings::ZoomOut", "cmd--".to_owned()),
    ];
    let mut t = Keys::parse(original).unwrap().bindings;
    shortcuts::set_key(
        &mut t,
        &defaults,
        "settings::ZoomIn",
        Some("cmd-="),
        "cmd-y",
    );
    let body = shortcuts::rewrite(original, &t).unwrap();
    assert!(
        body.starts_with("# 내 키맵\n[bindings]\n# 메모\n\"cmd-k\" = \"shell::Jump\" # 유지\n")
    );
    let parsed = Keys::parse(&body).unwrap().bindings;
    assert_eq!(parsed["cmd-y"].as_str(), Some("settings::ZoomIn"));
    assert_eq!(parsed["cmd-="].as_str(), Some(""));
    assert_eq!(parsed.len(), 3);

    // 사용자 키를 다시 바꾸면 그 항목만 바뀐다(기본 키 끄기는 유지).
    let mut t = parsed;
    shortcuts::set_key(
        &mut t,
        &defaults,
        "settings::ZoomIn",
        Some("cmd-y"),
        "alt-cmd-y",
    );
    assert!(!t.contains_key("cmd-y"));
    assert_eq!(t["alt-cmd-y"].as_str(), Some("settings::ZoomIn"));

    shortcuts::reset_action(&mut t, &defaults, "settings::ZoomIn");
    let body = shortcuts::rewrite(&body, &t).unwrap();
    assert_eq!(
        body,
        "# 내 키맵\n[bindings]\n# 메모\n\"cmd-k\" = \"shell::Jump\" # 유지\n\n"
    );

    // 줄 단위로 못 고치는 형식(인라인 표)은 표 전체를 다시 쓴다.
    let inline = "bindings = { \"cmd-k\" = \"shell::Jump\" }\n";
    let mut t = Keys::parse(inline).unwrap().bindings;
    shortcuts::set_key(&mut t, &defaults, "settings::ZoomOut", None, "cmd-u");
    let parsed = Keys::parse(&shortcuts::rewrite(inline, &t).unwrap()).unwrap();
    assert_eq!(parsed.bindings, t);

    assert_eq!(shortcuts::keycap_text("cmd-shift-w"), "⇧⌘W");
    assert_eq!(shortcuts::keycap_text("alt-cmd-left"), "⌥⌘←");
    assert_eq!(shortcuts::keycap_text("cmd-shift-enter"), "⇧⌘Enter");
    assert_eq!(shortcuts::keycap_text("ctrl-1"), "⌃1");
}

/// OWE-660: 같은 키를 쓰는 다른 액션을 찾는다. 키 표기 순서는 따지지 않고 같은 액션은 충돌이 아니다.
#[test]
fn owe660_conflict_detection() {
    let live = vec![
        ("settings::ZoomIn", "cmd-=".to_owned()),
        ("settings::ZoomOut", "cmd--".to_owned()),
        ("shell::NewAgent1", shortcuts::norm("alt-cmd-1").unwrap()),
        ("sidebar::NewAgent1", shortcuts::norm("cmd-alt-1").unwrap()),
    ];
    assert_eq!(
        shortcuts::conflicts(&live, "cmd--", "settings::ZoomIn"),
        ["settings::ZoomOut"]
    );
    assert!(shortcuts::conflicts(&live, "cmd-=", "settings::ZoomIn").is_empty());
    assert!(shortcuts::conflicts(&live, "cmd-y", "settings::ZoomIn").is_empty());
    assert_eq!(
        shortcuts::conflicts(&live, "cmd-alt-1", "shell::Jump"),
        ["shell::NewAgent1", "sidebar::NewAgent1"]
    );
}

/// OWE-660 실화면 흐름: 키캡 클릭 → 키 입력 기록 → 즉시 적용, 충돌은 확인 후 저장, Esc 취소, 행 초기화.
#[gpui::test]
fn owe660_recorder_records_conflicts_and_resets(cx: &mut TestAppContext) {
    let d = dir("owe660-recorder");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(3000.)));
    vcx.update(|_, cx| view.update(cx, |v, cx| v.set_tab(AdminTab::Shortcuts, cx)));
    vcx.run_until_parked();
    let click = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
        let b = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };
    let keys = || fs::read_to_string(d.join(KEYS_FILE)).unwrap_or_default();

    // 기록: ⌘= → ⌘Y. ⌘Y는 설정 동작이 아니라 기록으로 먹힌다.
    assert!(vcx.debug_bounds("sc-reset-settings::ZoomIn").is_none());
    click(vcx, "sc-settings::ZoomIn-cmd-=");
    vcx.simulate_keystrokes("cmd-y");
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert_eq!(bound(cx, "cmd-y"), Some(ZoomIn.name()));
        assert_eq!(bound(cx, "cmd-="), None);
    });
    let saved = Keys::parse(&keys()).unwrap().bindings;
    assert_eq!(saved["cmd-y"].as_str(), Some("settings::ZoomIn"));
    assert_eq!(saved["cmd-="].as_str(), Some(""));
    assert!(keys().contains("# "), "템플릿 주석 유지");

    // Esc는 취소: 파일이 그대로다.
    let before = keys();
    click(vcx, "sc-settings::ZoomIn-cmd-y");
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert_eq!(keys(), before);
    vcx.update(|_, cx| assert!(view.read(cx).shortcuts.target.is_none()));

    // 충돌: ⌘0(기본 크기)을 고르면 경고만 띄우고 저장하지 않는다. 확인하면 저장한다.
    click(vcx, "sc-settings::ZoomIn-cmd-y");
    vcx.simulate_keystrokes("cmd-0");
    vcx.run_until_parked();
    assert_eq!(keys(), before);
    assert!(vcx.debug_bounds("sc-conflict").is_some());
    click(vcx, "sc-conflict-save");
    vcx.update(|_, cx| {
        // `all_bindings_for_input`은 우선순위 높은 것부터: 나중에 얹힌 재정의가 기본 크기를 이긴다.
        let typed = [Keystroke::parse("cmd-0").unwrap()];
        let winner = cx.all_bindings_for_input(&typed);
        assert_eq!(
            winner.first().map(|b| b.action().name()),
            Some(ZoomIn.name())
        );
    });
    assert!(vcx.debug_bounds("sc-conflict").is_none());

    // 행 초기화: ZoomIn 항목만 지워 기본 키 ⌘=로 돌아간다.
    click(vcx, "sc-reset-settings::ZoomIn");
    vcx.update(|_, cx| {
        assert_eq!(bound(cx, "cmd-="), Some(ZoomIn.name()));
        assert_eq!(bound(cx, "cmd-0"), Some(ZoomReset.name()));
        assert_eq!(bound(cx, "cmd-y"), None);
    });
    assert!(Keys::parse(&keys()).unwrap().bindings.is_empty());
    assert!(vcx.debug_bounds("sc-reset-settings::ZoomIn").is_none());
    fs::remove_dir_all(d).ok();
}

/// OWE-658: 터미널 탭 설정의 기본값·직렬화 왕복과 옛 settings.toml 호환.
#[gpui::test]
fn owe658_terminal_settings_defaults_and_serialization(cx: &mut TestAppContext) {
    let s = Settings::default();
    assert_eq!(s.terminal_font_family, "Menlo");
    assert_eq!(s.terminal_cursor_shape, TerminalCursorShape::Block);
    assert!(!s.terminal_cursor_blink && !s.option_as_meta && !s.copy_on_select);
    // 새 키가 없는 기존 파일도 그대로 읽힌다(빠진 키는 기본값).
    assert_eq!(Settings::parse("theme = \"dark\"\n").unwrap(), s);
    let parsed = Settings::parse(
        "terminal_font_family = \"Monaco\"\nterminal_cursor_shape = \"bar\"\n\
         terminal_cursor_blink = true\noption_as_meta = true\ncopy_on_select = true\n",
    )
    .unwrap();
    assert_eq!(parsed.terminal_font_family, "Monaco");
    assert_eq!(parsed.terminal_cursor_shape, TerminalCursorShape::Bar);
    assert!(parsed.terminal_cursor_blink && parsed.option_as_meta && parsed.copy_on_select);
    assert_eq!(Settings::parse(&parsed.to_toml()).unwrap(), parsed);
    assert!(
        Settings {
            terminal_cursor_shape: TerminalCursorShape::Underline,
            ..Settings::default()
        }
        .to_toml()
        .contains("terminal_cursor_shape = \"underline\"")
    );
    assert!(Settings::parse("terminal_cursor_shape = \"beam\"\n").is_err());

    let d = dir("owe658-defaults");
    init(cx, &d);
    cx.update(|cx| {
        assert_eq!(terminal_font_family(cx), "Menlo");
        assert_eq!(
            terminal_cursor(cx),
            krill_proto::screen::CursorStyle::default()
        );
        assert!(!option_as_meta(cx) && !copy_on_select(cx));
        edit(cx, |s| {
            s.terminal_font_family = "  ".to_owned();
            s.terminal_cursor_shape = TerminalCursorShape::Underline;
            s.terminal_cursor_blink = true;
            s.option_as_meta = true;
            s.copy_on_select = true;
        });
        // 빈 글꼴 이름은 기본 Menlo로.
        assert_eq!(terminal_font_family(cx), "Menlo");
        assert_eq!(
            terminal_cursor(cx),
            krill_proto::screen::CursorStyle {
                shape: krill_proto::screen::CursorShape::Underline,
                blink: true,
            }
        );
        assert!(option_as_meta(cx) && copy_on_select(cx));
    });
    let saved = Settings::parse(&fs::read_to_string(d.join(SETTINGS_FILE)).unwrap()).unwrap();
    assert!(saved.option_as_meta && saved.copy_on_select && saved.terminal_cursor_blink);
    fs::remove_dir_all(d).ok();
}

/// OWE-658: 터미널 탭 컨트롤이 그려지고 클릭이 설정에 저장된다. 일반 탭에는 글꼴 행이 없다.
#[gpui::test]
fn owe658_terminal_tab_controls_toggle_and_persist(cx: &mut TestAppContext) {
    let d = dir("owe658-controls");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1100.), px(2400.)));
    let click = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::General, cx));
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("font-inc").is_none());
    assert!(vcx.debug_bounds("admin-tab-terminal").is_some());
    click(vcx, "admin-tab-terminal");
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    for selector in [
        "terminal-font-family",
        "cursor-block",
        "cursor-bar",
        "cursor-underline",
        "cursor-blink-on",
        "cursor-blink-off",
        "option-meta-on",
        "option-meta-off",
        "copy-select-on",
        "copy-select-off",
    ] {
        assert!(vcx.debug_bounds(selector).is_some(), "missing {selector}");
    }
    click(vcx, "cursor-bar");
    click(vcx, "cursor-blink-on");
    click(vcx, "option-meta-on");
    click(vcx, "copy-select-on");
    vcx.update(|_, cx| {
        let s = current(cx);
        assert_eq!(s.terminal_cursor_shape, TerminalCursorShape::Bar);
        assert!(s.terminal_cursor_blink && s.option_as_meta && s.copy_on_select);
    });
    click(vcx, "option-meta-off");
    vcx.update(|_, cx| assert!(!current(cx).option_as_meta));
    fs::remove_dir_all(d).ok();
}

/// OWE-668: SSH 탭 입력 폼 검증·추가, Tailscale 발견 기기 원클릭 추가, 첫 연결 도우미(인증 실패 키 생성·ssh-copy-id 및 원격 로그인 안내).
#[gpui::test]
fn owe668_ssh_tab_form_tailscale_and_onboarding_flow(cx: &mut TestAppContext) {
    let d = dir("owe668-ssh-ux");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1200.), px(2800.)));
    let click = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };

    vcx.update(|window, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Ssh, cx));
        window.draw(cx).clear(cx);
    });

    // Tailscale 피어가 없으면 발견된 기기 카드가 조용히 숨겨진다.
    assert!(vcx.debug_bounds("admin-ssh-discovered").is_none());
    assert!(vcx.debug_bounds("admin-ssh-form").is_some());
    assert!(vcx.debug_bounds("admin-ssh-input-port").is_none());

    // 빈 주소로 추가 클릭 시 한국어 검증 오류가 표시된다.
    click(vcx, "admin-ssh-add-submit");
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("admin-ssh-form-error").is_some());

    // 고급 옵션 펼치기 → 포트·키 입력 노출.
    click(vcx, "admin-ssh-advanced-toggle");
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("admin-ssh-input-port").is_some());
    assert!(vcx.debug_bounds("admin-ssh-input-key").is_some());

    // 잘못된 포트/주소 검증 실패 후 정상 호스트 추가.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            assert_eq!(
                v.submit_ssh_host("집 맥", "-oEvil", "", "", cx),
                Err(crate::ssh_hosts::HostInputError::InvalidAddress)
            );
            assert_eq!(
                v.submit_ssh_host("집 맥", "owen@10.0.0.5", "0", "", cx),
                Err(crate::ssh_hosts::HostInputError::InvalidPort)
            );
            v.submit_ssh_host(
                "집 맥미니",
                "owen@10.0.0.5",
                "2222",
                "~/.ssh/id_ed25519",
                cx,
            )
            .unwrap();
        });
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let hosts = view.read(cx).ssh_hosts();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].label, "집 맥미니");
        assert_eq!(hosts[0].destination, "owen@10.0.0.5");
        assert_eq!(hosts[0].port, Some(2222));
        assert_eq!(hosts[0].identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
    });

    // Tailscale 발견 기기 주입 후 한 번 눌러 추가.
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_tailscale_peers(
                vec![crate::ssh_hosts::TailscalePeer {
                    hostname: "Owen Studio".into(),
                    dns_name: "owen-studio.tailnet.ts.net".into(),
                    ipv4: Some("100.100.1.9".into()),
                    address: "owen-studio.tailnet.ts.net".into(),
                }],
                cx,
            );
        });
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("admin-ssh-discovered").is_some());
    click(vcx, "admin-ssh-add-peer-0");
    vcx.update(|_, cx| {
        assert_eq!(view.read(cx).ssh_hosts().len(), 2);
        assert!(
            view.read(cx)
                .ssh_hosts()
                .iter()
                .any(|h| h.destination == "owen-studio.tailnet.ts.net")
        );
    });

    // 연결 테스트 실패 1: Connection refused → 원격 로그인 켜기 안내 노출, 등록 버튼 숨김.
    let target_host = vcx.update(|_, cx| view.read(cx).ssh_hosts()[1].clone());
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.apply_ssh_test_result(
                target_host.clone(),
                Err("ssh: connect to host port 22: Connection refused".into()),
                cx,
            );
        });
        window.draw(cx).clear(cx);
        let status = view.read(cx).ssh_status.as_ref().unwrap().to_string();
        assert!(
            status.contains("원격 로그인") || status.contains("Remote Login"),
            "unexpected status: {status}"
        );
    });
    assert!(vcx.debug_bounds("admin-ssh-register-device").is_none());

    // 연결 테스트 실패 2: Permission denied → "이 기기 등록하기" 버튼 노출 → 기본 키 없으면 확인 후 생성 & ssh-copy-id 탭 열기.
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.apply_ssh_test_result(
                target_host.clone(),
                Err("Permission denied (publickey,password).".into()),
                cx,
            );
        });
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("admin-ssh-register-device").is_some());
    click(vcx, "admin-ssh-register-device");
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("admin-ssh-keygen-confirm").is_some());
    click(vcx, "admin-ssh-keygen-confirm-yes");
    vcx.run_until_parked();
    assert!(d.join(".ssh").join("id_ed25519").is_file());
    vcx.update(|_, cx| {
        let pending = take_pending_ssh_connect(cx).expect("pending_ssh_connect should be set");
        assert!(pending.copy_id_first);
        let cmd = crate::ssh_hosts::build_connect_command(&pending);
        assert!(cmd.starts_with("ssh-copy-id"), "unexpected cmd: {cmd}");
    });

    fs::remove_dir_all(d).ok();
}

/// AS-02: 일시 IO 오류를 `NotFound`로 취급해 설정을 기본값으로 리셋하면 안 된다.
/// 직전 설정을 유지하고 오류를 표시하며 동면 표식 동기화를 건너뛴다.
#[gpui::test]
fn as02_transient_io_error_keeps_settings_and_hibernation_flag(cx: &mut TestAppContext) {
    let d = dir("as02-io-error");
    fs::write(d.join(SETTINGS_FILE), "hibernation_enabled = true\n").unwrap();
    init(cx, &d);
    cx.update(|cx| assert!(current(cx).hibernation_enabled));
    assert!(d.join(HIBERNATION_FLAG).is_file());
    // 일시 IO 오류 흉내: settings.toml 자리에 디렉터리를 둔다(NotFound가 아닌 읽기 실패).
    fs::remove_file(d.join(SETTINGS_FILE)).unwrap();
    fs::create_dir(d.join(SETTINGS_FILE)).unwrap();
    cx.update(reload);
    cx.update(|cx| {
        assert!(
            current(cx).hibernation_enabled,
            "transient IO error must not reset settings to default"
        );
        assert!(
            cx.global::<Config>().error.is_some(),
            "transient IO error must be surfaced"
        );
    });
    assert!(
        d.join(HIBERNATION_FLAG).is_file(),
        "hibernation flag must survive transient read error"
    );
    fs::remove_dir_all(d).ok();
}

/// AS-01: 기록 즉시 삭제는 UI 스레드에서 동기 IPC를 하면 안 된다. 2차 클릭에
/// 백그라운드로 돌리고 in-flight 표시를 먼저 보인다.
#[gpui::test]
fn as01_history_clear_runs_off_ui_thread_with_pending(cx: &mut TestAppContext) {
    let d = dir("as01-clear-bg");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Resources, cx));
    });
    vcx.run_until_parked();
    let clear = vcx
        .debug_bounds("admin-history-clear")
        .expect("missing admin-history-clear");
    vcx.simulate_click(clear.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|_, cx| assert_eq!(view.read(cx).armed.as_deref(), Some("admin-history-clear")));
    // 2차 클릭과 같은 실행을 직접 호출한다. 클릭 이벤트는 주차 뒤에 배달되어
    // 백그라운드 완료와 분리할 수 없으므로, 동기 구간을 여기서 본다.
    vcx.update(|_, cx| {
        view.update(cx, SettingsView::clear_history);
    });
    // 주차 없이: in-flight 표시가 먼저 보여야 한다(동기 IPC면 이미 최종 메시지다).
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert!(v.history_clear_pending, "clear must run in background");
        assert_eq!(
            v.history_status.as_deref(),
            Some(text::RECORDING_DELETING.get())
        );
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert!(!v.history_clear_pending);
        assert_eq!(
            v.history_status.as_deref(),
            Some(text::RECORDING_DELETE_UNAVAILABLE.get())
        );
    });
    fs::remove_dir_all(d).ok();
}

/// AS-06: 설정 변경 관찰자의 계정 재스캔은 UI 스레드에서 동기 수행하면 안 된다.
/// 외부 변경은 다음 주차 뒤에 반영되고, 그 전에는 이전 스냅샷이 보여야 한다.
#[gpui::test]
fn as06_settings_observer_refreshes_accounts_off_ui_thread(cx: &mut TestAppContext) {
    use krill_proto::usage::{ProviderKind, UsageRequest};
    let d = dir("as06-observer");
    init(cx, &d);
    let add = |label: &str| {
        crate::accounts::request_local(
            &d,
            Some(&d),
            &UsageRequest::AddAccount {
                provider: ProviderKind::Claude,
                label: label.into(),
                email: None,
                profile_dir: d.join(label).display().to_string(),
            },
        )
        .unwrap()
    };
    add("Seeded");
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert!(
            view.read(cx)
                .accounts()
                .accounts
                .iter()
                .any(|a| a.label == "Seeded")
        );
    });
    // 뷰를 거치지 않은 외부 변경.
    add("External");
    // 설정 변경(관찰자 발화). 주차 전에는 이전 스냅샷이 보여야 한다.
    vcx.update(|_, cx| edit(cx, |s| s.zoom = 1.1));
    vcx.update(|_, cx| {
        let snap = view.read(cx).accounts();
        assert!(snap.accounts.iter().any(|a| a.label == "Seeded"));
        assert!(
            !snap.accounts.iter().any(|a| a.label == "External"),
            "observer rescan must not run synchronously on the UI thread"
        );
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert!(
            view.read(cx)
                .accounts()
                .accounts
                .iter()
                .any(|a| a.label == "External")
        );
    });
    fs::remove_dir_all(d).ok();
}

/// AS-06: Agents 탭 렌더마다 `agents.toml`을 다시 읽으면 안 된다. 수정 시각이
/// 같으면 캐시를 쓰고, 바뀌면 새 본문을 읽는다.
#[gpui::test]
fn as06_agents_file_read_is_cached_by_mtime(cx: &mut TestAppContext) {
    let d = dir("as06-agents-cache");
    init(cx, &d);
    let view = cx.new(SettingsView::new);
    view.update(cx, |v, cx| {
        assert_eq!(v.cached_agents_file(Some(&d), cx), None);
        assert_eq!(v.agents_file_reads, 0, "UI only schedules the read");
    });
    cx.run_until_parked();
    view.update(cx, |v, cx| {
        assert_eq!(v.agents_file_reads, 1);
        assert_eq!(v.cached_agents_file(Some(&d), cx), None);
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| view.read(cx).agents_file_reads), 1);
    fs::write(d.join(krill_proto::agent::FILE_NAME), "[agent.claude]\n").unwrap();
    view.update(cx, |v, cx| {
        assert_eq!(v.cached_agents_file(Some(&d), cx), None);
        assert_eq!(
            v.agents_file_reads, 1,
            "changed files are also read off UI thread"
        );
    });
    cx.run_until_parked();
    view.update(cx, |v, cx| {
        assert_eq!(
            v.cached_agents_file(Some(&d), cx),
            Some("[agent.claude]\n".into())
        );
        assert_eq!(v.agents_file_reads, 2);
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| view.read(cx).agents_file_reads), 2);
    fs::remove_dir_all(d).ok();
}

/// AS-07: `edit`이 읽은 밑본과 저장 직전 디스크가 다르면 외부 편집이 낀
/// 것이므로 덮어쓰지 않고 오류를 낸다.
#[test]
fn as07_save_rejects_stale_base_and_keeps_external_edit() {
    let d = dir("as07-conflict");
    let path = d.join(SETTINGS_FILE);
    let write_zoom = |zoom: f32| {
        let s = Settings {
            zoom,
            ..Settings::default()
        };
        fs::write(&path, s.to_toml()).unwrap();
    };
    write_zoom(1.0);
    save_settings(
        &path,
        &Settings {
            zoom: 1.0,
            ..Settings::default()
        },
        Some(&fs::read_to_string(&path).unwrap()),
    )
    .unwrap();
    let base = fs::read_to_string(&path).unwrap();
    // 밑본을 읽은 뒤 외부 편집이 끼어든다.
    write_zoom(1.5);
    let err = save_settings(
        &path,
        &Settings {
            zoom: 2.0,
            ..Settings::default()
        },
        Some(&base),
    )
    .expect_err("stale base must be rejected, not silently overwritten");
    assert_eq!(err.kind(), std::io::ErrorKind::Other);
    let kept = Settings::parse(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!((kept.zoom() - 1.5).abs() < f32::EPSILON);
    // 최신 밑본으로는 저장된다.
    let fresh = fs::read_to_string(&path).unwrap();
    save_settings(
        &path,
        &Settings {
            zoom: 2.0,
            ..Settings::default()
        },
        Some(&fresh),
    )
    .unwrap();
    let saved = Settings::parse(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!((saved.zoom() - 2.0).abs() < f32::EPSILON);
    fs::remove_dir_all(d).ok();
}

/// AS-08: `keys.toml` 동시 편집에 고정 임시 파일명이면 서로 밟는다.
/// 고유 임시명+원자 교체면 모두 성공하고 파일이 멀쩡하다.
#[test]
fn as08_concurrent_keys_edits_never_leave_torn_file() {
    use std::sync::{Arc, Barrier};
    let d = dir("as08-keys");
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let dd = d.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                for n in 0..50 {
                    shortcuts::edit_keys(&dd, |t| {
                        t.insert(format!("k-{i}"), toml::Value::String(format!("v-{n}")));
                    })
                    .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let body = fs::read_to_string(d.join(KEYS_FILE)).unwrap();
    assert!(Keys::parse(&body).is_ok(), "torn keys.toml: {body}");
    for entry in fs::read_dir(&d).unwrap() {
        let path = entry.unwrap().path();
        assert!(
            path.extension()
                .is_none_or(|e| !e.eq_ignore_ascii_case("tmp")),
            "leftover tmp file: {}",
            path.display()
        );
    }
    fs::remove_dir_all(d).ok();
}

#[test]
fn as12_settings_save_preserves_future_fields_and_source() {
    let d = dir("as12");
    let path = d.join(SETTINGS_FILE);
    let raw = "# future settings\nzoom = 1.0\nfuture = { enabled = true } # preserve\n[[quick_commands]]\nname = 'one'\ntext = 'echo one'\nfuture = 'command'\n[[webhooks]]\nprovider = 'ntfy'\nurl = 'https://ntfy.sh/test'\nfuture = 'webhook'\n";
    fs::write(&path, raw).unwrap();
    let mut settings = Settings::parse(raw).unwrap();
    settings.zoom = 1.2;
    settings.quick_commands[0].text = "echo edited".into();
    save_settings(&path, &settings, Some(raw)).unwrap();
    let body = fs::read_to_string(&path).unwrap();
    let value: toml::Value = toml::from_str(&body).unwrap();
    assert!(body.contains("future = { enabled = true } # preserve"));
    assert!(body.contains("# future settings"));
    assert_eq!(
        value["quick_commands"][0]["future"].as_str(),
        Some("command")
    );
    assert_eq!(value["webhooks"][0]["future"].as_str(), Some("webhook"));
    assert_eq!(Settings::parse(&body).unwrap(), settings);
    fs::remove_dir_all(d).unwrap();
}

#[test]
fn as12_keys_edit_preserves_future_fields_in_complex_toml() {
    let d = dir("as12-keys");
    let path = d.join(KEYS_FILE);
    let raw = "# future keys\nfuture = { mode = 'new' } # preserve\n[bindings]\n'cmd-j' = '''test::Probe'''\n";
    fs::write(&path, raw).unwrap();
    shortcuts::edit_keys(&d, |bindings| {
        bindings.insert("cmd-j".into(), "".into());
    })
    .unwrap();
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.contains("future = { mode = 'new' } # preserve"));
    assert!(body.contains("# future keys"));
    assert_eq!(
        Keys::parse(&body).unwrap().bindings["cmd-j"].as_str(),
        Some("")
    );
    fs::remove_dir_all(d).unwrap();
}

/// 교차 검수 AS-07: 밑본을 읽은 뒤 외부 작성자가 파일을 만들거나 지워도 덮어쓰지 않는다.
#[test]
fn settings_conflict_includes_external_creation_and_deletion() {
    let d = dir("cross-settings-presence");
    let path = d.join(SETTINGS_FILE);
    let initial = Settings::default().to_toml();
    fs::write(&path, &initial).unwrap();
    assert!(
        save_settings(&path, &Settings::default(), None).is_err(),
        "a file created after observing absence must not be overwritten"
    );
    fs::remove_file(&path).unwrap();
    assert!(
        save_settings(&path, &Settings::default(), Some(&initial)).is_err(),
        "external deletion must not be undone with a stale base"
    );
    assert!(!path.exists());
    fs::remove_dir_all(d).ok();
}

#[gpui::test]
fn agents_render_defers_file_io(cx: &mut TestAppContext) {
    let d = dir("cross-agents-render");
    init(cx, &d);
    fs::write(
        d.join(krill_proto::agent::FILE_NAME),
        "[agent.claude]\nname = \"Claude Code\"\nlaunch = [\"claude\", \"--model\", \"custom\"]\n",
    )
    .unwrap();
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            let _ = v.render_agents_tab(&Settings::default(), window, cx);
            assert_eq!(
                v.agents_file_reads, 0,
                "render must not read agents.toml synchronously"
            );
        });
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            let _ = v.render_agents_tab(&Settings::default(), window, cx);
            assert_eq!(
                v.agent_edits["claude"].args.read(cx).value().as_ref(),
                "--model custom"
            );
        });
    });
    fs::remove_dir_all(d).ok();
}

/// 갱신 시작 뒤 도착한 데몬 거부를 오래된 성공 스냅샷으로 덮으면 안 된다.
#[gpui::test]
fn stale_account_refresh_preserves_newer_request_error(cx: &mut TestAppContext) {
    let d = dir("cross-stale-account-refresh");
    init(cx, &d);
    let view = cx.new(SettingsView::new);
    view.update(cx, |v, cx| {
        v.refresh_accounts_async(cx);
        v.finish_account_rule_request(Err("daemon refused request".into()), cx);
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            view.read(cx).account_error.as_deref(),
            Some("daemon refused request")
        );
    });
    fs::remove_dir_all(d).ok();
}

/// OWE-708: 상태바와 계정 카드가 같은 경고(80)·위험(90) 기준을 쓴다.
#[test]
fn usage_tone_matches_shared_warn_and_critical_thresholds() {
    use krill_proto::usage::{CRITICAL_PERCENT, WARN_PERCENT};
    assert_eq!(usage_tone(WARN_PERCENT - 1), UsageTone::Normal);
    assert_eq!(usage_tone(WARN_PERCENT), UsageTone::Warn);
    assert_eq!(usage_tone(CRITICAL_PERCENT - 1), UsageTone::Warn);
    assert_eq!(usage_tone(CRITICAL_PERCENT), UsageTone::Critical);
    assert_eq!(usage_tone(100), UsageTone::Critical);
}

/// OWE-708: 접힌 규칙 요약 한 줄은 지표·임계치·대상·꺼짐을 담는다.
#[test]
fn rule_summary_names_metric_threshold_target_and_off() {
    use krill_proto::usage::{AutoSwitchMetric, AutoSwitchRule, ProviderKind};
    let mut rule = AutoSwitchRule {
        provider: ProviderKind::Claude,
        from_account_id: "claude-work".into(),
        enabled: true,
        threshold_percent: 85,
        metric: AutoSwitchMetric::Max,
        target_account_id: Some("claude-missing".into()),
    };
    assert_eq!(rule_summary(&rule, &[]), "Max ≥ 85% → claude-missing");
    rule.enabled = false;
    assert!(rule_summary(&rule, &[]).ends_with(text::OFF.get()));
}

/// OWE-704: 계정 제거는 첫 클릭에 무장(안내 문구)만 하고, 두 번째 클릭에 삭제한다.
/// OWE-705: 이름 변경은 라벨만 바꾸고 id·프로필 폴더를 유지한다.
#[gpui::test]
fn account_remove_needs_confirm_and_rename_keeps_id(cx: &mut TestAppContext) {
    use krill_proto::usage::{ProviderKind, UsageRequest};

    let d = dir("owe704-remove-confirm");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    let profile = d.join("work-profile");
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Accounts, cx);
            v.apply_account_request(
                UsageRequest::AddAccount {
                    provider: ProviderKind::Claude,
                    label: "Work".into(),
                    email: None,
                    profile_dir: profile.display().to_string(),
                },
                cx,
            );
        });
    });
    vcx.run_until_parked();
    let click = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };
    let has_work = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|_, cx| {
            view.read(cx)
                .accounts()
                .accounts
                .iter()
                .any(|a| a.id == "claude-work")
        })
    };

    // OWE-719: 이름 변경·제거는 … 메뉴에 있다. 메뉴 항목은 start_rename/start_remove를 부른다.
    assert!(vcx.debug_bounds("admin-account-menu-claude-work").is_some());
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.start_rename("claude-work".into(), "Work".into(), window, cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let input = view.read(cx).rename.as_ref().unwrap().1.clone();
        input.update(cx, |s, cx| s.set_value("Acme", window, cx));
    });
    click(vcx, "admin-rename-save-claude-work");
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert!(v.rename.is_none());
        let acc = v
            .accounts()
            .accounts
            .iter()
            .find(|a| a.id == "claude-work")
            .unwrap();
        assert_eq!(acc.label, "Acme");
        assert_eq!(acc.profile_dir, profile.display().to_string());
    });

    // 제거: 확인 상자 없이는 지우는 버튼이 없고, 취소하면 그대로, 확인 상자의 "제거"에서만 지운다.
    assert!(
        vcx.debug_bounds("admin-remove-confirm-claude-work")
            .is_none()
    );
    assert!(
        vcx.debug_bounds("admin-remove-account-claude-work")
            .is_none()
    );
    let start_remove = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|_, cx| view.update(cx, |v, cx| v.start_remove("claude-work".into(), cx)));
        vcx.run_until_parked();
    };
    start_remove(vcx);
    assert!(
        vcx.debug_bounds("admin-remove-confirm-claude-work")
            .is_some()
    );
    click(vcx, "admin-remove-cancel-claude-work");
    assert!(has_work(vcx), "cancel keeps the account");
    assert!(
        vcx.debug_bounds("admin-remove-confirm-claude-work")
            .is_none()
    );
    // Esc는 대화상자만 닫는다(설정 페이지는 그대로).
    start_remove(vcx);
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("admin-remove-confirm-claude-work")
            .is_none()
    );
    assert!(vcx.debug_bounds("admin-account-row-claude-work").is_some());
    assert!(has_work(vcx));
    start_remove(vcx);
    click(vcx, "admin-remove-account-claude-work");
    assert!(!has_work(vcx));
    assert!(profile.is_dir(), "custom profile dirs are never moved");

    // 고아 폴더 카드는 목록이 있을 때만 보이고, 정리 버튼도 첫 클릭은 무장만 한다.
    assert!(vcx.debug_bounds("admin-orphan-profiles").is_none());
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.orphan_profiles = vec![krill_proto::usage::OrphanProfile {
                provider: ProviderKind::OpenCode,
                dir: "/nonexistent/opencode-profile-2".into(),
            }];
            cx.notify();
        });
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-orphan-profiles").is_some());
    click(vcx, "admin-purge-orphans");
    vcx.update(|_, cx| {
        let v = view.read(cx);
        assert_eq!(v.armed.as_deref(), Some("admin-purge-orphans"));
        assert_eq!(v.orphan_profiles.len(), 1);
    });
    fs::remove_dir_all(d).unwrap();
}

/// MK-162: 일반 탭에 설정 점검 섹션(요약 + 점검 열기 버튼)이 있다.
#[gpui::test]
fn general_tab_shows_checks_section(cx: &mut TestAppContext) {
    let d = dir("checks-section");
    init(cx, &d);
    let (_, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("checks-summary").is_some());
    assert!(vcx.debug_bounds("open-checks").is_some());
    fs::remove_dir_all(d).unwrap();
}

/// OWE-706: 설치 감지에서 빠진 프로바이더는 접힌 "설치 안 됨" 묶음 아래로 간다.
#[gpui::test]
fn accounts_tab_folds_not_installed_providers(cx: &mut TestAppContext) {
    use krill_proto::usage::ProviderKind;

    let d = dir("owe706-installed");
    init(cx, &d);
    cx.update(|cx| {
        cx.set_global(crate::accounts::installed::Installed(Some(vec![
            ProviderKind::Claude,
            ProviderKind::OpenCode,
        ])));
    });
    let (_view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    vcx.run_until_parked();
    let tab = vcx.debug_bounds("admin-tab-accounts").unwrap();
    vcx.simulate_click(tab.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-add-account-claude").is_some());
    assert!(vcx.debug_bounds("admin-add-account-opencode").is_some());
    for hidden in ["admin-add-account-codex", "admin-add-account-agy"] {
        assert!(
            vcx.debug_bounds(hidden).is_none(),
            "{hidden} must be folded"
        );
    }
    let fold = vcx.debug_bounds("admin-accounts-not-installed").unwrap();
    vcx.simulate_click(fold.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("admin-add-account-codex").is_some());
    fs::remove_dir_all(d).unwrap();
}

/// OWE-704 실화면 QA: 무장한 "제거"가 커져 "이름 변경" 자리를 덮고, 바깥 클릭으로 풀리지 않아
/// "이름 변경"을 누르려다 계정이 삭제됐다. 무장해도 폭·위치가 그대로고, 바깥 클릭·Esc·3초 경과에 풀린다.
#[gpui::test]
fn confirm_button_keeps_bounds_and_disarms_outside_escape_and_timeout(cx: &mut TestAppContext) {
    use krill_proto::usage::{ProviderKind, UsageRequest};

    let d = dir("owe704-confirm-stable");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Accounts, cx);
            v.apply_account_request(
                UsageRequest::AddAccount {
                    provider: ProviderKind::Claude,
                    label: "Work".into(),
                    email: None,
                    profile_dir: d.join("work-profile").display().to_string(),
                },
                cx,
            );
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.orphan_profiles = vec![krill_proto::usage::OrphanProfile {
                provider: ProviderKind::OpenCode,
                dir: "/nonexistent/opencode-profile-2".into(),
            }];
            cx.notify();
        });
    });
    vcx.run_until_parked();
    // OWE-719: 계정 제거는 확인 상자로 옮겼다. 두 번 누르기 헬퍼는 고아 폴더 정리 버튼으로 검증한다.
    let rm = "admin-purge-orphans";
    let bounds = |vcx: &mut gpui::VisualTestContext, s: &'static str| {
        vcx.debug_bounds(s).unwrap_or_else(|| panic!("missing {s}"))
    };
    let click = |vcx: &mut gpui::VisualTestContext, s: &'static str| {
        let b = bounds(vcx, s);
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };
    let armed = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|_, cx| view.read(cx).armed.as_ref().map(ToString::to_string))
    };

    let rm_before = bounds(vcx, rm);
    click(vcx, rm);
    assert_eq!(armed(vcx).as_deref(), Some(rm));
    assert_eq!(bounds(vcx, rm), rm_before, "armed button must not grow");

    // 바깥(계정 행) 클릭은 무장만 푼다.
    click(vcx, "admin-account-row-claude-work");
    assert_eq!(armed(vcx), None);

    // Esc는 창을 닫지 않고 무장만 푼다.
    click(vcx, rm);
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert_eq!(armed(vcx), None);
    assert!(vcx.debug_bounds(rm).is_some());

    // 3초가 지나면 저절로 풀리고 실행하지 않는다.
    click(vcx, rm);
    vcx.executor().advance_clock(CONFIRM_TIMEOUT);
    vcx.run_until_parked();
    assert_eq!(armed(vcx), None, "timeout disarms");
    vcx.update(|_, cx| assert_eq!(view.read(cx).orphan_profiles.len(), 1));
    fs::remove_dir_all(d).ok();
}

/// 같은 헬퍼를 쓰는 다른 위험 버튼(웹훅 비우기·기록 삭제·오케스트레이션 초기화)도 무장 시 폭이 그대로고
/// 바깥 클릭으로 풀리며 실행하지 않는다.
#[gpui::test]
fn other_confirm_buttons_keep_width_and_disarm_on_outside_click(cx: &mut TestAppContext) {
    let d = dir("owe704-confirm-others");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    vcx.update(|_, cx| {
        edit(cx, |s| {
            s.webhooks.push(crate::webhook::Endpoint::Ntfy {
                url: "http://127.0.0.1:9/hook".into(),
            });
        });
    });
    for (tab, id) in [
        (AdminTab::Workflows, "admin-webhook-clear"),
        (AdminTab::Workflows, "admin-orch-reset"),
        (AdminTab::Resources, "admin-history-clear"),
    ] {
        vcx.update(|_, cx| view.update(cx, |v, cx| v.set_tab(tab, cx)));
        vcx.run_until_parked();
        let before = vcx
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("missing {id}"));
        vcx.simulate_click(before.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|_, cx| assert_eq!(view.read(cx).armed.as_deref(), Some(id)));
        assert_eq!(vcx.debug_bounds(id), Some(before), "{id} grew when armed");
        // 버튼 왼쪽 위 바깥(빈 곳) 클릭.
        let outside = gpui::point(px(2.), before.origin.y + before.size.height / 2.);
        vcx.simulate_click(outside, gpui::Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|_, cx| assert_eq!(view.read(cx).armed, None, "{id} stayed armed"));
    }
    vcx.update(|_, cx| assert_eq!(current(cx).webhooks.len(), 1));
    fs::remove_dir_all(d).ok();
}

/// OWE-704 실화면 QA: 단축키·원격 연결(SSH) 탭 아이콘이 빈 칸이었다(기본 번들에 없는 Keyboard·Server).
/// 모든 탭 아이콘이 실제 SVG로 그려져야 한다.
#[test]
fn admin_tab_icons_all_resolve_to_svg() {
    use gpui::AssetSource as _;
    for tab in AdminTab::ALL {
        let path = tab.icon().path();
        // 기본 번들은 없는 경로에 Err를 돌려준다(앱에선 빈 아이콘).
        let svg = gpui_kit_assets::Assets
            .load(&path)
            .ok()
            .flatten()
            .or_else(|| TabIcons.load(&path).unwrap());
        assert!(
            svg.is_some_and(|b| b.starts_with(b"<svg")),
            "{tab:?} icon {path} is not bundled"
        );
    }
}

/// OWE-719: 설정 내용 열은 사이드바 바로 오른쪽(가로 여백 `px_8`)에서 시작하고 최대 폭을 넘지 않는다. 넓은 창에서
/// 가운데 정렬로 왼쪽이 비고 내용이 오른쪽으로 몰리던 결함. 계정 행은 열 폭을 다 쓰고, 아바타는 왼쪽 끝,
/// 사용량 묶음·전환 버튼은 행 안 오른쪽에 있다.
#[gpui::test]
fn owe719_accounts_content_starts_next_to_sidebar_and_rows_fill_column(cx: &mut TestAppContext) {
    use krill_proto::usage::{ProviderKind, UsageRequest};

    let d = dir("owe719-layout");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Accounts, cx);
            v.apply_account_request(
                UsageRequest::AddAccount {
                    provider: ProviderKind::Claude,
                    label: "Work".into(),
                    email: None,
                    profile_dir: d.join("work").display().to_string(),
                },
                cx,
            );
        });
    });
    let bounds = |vcx: &mut gpui::VisualTestContext, s: &'static str| {
        vcx.debug_bounds(s).unwrap_or_else(|| panic!("missing {s}"))
    };
    for width in [1100., 1400., 1800.] {
        vcx.simulate_resize(size(px(width), px(4000.)));
        vcx.run_until_parked();
        let sidebar = bounds(vcx, "admin-sidebar");
        let column = bounds(vcx, "admin-content-column");
        let gap = column.origin.x - sidebar.right();
        assert!(
            gap >= px(31.) && gap <= px(33.),
            "{width}: content must start right after the sidebar, gap {gap:?}"
        );
        assert!(column.size.width <= px(760.), "{width}: {column:?}");
        let row = bounds(vcx, "admin-account-row-claude-work");
        assert_eq!(row.origin.x, column.origin.x, "{width}");
        assert_eq!(row.right(), column.right(), "{width}");
        let avatar = bounds(vcx, "admin-account-status-claude-work");
        let usage = bounds(vcx, "admin-account-usage-claude-work");
        let switch = bounds(vcx, "admin-switch-account-claude-work");
        assert!(avatar.origin.x - row.origin.x < px(20.), "{avatar:?}");
        assert!(usage.right() <= switch.origin.x, "{usage:?} {switch:?}");
        assert!(switch.right() <= row.right(), "{switch:?} {row:?}");
        assert!(row.size.height < px(70.), "row must stay one line: {row:?}");
        let sub = bounds(vcx, "admin-accounts-subheader-claude");
        assert!(sub.size.width > px(300.), "{sub:?}");
    }
    fs::remove_dir_all(d).ok();
}

/// OWE-719: 사용 중 표시는 활성 계정 하나에만 있고(배지·비활성 버튼 중복 없음), "활성 계정" 줄이 있다.
/// 비활성 계정에만 "전환"이 있고, 전환하면 표시가 옮겨 간다.
#[gpui::test]
fn owe719_single_in_use_marker_moves_with_switch(cx: &mut TestAppContext) {
    use krill_proto::usage::{ProviderKind, UsageRequest};

    let d = dir("owe719-in-use");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(4000.)));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Accounts, cx);
            v.apply_account_request(
                UsageRequest::AddAccount {
                    provider: ProviderKind::Claude,
                    label: "Work".into(),
                    email: None,
                    profile_dir: d.join("work").display().to_string(),
                },
                cx,
            );
        });
    });
    vcx.run_until_parked();
    let active = vcx.update(|_, cx| {
        view.read(cx)
            .accounts()
            .accounts
            .iter()
            .filter(|a| a.provider == ProviderKind::Claude && a.active)
            .map(|a| a.id.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(active, ["claude-host"]);
    assert!(vcx.debug_bounds("admin-accounts-active-claude").is_some());
    assert!(
        vcx.debug_bounds("admin-account-in-use-claude-host")
            .is_some()
    );
    assert!(
        vcx.debug_bounds("admin-account-in-use-claude-work")
            .is_none()
    );
    assert!(
        vcx.debug_bounds("admin-switch-account-claude-host")
            .is_none()
    );
    let switch = vcx
        .debug_bounds("admin-switch-account-claude-work")
        .expect("inactive account offers switch");
    vcx.simulate_click(switch.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("admin-account-in-use-claude-work")
            .is_some()
    );
    assert!(
        vcx.debug_bounds("admin-account-in-use-claude-host")
            .is_none()
    );
    assert!(
        vcx.debug_bounds("admin-switch-account-claude-host")
            .is_some()
    );
    assert!(
        vcx.debug_bounds("admin-switch-account-claude-work")
            .is_none()
    );
    fs::remove_dir_all(d).ok();
}

/// OWE-719 (1): 데몬 연결 꼬리가 "연결됨"이면 사용량 상태가 로그인 필요여도 "다시 로그인"을 주 동작으로 띄우지
/// 않는다. 꼬리가 로그인 필요면 띄운다. 꼬리를 지원하는 데몬일 때만 "사용량 직접 조회" 토글이 보인다.
#[gpui::test]
fn owe719_connection_tail_drives_login_action_and_direct_usage_toggle(cx: &mut TestAppContext) {
    use krill_proto::usage::{
        AccountConnection, AccountInfo, AccountStatus, AccountStatusInfo, ConnectionStatus,
        ConnectionsSnapshot, ProviderKind, Snapshot,
    };

    let d = dir("owe719-connection");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(4000.)));
    let set = |vcx: &mut gpui::VisualTestContext, tail: Option<ConnectionStatus>| {
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.tab = AdminTab::Accounts;
                v.accounts_snapshot.accounts = vec![AccountInfo {
                    id: "claude-work".into(),
                    provider: ProviderKind::Claude,
                    label: "Work".into(),
                    email: Some("w@example.com".into()),
                    profile_dir: d.join("work").display().to_string(),
                    active: true,
                    host_default: false,
                    stale: false,
                    usage: Snapshot::default(),
                }];
                v.accounts_snapshot.statuses = vec![AccountStatusInfo {
                    account_id: "claude-work".into(),
                    status: AccountStatus::NeedsLogin,
                    updated_at: Some(crate::accounts::display::now()),
                    detail: None,
                }];
                *v.accounts_snapshot.connections = ConnectionsSnapshot {
                    supported: tail.is_some(),
                    direct_usage: false,
                    accounts: tail
                        .map(|status| AccountConnection {
                            account_id: "claude-work".into(),
                            status,
                        })
                        .into_iter()
                        .collect(),
                };
                cx.notify();
            });
        });
        vcx.run_until_parked();
    };
    set(vcx, Some(ConnectionStatus::Connected));
    assert!(
        vcx.debug_bounds("admin-login-account-claude-work")
            .is_none()
    );
    assert!(
        vcx.debug_bounds("admin-account-status-claude-work")
            .is_some()
    );
    assert!(vcx.debug_bounds("admin-direct-usage-toggle").is_some());
    set(vcx, Some(ConnectionStatus::NeedsLogin));
    assert!(
        vcx.debug_bounds("admin-login-account-claude-work")
            .is_some()
    );
    set(vcx, None);
    assert!(vcx.debug_bounds("admin-direct-usage-toggle").is_none());
    assert!(
        vcx.debug_bounds("admin-login-account-claude-work")
            .is_some(),
        "old daemons fall back to the usage status"
    );
    fs::remove_dir_all(d).ok();
}

/// MK-156: 설정 General 탭에서 업데이트 확인 버튼·자동 재시작 토글·준비 완료 시 재시작 버튼이 동작한다.
#[gpui::test]
fn general_tab_renders_update_rows_and_toggles_auto_restart(cx: &mut TestAppContext) {
    use crate::updater::{CheckStatus, StagedUpdate, UpdateState};

    let d = dir("mk156-general-updates");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("settings-update-check").is_some());
    assert!(vcx.debug_bounds("update-auto-restart-on").is_some());
    assert!(vcx.debug_bounds("update-auto-restart-off").is_some());
    assert!(vcx.debug_bounds("settings-update-restart").is_none());

    // [업데이트 확인] 버튼 클릭 시 수동 점검이 시작되어 UpdateState가 갱신된다.
    let check_btn = vcx.debug_bounds("settings-update-check").unwrap();
    vcx.simulate_click(check_btn.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let status = cx
            .try_global::<UpdateState>()
            .map(|u| u.status.clone())
            .unwrap_or_default();
        assert!(!matches!(status, CheckStatus::Idle));
    });

    let toggle = vcx.debug_bounds("update-auto-restart-on").unwrap();
    vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert!(current(cx).auto_restart_update);
        let staged = StagedUpdate {
            state_dir: d.clone(),
            app_bundle: d.join("Krill.app"),
            from_version: "0.1.1".to_owned(),
            to_version: "0.2.0".to_owned(),
            manifest_url: "https://127.0.0.1:1/latest.json".to_owned(),
            relaunched: false,
        };
        cx.set_global(UpdateState {
            status: CheckStatus::Ready(staged.clone()),
            staged: Some(staged),
            card_collapsed: false,
        });
        view.update(cx, |_, cx| cx.notify());
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("settings-update-restart").is_some());

    let reloaded = Settings::parse(&fs::read_to_string(d.join(SETTINGS_FILE)).unwrap()).unwrap();
    assert!(reloaded.auto_restart_update);
    fs::remove_dir_all(d).ok();
}

/// OWE-719 QA D3: 계정 행 사용량 열의 "5시간" 라벨이 고정폭 20px에 갇혀 "5시/간"으로 줄바꿈됐다. 창 라벨은
/// 한 줄이어야 한다(두 라벨 높이가 같고, 막대가 같은 x에서 시작).
#[gpui::test]
fn owe719_usage_window_labels_never_wrap(cx: &mut TestAppContext) {
    use krill_proto::usage::{ProviderKind, UsageRequest};

    let d = dir("owe719-usage-label");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(4000.)));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Accounts, cx);
            v.apply_account_request(
                UsageRequest::AddAccount {
                    provider: ProviderKind::Claude,
                    label: "Work".into(),
                    email: None,
                    profile_dir: d.join("work").display().to_string(),
                },
                cx,
            );
        });
    });
    vcx.run_until_parked();
    assert_eq!(text::WINDOW_5H.get(), "5시간");
    let five = vcx.debug_bounds("admin-usage-label-5h").expect("5h label");
    let seven = vcx.debug_bounds("admin-usage-label-7d").expect("7d label");
    assert_eq!(five.size.height, seven.size.height, "{five:?} {seven:?}");
    assert_eq!(
        five.size.width, seven.size.width,
        "labels share a fixed width"
    );
    // 테스트 플랫폼 글꼴은 실제 폭을 재지 않는다(전부 반각 취급). 그래서 실화면 기준으로 막는다: 한글은
    // 전각(1em)이라 "5시간"은 1em × 2 + 숫자 반각이 필요하다. 20px(= 2em)에서 접혔다.
    let em = vcx.update(|window, _| crate::ui::TEXT_2XS.to_pixels(window.rem_size()));
    assert!(five.size.width >= em * 2.5, "{five:?} < 2.5em ({em:?})");
    fs::remove_dir_all(d).ok();
}

/// OWE-719 QA D4: "사용량 직접 조회"·"자동 계정 전환"이 현재 상태 없이 버튼 하나("켜기")만 보였다. 다른 설정과
/// 같은 켜기/끄기 세그먼트여야 하고, 같은 쪽을 다시 눌러도 상태가 뒤집히지 않는다.
#[gpui::test]
fn owe719_account_toggles_are_on_off_segments(cx: &mut TestAppContext) {
    use krill_proto::usage::ConnectionsSnapshot;

    let d = dir("owe719-segments");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(4000.)));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Accounts, cx);
            *v.accounts_snapshot.connections = ConnectionsSnapshot {
                supported: true,
                direct_usage: false,
                accounts: Vec::new(),
            };
            cx.notify();
        });
    });
    vcx.run_until_parked();
    for id in [
        "admin-direct-usage-on",
        "admin-direct-usage-off",
        "admin-auto-switch-on",
        "admin-auto-switch-off",
    ] {
        assert!(vcx.debug_bounds(id).is_some(), "missing {id}");
    }
    let enabled = |vcx: &mut gpui::VisualTestContext| {
        vcx.update(|_, cx| view.read(cx).accounts().auto_switch.enabled)
    };
    let initial = enabled(vcx);
    let target = if initial {
        "admin-auto-switch-on"
    } else {
        "admin-auto-switch-off"
    };
    for _ in 0..2 {
        let at = vcx.debug_bounds(target).unwrap().center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        vcx.run_until_parked();
        assert_eq!(enabled(vcx), initial, "pressing the current side keeps it");
    }
    let other = if initial {
        "admin-auto-switch-off"
    } else {
        "admin-auto-switch-on"
    };
    let at = vcx.debug_bounds(other).unwrap().center();
    vcx.simulate_click(at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(enabled(vcx), !initial);
}

#[gpui::test]
fn mobile_eighth_tab_qr_and_mode_invalidation(cx: &mut TestAppContext) {
    use krill_proto::mobile::{ConnectMode, MobileReply, MobileRequest};
    let d = dir("mobile");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = requests.clone();
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.mobile = mobile::Panel::new(std::sync::Arc::new(move |request| {
                record.lock().unwrap().push(request.clone());
                Ok(match request {
                    MobileRequest::MintPairingTicket { mode, .. } => MobileReply::PairingOffer {
                        url: "https://example.test/?pair=fixture#key=fixture".into(),
                        host: "example.test".into(),
                        port: 1234,
                        expires_at_ms: krill_proto::mobile::client::now_ms() + 300_000,
                        mode,
                        interfaces: Vec::new(),
                    },
                    MobileRequest::ConnectedDevices => MobileReply::ConnectedDevices(Vec::new()),
                    _ => MobileReply::Devices(Vec::new()),
                })
            }));
            assert_eq!(AdminTab::ALL.get(7), Some(&AdminTab::Mobile));
            v.set_tab(AdminTab::Mobile, cx);
            v.mobile_pair(ConnectMode::Lan, None, cx);
            assert!(v.mobile.offer.is_none());
        });
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        assert!(view.read(cx).mobile.offer.is_some());
        window.draw(cx).clear(cx);
        view.update(cx, |v, cx| {
            v.mobile_pair(ConnectMode::Tailscale, Some("100.64.1.2:0".into()), cx);
            assert!(v.mobile.offer.is_none());
        });
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        assert!(view.read(cx).mobile.offer.is_some());
        window.draw(cx).clear(cx);
    });
    assert!(requests.lock().unwrap().iter().any(|r| matches!(
        r,
        MobileRequest::MintPairingTicket {
            mode: ConnectMode::Tailscale,
            rotate: true,
            ..
        }
    )));
    fs::remove_dir_all(d).ok();
}

#[gpui::test]
fn owe728_workflows_tab_worktree_symlinks_and_repo_hooks(cx: &mut TestAppContext) {
    let d = dir("owe728-workflows");
    let repo_dir = d.join("sample-repo");
    fs::create_dir_all(&repo_dir).unwrap();
    let repo_canonical = fs::canonicalize(&repo_dir)
        .unwrap_or_else(|_| repo_dir.clone())
        .display()
        .to_string();
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1200.), px(2800.)));
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Workflows, cx));
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("admin-worktree-symlinks-card").is_some());
    assert!(vcx.debug_bounds("admin-repo-hooks-card").is_some());
    assert!(vcx.debug_bounds("admin-worktree-symlinks-empty").is_some());

    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            assert!(
                v.add_worktree_symlink_for(&repo_canonical, "../secret", cx)
                    .is_err()
            );
            assert_eq!(
                v.add_worktree_symlink_for(&repo_canonical, ".env", cx)
                    .unwrap(),
                vec![".env".to_owned()]
            );
            assert_eq!(
                v.add_worktree_symlink_for(&repo_canonical, "node_modules", cx)
                    .unwrap(),
                vec![".env".to_owned(), "node_modules".to_owned()]
            );
            v.save_repo_hooks_for(
                &repo_canonical,
                Some("pnpm install"),
                Some("./scripts/cleanup.sh"),
                cx,
            )
            .unwrap();
        });
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("admin-symlink-remove-0").is_some());
    assert!(vcx.debug_bounds("admin-repo-hook-row-0").is_some());

    let store = krill_proto::orchestration::WorktreeStore::load(&d).unwrap();
    assert_eq!(
        store.symlinks_for(&repo_canonical),
        vec![".env".to_owned(), "node_modules".to_owned()]
    );
    assert_eq!(
        store.setup_script_for(&repo_canonical).as_deref(),
        Some("pnpm install")
    );
    assert_eq!(
        store.archive_script_for(&repo_canonical).as_deref(),
        Some("./scripts/cleanup.sh")
    );

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.remove_worktree_symlink_for(&repo_canonical, ".env", cx);
        });
    });
    let store_after = krill_proto::orchestration::WorktreeStore::load(&d).unwrap();
    assert_eq!(
        store_after.symlinks_for(&repo_canonical),
        vec!["node_modules".to_owned()]
    );

    fs::remove_dir_all(d).ok();
}

/// OWE-737: 에이전트 탭의 '첫 프롬프트로 브랜치·세션 이름 자동 변경'·'프롬프트 캐시 카운트다운'·'캐시 유지 시간(5분/1시간)'·
/// '워크스페이스 신뢰 프리셋 자동 등록(기본 꺼짐·동의 확인 후 켜기)' 컨트롤이 `settings.toml` 및 플래그 파일에 동기화된다.
#[gpui::test]
fn owe737_agent_settings_toggle_auto_rename_and_cache_timer_and_sync_flag_file(
    cx: &mut TestAppContext,
) {
    let d = dir("owe737-agent-settings");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(6000.)));
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Agents, cx));
    });
    vcx.run_until_parked();

    for id in [
        "auto-rename-branch-on",
        "auto-rename-branch-off",
        "prompt-cache-timer-on",
        "prompt-cache-timer-off",
        "prompt-cache-ttl-5m",
        "prompt-cache-ttl-1h",
        "workspace-trust-preset-on",
        "workspace-trust-preset-off",
    ] {
        assert!(vcx.debug_bounds(id).is_some(), "missing {id}");
    }

    // 기본값: 자동 이름 변경·캐시 타이머는 켜짐, 워크스페이스 신뢰 프리셋은 꺼짐(동의 전).
    assert!(krill_proto::git::is_auto_rename_enabled(Some(&d)));
    assert!(!krill_proto::git::is_workspace_trust_preset_enabled(Some(
        &d
    )));
    assert!(vcx.debug_bounds("workspace-trust-preset-confirm").is_none());
    vcx.update(|_, cx| {
        let s = current(cx);
        assert!(s.auto_rename_branch_from_work);
        assert!(s.prompt_cache_timer_enabled);
        assert_eq!(s.prompt_cache_ttl_secs, 300);
        assert!(!s.workspace_trust_preset_enabled);
    });

    // 신뢰 프리셋 '켜기' 첫 클릭 시 바로 켜지지 않고 변경 파일·키 diff 확인 상자가 열린다.
    let trust_on_at = vcx
        .debug_bounds("workspace-trust-preset-on")
        .unwrap()
        .center();
    vcx.simulate_click(trust_on_at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("workspace-trust-preset-confirm").is_some());
    assert!(!krill_proto::git::is_workspace_trust_preset_enabled(Some(
        &d
    )));

    // '취소' 클릭 시 확인 상자가 닫히고 여전히 꺼짐 상태를 유지한다.
    let cancel_at = vcx
        .debug_bounds("workspace-trust-preset-confirm-cancel")
        .unwrap()
        .center();
    vcx.simulate_click(cancel_at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("workspace-trust-preset-confirm").is_none());
    assert!(!krill_proto::git::is_workspace_trust_preset_enabled(Some(
        &d
    )));

    // 다시 '켜기' → '동의 후 켜기' 클릭 시 비로소 활성화되고 플래그 파일이 생성된다.
    let trust_on_at = vcx
        .debug_bounds("workspace-trust-preset-on")
        .unwrap()
        .center();
    vcx.simulate_click(trust_on_at, gpui::Modifiers::default());
    vcx.run_until_parked();
    let confirm_yes_at = vcx
        .debug_bounds("workspace-trust-preset-confirm-yes")
        .unwrap()
        .center();
    vcx.simulate_click(confirm_yes_at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(krill_proto::git::is_workspace_trust_preset_enabled(Some(
        &d
    )));

    // '끄기'와 '1시간' 클릭 시 settings.toml 및 플래그 파일 반영 확인.
    let off_at = vcx.debug_bounds("auto-rename-branch-off").unwrap().center();
    vcx.simulate_click(off_at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(!krill_proto::git::is_auto_rename_enabled(Some(&d)));

    let ttl_1h_at = vcx.debug_bounds("prompt-cache-ttl-1h").unwrap().center();
    vcx.simulate_click(ttl_1h_at, gpui::Modifiers::default());
    vcx.run_until_parked();

    let saved = Settings::parse(&fs::read_to_string(d.join(SETTINGS_FILE)).unwrap()).unwrap();
    assert!(!saved.auto_rename_branch_from_work);
    assert_eq!(saved.prompt_cache_ttl_secs, 3_600);
    assert!(saved.workspace_trust_preset_enabled);

    // 신뢰 프리셋 '끄기' 클릭 시 즉시 비활성화되고 플래그 파일이 지워진다.
    let trust_off_at = vcx
        .debug_bounds("workspace-trust-preset-off")
        .unwrap()
        .center();
    vcx.simulate_click(trust_off_at, gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(!krill_proto::git::is_workspace_trust_preset_enabled(Some(
        &d
    )));
    fs::remove_dir_all(d).ok();
}

/// OWE-744: 가짜 `krill worktree space|cleanup --json` 실행기. 호출 인자를 기록하고
/// `space` → `report`, `cleanup --dry-run` → 현재 `candidates`, 그 밖의 `cleanup` → `summary`를 돌려주며
/// 실행 뒤에는 `after_cleanup` 후보로 바꾼다.
fn fake_space_cli(
    report: krill_proto::worktree_space::SpaceReport,
    candidates: Vec<krill_proto::worktree_space::WorktreeCleanupCandidate>,
    summary: krill_proto::worktree_space::CleanupSummary,
    after_cleanup: Vec<krill_proto::worktree_space::WorktreeCleanupCandidate>,
) -> (
    space::CliRunner,
    std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
) {
    use krill_proto::worktree_space::CleanupReport;
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
    let current = std::sync::Arc::new(std::sync::Mutex::new(candidates));
    let log = calls.clone();
    let run = space::CliRunner::new(move |args| {
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        log.lock().unwrap().push(args.clone());
        assert_eq!(args[0], "worktree");
        let json = if args[1] == "space" {
            serde_json::to_vec(&report)
        } else if args.iter().any(|a| a == "--dry-run") {
            serde_json::to_vec(&CleanupReport {
                dry_run: true,
                candidates: current.lock().unwrap().clone(),
                ..CleanupReport::default()
            })
        } else {
            *current.lock().unwrap() = after_cleanup.clone();
            serde_json::to_vec(&CleanupReport {
                summary: summary.clone(),
                ..CleanupReport::default()
            })
        };
        Ok(json.unwrap())
    });
    (run, calls)
}

fn fake_candidate(
    root: &std::path::Path,
    name: &str,
    pr_state: krill_proto::worktree_space::CleanupPrState,
    idle_days: u64,
    dirty: bool,
) -> krill_proto::worktree_space::WorktreeCleanupCandidate {
    krill_proto::worktree_space::WorktreeCleanupCandidate {
        repo: root.join("repo"),
        repo_name: "repo".into(),
        path: root.join(name),
        name: name.into(),
        branch: Some(format!("feat/{}", name.trim_start_matches("wt-"))),
        is_main: false,
        locked: false,
        dirty,
        dirty_count: usize::from(dirty),
        merged_into_main: false,
        pr_state,
        pr_number: None,
        last_active_secs: 0,
        idle_days,
        disk_bytes: 1024,
        artifact_bytes: 0,
    }
}

/// OWE-729 (MK-175): `Settings > Resources` 탭의 디스크 용량 분석(`Space`) 섹션이
/// `krill worktree space --json` 결과(워크트리별 전체 용량 및 `target`·`node_modules`·`.next` 아티팩트)를 그린다.
#[gpui::test]
fn owe729_resources_space_scan_and_artifact_breakdown(cx: &mut TestAppContext) {
    use krill_proto::worktree_space::{ArtifactUsage, SpaceReport, WorktreeSpaceEntry};

    let d = dir("owe729-space");
    init(cx, &d);
    let repo = d.join("repo");

    for q in ["클린업", "용량", "node_modules", "worktree cleanup"] {
        assert!(
            search::search_tabs(q).contains(&AdminTab::Resources),
            "search query {q} should match AdminTab::Resources"
        );
    }

    let art = |name: &str, bytes: u64| ArtifactUsage {
        name: name.into(),
        rel_path: name.into(),
        bytes,
        count: 1,
    };
    let entry = |name: &str, is_main: bool, artifacts: Vec<ArtifactUsage>| {
        let artifact_bytes = artifacts.iter().map(|a| a.bytes).sum::<u64>();
        WorktreeSpaceEntry {
            repo: repo.clone(),
            repo_name: "repo".into(),
            path: d.join(name),
            name: name.into(),
            branch: (!is_main).then(|| format!("feat/{name}")),
            is_main,
            total_bytes: artifact_bytes + 512,
            artifact_bytes,
            artifacts,
        }
    };
    let worktrees = vec![
        entry(
            "wt-heavy",
            false,
            vec![art("target", 128 * 1024), art("node_modules", 8 * 1024)],
        ),
        entry("wt-light", false, vec![art(".next", 4 * 1024)]),
        entry("repo", true, Vec::new()),
    ];
    let report = SpaceReport {
        scanned_at: 1,
        total_bytes: worktrees.iter().map(|w| w.total_bytes).sum(),
        total_artifact_bytes: worktrees.iter().map(|w| w.artifact_bytes).sum(),
        artifacts: vec![
            art("target", 128 * 1024),
            art("node_modules", 8 * 1024),
            art(".next", 4 * 1024),
        ],
        worktrees,
    };
    let (run, calls) = fake_space_cli(
        report,
        Vec::new(),
        krill_proto::worktree_space::CleanupSummary::default(),
        Vec::new(),
    );

    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1200.), px(2600.)));
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_space_repos(vec![repo.clone()], Some(run), cx);
            v.set_tab(AdminTab::Resources, cx);
        });
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));

    for selector in [
        "admin-space-section",
        "admin-space-summary",
        "admin-space-scan",
        "admin-worktree-cleanup-open",
        "admin-space-worktree-list",
        "admin-space-wt-wt-heavy",
        "admin-space-wt-wt-light",
        "admin-space-wt-repo",
        "admin-space-artifact-target",
        "admin-space-artifact-node_modules",
        "admin-space-artifact-.next",
    ] {
        assert!(vcx.debug_bounds(selector).is_some(), "missing {selector}");
    }
    assert!(vcx.debug_bounds("admin-space-error").is_none());

    let calls = calls.lock().unwrap().clone();
    let repo_arg = repo.to_string_lossy().into_owned();
    assert_eq!(
        calls[0],
        ["worktree", "space", "--repo", repo_arg.as_str(), "--json"]
    );
    assert!(calls[1].starts_with(&["worktree".into(), "cleanup".into()]));
    assert!(calls[1].iter().any(|a| a == "--dry-run"));
    vcx.update(|_, cx| {
        let report = view.read(cx).space.report.as_ref().unwrap();
        assert_eq!(report.worktrees.len(), 3);
        assert_eq!(report.worktrees[0].name, "wt-heavy");
    });

    fs::remove_dir_all(d).ok();
}

/// OWE-729 (MK-175): 워크트리 일괄 클린업(`Cleanup`) 모달의 필터(PR 상태, 경과 일수, dirty 제외)와
/// 선택 실행이 `krill worktree cleanup --worktree ... --json`으로 전달되고 요약·후보가 갱신되는지 검증.
#[gpui::test]
fn owe729_cleanup_modal_filters_and_bulk_delete_with_branch_safety(cx: &mut TestAppContext) {
    use krill_proto::worktree_space::{
        CleanupItemOutcome, CleanupPrState, CleanupSummary, SpaceReport,
    };

    let d = dir("owe729-cleanup");
    init(cx, &d);
    let repo = d.join("repo");

    let merged_old = fake_candidate(&d, "wt-merged-old", CleanupPrState::Merged, 20, false);
    let closed = fake_candidate(&d, "wt-closed-unmerged", CleanupPrState::Closed, 16, false);
    let dirty = fake_candidate(&d, "wt-dirty-merged", CleanupPrState::Merged, 25, true);
    let recent = fake_candidate(&d, "wt-recent-merged", CleanupPrState::Merged, 2, false);
    let outcome = |c: &krill_proto::worktree_space::WorktreeCleanupCandidate, deleted: bool| {
        CleanupItemOutcome {
            path: c.path.clone(),
            branch: c.branch.clone(),
            removed: true,
            branch_deleted: deleted,
            freed_bytes: c.disk_bytes,
            warnings: if deleted {
                Vec::new()
            } else {
                vec!["branch not merged; preserved".into()]
            },
            error: None,
        }
    };
    let summary = CleanupSummary {
        removed_count: 2,
        failed_count: 0,
        branches_deleted: 1,
        branches_preserved: 1,
        freed_bytes: 2048,
        items: vec![outcome(&merged_old, true), outcome(&closed, false)],
    };
    let (run, calls) = fake_space_cli(
        SpaceReport::default(),
        vec![
            merged_old.clone(),
            closed.clone(),
            dirty.clone(),
            recent.clone(),
        ],
        summary,
        vec![dirty.clone(), recent.clone()],
    );

    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1200.), px(2600.)));
    let click = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    };

    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_space_repos(vec![repo.clone()], Some(run), cx);
            v.set_tab(AdminTab::Resources, cx);
        });
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));

    click(vcx, "admin-worktree-cleanup-open");
    assert!(vcx.debug_bounds("admin-cleanup-modal").is_some());

    // 기본 필터(MergedOrClosed, 경과 일수 전체, dirty 제외): dirty 1개를 제외한 3개 일치
    vcx.update(|_, cx| {
        let matched = view.read(cx).space.filtered_candidates();
        assert_eq!(matched.len(), 3);
    });

    // 7일 이상 필터 클릭 -> wt-merged-old(20d), wt-closed-unmerged(16d) 2개 일치
    click(vcx, "admin-cleanup-days-7");
    vcx.update(|_, cx| {
        let matched = view.read(cx).space.filtered_candidates();
        assert_eq!(matched.len(), 2);
    });

    // Merged만 필터링 -> wt-merged-old 1개
    click(vcx, "admin-cleanup-pr-merged");
    vcx.update(|_, cx| {
        let matched = view.read(cx).space.filtered_candidates();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].branch.as_deref(), Some("feat/merged-old"));
    });

    // Closed만 필터링 -> wt-closed-unmerged 1개
    click(vcx, "admin-cleanup-pr-closed");
    vcx.update(|_, cx| {
        let matched = view.read(cx).space.filtered_candidates();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].branch.as_deref(), Some("feat/closed-unmerged"));
    });

    // 다시 MergedOrClosed + 30일 이상 -> 0개 일치
    click(vcx, "admin-cleanup-pr-both");
    click(vcx, "admin-cleanup-days-30");
    assert!(vcx.debug_bounds("admin-cleanup-empty").is_some());

    // 14일 이상 -> 20일·16일 된 두 워크트리 선택
    click(vcx, "admin-cleanup-days-14");
    vcx.update(|_, cx| {
        assert_eq!(view.read(cx).space.selected.len(), 2);
    });

    // 일괄 삭제 실행: 같은 필터와 선택 경로만 CLI로 넘긴다.
    click(vcx, "admin-cleanup-execute");
    assert!(vcx.debug_bounds("admin-cleanup-status").is_some());

    let exec = calls
        .lock()
        .unwrap()
        .iter()
        .find(|c| c[1] == "cleanup" && !c.iter().any(|a| a == "--dry-run"))
        .cloned()
        .expect("cleanup execution call");
    let pair = |flag: &str| {
        exec.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(pair("--pr"), ["merged-or-closed"]);
    assert_eq!(pair("--idle-days"), ["14"]);
    assert!(exec.iter().any(|a| a == "--exclude-dirty"));
    assert!(exec.iter().any(|a| a == "--json"));
    let mut targets = pair("--worktree");
    targets.sort();
    let mut want = vec![
        closed.path.to_string_lossy().into_owned(),
        merged_old.path.to_string_lossy().into_owned(),
    ];
    want.sort();
    assert_eq!(targets, want, "dirty·최근 워크트리는 넘기지 않는다");

    vcx.update(|_, cx| {
        let space = &view.read(cx).space;
        let summary = space.last_summary.as_ref().unwrap();
        assert_eq!(summary.removed_count, 2);
        assert_eq!(summary.branches_deleted, 1);
        assert_eq!(summary.branches_preserved, 1);
        assert_eq!(space.candidates.len(), 2, "실행 뒤 다시 스캔한 후보로 갱신");
        assert!(space.error.is_none());
    });

    fs::remove_dir_all(d).ok();
}

/// OWE-740 (MK-186): Workflows 탭 스케줄 추가/편집 모달(`precheck`·`workspace_mode`),
/// 데몬 경유 `Run Now`(오프라인 시 데몬 필요 오류), `skipped`/`finished` 배지,
/// `krilld` 격리 워크트리 생성 및 실행 상세 로그 뷰어 검증.
#[gpui::test]
fn workflows_schedule_modal_precheck_isolated_worktree_and_log_viewer(cx: &mut TestAppContext) {
    use krill_proto::schedule::{
        RUN_NOW, RUNS, ScheduleRun, Schedules, WorkspaceMode, load_coalesced_runs,
    };
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;
    use std::process::Command;
    use std::time::{Duration, UNIX_EPOCH};

    let d = std::path::PathBuf::from(format!("/tmp/mk-740-{}", std::process::id()));
    fs::remove_dir_all(&d).ok();
    let repo = d.join("repo");
    fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "test@krill.dev"][..],
        &["config", "user.name", "Krill Test"][..],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
    }
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    assert!(
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["commit", "-qm", "init"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );

    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(4000.)));

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Workflows, cx));
    });
    vcx.run_until_parked();

    let click_sel = |vcx: &mut gpui::VisualTestContext, sel: &'static str| {
        let b = vcx
            .debug_bounds(sel)
            .unwrap_or_else(|| panic!("missing selector {sel}"));
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };

    // 1. '+ 스케줄 추가' 클릭 → 모달 열림, 비-0 precheck 설정 후 저장
    assert!(vcx.debug_bounds("admin-schedules-empty").is_some());
    click_sel(vcx, "admin-schedule-add");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_some());

    let repo_str = repo.to_string_lossy().into_owned();
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_schedule_modal_values(
                "audit-job",
                "0 9 * * *",
                &repo_str,
                "/bin/sh -c",
                "echo SHOULD_NOT_RUN",
                "printf 'PRECHECK_SKIPPED_REASON\\n' && exit 2",
                WorkspaceMode::Reuse,
                window,
                cx,
            );
        });
    });
    vcx.run_until_parked();
    click_sel(vcx, "admin-schedule-modal-save");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_none());
    assert!(vcx.debug_bounds("admin-schedule-row-audit-job").is_some());

    // 2. 데몬 소켓이 없을 때 즉시 실행(Run Now) → 로컬 실행 없이 '데몬 연결 필요' 오류 배너 표시
    click_sel(vcx, "admin-schedule-run-now-audit-job");
    assert!(vcx.debug_bounds("admin-schedule-error").is_some());
    assert!(!d.join(RUN_NOW).exists());
    assert!(load_coalesced_runs(&d).unwrap().is_empty());

    // 3. 데몬 소켓 연결 가능 상태에서 Run Now → request_run_now 기록 후 krilld poll_schedules가 precheck 실행 및 skipped 기록
    let run_dir = d.join("run");
    fs::create_dir_all(&run_dir).unwrap();
    let _daemon_sock = UnixListener::bind(run_dir.join("krilld.sock")).unwrap();
    let registry = krilld::registry::Registry::new(None, Some(&d));

    click_sel(vcx, "admin-schedule-run-now-audit-job");
    assert!(vcx.debug_bounds("admin-schedule-error").is_none());
    assert!(d.join(RUN_NOW).exists());

    registry
        .poll_schedules(UNIX_EPOCH + Duration::from_mins(50))
        .unwrap();
    vcx.update(|_, cx| view.update(cx, |_, cx| cx.notify()));
    vcx.run_until_parked();

    let runs = load_coalesced_runs(&d).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].phase, "skipped");
    assert_eq!(runs[0].status, Some(2));
    assert_eq!(runs[0].output.as_deref(), Some("PRECHECK_SKIPPED_REASON"));
    assert_eq!(
        schedules::run_phase_badge_spec(&runs[0].phase).0,
        crate::ui::BadgeTone::Muted
    );
    assert!(vcx.debug_bounds("admin-schedule-run-badge-0").is_some());

    // skipped 실행 로그 열기·닫기 확인
    click_sel(vcx, "admin-schedule-run-log-0");
    assert!(vcx.debug_bounds("admin-schedule-log-viewer").is_some());
    vcx.update(|_, cx| {
        let log = view.read(cx).schedule_log().expect("skipped log open");
        assert_eq!(log.phase, "skipped");
        assert_eq!(log.output, "PRECHECK_SKIPPED_REASON");
    });
    click_sel(vcx, "admin-schedule-log-close");
    assert!(vcx.debug_bounds("admin-schedule-log-viewer").is_none());

    // 4. 편집 모달 열기 → Isolate 모드 버튼 클릭 + 통과하는 precheck 저장
    click_sel(vcx, "admin-schedule-edit-audit-job");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_some());
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_schedule_modal_values(
                "audit-job",
                "0 9 * * *",
                &repo_str,
                "/bin/sh -c",
                "printf 'ISOLATED_OUTPUT_OK:%s\\n' \"$(pwd)\"",
                "exit 0",
                WorkspaceMode::Reuse,
                window,
                cx,
            );
        });
    });
    vcx.run_until_parked();
    click_sel(vcx, "admin-schedule-mode-isolate");
    click_sel(vcx, "admin-schedule-modal-save");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_none());

    let saved = Schedules::load(&d).unwrap();
    assert_eq!(saved.schedule.len(), 1);
    assert_eq!(saved.schedule[0].workspace_mode, WorkspaceMode::Isolate);
    assert_eq!(saved.schedule[0].precheck.as_deref(), Some("exit 0"));

    // 5. 즉시 실행(Run Now) → krilld가 격리 워크트리 생성 후 실행 기록, 완료 로그 뷰어 확인
    click_sel(vcx, "admin-schedule-run-now-audit-job");
    registry
        .poll_schedules(UNIX_EPOCH + Duration::from_mins(51))
        .unwrap();

    let runs = load_coalesced_runs(&d).unwrap();
    assert_eq!(runs.len(), 2);
    let run_cwd = runs[0]
        .cwd
        .clone()
        .expect("krilld must create isolated worktree after passing precheck");
    assert_ne!(run_cwd, repo_str);
    assert!(std::path::Path::new(&run_cwd).join("README.md").is_file());

    // 세션 종료 후 캡처된 출력이 저널에 기록된 상태를 시뮬레이션해 로그 뷰어 열람 검증
    let finished_record = ScheduleRun {
        id: "audit-job".into(),
        minute: 51,
        phase: "finished".into(),
        session: Some(1),
        error: None,
        status: Some(0),
        cwd: Some(run_cwd.clone()),
        output: Some(format!("ISOLATED_OUTPUT_OK:{run_cwd}")),
    };
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(d.join(RUNS))
        .unwrap();
    writeln!(f, "{}", serde_json::to_string(&finished_record).unwrap()).unwrap();

    vcx.update(|_, cx| view.update(cx, |_, cx| cx.notify()));
    vcx.run_until_parked();

    let runs = load_coalesced_runs(&d).unwrap();
    let latest = &runs[0];
    assert_eq!(latest.phase, "finished");
    assert_eq!(
        schedules::run_phase_badge_spec(&latest.phase).0,
        crate::ui::BadgeTone::Success
    );

    click_sel(vcx, "admin-schedule-run-log-0");
    assert!(vcx.debug_bounds("admin-schedule-log-viewer").is_some());
    assert!(vcx.debug_bounds("admin-schedule-log-cwd").is_some());
    assert!(vcx.debug_bounds("admin-schedule-log-content").is_some());
    vcx.update(|_, cx| {
        let log = view.read(cx).schedule_log().expect("schedule log open");
        assert_eq!(log.id, "audit-job");
        assert_eq!(log.phase, "finished");
        assert_eq!(log.cwd.as_deref(), Some(run_cwd.as_str()));
        assert!(
            log.output.contains("ISOLATED_OUTPUT_OK:"),
            "unexpected output: {}",
            log.output
        );
    });

    fs::remove_dir_all(d).ok();
}

#[gpui::test]
fn one_click_setup_all_agents_and_skills_and_terminal_install_queue(cx: &mut TestAppContext) {
    let d = dir("one-click-setup-all");
    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(5000.)));
    let click = |vcx: &mut gpui::VisualTestContext, selector: &'static str| {
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };

    // 1. 에이전트 탭 진입(초기 agents.toml 캐시 로드를 위해 두 번 주차).
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Agents, cx));
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));

    // 개별 훅 버튼(cursor·opencode·claude·agy 포함)이 복사만 하지 않고 직접 훅 설정 파일을 설치하며,
    // codex는 터미널 자동 실행 큐에도 들어간다.
    for (selector, rel_path) in [
        ("admin-agent-hook-claude", ".claude/settings.json"),
        ("admin-agent-hook-agy", ".gemini/config/hooks.json"),
        ("admin-agent-hook-cursor", ".cursor/hooks.json"),
        (
            "admin-agent-hook-opencode",
            ".config/opencode/plugins/minikrill.js",
        ),
    ] {
        click(vcx, selector);
        assert!(
            d.join(rel_path).is_file(),
            "{selector} should install {rel_path}"
        );
    }
    click(vcx, "admin-agent-hook-codex");
    vcx.update(|_, cx| {
        assert_eq!(
            take_pending_terminal_command(cx).as_deref(),
            Some("krill hook install codex --yes")
        );
    });

    // 2. "모두 자동 설정" 버튼 클릭 시 에이전트 훅과 모든 내장 스킬이 한 번에 설치된다.
    //    스테이징된 `.local/share/minikrill/bin/krill`이 있으면 그 절대경로를 훅 설정에 기록한다.
    let d2 = dir("one-click-setup-all-btn");
    let staged_krill = d2.join(".local/share/minikrill/bin/krill");
    fs::create_dir_all(staged_krill.parent().unwrap()).unwrap();
    fs::write(&staged_krill, "#!/bin/sh\nexit 0\n").unwrap();
    vcx.update(|window, cx| {
        cx.global_mut::<Config>().dir = Some(d2.clone());
        window.draw(cx).clear(cx);
    });
    click(vcx, "admin-agents-setup-all");
    for rel_path in [
        ".claude/settings.json",
        ".gemini/config/hooks.json",
        ".cursor/hooks.json",
        ".config/opencode/plugins/minikrill.js",
        ".claude/skills/minikrill/SKILL.md",
        ".codex/skills/orchestration/SKILL.md",
        ".gemini/config/skills/worktree-checkpoints/SKILL.md",
        ".cursor/skills/minikrill/SKILL.md",
        ".config/opencode/skills/minikrill/SKILL.md",
    ] {
        assert!(
            d2.join(rel_path).is_file(),
            "admin-agents-setup-all should create {rel_path}"
        );
    }
    let claude_hook = fs::read_to_string(d2.join(".claude/settings.json")).unwrap();
    assert!(
        claude_hook.contains(&staged_krill.display().to_string()),
        "claude hook should record staged krill path: {claude_hook}"
    );

    // 3. 에이전트 탭의 "터미널에서 설치" 버튼 클릭 시 터미널 자동 실행 명령이 큐에 들어간다.
    click(vcx, "admin-agent-install-codex");
    vcx.update(|_, cx| {
        assert_eq!(
            take_pending_terminal_command(cx).as_deref(),
            Some("npm install -g @openai/codex")
        );
        assert_eq!(take_pending_terminal_command(cx), None);
    });

    // 4. 계정 탭의 미설치 공급자 카드에서도 "터미널에서 설치" 버튼이 터미널 자동 실행 명령을 큐에 넣는다.
    vcx.update(|window, cx| {
        cx.set_global(crate::accounts::installed::Installed(Some(Vec::new())));
        view.update(cx, |v, cx| {
            v.not_installed_open = true;
            v.set_tab(AdminTab::Accounts, cx);
        });
        window.draw(cx).clear(cx);
    });
    click(vcx, "admin-install-provider-claude");
    vcx.update(|_, cx| {
        assert_eq!(
            take_pending_terminal_command(cx).as_deref(),
            Some("curl -fsSL https://claude.ai/install.sh | bash")
        );
    });

    // 5. 여러 명령을 연속으로 큐에 넣어도 FIFO 순서대로 유실 없이 꺼내진다.
    vcx.update(|_, cx| {
        queue_terminal_command("echo first", cx);
        queue_terminal_command("echo second", cx);
        assert_eq!(
            take_pending_terminal_command(cx).as_deref(),
            Some("echo first")
        );
        assert_eq!(
            take_pending_terminal_command(cx).as_deref(),
            Some("echo second")
        );
        assert_eq!(take_pending_terminal_command(cx), None);
    });

    // 6. OpenCode 계정의 "+ 로그인" 호출 시 터미널 로그인 명령어가 자동으로 큐에 들어간다.
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.login_account(krill_proto::usage::ProviderKind::OpenCode, None, cx);
        });
        assert_eq!(
            take_pending_terminal_command(cx).as_deref(),
            Some("opencode auth login")
        );
        assert_eq!(take_pending_terminal_command(cx), None);
    });

    fs::remove_dir_all(d).ok();
    fs::remove_dir_all(d2).ok();
}

/// OWE-760: 스케줄 모달이 빈 id·cwd·잘못된 cron·빈/미종료 따옴표 command를 검증해 오류 배너를 표시하고
/// 모달을 닫지 않으며, 편집/저장 시 공백이 포함된 다중 단어 command 인자(`["bash", "-c", "cargo test --workspace"]`)를 보존한다.
#[gpui::test]
fn owe760_schedule_modal_validates_fields_and_preserves_quoted_command_args(
    cx: &mut TestAppContext,
) {
    use krill_proto::schedule::{Schedules, WorkspaceMode};

    let d = dir("owe760-schedule-modal");
    let repo = d.join("repo");
    fs::create_dir_all(&repo).unwrap();
    let repo_str = fs::canonicalize(&repo)
        .unwrap_or_else(|_| repo.clone())
        .display()
        .to_string();

    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(4000.)));

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Workflows, cx));
    });
    vcx.run_until_parked();

    let click_sel = |vcx: &mut gpui::VisualTestContext, sel: &'static str| {
        let b = vcx
            .debug_bounds(sel)
            .unwrap_or_else(|| panic!("missing selector {sel}"));
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };

    click_sel(vcx, "admin-schedule-add");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_some());

    // 1. 빈 id, 빈/상대 cwd, 잘못된 cron, 빈 command, 미종료 따옴표 command, 빈 prompt 검증
    for (id, cron, cwd, cmd, prompt) in [
        ("", "0 9 * * *", repo_str.as_str(), "claude -p", "check"),
        (
            "bad id",
            "0 9 * * *",
            repo_str.as_str(),
            "claude -p",
            "check",
        ),
        ("valid-id", "0 9 * * *", "", "claude -p", "check"),
        (
            "valid-id",
            "0 9 * * *",
            "relative/path",
            "claude -p",
            "check",
        ),
        (
            "valid-id",
            "invalid",
            repo_str.as_str(),
            "claude -p",
            "check",
        ),
        ("valid-id", "0 9 * * *", repo_str.as_str(), "", "check"),
        (
            "valid-id",
            "0 9 * * *",
            repo_str.as_str(),
            "bash -c 'unclosed",
            "check",
        ),
        (
            "valid-id",
            "0 9 * * *",
            repo_str.as_str(),
            "claude -p",
            "bad\0prompt",
        ),
    ] {
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.set_schedule_modal_values(
                    id,
                    cron,
                    cwd,
                    cmd,
                    prompt,
                    "",
                    WorkspaceMode::Reuse,
                    window,
                    cx,
                );
            });
        });
        vcx.run_until_parked();
        click_sel(vcx, "admin-schedule-modal-save");
        assert!(
            vcx.debug_bounds("admin-schedule-modal").is_some(),
            "modal should remain open for invalid input ({id:?}, {cron:?}, {cwd:?}, {cmd:?}, {prompt:?})"
        );
        assert!(vcx.debug_bounds("admin-schedule-modal-error").is_some());
        vcx.update(|_, cx| {
            assert!(view.read(cx).schedule_modal_error().is_some());
        });
        assert!(Schedules::load(&d).unwrap().schedule.is_empty());
    }

    // 2. 공백 포함 인자를 가진 스케줄 저장 및 편집 모달 왕복 보존 검증
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_schedule_modal_values(
                "multi-arg-job",
                "0 9 * * *",
                &repo_str,
                "bash -c 'cargo test --workspace'",
                "run suite",
                "",
                WorkspaceMode::Reuse,
                window,
                cx,
            );
        });
    });
    vcx.run_until_parked();
    click_sel(vcx, "admin-schedule-modal-save");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_none());

    let saved = Schedules::load(&d).unwrap();
    assert_eq!(saved.schedule.len(), 1);
    assert_eq!(
        saved.schedule[0].command,
        vec![
            "bash".to_owned(),
            "-c".to_owned(),
            "cargo test --workspace".to_owned()
        ]
    );

    // 편집 모달을 다시 열어 그대로 저장해도 공백 포함 인자가 쪼개지지 않고 보존된다.
    click_sel(vcx, "admin-schedule-edit-multi-arg-job");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_some());
    vcx.update(|_, cx| {
        let cmd_val = view
            .read(cx)
            .schedule_modal
            .as_ref()
            .unwrap()
            .command
            .read(cx)
            .value()
            .to_string();
        assert_eq!(cmd_val, "bash -c 'cargo test --workspace'");
    });
    click_sel(vcx, "admin-schedule-modal-save");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_none());

    let after_edit = Schedules::load(&d).unwrap();
    assert_eq!(
        after_edit.schedule[0].command,
        vec![
            "bash".to_owned(),
            "-c".to_owned(),
            "cargo test --workspace".to_owned()
        ]
    );

    // JSON 배열 형태 입력 및 작은따옴표가 포함된 인자 왕복 검증
    click_sel(vcx, "admin-schedule-edit-multi-arg-job");
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_schedule_modal_values(
                "multi-arg-job",
                "0 9 * * *",
                &repo_str,
                r#"["/bin/sh", "-c", "printf 'hello world\n' && cargo check"]"#,
                "run suite",
                "",
                WorkspaceMode::Reuse,
                window,
                cx,
            );
        });
    });
    vcx.run_until_parked();
    click_sel(vcx, "admin-schedule-modal-save");
    let after_json = Schedules::load(&d).unwrap();
    assert_eq!(
        after_json.schedule[0].command,
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "printf 'hello world\n' && cargo check".to_owned()
        ]
    );

    click_sel(vcx, "admin-schedule-edit-multi-arg-job");
    click_sel(vcx, "admin-schedule-modal-save");
    let after_roundtrip = Schedules::load(&d).unwrap();
    assert_eq!(
        after_roundtrip.schedule[0].command,
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "printf 'hello world\n' && cargo check".to_owned()
        ]
    );

    fs::remove_dir_all(d).ok();
}

/// OWE-761: Workflows 탭에서 저장소 경로(`admin-workflow-repo-input`)를 변경하면
/// 해당 저장소의 setup/archive 훅 입력창이 즉시 동기화되어 다른 저장소의 훅을 덮어쓰지 않는다.
#[gpui::test]
fn owe761_workflow_repo_path_change_syncs_hooks_without_clobbering_other_repo(
    cx: &mut TestAppContext,
) {
    let d = dir("owe761-repo-hooks-sync");
    let repo_a_dir = d.join("repo-a");
    let repo_b_dir = d.join("repo-b");
    fs::create_dir_all(&repo_a_dir).unwrap();
    fs::create_dir_all(&repo_b_dir).unwrap();
    let repo_a = fs::canonicalize(&repo_a_dir)
        .unwrap_or_else(|_| repo_a_dir.clone())
        .display()
        .to_string();
    let repo_b = fs::canonicalize(&repo_b_dir)
        .unwrap_or_else(|_| repo_b_dir.clone())
        .display()
        .to_string();

    init(cx, &d);
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(4000.)));

    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Workflows, cx);
            v.save_repo_hooks_for(
                &repo_a,
                Some("pnpm install"),
                Some("./scripts/cleanup-a.sh"),
                cx,
            )
            .unwrap();
            v.save_repo_hooks_for(
                &repo_b,
                Some("cargo fetch"),
                Some("./scripts/keep-b-archive.sh"),
                cx,
            )
            .unwrap();
            v.load_repo_hooks_for(&repo_a, window, cx);
        });
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();

    vcx.update(|_, cx| {
        let form = view.read(cx).workflow_form.as_ref().unwrap();
        assert_eq!(form.setup_script.read(cx).value().as_str(), "pnpm install");
        assert_eq!(
            form.archive_script.read(cx).value().as_str(),
            "./scripts/cleanup-a.sh"
        );
    });

    // 저장소 경로를 repo_b로 바꾸면 setup/archive 입력창이 repo_b의 기존 설정으로 자동 동기화된다.
    vcx.update(|window, cx| {
        let repo_input = view
            .read(cx)
            .workflow_form
            .as_ref()
            .unwrap()
            .repo_path
            .clone();
        repo_input.update(cx, |inp, cx| inp.set_value(repo_b.clone(), window, cx));
    });
    vcx.run_until_parked();

    vcx.update(|window, cx| {
        let form = view.read(cx).workflow_form.as_ref().unwrap();
        assert_eq!(form.setup_script.read(cx).value().as_str(), "cargo fetch");
        assert_eq!(
            form.archive_script.read(cx).value().as_str(),
            "./scripts/keep-b-archive.sh"
        );
        // setup_script만 수정
        let setup_inp = form.setup_script.clone();
        setup_inp.update(cx, |inp, cx| {
            inp.set_value("cargo check --workspace", window, cx);
        });
    });
    vcx.run_until_parked();

    let save_bounds = vcx
        .debug_bounds("admin-repo-hooks-save")
        .expect("admin-repo-hooks-save");
    vcx.simulate_click(save_bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let store = krill_proto::orchestration::WorktreeStore::load(&d).unwrap();
    assert_eq!(
        store.setup_script_for(&repo_b).as_deref(),
        Some("cargo check --workspace")
    );
    assert_eq!(
        store.archive_script_for(&repo_b).as_deref(),
        Some("./scripts/keep-b-archive.sh"),
        "repo B archive hook must not be overwritten by repo A's hook"
    );
    assert_eq!(
        store.setup_script_for(&repo_a).as_deref(),
        Some("pnpm install")
    );
    assert_eq!(
        store.archive_script_for(&repo_a).as_deref(),
        Some("./scripts/cleanup-a.sh")
    );

    fs::remove_dir_all(d).ok();
}

/// OWE-771: `settings::init_at`이 호출되지 않았거나 `None`인 단위 테스트에서 `SettingsView::new`·
/// `agent_history`·`resources` 기본 핸들러가 전역 데몬(`Paths::dev()`)을 기동하거나 오프라인으로 전환하지 않는다.
#[gpui::test]
fn owe771_uninitialized_settings_and_default_panels_never_probe_daemon(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        assert_eq!(
            state_dir(cx),
            None,
            "uninitialized settings::state_dir must not fall back to process daemon::state_dir in tests"
        );
    });
    let view = cx.new(SettingsView::new);
    cx.run_until_parked();
    view.update(cx, |v, _| {
        assert!(
            !v.accounts_offline,
            "uninitialized SettingsView in unit test must not probe daemon or enter offline mode"
        );
    });
    let hist_err =
        (crate::agent_history::daemon_call_for(None))(&krill_proto::control::Request::List)
            .unwrap_err();
    assert_eq!(hist_err.kind(), std::io::ErrorKind::NotConnected);
    let res_err = (crate::resources::daemon_call_for(None))(&krill_proto::control::Request::List)
        .unwrap_err();
    assert_eq!(res_err.kind(), std::io::ErrorKind::NotConnected);
}

/// OWE-776: 스케줄 추가/편집 모달에서 `전달 프롬프트 (선택)`을 비워 두어도 정상 저장·편집 왕복되며,
/// 필수 필드(ID·cwd·cron·command) 검증 실패 시 현재 언어에 맞는 다국어 오류 문구를 표시한다.
#[gpui::test]
fn owe776_schedule_modal_allows_empty_optional_prompt_and_localizes_validation_errors(
    cx: &mut TestAppContext,
) {
    use krill_proto::schedule::{Schedules, WorkspaceMode};

    let d = dir("owe776-optional-prompt");
    let alpha = d.join("alpha");
    fs::create_dir_all(&alpha).unwrap();
    let alpha_str = fs::canonicalize(&alpha)
        .unwrap_or_else(|_| alpha.clone())
        .display()
        .to_string();

    init(cx, &d);
    cx.update(|cx| crate::i18n::set_language(crate::i18n::Language::Ko, cx));
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    vcx.simulate_resize(size(px(1400.), px(4000.)));

    vcx.update(|_, cx| {
        view.update(cx, |v, cx| v.set_tab(AdminTab::Workflows, cx));
    });
    vcx.run_until_parked();

    let click_sel = |vcx: &mut gpui::VisualTestContext, sel: &'static str| {
        let b = vcx
            .debug_bounds(sel)
            .unwrap_or_else(|| panic!("missing selector {sel}"));
        vcx.simulate_click(b.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
    };

    click_sel(vcx, "admin-schedule-add");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_some());

    // 1. 필수 필드(빈 ID 등) 오류 시 한글 다국어 검증 문구 표시 확인
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_schedule_modal_values(
                "",
                "0 0 1 1 *",
                &alpha_str,
                r#"/bin/echo "two words""#,
                "",
                "",
                WorkspaceMode::Reuse,
                window,
                cx,
            );
        });
    });
    vcx.run_until_parked();
    click_sel(vcx, "admin-schedule-modal-save");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_some());
    vcx.update(|_, cx| {
        assert_eq!(
            view.read(cx).schedule_modal_error(),
            Some(
                crate::i18n::catalog::settings::SCHEDULE_ERR_ID
                    .in_language(crate::i18n::Language::Ko)
            )
        );
    });

    // 2. QA 재현 시나리오: ID=qa-schedule, cron=0 0 1 1 *, cwd=.../alpha,
    //    command=/bin/echo "two words", prompt=""(비움) → 저장 성공 및 모달 닫힘
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_schedule_modal_values(
                "qa-schedule",
                "0 0 1 1 *",
                &alpha_str,
                r#"/bin/echo "two words""#,
                "",
                "",
                WorkspaceMode::Reuse,
                window,
                cx,
            );
        });
    });
    vcx.run_until_parked();
    click_sel(vcx, "admin-schedule-modal-save");
    assert!(
        vcx.debug_bounds("admin-schedule-modal").is_none(),
        "schedule modal should close when optional prompt is left empty"
    );
    assert!(vcx.debug_bounds("admin-schedule-error").is_none());
    assert!(vcx.debug_bounds("admin-schedule-row-qa-schedule").is_some());

    let saved = Schedules::load(&d).unwrap();
    assert_eq!(saved.schedule.len(), 1);
    assert_eq!(saved.schedule[0].id, "qa-schedule");
    assert_eq!(saved.schedule[0].cron, "0 0 1 1 *");
    assert_eq!(saved.schedule[0].cwd, alpha_str);
    assert_eq!(
        saved.schedule[0].command,
        vec!["/bin/echo".to_owned(), "two words".to_owned()]
    );
    assert_eq!(saved.schedule[0].prompt, "");

    // 3. 편집 모달을 다시 열어도 빈 프롬프트와 공백 포함 인자가 그대로 유지되고 재저장된다.
    click_sel(vcx, "admin-schedule-edit-qa-schedule");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_some());
    vcx.update(|_, cx| {
        let modal = view.read(cx).schedule_modal.as_ref().unwrap();
        assert_eq!(modal.prompt.read(cx).value().as_str(), "");
        assert_eq!(
            modal.command.read(cx).value().as_str(),
            "/bin/echo 'two words'"
        );
    });
    click_sel(vcx, "admin-schedule-modal-save");
    assert!(vcx.debug_bounds("admin-schedule-modal").is_none());
    let after_edit = Schedules::load(&d).unwrap();
    assert_eq!(after_edit.schedule[0].prompt, "");
    assert_eq!(
        after_edit.schedule[0].command,
        vec!["/bin/echo".to_owned(), "two words".to_owned()]
    );

    fs::remove_dir_all(d).ok();
}
