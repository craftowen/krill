// Portions of the computer-use runtime and macOS execution backend are adapted from Orca
// (.ref/orca/src/cli/handlers/computer.ts, .ref/orca/native/computer-use-macos/):
// Copyright (c) 2026 Lovecast Inc. Licensed under the MIT License (.ref/orca/LICENSE).

//! `krill computer` 스냅샷 상태 캐시, 모의 환경, macOS 실행 백엔드.

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use krill_proto::daemon;

pub use super::model::*;

const MOCK_FILE_NAME: &str = "computer-mock.json";
const SNAPSHOT_CACHE_FILE_NAME: &str = "computer-state.json";

pub fn mock_file_path() -> Option<PathBuf> {
    daemon::state_dir().map(|dir| dir.join(MOCK_FILE_NAME))
}

pub fn snapshot_cache_path() -> Option<PathBuf> {
    daemon::state_dir().map(|dir| dir.join(SNAPSHOT_CACHE_FILE_NAME))
}

pub fn load_mock_env() -> Result<Option<MockEnvironment>, String> {
    let Some(path) = mock_file_path() else {
        return Ok(None);
    };
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|err| format!("parse {}: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("read {}: {err}", path.display())),
    }
}

pub fn save_mock_env(env: &MockEnvironment) -> Result<(), String> {
    let Some(path) = mock_file_path() else {
        return Err("cannot resolve KRILL_STATE_DIR".to_owned());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(env).map_err(|e| e.to_string())?;
    fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

pub fn save_cached_snapshot(snapshot: &AppStateSnapshot) -> Result<(), String> {
    let Some(path) = snapshot_cache_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(snapshot).map_err(|e| e.to_string())?;
    fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

pub fn load_cached_snapshot() -> Result<Option<AppStateSnapshot>, String> {
    let Some(path) = snapshot_cache_path() else {
        return Ok(None);
    };
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| format!("parse {}: {e}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("read {}: {err}", path.display())),
    }
}

pub fn build_permission_capabilities(
    accessibility_granted: bool,
    screenshots_granted: bool,
) -> Vec<PermissionCapability> {
    vec![
        PermissionCapability {
            id: PermissionId::Accessibility.as_str().to_owned(),
            label: PermissionId::Accessibility.label().to_owned(),
            required_for: vec![
                "getAppState".to_owned(),
                "click".to_owned(),
                "performSecondaryAction".to_owned(),
                "scroll".to_owned(),
                "drag".to_owned(),
                "typeText".to_owned(),
                "pressKey".to_owned(),
                "hotkey".to_owned(),
                "pasteText".to_owned(),
                "setValue".to_owned(),
            ],
            granted: accessibility_granted,
            can_prompt: true,
            status: if accessibility_granted {
                "granted".to_owned()
            } else {
                "denied".to_owned()
            },
            instructions: format!(
                "Enable Accessibility for Krill or your terminal host in System Settings ({ACCESSIBILITY_SETTINGS_URL})."
            ),
        },
        PermissionCapability {
            id: PermissionId::Screenshots.as_str().to_owned(),
            label: PermissionId::Screenshots.label().to_owned(),
            required_for: vec!["getAppState".to_owned()],
            granted: screenshots_granted,
            can_prompt: true,
            status: if screenshots_granted {
                "granted".to_owned()
            } else {
                "denied".to_owned()
            },
            instructions: format!(
                "Enable Screen Recording for Krill or your terminal host in System Settings ({SCREENSHOTS_SETTINGS_URL})."
            ),
        },
    ]
}

pub fn query_permissions() -> Result<(bool, bool), String> {
    if let Some(mock) = load_mock_env()? {
        return Ok((mock.accessibility_granted, mock.screenshots_granted));
    }
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        return Ok((true, true));
    }
    let script = r#"
ObjC.import("ApplicationServices");
ObjC.import("CoreGraphics");
const ax = Boolean($.AXIsProcessTrusted());
const sc = Boolean($.CGPreflightScreenCaptureAccess());
JSON.stringify({ accessibility: ax, screenshots: sc });
"#;
    let out = run_jxa(script, &[])?;
    let val: serde_json::Value =
        serde_json::from_str(&out).map_err(|e| format!("parse permission probe: {e}"))?;
    let ax = val["accessibility"].as_bool().unwrap_or(false);
    let sc = val["screenshots"].as_bool().unwrap_or(false);
    Ok((ax, sc))
}

pub fn open_permission_settings(id: PermissionId) -> Result<(), String> {
    if load_mock_env()?.is_some() || std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        return Ok(());
    }
    let status = Command::new("/usr/bin/open")
        .arg(id.settings_url())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("open {}: {e}", id.settings_url()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("open {} exited with {status}", id.settings_url()))
    }
}

