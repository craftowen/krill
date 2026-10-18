// Portions of the computer-use action subcommands and verification metadata are adapted
// from Orca (.ref/orca/src/cli/handlers/computer.ts, .ref/orca/native/computer-use-macos/):
// Copyright (c) 2026 Lovecast Inc. Licensed under the MIT License (.ref/orca/LICENSE).

//! `krill computer` 포인터·키보드·클립보드·값 설정 액션 서브커맨드 구현(MK-187 / OWE-741).

use std::io::Read as _;

use krill_proto::input::MAX_INPUT;

use super::engine::{
    ActionResultPayload, AppSummary, ClipboardMetadata, LiveAction, LiveActionOutcome, Point,
    VerificationMetadata, WindowSummary, execute_live_action, index_and_render_tree, load_mock_env,
    matches_app_selector, mutate_element_by_ref, mutate_focused_element, normalize_key_name,
    normalize_modifier_name, parse_hotkey_chord, read_system_clipboard, save_mock_env,
    write_system_clipboard,
};
use super::{
    CliError, CommonWindowOpts, ensure_current_snapshot, parse_element_ref, parse_f64, parse_usize,
    print_action_result, take_val, try_parse_common_opt,
};

fn verification_from_outcome(outcome: &LiveActionOutcome) -> VerificationMetadata {
    VerificationMetadata {
        status: outcome.status.clone(),
        reason: outcome.reason.clone(),
        retried: false,
    }
}

