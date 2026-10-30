// Portions of the computer-use CLI subcommand contract and output formatting are adapted
// from Orca (.ref/orca/src/cli/specs/computer.ts, .ref/orca/src/cli/handlers/computer.ts,
// .ref/orca/src/cli/computer-format.ts):
// Copyright (c) 2026 Lovecast Inc. Licensed under the MIT License (.ref/orca/LICENSE).

//! `krill computer` CLI 서브커맨드 디스패처(MK-187 / OWE-741).

mod actions;
pub mod engine;
mod lifecycle;
mod mcp;
pub mod model;
#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::process::ExitCode;

use actions::{
    cmd_click, cmd_drag, cmd_hotkey, cmd_paste_text, cmd_press_key, cmd_scroll,
    cmd_secondary_action, cmd_set_value, cmd_type_text,
};
use engine::{
    ActionResultPayload, AppStateSnapshot, PermissionId, PermissionsPayload,
    build_permission_capabilities, capabilities_payload, get_app_state, list_apps, list_windows,
    open_permission_settings, query_permissions,
};

const COMPUTER_USAGE: &str = "\
usage: krill computer <subcommand> [options]

subcommands:
  serve --experimental [--worker <absolute krill-app path>]  (MCP 2025-11-25 stdio, native experimental)
  capabilities [--json]
  permissions [--id <accessibility|screenshots>] [--json]
  list-apps [--json]
  list-windows --app <name|bundle|pid:N> [--json]
  get-app-state --app <name|bundle|pid:N> [--window-id <id> | --window-index <n>] [--worktree <selector> | --session <id>] [--restore-window] [--snapshot <token>] [--screenshot] [--no-screenshot] [--json]
  get-app-state --snapshot <token> [--offset <n>] [--limit <1..80>] [--format <compact|full>] [--json]
  click (--element <n> | --x <num> --y <num>) [--button <left|right|middle>] [--click-count <n>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--snapshot <token>] [--screenshot] [--no-screenshot] [--json]
  perform-secondary-action --element <n> [--action <AXShowMenu|AXPress>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--snapshot <token>] [--screenshot] [--no-screenshot] [--json]
  scroll [--element <n> | --x <num> --y <num>] [--delta-x <num>] [--delta-y <num>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--snapshot <token>] [--screenshot] [--no-screenshot] [--json]
  drag (--from-element <n> | --from-x <num> --from-y <num>) (--to-element <n> | --to-x <num> --to-y <num>) [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--snapshot <token>] [--screenshot] [--no-screenshot] [--json]
  type-text --text <string> [--element <n>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--snapshot <token>] [--screenshot] [--no-screenshot] [--json]
  press-key --key <name> [--modifiers <mod,mod>] [--repeat <n>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--snapshot <token>] [--screenshot] [--no-screenshot] [--json]
  hotkey --keys <chord> [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--snapshot <token>] [--screenshot] [--no-screenshot] [--json]
  paste-text --text <string> [--element <n>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--snapshot <token>] [--screenshot] [--no-screenshot] [--json]
  set-value --element <n> --value <string> [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--snapshot <token>] [--screenshot] [--no-screenshot] [--json]";

#[derive(Debug, Default)]
pub(super) struct CommonWindowOpts {
    pub(super) app: Option<String>,
    pub(super) snapshot: Option<String>,
    pub(super) format: Option<String>,
    pub(super) limit: Option<usize>,
    pub(super) offset: usize,
    pub(super) window_id: Option<String>,
    pub(super) window_index: Option<usize>,
    pub(super) worktree: Option<String>,
    pub(super) session: Option<String>,
    pub(super) restore_window: bool,
    pub(super) no_screenshot: bool,
    pub(super) json: bool,
}