pub fn capabilities_payload() -> Result<CapabilitiesPayload, String> {
    let (ax, sc) = query_permissions()?;
    let supported = cfg!(target_os = "macos");
    Ok(CapabilitiesPayload {
        platform: if supported {
            "darwin".to_owned()
        } else {
            std::env::consts::OS.to_owned()
        },
        supported,
        permissions: build_permission_capabilities(ax, sc),
        actions: vec![
            "listApps".to_owned(),
            "listWindows".to_owned(),
            "getAppState".to_owned(),
            "click".to_owned(),
            "performSecondaryAction".to_owned(),
            "scroll".to_owned(),
            "drag".to_owned(),
            "typeText".to_owned(),
            "pressKey".to_owned(),
            "hotkey".to_owned(),
            "pasteText".to_owned(),
            "setValue".to_owned(),
        ],
        notes: vec![
            "Prefer semantic element references (--element <N>) from get-app-state before falling back to coordinates.".to_owned(),
            "paste-text preserves and restores existing clipboard contents.".to_owned(),
        ],
    })
}

pub fn list_apps() -> Result<Vec<AppSummary>, String> {
    if let Some(mock) = load_mock_env()? {
        return Ok(mock
            .apps
            .into_iter()
            .map(|a| AppSummary {
                name: a.name,
                bundle_id: a.bundle_id,
                pid: a.pid,
                is_active: a.is_active,
                is_hidden: a.is_hidden,
            })
            .collect());
    }
    let script = r#"
ObjC.import("AppKit");
const apps = $.NSWorkspace.sharedWorkspace.runningApplications.js;
const rows = [];
for (const app of apps) {
  const policy = Number(app.activationPolicy);
  if (policy !== 0 && policy !== 1) continue;
  const name = ObjC.unwrap(app.localizedName) || "";
  if (!name) continue;
  rows.push({
    name,
    bundleId: ObjC.unwrap(app.bundleIdentifier) || null,
    pid: Number(app.processIdentifier),
    isActive: Boolean(app.active),
    isHidden: Boolean(app.hidden),
  });
}
JSON.stringify(rows);
"#;
    let out = run_jxa(script, &[])?;
    serde_json::from_str(&out).map_err(|e| format!("parse list-apps output: {e}"))
}

