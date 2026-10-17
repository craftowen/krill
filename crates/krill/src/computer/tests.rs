use std::ffi::OsString;
use std::process::ExitCode;

use super::engine::{
    AppSummary, ElementNode, MockApp, MockEnvironment, MockWindow, PermissionId, Point, Rect,
    build_permission_capabilities, index_and_render_tree, matches_app_selector,
    mutate_element_by_ref, mutate_focused_element, normalize_key_name, normalize_modifier_name,
    parse_hotkey_chord, verify_not_self_targeted,
};
use super::run;

fn sample_ax_tree() -> ElementNode {
    ElementNode {
        reference_id: None,
        role: "AXWindow".to_owned(),
        subrole: Some("AXStandardWindow".to_owned()),
        title: Some("Calculator".to_owned()),
        description: None,
        value: None,
        bounds: Some(Rect {
            x: 100.0,
            y: 100.0,
            width: 400.0,
            height: 300.0,
        }),
        enabled: true,
        focused: false,
        selected: false,
        actions: Vec::new(),
        children: vec![
            ElementNode {
                reference_id: None,
                role: "AXTextField".to_owned(),
                subrole: None,
                title: Some("Query".to_owned()),
                description: None,
                value: Some("initial".to_owned()),
                bounds: Some(Rect {
                    x: 120.0,
                    y: 130.0,
                    width: 200.0,
                    height: 30.0,
                }),
                enabled: true,
                focused: true,
                selected: false,
                actions: vec!["AXConfirm".to_owned()],
                children: Vec::new(),
            },
            ElementNode {
                reference_id: None,
                role: "AXButton".to_owned(),
                subrole: None,
                title: Some("Submit".to_owned()),
                description: None,
                value: None,
                bounds: Some(Rect {
                    x: 330.0,
                    y: 130.0,
                    width: 60.0,
                    height: 30.0,
                }),
                enabled: true,
                focused: false,
                selected: false,
                actions: vec!["AXPress".to_owned(), "AXShowMenu".to_owned()],
                children: Vec::new(),
            },
        ],
    }
}

#[test]
fn key_and_hotkey_normalization_handles_standard_chords() {
    assert_eq!(normalize_key_name("Enter").unwrap(), "return");
    assert_eq!(normalize_key_name("ESC").unwrap(), "escape");
    assert_eq!(normalize_key_name("ArrowDown").unwrap(), "down");
    assert_eq!(normalize_key_name("K").unwrap(), "k");
    assert!(normalize_key_name("").is_err());
    assert!(normalize_key_name("NotAKey").is_err());

    assert_eq!(normalize_modifier_name("cmd").unwrap(), "command");
    assert_eq!(normalize_modifier_name("opt").unwrap(), "option");
    assert_eq!(normalize_modifier_name("ctrl").unwrap(), "control");
    assert_eq!(normalize_modifier_name("shift").unwrap(), "shift");
    assert!(normalize_modifier_name("hyper").is_err());

    let (mods, key) = parse_hotkey_chord("Cmd+Shift+P").unwrap();
    assert_eq!(mods, vec!["command".to_owned(), "shift".to_owned()]);
    assert_eq!(key, "p");
    assert!(parse_hotkey_chord("P").is_err());
}

#[test]
fn ax_tree_indexing_assigns_sequential_reference_ids_and_mutates_values() {
    let mut root = sample_ax_tree();
    let (flat, focused_id, total_count, tree_text) = index_and_render_tree(&mut root);
    assert_eq!(total_count, 3);
    assert_eq!(flat.len(), 2);
    assert_eq!(focused_id, Some(1));
    assert!(tree_text.contains("[#1] AXTextField \"Query\" value=\"initial\""));
    assert!(tree_text.contains("[#2] AXButton \"Submit\""));

    let (bounds, enabled) =
        mutate_element_by_ref(&mut root, 1, Some("replaced"), Some("!"), true).unwrap();
    assert!(enabled);
    assert_eq!(bounds.unwrap().center(), Point { x: 220.0, y: 145.0 });
    let (flat_after, _, _, _) = index_and_render_tree(&mut root);
    assert_eq!(flat_after[0].value.as_deref(), Some("replaced!"));

    let (focused_ref, _) = mutate_focused_element(&mut root, None, Some("?")).unwrap();
    assert_eq!(focused_ref, Some(1));
    let (flat_final, _, _, _) = index_and_render_tree(&mut root);
    assert_eq!(flat_final[0].value.as_deref(), Some("replaced!?"));
}