pub fn run(raw: &[OsString]) -> ExitCode {
    let Ok(args) = raw
        .iter()
        .cloned()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        eprintln!("arguments must be UTF-8");
        return ExitCode::from(2);
    };
    let Some((sub, rest)) = args.split_first() else {
        eprintln!("{COMPUTER_USAGE}");
        return ExitCode::from(2);
    };
    // `krill computer --help` 또는 서브커맨드 직후(`capabilities --help`)만 도움말로 본다.
    // 하위 명령의 옵션값 자리는 각 cmd_*가 판단한다.
    if matches!(
        args.first().map(String::as_str),
        Some("-h" | "--help" | "help")
    ) || matches!(rest.first(), Some(s) if matches!(s.as_str(), "-h" | "--help" | "help"))
    {
        println!("{COMPUTER_USAGE}");
        return ExitCode::SUCCESS;
    }
    // Persistent mode owns its worker and must never enter the legacy desktop executor.
    let result = if sub == "serve" {
        mcp::run(rest)
    } else {
        engine::with_execution(sub, rest, || match sub.as_str() {
            "capabilities" => cmd_capabilities(rest),
            "permissions" => cmd_permissions(rest),
            "list-apps" => cmd_list_apps(rest),
            "list-windows" => cmd_list_windows(rest),
            "get-app-state" | "snapshot" => cmd_get_app_state(rest),
            "click" => cmd_click(rest),
            "perform-secondary-action" | "secondary-action" => cmd_secondary_action(rest),
            "scroll" => cmd_scroll(rest),
            "drag" => cmd_drag(rest),
            "type-text" => cmd_type_text(rest),
            "press-key" => cmd_press_key(rest),
            "hotkey" => cmd_hotkey(rest),
            "paste-text" => cmd_paste_text(rest),
            "set-value" => cmd_set_value(rest),
            other => {
                eprintln!("unknown computer subcommand: {other}\n{COMPUTER_USAGE}");
                Err(CliError::Usage(format!(
                    "unknown computer subcommand: {other}"
                )))
            }
        })
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::Usage(msg)) => {
            eprintln!("{msg}");
            ExitCode::from(2)
        }
        Err(CliError::Runtime(msg)) => {
            eprintln!("krill computer: {msg}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug)]
pub(super) enum CliError {
    Usage(String),
    Runtime(String),
}

pub(super) fn take_val(args: &[String], idx: &mut usize, flag: &str) -> Result<String, CliError> {
    *idx += 1;
    args.get(*idx)
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("missing value for {flag}")))
}

pub(super) fn parse_usize(val: &str, flag: &str) -> Result<usize, CliError> {
    val.parse::<usize>()
        .map_err(|_| CliError::Usage(format!("invalid integer for {flag}: {val}")))
}

pub(super) fn parse_element_ref(val: &str, flag: &str) -> Result<usize, CliError> {
    let trimmed = val.trim();
    let stripped = trimmed
        .strip_prefix("@e")
        .or_else(|| trimmed.strip_prefix("@E"))
        .or_else(|| trimmed.strip_prefix('@'))
        .or_else(|| trimmed.strip_prefix('#'))
        .or_else(|| trimmed.strip_prefix('e'))
        .or_else(|| trimmed.strip_prefix('E'))
        .unwrap_or(trimmed);
    let parsed = stripped
        .parse::<usize>()
        .map_err(|_| CliError::Usage(format!("invalid element reference for {flag}: {val}")))?;
    if parsed == 0 {
        return Err(CliError::Usage(format!(
            "element reference for {flag} must be >= 1"
        )));
    }
    Ok(parsed)
}

pub(super) fn parse_f64(val: &str, flag: &str) -> Result<f64, CliError> {
    let parsed = val
        .parse::<f64>()
        .map_err(|_| CliError::Usage(format!("invalid number for {flag}: {val}")))?;
    if !parsed.is_finite() {
        return Err(CliError::Usage(format!("{flag} must be finite")));
    }
    Ok(parsed)
}