pub fn list_windows(app_selector: &str) -> Result<Vec<WindowSummary>, String> {
    if let Some(mock) = load_mock_env()? {
        let Some(app) = mock.apps.into_iter().find(|a| {
            matches_app_selector(
                &AppSummary {
                    name: a.name.clone(),
                    bundle_id: a.bundle_id.clone(),
                    pid: a.pid,
                    is_active: a.is_active,
                    is_hidden: a.is_hidden,
                },
                app_selector,
            )
        }) else {
            return Err(format!("application not found: {app_selector}"));
        };
        return Ok(app
            .windows
            .into_iter()
            .enumerate()
            .map(|(idx, w)| WindowSummary {
                id: w.id,
                index: idx + 1,
                title: w.title,
                app_name: app.name.clone(),
                bundle_id: app.bundle_id.clone(),
                pid: app.pid,
                bounds: w.bounds,
                is_focused: w.is_focused,
                is_main: w.is_main,
                is_minimized: w.is_minimized,
            })
            .collect());
    }
    let apps = list_apps()?;
    let Some(app) = apps
        .into_iter()
        .find(|a| matches_app_selector(a, app_selector))
    else {
        return Err(format!("application not found: {app_selector}"));
    };
    let script = r#"
ObjC.import("CoreGraphics");
const targetPid = Number($.NSProcessInfo.processInfo.arguments.js[4]);
const targetName = ObjC.unwrap($.NSProcessInfo.processInfo.arguments.js[5]);
const targetBundle = ObjC.unwrap($.NSProcessInfo.processInfo.arguments.js[6]) || null;
const list = ObjC.deepUnwrap($.CGWindowListCopyWindowInfo(1, 0)) || [];
const rows = [];
let idx = 1;
for (const w of list) {
  if (Number(w.kCGWindowOwnerPID) !== targetPid) continue;
  if (Number(w.kCGWindowLayer || 0) !== 0) continue;
  const b = w.kCGWindowBounds || { X: 0, Y: 0, Width: 0, Height: 0 };
  rows.push({
    id: String(w.kCGWindowNumber || idx),
    index: idx,
    title: String(w.kCGWindowName || targetName),
    appName: targetName,
    bundleId: targetBundle,
    pid: targetPid,
    bounds: {
      x: Number(b.X || 0),
      y: Number(b.Y || 0),
      width: Number(b.Width || 0),
      height: Number(b.Height || 0),
    },
    isFocused: idx === 1,
    isMain: idx === 1,
    isMinimized: false,
  });
  idx += 1;
}
JSON.stringify(rows);
"#;
    let out = run_jxa(
        script,
        &[
            &app.pid.to_string(),
            &app.name,
            app.bundle_id.as_deref().unwrap_or(""),
        ],
    )?;
    serde_json::from_str(&out).map_err(|e| format!("parse list-windows output: {e}"))
}