pub(super) fn cmd_click(args: &[String]) -> Result<(), CliError> {
    let mut common = CommonWindowOpts::default();
    let mut element: Option<usize> = None;
    let mut x: Option<f64> = None;
    let mut y: Option<f64> = None;
    let mut button = "left".to_owned();
    let mut click_count = 1usize;
    let mut modifiers: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if try_parse_common_opt(args, &mut i, &mut common)? {
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--element" | "--element-index" | "--ref" => {
                let raw = take_val(args, &mut i, "--element")?;
                element = Some(parse_element_ref(&raw, "--element")?);
            }
            "--x" => {
                let raw = take_val(args, &mut i, "--x")?;
                x = Some(parse_f64(&raw, "--x")?);
            }
            "--y" => {
                let raw = take_val(args, &mut i, "--y")?;
                y = Some(parse_f64(&raw, "--y")?);
            }
            "--button" | "--mouse-button" => {
                let raw = take_val(args, &mut i, "--button")?.to_ascii_lowercase();
                if !matches!(raw.as_str(), "left" | "right" | "middle") {
                    return Err(CliError::Usage(format!(
                        "invalid --button '{raw}' (expected left, right, or middle)"
                    )));
                }
                button = raw;
            }
            "--click-count" => {
                let raw = take_val(args, &mut i, "--click-count")?;
                click_count = parse_usize(&raw, "--click-count")?;
                if click_count == 0 {
                    return Err(CliError::Usage("--click-count must be >= 1".to_owned()));
                }
            }
            "--modifiers" => {
                let raw = take_val(args, &mut i, "--modifiers")?;
                for part in raw
                    .split([',', '+'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let norm = normalize_modifier_name(part)
                        .map_err(CliError::Usage)?
                        .to_owned();
                    if !modifiers.contains(&norm) {
                        modifiers.push(norm);
                    }
                }
            }
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    if element.is_none() && (x.is_none() || y.is_none()) {
        return Err(CliError::Usage(
            "click requires --element <n> or both --x <num> and --y <num>".to_owned(),
        ));
    }
    let before = ensure_current_snapshot(&common)?;
    let selector = common.app.as_deref().unwrap_or(&before.app.name);
    let (target_point, outcome, bounds_unverified) = if let Some(ref_id) = element {
        let Some(found) = before.elements.iter().find(|e| e.reference_id == ref_id) else {
            return Err(CliError::Runtime(format!(
                "element #{ref_id} not found in snapshot"
            )));
        };
        if !found.enabled {
            return Err(CliError::Runtime(format!("element #{ref_id} is disabled")));
        }
        let pt = found.bounds.map(super::model::Rect::center);
        let out = apply_mock_or_live_element_mutation(
            selector,
            Some(&before.app),
            before.window.as_ref(),
            ref_id,
            None,
            None,
            true,
            &format!("click:{button}:{click_count}:#{ref_id}"),
            &LiveAction::Click {
                target_ref: Some(ref_id),
                point: pt,
                button: button.clone(),
                click_count,
                modifiers: modifiers.clone(),
            },
        )?;
        (pt, out, false)
    } else {
        let pt = Point {
            x: x.unwrap_or(0.0),
            y: y.unwrap_or(0.0),
        };
        let out_of_bounds = before
            .window
            .as_ref()
            .is_some_and(|w| !w.bounds.contains(pt));
        let out = record_mock_or_live_action(
            selector,
            Some(&before.app),
            before.window.as_ref(),
            &format!("click:{button}:{click_count}:{:.1},{:.1}", pt.x, pt.y),
            &LiveAction::Click {
                target_ref: None,
                point: Some(pt),
                button: button.clone(),
                click_count,
                modifiers: modifiers.clone(),
            },
        )?;
        (Some(pt), out, out_of_bounds)
    };
    let verification = if !outcome.ok || outcome.status != "verified" {
        verification_from_outcome(&outcome)
    } else if bounds_unverified {
        VerificationMetadata {
            status: "unverified".to_owned(),
            reason: Some("target coordinates are outside window bounds".to_owned()),
            retried: true,
        }
    } else {
        verification_from_outcome(&outcome)
    };
    let after = ensure_current_snapshot(&common)?;
    let mut notes = vec![format!("button={button} count={click_count}")];
    if let Some(note) = outcome.note {
        notes.push(note);
    }
    let res = ActionResultPayload {
        action: "click".to_owned(),
        ok: outcome.ok,
        verification,
        target_reference_id: element,
        target_point,
        app_state: Some(after),
        clipboard: None,
        notes,
    };
    print_action_result(&res, common.json)
}

pub(super) fn cmd_secondary_action(args: &[String]) -> Result<(), CliError> {
    let mut common = CommonWindowOpts::default();
    let mut element: Option<usize> = None;
    let mut action_name = "AXShowMenu".to_owned();
    let mut i = 0;
    while i < args.len() {
        if try_parse_common_opt(args, &mut i, &mut common)? {
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--element" | "--element-index" | "--ref" => {
                let raw = take_val(args, &mut i, "--element")?;
                element = Some(parse_element_ref(&raw, "--element")?);
            }
            "--action" => action_name = take_val(args, &mut i, "--action")?,
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    let ref_id = element
        .ok_or_else(|| CliError::Usage("perform-secondary-action requires --element <n>".into()))?;
    let before = ensure_current_snapshot(&common)?;
    let Some(found) = before.elements.iter().find(|e| e.reference_id == ref_id) else {
        return Err(CliError::Runtime(format!(
            "element #{ref_id} not found in snapshot"
        )));
    };
    if !found.enabled {
        return Err(CliError::Runtime(format!("element #{ref_id} is disabled")));
    }
    let pt = found.bounds.map(super::model::Rect::center);
    let selector = common.app.as_deref().unwrap_or(&before.app.name);
    let outcome = apply_mock_or_live_element_mutation(
        selector,
        Some(&before.app),
        before.window.as_ref(),
        ref_id,
        None,
        None,
        true,
        &format!("secondary:{action_name}:#{ref_id}"),
        &LiveAction::SecondaryAction {
            target_ref: ref_id,
            point: pt,
            action_name: action_name.clone(),
        },
    )?;
    let after = ensure_current_snapshot(&common)?;
    let mut notes = vec![format!("axAction={action_name}")];
    if let Some(note) = outcome.note.clone() {
        notes.push(note);
    }
    let res = ActionResultPayload {
        action: "performSecondaryAction".to_owned(),
        ok: outcome.ok,
        verification: verification_from_outcome(&outcome),
        target_reference_id: Some(ref_id),
        target_point: pt,
        app_state: Some(after),
        clipboard: None,
        notes,
    };
    print_action_result(&res, common.json)
}

pub(super) fn cmd_scroll(args: &[String]) -> Result<(), CliError> {
    let mut common = CommonWindowOpts::default();
    let mut element: Option<usize> = None;
    let mut x: Option<f64> = None;
    let mut y: Option<f64> = None;
    let mut delta_x = 0.0f64;
    let mut delta_y = 0.0f64;
    let mut direction: Option<String> = None;
    let mut pages = 1.0f64;
    let mut i = 0;
    while i < args.len() {
        if try_parse_common_opt(args, &mut i, &mut common)? {
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--element" | "--element-index" | "--ref" => {
                let raw = take_val(args, &mut i, "--element")?;
                element = Some(parse_element_ref(&raw, "--element")?);
            }
            "--x" => {
                let raw = take_val(args, &mut i, "--x")?;
                x = Some(parse_f64(&raw, "--x")?);
            }
            "--y" => {
                let raw = take_val(args, &mut i, "--y")?;
                y = Some(parse_f64(&raw, "--y")?);
            }
            "--delta-x" | "--dx" => {
                let raw = take_val(args, &mut i, "--delta-x")?;
                delta_x = parse_f64(&raw, "--delta-x")?;
            }
            "--delta-y" | "--dy" => {
                let raw = take_val(args, &mut i, "--delta-y")?;
                delta_y = parse_f64(&raw, "--delta-y")?;
            }
            "--direction" => {
                direction = Some(take_val(args, &mut i, "--direction")?.to_ascii_lowercase());
            }
            "--pages" => {
                let raw = take_val(args, &mut i, "--pages")?;
                pages = parse_f64(&raw, "--pages")?;
            }
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    if let Some(dir) = &direction {
        let step = 120.0 * pages;
        match dir.as_str() {
            "up" => delta_y = step,
            "down" => delta_y = -step,
            "left" => delta_x = step,
            "right" => delta_x = -step,
            other => {
                return Err(CliError::Usage(format!(
                    "invalid --direction '{other}' (expected up, down, left, or right)"
                )));
            }
        }
    }
    if delta_x == 0.0 && delta_y == 0.0 {
        return Err(CliError::Usage(
            "scroll requires non-zero --delta-x/--delta-y or --direction".to_owned(),
        ));
    }
    let before = ensure_current_snapshot(&common)?;
    let target_point = if let Some(ref_id) = element {
        let Some(found) = before.elements.iter().find(|e| e.reference_id == ref_id) else {
            return Err(CliError::Runtime(format!(
                "element #{ref_id} not found in snapshot"
            )));
        };
        found.bounds.map(super::model::Rect::center)
    } else if let (Some(px), Some(py)) = (x, y) {
        Some(Point { x: px, y: py })
    } else {
        before.window.as_ref().map(|w| w.bounds.center())
    };
    let selector = common.app.as_deref().unwrap_or(&before.app.name);
    let outcome = record_mock_or_live_action(
        selector,
        Some(&before.app),
        before.window.as_ref(),
        &format!("scroll:{delta_x:.1},{delta_y:.1}"),
        &LiveAction::Scroll {
            point: target_point,
            delta_x,
            delta_y,
        },
    )?;
    let after = ensure_current_snapshot(&common)?;
    let mut notes = vec![format!("delta=({delta_x:.1}, {delta_y:.1})")];
    if let Some(note) = outcome.note.clone() {
        notes.push(note);
    }
    let res = ActionResultPayload {
        action: "scroll".to_owned(),
        ok: outcome.ok,
        verification: verification_from_outcome(&outcome),
        target_reference_id: element,
        target_point,
        app_state: Some(after),
        clipboard: None,
        notes,
    };
    print_action_result(&res, common.json)
}

pub(super) fn cmd_drag(args: &[String]) -> Result<(), CliError> {
    let mut common = CommonWindowOpts::default();
    let mut from_element: Option<usize> = None;
    let mut to_element: Option<usize> = None;
    let mut from_x: Option<f64> = None;
    let mut from_y: Option<f64> = None;
    let mut to_x: Option<f64> = None;
    let mut to_y: Option<f64> = None;
    let mut i = 0;
    while i < args.len() {
        if try_parse_common_opt(args, &mut i, &mut common)? {
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--from-element" | "--from-element-index" | "--from-ref" => {
                let raw = take_val(args, &mut i, "--from-element")?;
                from_element = Some(parse_element_ref(&raw, "--from-element")?);
            }
            "--to-element" | "--to-element-index" | "--to-ref" => {
                let raw = take_val(args, &mut i, "--to-element")?;
                to_element = Some(parse_element_ref(&raw, "--to-element")?);
            }
            "--from-x" => {
                let raw = take_val(args, &mut i, "--from-x")?;
                from_x = Some(parse_f64(&raw, "--from-x")?);
            }
            "--from-y" => {
                let raw = take_val(args, &mut i, "--from-y")?;
                from_y = Some(parse_f64(&raw, "--from-y")?);
            }
            "--to-x" => {
                let raw = take_val(args, &mut i, "--to-x")?;
                to_x = Some(parse_f64(&raw, "--to-x")?);
            }
            "--to-y" => {
                let raw = take_val(args, &mut i, "--to-y")?;
                to_y = Some(parse_f64(&raw, "--to-y")?);
            }
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    if from_element.is_none() && (from_x.is_none() || from_y.is_none()) {
        return Err(CliError::Usage(
            "drag requires --from-element <n> or --from-x/--from-y".to_owned(),
        ));
    }
    if to_element.is_none() && (to_x.is_none() || to_y.is_none()) {
        return Err(CliError::Usage(
            "drag requires --to-element <n> or --to-x/--to-y".to_owned(),
        ));
    }
    let before = ensure_current_snapshot(&common)?;
    let start_pt = if let Some(ref_id) = from_element {
        before
            .elements
            .iter()
            .find(|e| e.reference_id == ref_id)
            .and_then(|e| e.bounds.map(super::model::Rect::center))
            .ok_or_else(|| {
                CliError::Runtime(format!("from-element #{ref_id} not found or lacks bounds"))
            })?
    } else {
        Point {
            x: from_x.unwrap_or(0.0),
            y: from_y.unwrap_or(0.0),
        }
    };
    let end_pt = if let Some(ref_id) = to_element {
        before
            .elements
            .iter()
            .find(|e| e.reference_id == ref_id)
            .and_then(|e| e.bounds.map(super::model::Rect::center))
            .ok_or_else(|| {
                CliError::Runtime(format!("to-element #{ref_id} not found or lacks bounds"))
            })?
    } else {
        Point {
            x: to_x.unwrap_or(0.0),
            y: to_y.unwrap_or(0.0),
        }
    };
    let selector = common.app.as_deref().unwrap_or(&before.app.name);
    let outcome = record_mock_or_live_action(
        selector,
        Some(&before.app),
        before.window.as_ref(),
        &format!(
            "drag:{:.1},{:.1}->{:.1},{:.1}",
            start_pt.x, start_pt.y, end_pt.x, end_pt.y
        ),
        &LiveAction::Drag {
            from: start_pt,
            to: end_pt,
        },
    )?;
    let after = ensure_current_snapshot(&common)?;
    let mut notes = vec![format!(
        "from=({:.1},{:.1}) to=({:.1},{:.1})",
        start_pt.x, start_pt.y, end_pt.x, end_pt.y
    )];
    if let Some(note) = outcome.note.clone() {
        notes.push(note);
    }
    let res = ActionResultPayload {
        action: "drag".to_owned(),
        ok: outcome.ok,
        verification: verification_from_outcome(&outcome),
        target_reference_id: to_element.or(from_element),
        target_point: Some(end_pt),
        app_state: Some(after),
        clipboard: None,
        notes,
    };
    print_action_result(&res, common.json)
}

pub(super) fn cmd_type_text(args: &[String]) -> Result<(), CliError> {
    let mut common = CommonWindowOpts::default();
    let mut element: Option<usize> = None;
    let mut text: Option<String> = None;
    let mut text_stdin = false;
    let mut i = 0;
    while i < args.len() {
        if try_parse_common_opt(args, &mut i, &mut common)? {
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--element" | "--element-index" | "--ref" => {
                let raw = take_val(args, &mut i, "--element")?;
                element = Some(parse_element_ref(&raw, "--element")?);
            }
            "--text" => text = Some(take_val(args, &mut i, "--text")?),
            "--text-stdin" => text_stdin = true,
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    let text = resolve_text_or_stdin(text, text_stdin, "type-text", "--text", "--text-stdin")?;
    let before = ensure_current_snapshot(&common)?;
    let target_ref = element.or(before.focused_element_id);
    let focus_point = target_ref.and_then(|ref_id| {
        before
            .elements
            .iter()
            .find(|e| e.reference_id == ref_id)
            .and_then(|e| e.bounds.map(super::model::Rect::center))
    });
    let selector = common.app.as_deref().unwrap_or(&before.app.name);
    let live_action = LiveAction::TypeText {
        target_ref: element,
        focus_point,
        text: text.clone(),
    };
    let outcome = if let Some(ref_id) = target_ref {
        apply_mock_or_live_element_mutation(
            selector,
            Some(&before.app),
            before.window.as_ref(),
            ref_id,
            None,
            Some(&text),
            true,
            &format!("type-text:{text}"),
            &live_action,
        )?
    } else {
        record_mock_or_live_action(
            selector,
            Some(&before.app),
            before.window.as_ref(),
            &format!("type-text:{text}"),
            &live_action,
        )?
    };
    let after = ensure_current_snapshot(&common)?;
    let mut notes = vec![format!("typed {} chars", text.chars().count())];
    if let Some(note) = outcome.note.clone() {
        notes.push(note);
    }
    let res = ActionResultPayload {
        action: "typeText".to_owned(),
        ok: outcome.ok,
        verification: verification_from_outcome(&outcome),
        target_reference_id: target_ref,
        target_point: None,
        app_state: Some(after),
        clipboard: None,
        notes,
    };
    print_action_result(&res, common.json)
}

pub(super) fn cmd_press_key(args: &[String]) -> Result<(), CliError> {
    let mut common = CommonWindowOpts::default();
    let mut key: Option<String> = None;
    let mut modifiers: Vec<String> = Vec::new();
    let mut repeat = 1usize;
    let mut i = 0;
    while i < args.len() {
        if try_parse_common_opt(args, &mut i, &mut common)? {
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--key" => key = Some(take_val(args, &mut i, "--key")?),
            "--modifiers" => {
                let raw = take_val(args, &mut i, "--modifiers")?;
                for part in raw
                    .split([',', '+'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    let norm = normalize_modifier_name(part)
                        .map_err(CliError::Usage)?
                        .to_owned();
                    if !modifiers.contains(&norm) {
                        modifiers.push(norm);
                    }
                }
            }
            "--repeat" => {
                let raw = take_val(args, &mut i, "--repeat")?;
                repeat = parse_usize(&raw, "--repeat")?;
                if repeat == 0 {
                    return Err(CliError::Usage("--repeat must be >= 1".to_owned()));
                }
            }
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    let raw_key = key.ok_or_else(|| CliError::Usage("press-key requires --key <name>".into()))?;
    let norm_key = normalize_key_name(&raw_key).map_err(CliError::Usage)?;
    let before = ensure_current_snapshot(&common)?;
    let selector = common.app.as_deref().unwrap_or(&before.app.name);
    let outcome = record_mock_or_live_action(
        selector,
        Some(&before.app),
        before.window.as_ref(),
        &format!(
            "press-key:{norm_key}:mods={}:repeat={repeat}",
            modifiers.join("+")
        ),
        &LiveAction::PressKey {
            key: norm_key.clone(),
            modifiers: modifiers.clone(),
            repeat,
        },
    )?;
    let after = ensure_current_snapshot(&common)?;
    let mut notes = vec![format!(
        "key={norm_key} modifiers={modifiers:?} repeat={repeat}"
    )];
    if let Some(note) = outcome.note.clone() {
        notes.push(note);
    }
    let res = ActionResultPayload {
        action: "pressKey".to_owned(),
        ok: outcome.ok,
        verification: verification_from_outcome(&outcome),
        target_reference_id: after.focused_element_id,
        target_point: None,
        app_state: Some(after),
        clipboard: None,
        notes,
    };
    print_action_result(&res, common.json)
}

pub(super) fn cmd_hotkey(args: &[String]) -> Result<(), CliError> {
    let mut common = CommonWindowOpts::default();
    let mut keys: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if try_parse_common_opt(args, &mut i, &mut common)? {
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--keys" | "--chord" | "--key" => keys = Some(take_val(args, &mut i, "--keys")?),
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    let raw_chord = keys.ok_or_else(|| CliError::Usage("hotkey requires --keys <chord>".into()))?;
    let (mods, key) = parse_hotkey_chord(&raw_chord).map_err(CliError::Usage)?;
    let before = ensure_current_snapshot(&common)?;
    let selector = common.app.as_deref().unwrap_or(&before.app.name);
    let outcome = record_mock_or_live_action(
        selector,
        Some(&before.app),
        before.window.as_ref(),
        &format!("hotkey:{}+{}", mods.join("+"), key),
        &LiveAction::Hotkey {
            modifiers: mods.clone(),
            key: key.clone(),
        },
    )?;
    let after = ensure_current_snapshot(&common)?;
    let mut notes = vec![format!("chord={}+{}", mods.join("+"), key)];
    if let Some(note) = outcome.note.clone() {
        notes.push(note);
    }
    let res = ActionResultPayload {
        action: "hotkey".to_owned(),
        ok: outcome.ok,
        verification: verification_from_outcome(&outcome),
        target_reference_id: after.focused_element_id,
        target_point: None,
        app_state: Some(after),
        clipboard: None,
        notes,
    };
    print_action_result(&res, common.json)
}

pub(super) fn cmd_paste_text(args: &[String]) -> Result<(), CliError> {
    let mut common = CommonWindowOpts::default();
    let mut element: Option<usize> = None;
    let mut text: Option<String> = None;
    let mut text_stdin = false;
    let mut i = 0;
    while i < args.len() {
        if try_parse_common_opt(args, &mut i, &mut common)? {
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--element" | "--element-index" | "--ref" => {
                let raw = take_val(args, &mut i, "--element")?;
                element = Some(parse_element_ref(&raw, "--element")?);
            }
            "--text" => text = Some(take_val(args, &mut i, "--text")?),
            "--text-stdin" => text_stdin = true,
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    let text = resolve_text_or_stdin(text, text_stdin, "paste-text", "--text", "--text-stdin")?;
    let before = ensure_current_snapshot(&common)?;
    let target_ref = element.or(before.focused_element_id);
    let focus_point = target_ref.and_then(|ref_id| {
        before
            .elements
            .iter()
            .find(|e| e.reference_id == ref_id)
            .and_then(|e| e.bounds.map(super::model::Rect::center))
    });
    let selector = common.app.as_deref().unwrap_or(&before.app.name);
    let (previous_clipboard, outcome) = execute_paste_with_clipboard_preservation(
        selector,
        Some(&before.app),
        before.window.as_ref(),
        target_ref,
        focus_point,
        &text,
    )
    .map_err(CliError::Runtime)?;
    let after = ensure_current_snapshot(&common)?;
    let mut notes = vec![format!(
        "pasted {} chars with clipboard restoration",
        text.chars().count()
    )];
    if let Some(note) = outcome.note.clone() {
        notes.push(note);
    }
    let res = ActionResultPayload {
        action: "pasteText".to_owned(),
        ok: outcome.ok,
        verification: verification_from_outcome(&outcome),
        target_reference_id: target_ref,
        target_point: None,
        app_state: Some(after),
        clipboard: Some(ClipboardMetadata {
            preserved: true,
            restored: true,
            previous_text: Some(previous_clipboard),
        }),
        notes,
    };
    print_action_result(&res, common.json)
}

pub(super) fn cmd_set_value(args: &[String]) -> Result<(), CliError> {
    let mut common = CommonWindowOpts::default();
    let mut element: Option<usize> = None;
    let mut value: Option<String> = None;
    let mut value_stdin = false;
    let mut i = 0;
    while i < args.len() {
        if try_parse_common_opt(args, &mut i, &mut common)? {
            i += 1;
            continue;
        }
        match args[i].as_str() {
            "--element" | "--element-index" | "--ref" => {
                let raw = take_val(args, &mut i, "--element")?;
                element = Some(parse_element_ref(&raw, "--element")?);
            }
            "--value" => value = Some(take_val(args, &mut i, "--value")?),
            "--value-stdin" => value_stdin = true,
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    let ref_id =
        element.ok_or_else(|| CliError::Usage("set-value requires --element <n>".into()))?;
    let value = resolve_text_or_stdin(value, value_stdin, "set-value", "--value", "--value-stdin")?;
    let before = ensure_current_snapshot(&common)?;
    let Some(found) = before.elements.iter().find(|e| e.reference_id == ref_id) else {
        return Err(CliError::Runtime(format!(
            "element #{ref_id} not found in snapshot"
        )));
    };
    if !found.enabled {
        return Err(CliError::Runtime(format!("element #{ref_id} is disabled")));
    }
    let focus_point = found.bounds.map(super::model::Rect::center);
    let selector = common.app.as_deref().unwrap_or(&before.app.name);
    let outcome = apply_mock_or_live_element_mutation(
        selector,
        Some(&before.app),
        before.window.as_ref(),
        ref_id,
        Some(&value),
        None,
        true,
        &format!("set-value:#{ref_id}={value}"),
        &LiveAction::SetValue {
            target_ref: ref_id,
            focus_point,
            value: value.clone(),
        },
    )?;
    let after = ensure_current_snapshot(&common)?;
    let verified = outcome.ok
        && after
            .elements
            .iter()
            .find(|e| e.reference_id == ref_id)
            .is_some_and(|e| e.value.as_deref() == Some(value.as_str()));
    let verification = if !outcome.ok || outcome.status != "verified" {
        verification_from_outcome(&outcome)
    } else if verified {
        VerificationMetadata {
            status: "verified".to_owned(),
            reason: None,
            retried: false,
        }
    } else {
        VerificationMetadata {
            status: "unverified".to_owned(),
            reason: Some("element value did not match requested value after set-value".to_owned()),
            retried: true,
        }
    };
    let mut notes = vec![format!("set value on #{ref_id}")];
    if let Some(note) = outcome.note {
        notes.push(note);
    }
    let res = ActionResultPayload {
        action: "setValue".to_owned(),
        ok: outcome.ok,
        verification,
        target_reference_id: Some(ref_id),
        target_point: None,
        app_state: Some(after),
        clipboard: None,
        notes,
    };
    print_action_result(&res, common.json)
}

#[expect(
    clippy::too_many_arguments,
    reason = "internal helper unifies mock tree mutation and live OS action dispatch"
)]
fn apply_mock_or_live_element_mutation(
    app_selector: &str,
    resolved_app: Option<&AppSummary>,
    selected_window: Option<&WindowSummary>,
    target_ref: usize,
    new_value: Option<&str>,
    append_text: Option<&str>,
    focus: bool,
    action_record: &str,
    live_action: &LiveAction,
) -> Result<LiveActionOutcome, CliError> {
    if let Some(mut mock) = load_mock_env().map_err(CliError::Runtime)? {
        mock.last_action = Some(action_record.to_owned());
        let Some(app) = mock.apps.iter_mut().find(|a| {
            resolved_app.is_some_and(|r| r.pid > 0 && a.pid == r.pid)
                || matches_app_selector(&a.summary(), app_selector)
        }) else {
            return Err(CliError::Runtime(format!(
                "application not found: {app_selector}"
            )));
        };
        let win_opt = if let Some(target_win) = selected_window {
            let idx = target_win.index.saturating_sub(1);
            if let Some(pos) = app.windows.iter().position(|w| w.id == target_win.id) {
                app.windows.get_mut(pos)
            } else {
                app.windows.get_mut(idx)
            }
        } else {
            app.windows.first_mut()
        };
        if let Some(win) = win_opt {
            let _ = index_and_render_tree(&mut win.root);
            let Some((_, enabled)) =
                mutate_element_by_ref(&mut win.root, target_ref, new_value, append_text, focus)
            else {
                return Err(CliError::Runtime(format!(
                    "element #{target_ref} not found"
                )));
            };
            if !enabled {
                return Err(CliError::Runtime(format!(
                    "element #{target_ref} is disabled"
                )));
            }
        }
        save_mock_env(&mock).map_err(CliError::Runtime)?;
        return Ok(LiveActionOutcome {
            ok: true,
            status: "verified".to_owned(),
            reason: None,
            note: None,
        });
    }
    execute_live_action(app_selector, resolved_app, selected_window, live_action)
        .map_err(CliError::Runtime)
}

fn record_mock_or_live_action(
    app_selector: &str,
    resolved_app: Option<&AppSummary>,
    selected_window: Option<&WindowSummary>,
    action_record: &str,
    live_action: &LiveAction,
) -> Result<LiveActionOutcome, CliError> {
    if let Some(mut mock) = load_mock_env().map_err(CliError::Runtime)? {
        mock.last_action = Some(action_record.to_owned());
        save_mock_env(&mock).map_err(CliError::Runtime)?;
        return Ok(LiveActionOutcome {
            ok: true,
            status: "verified".to_owned(),
            reason: None,
            note: None,
        });
    }
    execute_live_action(app_selector, resolved_app, selected_window, live_action)
        .map_err(CliError::Runtime)
}

fn execute_paste_with_clipboard_preservation(
    app_selector: &str,
    resolved_app: Option<&AppSummary>,
    selected_window: Option<&WindowSummary>,
    target_ref: Option<usize>,
    focus_point: Option<Point>,
    text: &str,
) -> Result<(String, LiveActionOutcome), String> {
    if let Some(mut mock) = load_mock_env()? {
        let prev = mock.clipboard.clone();
        text.clone_into(&mut mock.clipboard);
        if let Some(app) = mock.apps.iter_mut().find(|a| {
            resolved_app.is_some_and(|r| r.pid > 0 && a.pid == r.pid)
                || matches_app_selector(&a.summary(), app_selector)
        }) {
            let win_opt = if let Some(target_win) = selected_window {
                let idx = target_win.index.saturating_sub(1);
                if let Some(pos) = app.windows.iter().position(|w| w.id == target_win.id) {
                    app.windows.get_mut(pos)
                } else {
                    app.windows.get_mut(idx)
                }
            } else {
                app.windows.first_mut()
            };
            if let Some(win) = win_opt {
                let _ = index_and_render_tree(&mut win.root);
                if let Some(ref_id) = target_ref {
                    let _ = mutate_element_by_ref(&mut win.root, ref_id, None, Some(text), true);
                } else {
                    let _ = mutate_focused_element(&mut win.root, None, Some(text));
                }
            }
        }
        mock.clipboard.clone_from(&prev);
        mock.clipboard_restore_count = mock.clipboard_restore_count.saturating_add(1);
        mock.last_action = Some(format!("paste-text:{text}"));
        save_mock_env(&mock)?;
        return Ok((
            prev,
            LiveActionOutcome {
                ok: true,
                status: "verified".to_owned(),
                reason: None,
                note: None,
            },
        ));
    }
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some()
        && std::env::var_os("KRILL_COMPUTER_OSASCRIPT_BIN").is_none()
    {
        return Ok((
            String::new(),
            LiveActionOutcome {
                ok: false,
                status: "unverified".to_owned(),
                reason: Some(
                    "live paste skipped because KRILL_TEST_OFFLINE is set without mock environment"
                        .to_owned(),
                ),
                note: None,
            },
        ));
    }
    let prev = read_system_clipboard()?;
    write_system_clipboard(text)?;
    let paste_res = execute_live_action(
        app_selector,
        resolved_app,
        selected_window,
        &LiveAction::PasteText {
            target_ref,
            focus_point,
        },
    );
    let restore_res = write_system_clipboard(&prev);
    let outcome = paste_res?;
    restore_res?;
    Ok((prev, outcome))
}

pub(super) fn read_bounded_text(
    reader: impl std::io::Read,
    subcommand: &str,
) -> Result<String, CliError> {
    let limit = u64::try_from(MAX_INPUT + 1).unwrap_or(u64::MAX);
    let mut buf = Vec::new();
    reader
        .take(limit)
        .read_to_end(&mut buf)
        .map_err(|e| CliError::Runtime(format!("read stdin: {e}")))?;
    if buf.len() > MAX_INPUT {
        return Err(CliError::Runtime(format!(
            "{subcommand} input exceeds maximum length of {MAX_INPUT} bytes"
        )));
    }
    String::from_utf8(buf)
        .map_err(|e| CliError::Runtime(format!("{subcommand} stdin must be valid UTF-8: {e}")))
}

fn resolve_text_or_stdin(
    inline: Option<String>,
    from_stdin: bool,
    subcommand: &str,
    inline_flag: &str,
    stdin_flag: &str,
) -> Result<String, CliError> {
    if inline.is_some() && from_stdin {
        return Err(CliError::Usage(format!(
            "cannot specify both {inline_flag} and {stdin_flag}"
        )));
    }
    if let Some(text) = inline {
        if text.len() > MAX_INPUT {
            return Err(CliError::Runtime(format!(
                "{subcommand} input exceeds maximum length of {MAX_INPUT} bytes"
            )));
        }
        return Ok(text);
    }
    if from_stdin {
        return read_bounded_text(std::io::stdin(), subcommand);
    }
    Err(CliError::Usage(format!(
        "{subcommand} requires {inline_flag} <string> or {stdin_flag}"
    )))
}