pub(super) fn try_parse_common_opt(
    args: &[String],
    idx: &mut usize,
    common: &mut CommonWindowOpts,
) -> Result<bool, CliError> {
    match args[*idx].as_str() {
        "--app" => common.app = Some(take_val(args, idx, "--app")?),
        "--window-id" => common.window_id = Some(take_val(args, idx, "--window-id")?),
        "--window-index" => {
            let raw = take_val(args, idx, "--window-index")?;
            let n = parse_usize(&raw, "--window-index")?;
            if n == 0 {
                return Err(CliError::Usage("--window-index must be >= 1".to_owned()));
            }
            common.window_index = Some(n);
        }
        "--format" => {
            let format = take_val(args, idx, "--format")?;
            if !matches!(format.as_str(), "compact" | "full") {
                return Err(CliError::Usage(
                    "--format must be compact or full".to_owned(),
                ));
            }
            common.format = Some(format);
        }
        "--limit" => {
            let limit = parse_usize(&take_val(args, idx, "--limit")?, "--limit")?;
            if !(1..=80).contains(&limit) {
                return Err(CliError::Usage(
                    "--limit must be between 1 and 80".to_owned(),
                ));
            }
            common.limit = Some(limit);
        }
        "--offset" => {
            common.offset = parse_usize(&take_val(args, idx, "--offset")?, "--offset")?;
            if common.offset > 160 {
                return Err(CliError::Usage("--offset must be <= 160".to_owned()));
            }
        }
        "--screenshot" => common.no_screenshot = false,
        "--snapshot" => common.snapshot = Some(take_val(args, idx, "--snapshot")?),
        "--worktree" => common.worktree = Some(take_val(args, idx, "--worktree")?),
        "--session" => common.session = Some(take_val(args, idx, "--session")?),
        "--restore-window" => common.restore_window = true,
        "--no-screenshot" => common.no_screenshot = true,
        "--json" => common.json = true,
        _ => return Ok(false),
    }
    if common.window_id.is_some() && common.window_index.is_some() {
        return Err(CliError::Usage(
            "cannot specify both --window-id and --window-index".to_owned(),
        ));
    }
    if common.worktree.is_some() && common.session.is_some() {
        return Err(CliError::Usage(
            "cannot specify both --worktree and --session".to_owned(),
        ));
    }
    Ok(true)
}

pub(super) fn print_json<T: serde::Serialize>(val: &T) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(val).map_err(|e| CliError::Runtime(e.to_string()))?;
    println!("{text}");
    Ok(())
}

fn cmd_capabilities(args: &[String]) -> Result<(), CliError> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
    }
    let payload = capabilities_payload().map_err(CliError::Runtime)?;
    if json {
        return print_json(&payload);
    }
    println!(
        "platform   {} (supported: {})",
        payload.platform, payload.supported
    );
    for perm in &payload.permissions {
        println!(
            "permission {:<14} {:<8} {}",
            perm.id, perm.status, perm.label
        );
    }
    println!("actions    {}", payload.actions.join(", "));
    Ok(())
}

fn cmd_permissions(args: &[String]) -> Result<(), CliError> {
    let mut json = false;
    let mut filter_id: Option<PermissionId> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--id" => {
                let raw = take_val(args, &mut i, "--id")?;
                filter_id = Some(PermissionId::parse(&raw).ok_or_else(|| {
                    CliError::Usage(format!(
                        "invalid permission id '{raw}' (expected accessibility or screenshots)"
                    ))
                })?);
            }
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    let (ax, sc) = query_permissions().map_err(CliError::Runtime)?;
    let mut perms = build_permission_capabilities(ax, sc);
    let mut opened_settings_for = None;
    if let Some(id) = filter_id {
        perms.retain(|p| p.id == id.as_str());
        if perms.iter().any(|p| !p.granted) {
            open_permission_settings(id).ok();
            opened_settings_for = Some(id.as_str().to_owned());
        }
    }
    let all_granted = perms.iter().all(|p| p.granted);
    let payload = PermissionsPayload {
        platform: if cfg!(target_os = "macos") {
            "darwin".to_owned()
        } else {
            std::env::consts::OS.to_owned()
        },
        supported: cfg!(target_os = "macos"),
        all_granted,
        permissions: perms,
        opened_settings_for,
    };
    if json {
        return print_json(&payload);
    }
    for perm in &payload.permissions {
        println!(
            "{:<14} {:<8} {} ({})",
            perm.id, perm.status, perm.label, perm.instructions
        );
    }
    Ok(())
}