pub fn get_app_state(
    app_selector: &str,
    window_id: Option<&str>,
    window_index: Option<usize>,
    restore_window: bool,
    no_screenshot: bool,
    context_note: Option<String>,
) -> Result<AppStateSnapshot, String> {
    if let Some(mut mock) = load_mock_env()? {
        if !mock.accessibility_granted {
            return Err(format!(
                "accessibility permission denied; enable in System Settings ({ACCESSIBILITY_SETTINGS_URL})"
            ));
        }
        let Some(app_idx) = mock.apps.iter().position(|a| {
            matches_app_selector(
                &AppSummary {
                    name: a.name.clone(),
                    bundle_id: a.bundle_id.clone(),
                    pid: a.pid,
                    is_active: a.is_active,
                    is_hidden: a.is_hidden,
                },
                app_selector,
            )
        }) else {
            return Err(format!("application not found: {app_selector}"));
        };
        let app_entry = &mut mock.apps[app_idx];
        if restore_window {
            app_entry.is_hidden = false;
            for w in &mut app_entry.windows {
                w.is_minimized = false;
            }
        }
        let app_summary = AppSummary {
            name: app_entry.name.clone(),
            bundle_id: app_entry.bundle_id.clone(),
            pid: app_entry.pid,
            is_active: app_entry.is_active,
            is_hidden: app_entry.is_hidden,
        };
        let win_pos = if let Some(id) = window_id {
            Some(
                app_entry
                    .windows
                    .iter()
                    .position(|w| w.id == id)
                    .ok_or_else(|| format!("window id not found: {id}"))?,
            )
        } else if let Some(idx) = window_index {
            let zero_based = idx
                .checked_sub(1)
                .ok_or_else(|| "window index must be >= 1".to_owned())?;
            if zero_based >= app_entry.windows.len() {
                return Err(format!("window index out of range: {idx}"));
            }
            Some(zero_based)
        } else if app_entry.windows.is_empty() {
            None
        } else {
            Some(0)
        };

        let mut notes = Vec::new();
        if let Some(note) = context_note {
            notes.push(note);
        }
        let (window_summary, root_node, flat, focused_id, total_count, tree_text, screenshot) =
            if let Some(pos) = win_pos {
                let win = &mut app_entry.windows[pos];
                let mut root = win.root.clone();
                let (flat, focused_id, total_count, tree_text) = index_and_render_tree(&mut root);
                win.root = root.clone();
                let summary = WindowSummary {
                    id: win.id.clone(),
                    index: pos + 1,
                    title: win.title.clone(),
                    app_name: app_summary.name.clone(),
                    bundle_id: app_summary.bundle_id.clone(),
                    pid: app_summary.pid,
                    bounds: win.bounds,
                    is_focused: win.is_focused,
                    is_main: win.is_main,
                    is_minimized: win.is_minimized,
                };
                let shot = if no_screenshot || !mock.screenshots_granted {
                    if !no_screenshot && !mock.screenshots_granted {
                        notes.push("screenshot skipped: screenshots permission denied".to_owned());
                    }
                    None
                } else {
                    let shot_path = write_mock_screenshot(&win.id)?;
                    Some(ScreenshotMetadata {
                        path: shot_path.display().to_string(),
                        width: clamp_pixel_dim(win.bounds.width),
                        height: clamp_pixel_dim(win.bounds.height),
                        scale_factor: 2.0,
                        window_id: Some(win.id.clone()),
                    })
                };
                (
                    Some(summary),
                    Some(root),
                    flat,
                    focused_id,
                    total_count,
                    tree_text,
                    shot,
                )
            } else {
                notes.push("application has no open windows".to_owned());
                (None, None, Vec::new(), None, 0, String::new(), None)
            };

        save_mock_env(&mock)?;
        let snapshot = AppStateSnapshot {
            snapshot_id: format!("snap-{}", app_summary.pid),
            captured_at: "2026-10-17T00:00:00Z".to_owned(),
            app: app_summary,
            window: window_summary,
            focused_element_id: focused_id,
            element_count: total_count,
            interactive_count: flat.len(),
            tree_text,
            elements: flat,
            root: root_node,
            screenshot,
            notes,
        };
        save_cached_snapshot(&snapshot)?;
        return Ok(snapshot);
    }

    let (ax, sc) = query_permissions()?;
    if !ax {
        return Err(format!(
            "accessibility permission denied; enable in System Settings ({ACCESSIBILITY_SETTINGS_URL})"
        ));
    }
    let apps = list_apps()?;
    let Some(app) = apps
        .into_iter()
        .find(|a| matches_app_selector(a, app_selector))
    else {
        return Err(format!("application not found: {app_selector}"));
    };
    let windows = list_windows(app_selector)?;
    let selected_window = if let Some(id) = window_id {
        Some(
            windows
                .into_iter()
                .find(|w| w.id == id)
                .ok_or_else(|| format!("window id not found: {id}"))?,
        )
    } else if let Some(idx) = window_index {
        Some(
            windows
                .into_iter()
                .find(|w| w.index == idx)
                .ok_or_else(|| format!("window index out of range: {idx}"))?,
        )
    } else {
        windows.into_iter().next()
    };

    let mut root = query_live_window_ax_tree(&app, selected_window.as_ref(), restore_window)?;
    let (flat, focused_id, total_count, tree_text) = index_and_render_tree(&mut root);
    let mut notes = Vec::new();
    if let Some(note) = context_note {
        notes.push(note);
    }
    let screenshot = if no_screenshot || !sc {
        None
    } else if let Some(win) = &selected_window {
        capture_live_window_screenshot(win).ok()
    } else {
        None
    };
    let snapshot = AppStateSnapshot {
        snapshot_id: format!("snap-{}", app.pid),
        captured_at: "live".to_owned(),
        app,
        window: selected_window,
        focused_element_id: focused_id,
        element_count: total_count,
        interactive_count: flat.len(),
        tree_text,
        elements: flat,
        root: Some(root),
        screenshot,
        notes,
    };
    save_cached_snapshot(&snapshot)?;
    Ok(snapshot)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "window bounds dimensions are clamped to >= 1.0 before converting to u32 pixels"
)]
fn clamp_pixel_dim(v: f64) -> u32 {
    v.max(1.0) as u32
}

