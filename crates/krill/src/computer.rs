// Portions of the computer-use CLI subcommand contract and output formatting are adapted
// from Orca (.ref/orca/src/cli/specs/computer.ts, .ref/orca/src/cli/handlers/computer.ts,
// .ref/orca/src/cli/computer-format.ts):
// Copyright (c) 2026 Lovecast Inc. Licensed under the MIT License (.ref/orca/LICENSE).

//! `krill computer` CLI 서브커맨드 디스패처(MK-187 / OWE-741).

mod actions;
pub mod engine;
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
    load_cached_snapshot, load_mock_env, open_permission_settings, query_permissions,
    verify_not_self_targeted,
};

const COMPUTER_USAGE: &str = "\
usage: krill computer <subcommand> [options]

subcommands:
  capabilities [--json]
  permissions [--id <accessibility|screenshots>] [--json]
  list-apps [--json]
  list-windows --app <name|bundle|pid:N> [--json]
  get-app-state --app <name|bundle|pid:N> [--window-id <id> | --window-index <n>] [--worktree <selector> | --session <id>] [--restore-window] [--no-screenshot] [--json]
  click (--element <n> | --x <num> --y <num>) [--button <left|right|middle>] [--click-count <n>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--no-screenshot] [--json]
  perform-secondary-action --element <n> [--action <AXShowMenu|AXPress>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--no-screenshot] [--json]
  scroll [--element <n> | --x <num> --y <num>] [--delta-x <num>] [--delta-y <num>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--no-screenshot] [--json]
  drag (--from-element <n> | --from-x <num> --from-y <num>) (--to-element <n> | --to-x <num> --to-y <num>) [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--no-screenshot] [--json]
  type-text --text <string> [--element <n>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--no-screenshot] [--json]
  press-key --key <name> [--modifiers <mod,mod>] [--repeat <n>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--no-screenshot] [--json]
  hotkey --keys <chord> [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--no-screenshot] [--json]
  paste-text --text <string> [--element <n>] [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--no-screenshot] [--json]
  set-value --element <n> --value <string> [--app <name>] [--window-id <id> | --window-index <n>] [--restore-window] [--no-screenshot] [--json]";

#[derive(Debug, Default)]
pub(super) struct CommonWindowOpts {
    pub(super) app: Option<String>,
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
    if matches!(sub.as_str(), "-h" | "--help" | "help") {
        println!("{COMPUTER_USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = match sub.as_str() {
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
            return ExitCode::from(2);
        }
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
    let mut common = CommonWindowOpts::default();
    let mut i = 0;
    while i < args.len() {
        if !try_parse_common_opt(args, &mut i, &mut common)? {
            return Err(CliError::Usage(format!("unknown flag: {}", args[i])));
        }
        i += 1;
    }
    let app = common
        .app
        .clone()
        .ok_or_else(|| CliError::Usage("--app is required".to_owned()))?;
    let snapshot = get_app_state(
        &app,
        common.window_id.as_deref(),
        common.window_index,
        common.restore_window,
        common.no_screenshot,
        context_note_from_opts(&common),
    )
    .map_err(CliError::Runtime)?;
    if common.json {
        return print_json(&snapshot);
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

fn resolve_target_app_name(common: &CommonWindowOpts) -> Result<String, CliError> {
    if let Some(app) = &common.app {
        return Ok(app.clone());
    }
    if let Some(cached) = load_cached_snapshot().map_err(CliError::Runtime)? {
        return Ok(cached.app.name);
    }
    if let Some(mock) = load_mock_env().map_err(CliError::Runtime)?
        && let Some(first) = mock
            .apps
            .iter()
            .find(|a| a.is_active)
            .or_else(|| mock.apps.first())
    {
        return Ok(first.name.clone());
    }
    Err(CliError::Usage(
        "target application unknown; pass --app <name> or run get-app-state first".to_owned(),
    ))
}

pub(super) fn ensure_current_snapshot(
    common: &CommonWindowOpts,
) -> Result<AppStateSnapshot, CliError> {
    let app_name = resolve_target_app_name(common)?;
    let cached_win_id =
        if common.app.is_none() && common.window_id.is_none() && common.window_index.is_none() {
            load_cached_snapshot()
                .ok()
                .flatten()
                .and_then(|s| s.window.map(|w| w.id))
        } else {
            None
        };
    let effective_window_id = common.window_id.as_deref().or(cached_win_id.as_deref());
    let snap = get_app_state(
        &app_name,
        effective_window_id,
        common.window_index,
        common.restore_window,
        common.no_screenshot,
        context_note_from_opts(common),
    )
    .map_err(CliError::Runtime)?;
    verify_not_self_targeted(&snap.app.name, snap.app.bundle_id.as_deref())
        .map_err(CliError::Runtime)?;
    Ok(snap)
}

pub(super) fn print_action_result(res: &ActionResultPayload, json: bool) -> Result<(), CliError> {
    if json {
        return print_json(res);
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
    Ok(())
}