fn cmd_list_apps(args: &[String]) -> Result<(), CliError> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
    }
    let apps = list_apps().map_err(CliError::Runtime)?;
    if json {
        return print_json(&serde_json::json!({ "apps": apps }));
    }
    for app in &apps {
        let active = if app.is_active { " [active]" } else { "" };
        let hidden = if app.is_hidden { " [hidden]" } else { "" };
        println!(
            "{:<6} {:<24} {}{}{}",
            app.pid,
            app.name,
            app.bundle_id.as_deref().unwrap_or("-"),
            active,
            hidden
        );
    }
    Ok(())
}

fn cmd_list_windows(args: &[String]) -> Result<(), CliError> {
    let mut app: Option<String> = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--app" => app = Some(take_val(args, &mut i, "--app")?),
            "--json" => json = true,
            other => return Err(CliError::Usage(format!("unknown flag: {other}"))),
        }
        i += 1;
    }
    let app = app.ok_or_else(|| CliError::Usage("--app is required".to_owned()))?;
    let windows = list_windows(&app).map_err(CliError::Runtime)?;
    if json {
        return print_json(&serde_json::json!({ "windows": windows }));
    }
    for w in &windows {
        println!(
            "#{} id={} \"{}\" ({:.0},{:.0} {:.0}x{:.0})",
            w.index, w.id, w.title, w.bounds.x, w.bounds.y, w.bounds.width, w.bounds.height
        );
    }
    Ok(())
}

pub(super) fn context_note_from_opts(common: &CommonWindowOpts) -> Option<String> {
    if let Some(wt) = &common.worktree {
        Some(format!("associated worktree: {wt}"))
    } else {
        common
            .session
            .as_ref()
            .map(|s| format!("associated session: {s}"))
    }
}

fn cmd_get_app_state(args: &[String]) -> Result<(), CliError> {
    let mut common = CommonWindowOpts {
        no_screenshot: true,
        ..CommonWindowOpts::default()
    };
    let mut i = 0;
    while i < args.len() {
        if !try_parse_common_opt(args, &mut i, &mut common)? {
            return Err(CliError::Usage(format!("unknown flag: {}", args[i])));
        }
        i += 1;
    }
    if common.offset > 0 && common.snapshot.is_none() {
        return Err(CliError::Usage(
            "paging requires --snapshot <snapshotId>; it never refreshes a page".to_owned(),
        ));
    }
    let snapshot = if let Some(id) = common.snapshot.as_deref() {
        if common.restore_window {
            return Err(CliError::Usage(
                "cached snapshot paging cannot restore a window".to_owned(),
            ));
        }
        engine::load_snapshot(Some(id))
            .map_err(CliError::Runtime)?
            .ok_or_else(|| CliError::Runtime("snapshot absent or expired from cache".to_owned()))?
    } else {
        let app = common
            .app
            .as_deref()
            .ok_or_else(|| CliError::Usage("--app is required".to_owned()))?;
        get_app_state(
            app,
            common.window_id.as_deref(),
            common.window_index,
            common.restore_window,
            common.no_screenshot,
            context_note_from_opts(&common),
        )
        .map_err(CliError::Runtime)?
    };
    if common.json {
        return print_json(&snapshot_json(&snapshot, &common)?);
    }
    print_snapshot_human(&snapshot);
    Ok(())
}