fn write_mock_screenshot(window_id: &str) -> Result<PathBuf, String> {
    let dir = daemon::state_dir().ok_or_else(|| "cannot resolve KRILL_STATE_DIR".to_owned())?;
    let shots = dir.join("screenshots");
    fs::create_dir_all(&shots).map_err(|e| format!("create {}: {e}", shots.display()))?;
    let path = shots.join(format!("window-{window_id}.png"));
    fs::write(&path, b"\x89PNG\r\n\x1a\n").map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

fn capture_live_window_screenshot(win: &WindowSummary) -> Result<ScreenshotMetadata, String> {
    let dir = daemon::state_dir().unwrap_or_else(std::env::temp_dir);
    let shots = dir.join("screenshots");
    fs::create_dir_all(&shots).map_err(|e| format!("create {}: {e}", shots.display()))?;
    let path = shots.join(format!("window-{}.png", win.id));
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-o", "-l", &win.id])
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("screencapture: {e}"))?;
    if !status.success() {
        return Err(format!("screencapture exited with {status}"));
    }
    Ok(ScreenshotMetadata {
        path: path.display().to_string(),
        width: clamp_pixel_dim(win.bounds.width),
        height: clamp_pixel_dim(win.bounds.height),
        scale_factor: 2.0,
        window_id: Some(win.id.clone()),
    })
}

fn query_live_window_ax_tree(
    app: &AppSummary,
    win: Option<&WindowSummary>,
    restore_window: bool,
) -> Result<ElementNode, String> {
    let script = r#"
const targetName = ObjC.unwrap($.NSProcessInfo.processInfo.arguments.js[4]);
const restore = ObjC.unwrap($.NSProcessInfo.processInfo.arguments.js[5]) === "1";
const se = Application("System Events");
const proc = se.processes.byName(targetName);
if (restore) {
  try { proc.visible = true; } catch (_) {}
}
const winBounds = { x: 0, y: 0, width: 800, height: 600 };
let children = [];
try {
  const wins = proc.windows();
  if (wins.length > 0) {
    const w = wins[0];
    const pos = w.position();
    const sz = w.size();
    winBounds.x = Number(pos[0] || 0);
    winBounds.y = Number(pos[1] || 0);
    winBounds.width = Number(sz[0] || 800);
    winBounds.height = Number(sz[1] || 600);
    const elems = w.uiElements();
    for (let i = 0; i < Math.min(elems.length, 40); i++) {
      const el = elems[i];
      let role = "AXGroup";
      let title = null;
      let val = null;
      let b = null;
      try { role = String(el.role() || "AXGroup"); } catch (_) {}
      try { title = el.title() ? String(el.title()) : (el.description() ? String(el.description()) : null); } catch (_) {}
      try { val = el.value() !== null && el.value() !== undefined ? String(el.value()) : null; } catch (_) {}
      try {
        const p = el.position();
        const s = el.size();
        b = { x: Number(p[0] || 0), y: Number(p[1] || 0), width: Number(s[0] || 0), height: Number(s[1] || 0) };
      } catch (_) {}
      children.push({
        role,
        title,
        value: val,
        bounds: b,
        enabled: true,
        focused: false,
        selected: false,
        actions: ["AXPress"],
        children: [],
      });
    }
  }
} catch (_) {}
JSON.stringify({
  role: "AXWindow",
  title: targetName,
  bounds: winBounds,
  enabled: true,
  focused: true,
  selected: false,
  actions: [],
  children,
});
"#;
    let out = run_jxa(script, &[&app.name, if restore_window { "1" } else { "0" }])?;
    let mut node: ElementNode =
        serde_json::from_str(&out).map_err(|e| format!("parse AX tree: {e}"))?;
    if let Some(w) = win {
        node.title = Some(w.title.clone());
        node.bounds = Some(w.bounds);
    }
    Ok(node)
}

pub fn read_system_clipboard() -> Result<String, String> {
    let out = Command::new("/usr/bin/pbpaste")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("pbpaste: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn write_system_clipboard(text: &str) -> Result<(), String> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("pbcopy: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("pbcopy write: {e}"))?;
    }
    let status = child.wait().map_err(|e| format!("pbcopy wait: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("pbcopy exited with {status}"))
    }
}

pub fn run_jxa(script: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("/usr/bin/osascript");
    cmd.args(["-l", "JavaScript", "-e", script]);
    for arg in args {
        cmd.arg(arg);
    }
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("osascript: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