#[test]
fn permission_capabilities_and_self_target_guard_behave_as_expected() {
    assert_eq!(
        PermissionId::parse("accessibility"),
        Some(PermissionId::Accessibility)
    );
    assert_eq!(
        PermissionId::parse("screenshots"),
        Some(PermissionId::Screenshots)
    );
    assert_eq!(PermissionId::parse("invalid"), None);

    let caps = build_permission_capabilities(true, false);
    assert_eq!(caps.len(), 2);
    assert!(caps[0].granted);
    assert!(!caps[1].granted);
    assert_eq!(caps[1].status, "denied");

    assert!(verify_not_self_targeted("Krill", Some("dev.krill.app")).is_err());
    assert!(verify_not_self_targeted("Calculator", Some("com.apple.calculator")).is_ok());

    let summary = AppSummary {
        name: "Calculator".to_owned(),
        bundle_id: Some("com.apple.calculator".to_owned()),
        pid: 4242,
        is_active: true,
        is_hidden: false,
    };
    assert!(matches_app_selector(&summary, "Calculator"));
    assert!(matches_app_selector(&summary, "com.apple.calculator"));
    assert!(matches_app_selector(&summary, "pid:4242"));
    assert!(!matches_app_selector(&summary, "Safari"));
}

#[test]
fn cli_usage_and_validation_errors_return_exit_code_2() {
    let args = |items: &[&str]| -> Vec<OsString> { items.iter().map(OsString::from).collect() };
    assert_eq!(run(&args(&[])), ExitCode::from(2));
    assert_eq!(run(&args(&["--help"])), ExitCode::SUCCESS);
    assert_eq!(run(&args(&["unknown-sub"])), ExitCode::from(2));
    assert_eq!(run(&args(&["list-windows"])), ExitCode::from(2));
    assert_eq!(run(&args(&["get-app-state"])), ExitCode::from(2));
    assert_eq!(
        run(&args(&[
            "get-app-state",
            "--app",
            "Calc",
            "--window-id",
            "1",
            "--window-index",
            "1"
        ])),
        ExitCode::from(2)
    );
    assert_eq!(run(&args(&["click"])), ExitCode::from(2));
    assert_eq!(run(&args(&["scroll"])), ExitCode::from(2));
    assert_eq!(run(&args(&["drag"])), ExitCode::from(2));
    assert_eq!(run(&args(&["type-text"])), ExitCode::from(2));
    assert_eq!(run(&args(&["press-key"])), ExitCode::from(2));
    assert_eq!(run(&args(&["hotkey", "--keys", "P"])), ExitCode::from(2));
    assert_eq!(run(&args(&["paste-text"])), ExitCode::from(2));
    assert_eq!(
        run(&args(&["set-value", "--element", "1"])),
        ExitCode::from(2)
    );
    assert_eq!(
        run(&args(&["permissions", "--id", "bogus"])),
        ExitCode::from(2)
    );

    let mock = MockEnvironment {
        accessibility_granted: true,
        screenshots_granted: true,
        clipboard: "saved".to_owned(),
        clipboard_restore_count: 0,
        apps: vec![MockApp {
            name: "Calculator".to_owned(),
            bundle_id: Some("com.apple.calculator".to_owned()),
            pid: 4242,
            is_active: true,
            is_hidden: false,
            windows: vec![MockWindow {
                id: "101".to_owned(),
                title: "Calculator".to_owned(),
                bounds: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 400.0,
                    height: 300.0,
                },
                is_focused: true,
                is_main: true,
                is_minimized: false,
                root: sample_ax_tree(),
            }],
        }],
        last_action: None,
    };
    assert_eq!(mock.apps.len(), 1);
}