pub(super) fn print_snapshot_human(snap: &AppStateSnapshot) {
    println!(
        "app        {} (pid {}, bundle {})",
        snap.app.name,
        snap.app.pid,
        snap.app.bundle_id.as_deref().unwrap_or("-")
    );
    if let Some(w) = &snap.window {
        println!(
            "window     #{} id={} \"{}\" @({:.0},{:.0} {:.0}x{:.0})",
            w.index, w.id, w.title, w.bounds.x, w.bounds.y, w.bounds.width, w.bounds.height
        );
    }
    println!("snapshot   {}", snap.snapshot_id);
    println!(
        "elements   {} interactive / {} total (focused: {:?})",
        snap.interactive_count, snap.element_count, snap.focused_element_id
    );
    if let Some(shot) = &snap.screenshot {
        println!(
            "screenshot {} ({}x{} @{:.1}x)",
            shot.path, shot.width, shot.height, shot.scale_factor
        );
    }
    if !snap.tree_text.is_empty() {
        print!("{}", snap.tree_text);
    }
}

pub(super) fn ensure_current_snapshot(
    common: &CommonWindowOpts,
) -> Result<AppStateSnapshot, CliError> {
    let before = if common.snapshot.is_some() {
        engine::load_snapshot(common.snapshot.as_deref())
    } else {
        engine::load_cached_snapshot()
    }
    .map_err(CliError::Runtime)?
    .ok_or_else(|| {
        CliError::Runtime(
            "observe with get-app-state first; no caller-scoped snapshot exists".to_owned(),
        )
    })?;
    engine::verify_target_app(&before.app).map_err(CliError::Runtime)?;
    if before.captured_at_ms > engine::now_ms().saturating_add(1000)
        || engine::now_ms().saturating_sub(before.captured_at_ms) > 120_000
        || before.captured_at_ms == 0
    {
        return Err(CliError::Runtime(
            "snapshot expired; observe again".to_owned(),
        ));
    }
    if common
        .app
        .as_ref()
        .is_some_and(|selector| !engine::matches_app_selector(&before.app, selector))
        || common
            .window_id
            .as_ref()
            .is_some_and(|id| before.window.as_ref().is_none_or(|win| &win.id != id))
        || common
            .window_index
            .is_some_and(|index| before.window.as_ref().is_none_or(|win| win.index != index))
    {
        return Err(CliError::Runtime(
            "target does not match original snapshot; observe the intended window first".to_owned(),
        ));
    }
    if engine::is_native_snapshot(&before) {
        if common.restore_window {
            return Err(CliError::Runtime(
                "native snapshots cannot restore windows; observe without KRILL_COMPUTER_ENGINE=native"
                    .to_owned(),
            ));
        }
        // The native action re-observes in its own worker and checks the target path there.
        engine::set_action_snapshot(before.clone());
        return Ok(before);
    }
    // Restore is a mutation and is guarded and leased before this observation.
    let fresh = engine::observe_app_state(
        &format!("pid:{}", before.app.pid),
        before.window.as_ref().map(|win| win.id.as_str()),
        None,
        common.restore_window,
        true,
        None,
    )
    .map_err(CliError::Runtime)?;
    engine::validate_snapshot_identity(&before, &fresh).map_err(CliError::Runtime)?;
    if let (Some(original), Some(current)) = (&before.root, &fresh.root) {
        engine::validate_structure(original, current).map_err(CliError::Runtime)?;
    } else {
        return Err(CliError::Runtime("AX observation unavailable".to_owned()));
    }
    engine::set_action_snapshot(before.clone());
    Ok(before)
}

pub(super) struct ActionObservation {
    pub(super) snapshot: Option<AppStateSnapshot>,
    pub(super) metadata: engine::ObservationMetadata,
}

pub(super) fn observe_after_action(
    common: &CommonWindowOpts,
    before: &AppStateSnapshot,
) -> ActionObservation {
    let result = engine::observe_app_state(
        &format!("pid:{}", before.app.pid),
        before.window.as_ref().map(|w| w.id.as_str()),
        None,
        false,
        common.no_screenshot,
        context_note_from_opts(common),
    )
    .and_then(|snapshot| {
        if snapshot.app.pid != before.app.pid
            || snapshot.app.bundle_id != before.app.bundle_id
            || snapshot.process_start != before.process_start
            || snapshot.window.as_ref().map(|w| &w.id) != before.window.as_ref().map(|w| &w.id)
        {
            return Err(
                "post-action target instance changed; input delivery is retained separately"
                    .to_owned(),
            );
        }
        engine::save_cached_snapshot(&snapshot)?;
        Ok(snapshot)
    });
    match result {
        Ok(snapshot) => ActionObservation {
            snapshot: Some(snapshot),
            metadata: engine::ObservationMetadata {
                status: "observed".to_owned(),
                reason: None,
            },
        },
        Err(error) => ActionObservation {
            snapshot: None,
            metadata: engine::ObservationMetadata {
                status: "failed".to_owned(),
                reason: Some(error),
            },
        },
    }
}

pub(super) fn print_action_result(res: &ActionResultPayload, json: bool) -> Result<(), CliError> {
    if json {
        let mut value = serde_json::to_value(res).map_err(|e| CliError::Runtime(e.to_string()))?;
        value["contract"] = serde_json::json!("computer/2");
        if let Some(snapshot) = &res.app_state {
            value["appState"] = snapshot_json(snapshot, &CommonWindowOpts::default())?;
        }
        print_json(&value)?;
        return action_exit(res);
    }
    println!(
        "action     {} ({}, verification: {})",
        res.action,
        if res.ok { "ok" } else { "failed" },
        res.verification.status
    );
    if let Some(id) = res.target_reference_id {
        println!("element    #{id}");
    }
    if let Some(pt) = res.target_point {
        println!("point      ({:.1}, {:.1})", pt.x, pt.y);
    }
    if let Some(clip) = &res.clipboard {
        println!(
            "clipboard  preserved={} restored={}",
            clip.preserved, clip.restored
        );
    }
    if let Some(snap) = &res.app_state {
        print_snapshot_human(snap);
    }
    action_exit(res)
}

fn action_exit(res: &ActionResultPayload) -> Result<(), CliError> {
    if !res.ok
        || matches!(
            res.delivery,
            engine::Delivery::NotDispatched | engine::Delivery::Unknown | engine::Delivery::Partial
        )
        || res.cleanup == engine::Cleanup::Incomplete
        || res.observation.status == "failed"
    {
        return Err(CliError::Runtime(
            "action failed or has an unknown/partial outcome; do not replay".to_owned(),
        ));
    }
    Ok(())
}

fn snapshot_json(
    snapshot: &AppStateSnapshot,
    options: &CommonWindowOpts,
) -> Result<serde_json::Value, CliError> {
    let mut value = serde_json::to_value(snapshot).map_err(|e| CliError::Runtime(e.to_string()))?;
    value["contract"] = serde_json::json!("computer/2");
    let full = options.format.as_deref() == Some("full");
    if !full && let Some(object) = value.as_object_mut() {
        object.remove("root");
        object.remove("treeText");
    }
    let page: Vec<_> = snapshot
        .elements
        .iter()
        .skip(options.offset)
        .take(options.limit.unwrap_or(40))
        .collect();
    let next = options.offset.saturating_add(page.len());
    if !full {
        value["elements"] =
            serde_json::to_value(&page).map_err(|e| CliError::Runtime(e.to_string()))?;
    }
    value["coverage"] = serde_json::json!({"kind":"bounded", "truncated":snapshot.coverage_truncated,
        "observedNodes":snapshot.element_count, "totalObservedElements":snapshot.elements.len(),
        "returnedElements":if full { snapshot.elements.len() } else { page.len() },
        "nextOffset":if !full && next < snapshot.elements.len() { Some(next) } else { None },
        "snapshotPagingOnly":true});
    Ok(value)
}