#[test]
fn element_ref_parsing_and_bounded_stdin_enforced() {
    use super::actions::read_bounded_text;
    use super::parse_element_ref;
    use krill_proto::input::MAX_INPUT;

    assert_eq!(parse_element_ref("1", "--ref").unwrap(), 1);
    assert_eq!(parse_element_ref("@e1", "--ref").unwrap(), 1);
    assert_eq!(parse_element_ref("@E42", "--ref").unwrap(), 42);
    assert_eq!(parse_element_ref("e7", "--ref").unwrap(), 7);
    assert_eq!(parse_element_ref("#9", "--ref").unwrap(), 9);
    assert!(parse_element_ref("0", "--ref").is_err());
    assert!(parse_element_ref("bogus", "--ref").is_err());

    let exact = vec![b'a'; MAX_INPUT];
    let ok_text = read_bounded_text(std::io::Cursor::new(exact), "type-text").unwrap();
    assert_eq!(ok_text.len(), MAX_INPUT);

    // Infinite stream must terminate immediately at MAX_INPUT + 1 bytes without OOM
    let err = read_bounded_text(std::io::repeat(b'x'), "type-text").unwrap_err();
    match err {
        super::CliError::Runtime(msg) => assert!(msg.contains("exceeds maximum length"), "{msg}"),
        super::CliError::Usage(msg) => panic!("expected runtime error, got usage: {msg}"),
    }
}

#[test]
fn live_action_payload_includes_resolved_pid_window_and_keycodes() {
    use super::engine::{LiveAction, WindowSummary, build_live_action_payload};
    use super::model::{key_to_macos_keycode, modifiers_to_cg_flags};

    assert_eq!(key_to_macos_keycode("return"), Some(36));
    assert_eq!(key_to_macos_keycode("escape"), Some(53));
    assert_eq!(key_to_macos_keycode("a"), None);
    assert_eq!(
        modifiers_to_cg_flags(&["command".to_owned(), "shift".to_owned()]),
        0x0010_0000 | 0x0002_0000
    );

    let app = AppSummary {
        name: "TextEdit".to_owned(),
        bundle_id: Some("com.apple.TextEdit".to_owned()),
        pid: 7777,
        is_active: true,
        is_hidden: false,
    };
    let win = WindowSummary {
        id: "502".to_owned(),
        index: 2,
        title: "Second Document".to_owned(),
        app_name: "TextEdit".to_owned(),
        bundle_id: Some("com.apple.TextEdit".to_owned()),
        pid: 7777,
        bounds: Rect {
            x: 50.0,
            y: 60.0,
            width: 600.0,
            height: 400.0,
        },
        is_focused: false,
        is_main: false,
        is_minimized: false,
    };

    let click_payload = build_live_action_payload(
        &app,
        Some(&win),
        &LiveAction::Click {
            target_ref: Some(1),
            point: Some(Point { x: 100.0, y: 120.0 }),
            button: "left".to_owned(),
            click_count: 1,
            modifiers: vec!["command".to_owned()],
        },
    );
    assert_eq!(click_payload["kind"], "click");
    assert_eq!(click_payload["pid"], 7777);
    assert_eq!(click_payload["appName"], "TextEdit");
    assert_eq!(click_payload["winId"], "502");
    assert_eq!(click_payload["winIndex"], 2);
    assert_eq!(click_payload["winTitle"], "Second Document");
    assert_eq!(click_payload["targetRef"], 1);
    assert_eq!(click_payload["cgFlags"], 0x0010_0000);

    let key_payload = build_live_action_payload(
        &app,
        Some(&win),
        &LiveAction::PressKey {
            key: "return".to_owned(),
            modifiers: vec!["shift".to_owned()],
            repeat: 2,
        },
    );
    assert_eq!(key_payload["kind"], "pressKey");
    assert_eq!(key_payload["keyCode"], 36);
    assert_eq!(key_payload["usingMods"][0], "shift down");
    assert_eq!(key_payload["repeat"], 2);
}
