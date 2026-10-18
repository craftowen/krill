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

/// `krill computer` 권한 프로브용 JXA 스크립트.
///
/// macOS 26.6 이후 일부 환경에서는 JXA `ObjC` 브리지에
/// `CGPreflightScreenCaptureAccess` 심볼이 노출되지 않아 호출 시
/// `TypeError: $.CGPreflightScreenCaptureAccess is not a function (-2700)`으로
/// 프로브 전체가 실패한다(OWE-902). 호출 전 `typeof`로 존재 여부를 확인하고,
/// 심볼이 없으면 화면 기록 권한 확인을 생략(허용으로 간주하고 진행)한다.
/// 스크린샷 실패는 호출 측에서 무시하므로(`get_app_state`의 `.ok()`),
/// 생략하고 진행해도 안전하다. 손쉬운 사용 프로브가 불가능한 경우는
/// 실패 닫힘(거부)으로 보고한다.
pub const PERMISSION_PROBE_JXA_SCRIPT: &str = r#"
ObjC.import("ApplicationServices");
ObjC.import("CoreGraphics");
let accessibility = false;
try {
  if (typeof $.AXIsProcessTrusted === "function") {
    accessibility = Boolean($.AXIsProcessTrusted());
  }
} catch (_) {}
let screenshots = true;
try {
  if (typeof $.CGPreflightScreenCaptureAccess === "function") {
    screenshots = Boolean($.CGPreflightScreenCaptureAccess());
  }
} catch (_) {}
JSON.stringify({ accessibility: accessibility, screenshots: screenshots });
"#;

/// JXA 권한 프로브 출력(`{"accessibility": bool, "screenshots": bool}`) 파싱.
pub fn parse_permission_probe_json(raw: &str) -> Result<(bool, bool), String> {
    let val: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("parse permission probe: {e}"))?;
    let accessibility = val["accessibility"].as_bool().unwrap_or(false);
    let screenshots = val["screenshots"].as_bool().unwrap_or(false);
    Ok((accessibility, screenshots))
}

pub fn query_permissions() -> Result<(bool, bool), String> {
    if let Some(mock) = load_mock_env()? {
        return Ok((mock.accessibility_granted, mock.screenshots_granted));
    }
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        return Ok((true, true));
    }
    let out = run_jxa(PERMISSION_PROBE_JXA_SCRIPT, &[])?;
    parse_permission_probe_json(&out)
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
const rawApps = $.NSWorkspace.sharedWorkspace.runningApplications;
const apps = rawApps ? (rawApps.js || []) : [];
const rows = [];
for (let i = 0; i < apps.length; i++) {
  const app = apps[i];
  const policy = Number(app.activationPolicy);
  if (policy !== 0 && policy !== 1) continue;
  let name = ObjC.unwrap(app.localizedName) || "";
  if (!name && app.executableURL) {
    try { name = ObjC.unwrap(app.executableURL.lastPathComponent) || ""; } catch (_) {}
  }
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
    let app = resolve_live_app(app_selector)?;
    list_windows_for_app(&app)
}

pub fn resolve_live_app(app_selector: &str) -> Result<AppSummary, String> {
    let apps = list_apps()?;
    if let Some(app) = apps
        .into_iter()
        .find(|a| matches_app_selector(a, app_selector))
    {
        return Ok(app);
    }
    Err(format!("application not found: {app_selector}"))
}

fn list_windows_for_app(app: &AppSummary) -> Result<Vec<WindowSummary>, String> {
    let script = r#"
ObjC.import("CoreGraphics");
function run(argv) {
  const targetPid = Number(argv[0] || 0);
  const targetName = String(argv[1] || "");
  const targetBundle = argv[2] ? String(argv[2]) : null;
  let list = [];
  try {
    const rawRef = $.CGWindowListCopyWindowInfo(1, 0);
    if (rawRef) {
      const unwrapped = ObjC.deepUnwrap(ObjC.castRefToObject(rawRef));
      if (Array.isArray(unwrapped)) list = unwrapped;
    }
  } catch (_) {}
  const rows = [];
  let idx = 1;
  for (let i = 0; i < list.length; i++) {
    const w = list[i];
    if (!w || Number(w.kCGWindowOwnerPID) !== targetPid) continue;
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
  return JSON.stringify(rows);
}
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
    let app = resolve_live_app(app_selector)?;
    let windows = list_windows_for_app(&app)?;
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
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        let path = write_mock_screenshot(&win.id)?;
        return Ok(ScreenshotMetadata {
            path: path.display().to_string(),
            width: clamp_pixel_dim(win.bounds.width),
            height: clamp_pixel_dim(win.bounds.height),
            scale_factor: 2.0,
            window_id: Some(win.id.clone()),
        });
    }
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

pub const LIVE_WINDOW_AX_JXA_SCRIPT: &str = r#"
function run(argv) {
  const targetPid = Number(argv[0] || 0);
  const targetName = String(argv[1] || "");
  const restore = String(argv[2] || "0") === "1";
  const targetWinId = String(argv[3] || "");
  const targetWinIndex = Number(argv[4] || "0");
  const targetWinTitle = String(argv[5] || "");

  const se = Application("System Events");
  let proc = null;
  if (targetPid > 0) {
    try {
      const byPid = se.processes.whose({ unixId: targetPid });
      if (byPid && byPid.length > 0) proc = se.processes.whose({ unixId: targetPid }).first;
    } catch (_) {}
  }
  if (!proc) {
    proc = se.processes.byName(targetName);
  }
  if (restore) {
    try { proc.visible = true; } catch (_) {}
  }

  function selectTargetWindow(wins, winId, winIndex, winTitle) {
    let winCount = 0;
    try { winCount = Number(wins ? wins.length : 0); } catch (_) { return null; }
    if (!winCount || winCount <= 0) return null;
    if (winId) {
      for (let i = 0; i < winCount; i++) {
        const cand = wins[i];
        try {
          const axNum = cand.attributes.byName("AXWindowNumber").value();
          if (axNum !== null && axNum !== undefined && String(axNum) === winId) return cand;
        } catch (_) {}
        try {
          if (String(cand.id()) === winId) return cand;
        } catch (_) {}
      }
    }
    if (winIndex >= 1 && winIndex <= winCount) {
      const byIdx = wins[winIndex - 1];
      if (!winTitle) return byIdx;
      try {
        if (String(byIdx.name() || "") === winTitle) return byIdx;
      } catch (_) {}
      for (let i = 0; i < winCount; i++) {
        const cand = wins[i];
        try {
          if (String(cand.name() || "") === winTitle) return cand;
        } catch (_) {}
      }
      return byIdx;
    }
    if (winTitle) {
      for (let i = 0; i < winCount; i++) {
        const cand = wins[i];
        try {
          if (String(cand.name() || "") === winTitle) return cand;
        } catch (_) {}
      }
    }
    return wins[0];
  }

  const INTERACTIVE_ROLES = {
    AXButton: true,
    AXTextField: true,
    AXTextArea: true,
    AXCheckBox: true,
    AXRadioButton: true,
    AXPopUpButton: true,
    AXComboBox: true,
    AXMenuItem: true,
    AXLink: true,
    AXTab: true,
    AXSlider: true,
    AXRow: true,
    AXCell: true,
  };

  let budget = 160;
  function readChildren(parent, depth) {
    if (depth >= 4 || budget <= 0) return [];
    const elems = parent.uiElements;
    let roles = [];
    try { roles = elems.role(); } catch (_) { return []; }
    const count = Math.min(Array.isArray(roles) ? roles.length : 0, 40);
    if (count === 0) return [];
    let subroles = [], titles = [], descs = [], vals = [], positions = [], sizes = [], enableds = [], focuseds = [];
    try { subroles = elems.subrole(); } catch (_) {}
    try { titles = elems.title(); } catch (_) {}
    try { descs = elems.description(); } catch (_) {}
    try { vals = elems.value(); } catch (_) {}
    try { positions = elems.position(); } catch (_) {}
    try { sizes = elems.size(); } catch (_) {}
    try { enableds = elems.enabled(); } catch (_) {}
    try { focuseds = elems.focused(); } catch (_) {}
    const out = [];
    for (let i = 0; i < count && budget > 0; i++) {
      budget -= 1;
      const el = elems[i];
      const role = roles[i] ? String(roles[i]) : "AXGroup";
      const subrole = subroles[i] ? String(subroles[i]) : null;
      const title = titles[i] ? String(titles[i]) : null;
      const description = descs[i] ? String(descs[i]) : null;
      const rawVal = vals[i];
      const val = (rawVal !== null && rawVal !== undefined && (typeof rawVal === "string" || typeof rawVal === "number" || typeof rawVal === "boolean"))
        ? String(rawVal).slice(0, 500)
        : null;
      const p = positions[i];
      const s = sizes[i];
      const b = (Array.isArray(p) && Array.isArray(s))
        ? { x: Number(p[0] || 0), y: Number(p[1] || 0), width: Number(s[0] || 0), height: Number(s[1] || 0) }
        : null;
      const enabled = enableds[i] !== undefined ? Boolean(enableds[i]) : true;
      const focused = Boolean(focuseds[i]);
      const actions = INTERACTIVE_ROLES[role] ? ["AXPress"] : [];
      const children = readChildren(el, depth + 1);
      out.push({
        role,
        subrole,
        title,
        description,
        value: val,
        bounds: b,
        enabled,
        focused,
        selected: false,
        actions,
        children,
      });
    }
    return out;
  }

  let winTitle = targetWinTitle || targetName;
  let winBounds = null;
  let children = [];
  try {
    const wins = proc.windows;
    const w = selectTargetWindow(wins, targetWinId, targetWinIndex, targetWinTitle);
    if (w) {
      if (restore) {
        try { w.attributes.byName("AXMinimized").value = false; } catch (_) {}
      }
      try {
        const nm = w.name();
        if (nm && (!targetWinTitle || targetWinTitle === targetName)) winTitle = String(nm);
      } catch (_) {}
      try {
        const pos = w.position();
        const sz = w.size();
        winBounds = {
          x: Number(pos[0] || 0),
          y: Number(pos[1] || 0),
          width: Number(sz[0] || 800),
          height: Number(sz[1] || 600),
        };
      } catch (_) {}
      children = readChildren(w, 0);
    }
  } catch (_) {}
  return JSON.stringify({
    role: "AXWindow",
    title: winTitle,
    bounds: winBounds,
    enabled: true,
    focused: true,
    selected: false,
    actions: [],
    children,
  });
}
"#;

fn query_live_window_ax_tree(
    app: &AppSummary,
    win: Option<&WindowSummary>,
    restore_window: bool,
) -> Result<ElementNode, String> {
    let pid_str = app.pid.to_string();
    let effective_restore = restore_window
        && (std::env::var_os("KRILL_TEST_OFFLINE").is_none()
            || std::env::var_os("KRILL_COMPUTER_OSASCRIPT_BIN").is_some());
    let restore_str = if effective_restore { "1" } else { "0" };
    let win_id_str = win.map_or("", |w| w.id.as_str());
    let win_idx_str = win.map_or(0usize, |w| w.index).to_string();
    let win_title_str = win.map_or("", |w| w.title.as_str());
    let out = run_jxa(
        LIVE_WINDOW_AX_JXA_SCRIPT,
        &[
            &pid_str,
            &app.name,
            restore_str,
            win_id_str,
            &win_idx_str,
            win_title_str,
        ],
    )?;
    let mut node: ElementNode =
        serde_json::from_str(&out).map_err(|e| format!("parse AX tree: {e}"))?;
    if let Some(w) = win {
        if node.title.as_deref().unwrap_or("").is_empty() {
            node.title = Some(w.title.clone());
        }
        if node.bounds.is_none() {
            node.bounds = Some(w.bounds);
        }
    } else if node.bounds.is_none() {
        node.bounds = Some(Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        });
    }
    Ok(node)
}

#[derive(Debug, Clone, PartialEq)]
pub enum LiveAction {
    Click {
        target_ref: Option<usize>,
        point: Option<Point>,
        button: String,
        click_count: usize,
        modifiers: Vec<String>,
    },
    SecondaryAction {
        target_ref: usize,
        point: Option<Point>,
        action_name: String,
    },
    Scroll {
        point: Option<Point>,
        delta_x: f64,
        delta_y: f64,
    },
    Drag {
        from: Point,
        to: Point,
    },
    TypeText {
        target_ref: Option<usize>,
        focus_point: Option<Point>,
        text: String,
    },
    PressKey {
        key: String,
        modifiers: Vec<String>,
        repeat: usize,
    },
    Hotkey {
        modifiers: Vec<String>,
        key: String,
    },
    SetValue {
        target_ref: usize,
        focus_point: Option<Point>,
        value: String,
    },
    PasteText {
        target_ref: Option<usize>,
        focus_point: Option<Point>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveActionOutcome {
    pub ok: bool,
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

pub fn build_live_action_payload(
    app: &AppSummary,
    win: Option<&WindowSummary>,
    action: &LiveAction,
) -> serde_json::Value {
    let win_id = win.map(|w| w.id.clone());
    let win_index = win.map_or(0, |w| w.index);
    let win_title = win.map(|w| w.title.clone());
    match action {
        LiveAction::Click {
            target_ref,
            point,
            button,
            click_count,
            modifiers,
        } => serde_json::json!({
            "kind": "click",
            "pid": app.pid,
            "appName": app.name,
            "bundleId": app.bundle_id,
            "winId": win_id,
            "winIndex": win_index,
            "winTitle": win_title,
            "targetRef": target_ref,
            "point": point,
            "button": button,
            "clickCount": click_count,
            "modifiers": modifiers,
            "cgFlags": modifiers_to_cg_flags(modifiers),
        }),
        LiveAction::SecondaryAction {
            target_ref,
            point,
            action_name,
        } => serde_json::json!({
            "kind": "secondaryAction",
            "pid": app.pid,
            "appName": app.name,
            "bundleId": app.bundle_id,
            "winId": win_id,
            "winIndex": win_index,
            "winTitle": win_title,
            "targetRef": target_ref,
            "point": point,
            "actionName": action_name,
        }),
        LiveAction::Scroll {
            point,
            delta_x,
            delta_y,
        } => serde_json::json!({
            "kind": "scroll",
            "pid": app.pid,
            "appName": app.name,
            "bundleId": app.bundle_id,
            "winId": win_id,
            "winIndex": win_index,
            "winTitle": win_title,
            "point": point,
            "deltaX": delta_x,
            "deltaY": delta_y,
        }),
        LiveAction::Drag { from, to } => serde_json::json!({
            "kind": "drag",
            "pid": app.pid,
            "appName": app.name,
            "bundleId": app.bundle_id,
            "winId": win_id,
            "winIndex": win_index,
            "winTitle": win_title,
            "from": from,
            "to": to,
        }),
        LiveAction::TypeText {
            target_ref,
            focus_point,
            text,
        } => serde_json::json!({
            "kind": "typeText",
            "pid": app.pid,
            "appName": app.name,
            "bundleId": app.bundle_id,
            "winId": win_id,
            "winIndex": win_index,
            "winTitle": win_title,
            "targetRef": target_ref,
            "point": focus_point,
            "text": text,
        }),
        LiveAction::PressKey {
            key,
            modifiers,
            repeat,
        } => {
            let using_mods: Vec<&str> = modifiers
                .iter()
                .filter_map(|m| modifier_to_system_events_using(m))
                .collect();
            serde_json::json!({
                "kind": "pressKey",
                "pid": app.pid,
                "appName": app.name,
                "bundleId": app.bundle_id,
                "winId": win_id,
                "winIndex": win_index,
                "winTitle": win_title,
                "key": key,
                "keyCode": key_to_macos_keycode(key),
                "modifiers": modifiers,
                "usingMods": using_mods,
                "cgFlags": modifiers_to_cg_flags(modifiers),
                "repeat": repeat,
            })
        }
        LiveAction::Hotkey { modifiers, key } => {
            let using_mods: Vec<&str> = modifiers
                .iter()
                .filter_map(|m| modifier_to_system_events_using(m))
                .collect();
            serde_json::json!({
                "kind": "hotkey",
                "pid": app.pid,
                "appName": app.name,
                "bundleId": app.bundle_id,
                "winId": win_id,
                "winIndex": win_index,
                "winTitle": win_title,
                "key": key,
                "keyCode": key_to_macos_keycode(key),
                "modifiers": modifiers,
                "usingMods": using_mods,
                "cgFlags": modifiers_to_cg_flags(modifiers),
                "repeat": 1usize,
            })
        }
        LiveAction::SetValue {
            target_ref,
            focus_point,
            value,
        } => serde_json::json!({
            "kind": "setValue",
            "pid": app.pid,
            "appName": app.name,
            "bundleId": app.bundle_id,
            "winId": win_id,
            "winIndex": win_index,
            "winTitle": win_title,
            "targetRef": target_ref,
            "point": focus_point,
            "value": value,
        }),
        LiveAction::PasteText {
            target_ref,
            focus_point,
        } => serde_json::json!({
            "kind": "pasteText",
            "pid": app.pid,
            "appName": app.name,
            "bundleId": app.bundle_id,
            "winId": win_id,
            "winIndex": win_index,
            "winTitle": win_title,
            "targetRef": target_ref,
            "point": focus_point,
        }),
    }
}

pub const LIVE_ACTION_JXA_SCRIPT: &str = r#"
ObjC.import("AppKit");
ObjC.import("CoreGraphics");
ObjC.import("Foundation");

function run(argv) {
const req = JSON.parse(String(argv[0] || "{}"));
const se = Application("System Events");

function getProcess() {
  if (req.pid && Number(req.pid) > 0) {
    try {
      const targetPid = Number(req.pid);
      const byPid = se.processes.whose({ unixId: targetPid });
      if (byPid && byPid.length > 0) return se.processes.whose({ unixId: targetPid }).first;
    } catch (_) {}
  }
  return se.processes.byName(String(req.appName || ""));
}

function activateTargetApp() {
  let activated = false;
  if (req.pid && Number(req.pid) > 0) {
    try {
      const running = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(req.pid));
      if (running && !running.isNil()) {
        activated = Boolean(running.activateWithOptions(3));
        if (!activated) {
          try {
            getProcess().frontmost = true;
            activated = true;
          } catch (_) {}
        }
        return activated;
      }
    } catch (_) {}
  }
  if (!activated && req.bundleId) {
    try {
      const app = Application(String(req.bundleId));
      app.activate();
      activated = true;
    } catch (_) {}
  }
  if (!activated && req.appName) {
    try {
      const app = Application(String(req.appName));
      app.activate();
      activated = true;
    } catch (_) {}
  }
  return activated;
}

function selectTargetWindow(wins, winId, winIndex, winTitle) {
  let winCount = 0;
  try { winCount = Number(wins ? wins.length : 0); } catch (_) { return null; }
  if (!winCount || winCount <= 0) return null;
  const idStr = winId ? String(winId) : "";
  const idxNum = Number(winIndex || 0);
  const titleStr = winTitle ? String(winTitle) : "";
  if (idStr) {
    for (let i = 0; i < winCount; i++) {
      const cand = wins[i];
      try {
        const axNum = cand.attributes.byName("AXWindowNumber").value();
        if (axNum !== null && axNum !== undefined && String(axNum) === idStr) return cand;
      } catch (_) {}
      try {
        if (String(cand.id()) === idStr) return cand;
      } catch (_) {}
    }
  }
  if (idxNum >= 1 && idxNum <= winCount) {
    const byIdx = wins[idxNum - 1];
    if (!titleStr) return byIdx;
    try {
      if (String(byIdx.name() || "") === titleStr) return byIdx;
    } catch (_) {}
    for (let i = 0; i < winCount; i++) {
      const cand = wins[i];
      try {
        if (String(cand.name() || "") === titleStr) return cand;
      } catch (_) {}
    }
    return byIdx;
  }
  if (titleStr) {
    for (let i = 0; i < winCount; i++) {
      const cand = wins[i];
      try {
        if (String(cand.name() || "") === titleStr) return cand;
      } catch (_) {}
    }
  }
  return wins[0];
}

const INTERACTIVE_ROLES = {
  AXButton: true,
  AXTextField: true,
  AXTextArea: true,
  AXCheckBox: true,
  AXRadioButton: true,
  AXPopUpButton: true,
  AXComboBox: true,
  AXMenuItem: true,
  AXLink: true,
  AXTab: true,
  AXSlider: true,
  AXRow: true,
  AXCell: true,
};

function findInteractiveElementByRef(win, targetRef) {
  if (!win || !targetRef || targetRef < 1) return null;
  let budget = 160;
  let nextRef = 1;
  function visitChildren(parent, depth) {
    if (depth >= 4 || budget <= 0) return null;
    const elems = parent.uiElements;
    let roles = [];
    try { roles = elems.role(); } catch (_) { return null; }
    const count = Math.min(Array.isArray(roles) ? roles.length : 0, 40);
    if (count === 0) return null;
    for (let i = 0; i < count && budget > 0; i++) {
      budget -= 1;
      const el = elems[i];
      const role = roles[i] ? String(roles[i]) : "AXGroup";
      if (INTERACTIVE_ROLES[role]) {
        const assigned = nextRef;
        nextRef += 1;
        if (assigned === targetRef) return el;
      }
      const found = visitChildren(el, depth + 1);
      if (found) return found;
    }
    return null;
  }
  return visitChildren(win, 0);
}

function postMouseClick(pt, button, clickCount, cgFlags) {
  if (!pt) return false;
  const loc = { x: Number(pt.x), y: Number(pt.y) };
  let downType = 1;
  let upType = 2;
  let mouseBtn = 0;
  if (button === "right") {
    downType = 3;
    upType = 4;
    mouseBtn = 1;
  } else if (button === "middle") {
    downType = 25;
    upType = 26;
    mouseBtn = 2;
  }
  const count = Math.max(1, Number(clickCount || 1));
  const flags = Number(cgFlags || 0);
  for (let i = 1; i <= count; i++) {
    const downEv = $.CGEventCreateMouseEvent(null, downType, loc, mouseBtn);
    const upEv = $.CGEventCreateMouseEvent(null, upType, loc, mouseBtn);
    if (!downEv || !upEv) return false;
    $.CGEventSetIntegerValueField(downEv, 1, i);
    $.CGEventSetIntegerValueField(upEv, 1, i);
    if (flags > 0) {
      $.CGEventSetFlags(downEv, flags);
      $.CGEventSetFlags(upEv, flags);
    }
    $.CGEventPost(0, downEv);
    $.CGEventPost(0, upEv);
  }
  return true;
}

function focusTargetElementIfAny() {
  if (!req.targetRef) return true;
  try {
    const proc = getProcess();
    const w = selectTargetWindow(proc.windows, req.winId, req.winIndex, req.winTitle);
    const el = findInteractiveElementByRef(w, Number(req.targetRef));
    if (el) {
      try {
        el.focused = true;
        return true;
      } catch (_) {}
    }
  } catch (_) {}
  if (req.point) {
    return postMouseClick(req.point, "left", 1, 0);
  }
  return false;
}

function executeRequest() {
  activateTargetApp();
  const kind = String(req.kind || "");

  if (kind === "click") {
    const isSimpleLeft = (!req.button || req.button === "left") &&
      Number(req.clickCount || 1) === 1 &&
      (!req.modifiers || req.modifiers.length === 0);
    if (req.targetRef && isSimpleLeft) {
      try {
        const proc = getProcess();
        const w = selectTargetWindow(proc.windows, req.winId, req.winIndex, req.winTitle);
        const el = findInteractiveElementByRef(w, Number(req.targetRef));
        if (el) {
          try { el.focused = true; } catch (_) {}
          try {
            el.actions.byName("AXPress").perform();
            return { ok: true, status: "verified", reason: null, note: "performed AXPress" };
          } catch (_) {}
        }
      } catch (_) {}
    }
    if (req.point && postMouseClick(req.point, req.button || "left", req.clickCount || 1, req.cgFlags || 0)) {
      return { ok: true, status: "verified", reason: null, note: "posted CGEvent click" };
    }
    return {
      ok: false,
      status: "unverified",
      reason: "target element could not be clicked via AXPress or CGEvent coordinates",
      note: null,
    };
  }

  if (kind === "secondaryAction") {
    const actionName = String(req.actionName || "AXShowMenu");
    try {
      const proc = getProcess();
      const w = selectTargetWindow(proc.windows, req.winId, req.winIndex, req.winTitle);
      const el = findInteractiveElementByRef(w, Number(req.targetRef));
      if (el) {
        try { el.focused = true; } catch (_) {}
        el.actions.byName(actionName).perform();
        return { ok: true, status: "verified", reason: null, note: "performed " + actionName };
      }
    } catch (_) {}
    if (actionName === "AXShowMenu" && req.point && postMouseClick(req.point, "right", 1, 0)) {
      return { ok: true, status: "verified", reason: null, note: "fallback right-click for AXShowMenu" };
    }
    return {
      ok: false,
      status: "unverified",
      reason: "failed to perform secondary AX action " + actionName + " on element #" + req.targetRef,
      note: null,
    };
  }

  if (kind === "scroll") {
    if (req.point) {
      const moveEv = $.CGEventCreateMouseEvent(null, 5, { x: Number(req.point.x), y: Number(req.point.y) }, 0);
      if (moveEv) $.CGEventPost(0, moveEv);
    }
    const dy = Math.round(Number(req.deltaY || 0));
    const dx = Math.round(Number(req.deltaX || 0));
    const scrollEv = $.CGEventCreateScrollWheelEvent(null, 0, 2, dy, dx);
    if (!scrollEv) {
      return { ok: false, status: "unverified", reason: "failed to create CGEvent scroll event", note: null };
    }
    $.CGEventPost(0, scrollEv);
    return { ok: true, status: "verified", reason: null, note: "posted CGEvent scroll" };
  }

  if (kind === "drag") {
    const fromPt = { x: Number(req.from.x), y: Number(req.from.y) };
    const toPt = { x: Number(req.to.x), y: Number(req.to.y) };
    const moveEv = $.CGEventCreateMouseEvent(null, 5, fromPt, 0);
    const downEv = $.CGEventCreateMouseEvent(null, 1, fromPt, 0);
    if (!moveEv || !downEv) {
      return { ok: false, status: "unverified", reason: "failed to create CGEvent drag start", note: null };
    }
    $.CGEventPost(0, moveEv);
    $.CGEventPost(0, downEv);
    const steps = 5;
    for (let s = 1; s <= steps; s++) {
      const ratio = s / steps;
      const stepPt = {
        x: fromPt.x + (toPt.x - fromPt.x) * ratio,
        y: fromPt.y + (toPt.y - fromPt.y) * ratio,
      };
      const dragEv = $.CGEventCreateMouseEvent(null, 6, stepPt, 0);
      if (dragEv) $.CGEventPost(0, dragEv);
      $.NSThread.sleepForTimeInterval(0.01);
    }
    const upEv = $.CGEventCreateMouseEvent(null, 2, toPt, 0);
    if (!upEv) {
      return { ok: false, status: "unverified", reason: "failed to create CGEvent drag end", note: null };
    }
    $.CGEventPost(0, upEv);
    return { ok: true, status: "verified", reason: null, note: "posted CGEvent drag" };
  }

  if (kind === "typeText") {
    if (req.targetRef && !focusTargetElementIfAny()) {
      return {
        ok: false,
        status: "unverified",
        reason: "could not focus target element #" + req.targetRef + " before typing",
        note: null,
      };
    }
    try {
      const text = String(req.text || "");
      const lines = text.split(/\r?\n/);
      for (let li = 0; li < lines.length; li++) {
        if (li > 0) se.keyCode(36);
        const tabs = lines[li].split("\t");
        for (let ti = 0; ti < tabs.length; ti++) {
          if (ti > 0) se.keyCode(48);
          if (tabs[ti].length > 0) se.keystroke(tabs[ti]);
        }
      }
      return { ok: true, status: "verified", reason: null, note: "typed text via System Events" };
    } catch (err) {
      return { ok: false, status: "unverified", reason: "System Events keystroke failed: " + String(err), note: null };
    }
  }

  if (kind === "pressKey" || kind === "hotkey") {
    try {
      const repeat = Math.max(1, Number(req.repeat || 1));
      const usingMods = Array.isArray(req.usingMods) ? req.usingMods : [];
      const opts = usingMods.length > 0 ? { using: usingMods } : undefined;
      for (let r = 0; r < repeat; r++) {
        if (req.keyCode !== null && req.keyCode !== undefined) {
          if (opts) {
            se.keyCode(Number(req.keyCode), opts);
          } else {
            se.keyCode(Number(req.keyCode));
          }
        } else {
          if (opts) {
            se.keystroke(String(req.key), opts);
          } else {
            se.keystroke(String(req.key));
          }
        }
      }
      return { ok: true, status: "verified", reason: null, note: "dispatched key event via System Events" };
    } catch (err) {
      return { ok: false, status: "unverified", reason: "System Events key dispatch failed: " + String(err), note: null };
    }
  }

  if (kind === "setValue") {
    try {
      const proc = getProcess();
      const w = selectTargetWindow(proc.windows, req.winId, req.winIndex, req.winTitle);
      const el = findInteractiveElementByRef(w, Number(req.targetRef));
      if (!el) {
        return {
          ok: false,
          status: "unverified",
          reason: "element #" + req.targetRef + " not found in live window AX tree",
          note: null,
        };
      }
      try { el.focused = true; } catch (_) {}
      let updated = false;
      try {
        el.value = String(req.value);
        updated = true;
      } catch (_) {}
      if (!updated) {
        try {
          el.attributes.byName("AXValue").value = String(req.value);
          updated = true;
        } catch (_) {}
      }
      if (!updated) {
        return {
          ok: false,
          status: "unverified",
          reason: "AXValue attribute is not writable on element #" + req.targetRef,
          note: null,
        };
      }
      return { ok: true, status: "verified", reason: null, note: "updated AXValue" };
    } catch (err) {
      return { ok: false, status: "unverified", reason: "setValue failed: " + String(err), note: null };
    }
  }

  if (kind === "pasteText") {
    if (req.targetRef && !focusTargetElementIfAny()) {
      return {
        ok: false,
        status: "unverified",
        reason: "could not focus target element #" + req.targetRef + " before paste",
        note: null,
      };
    }
    try {
      se.keystroke("v", { using: "command down" });
      $.NSThread.sleepForTimeInterval(0.05);
      return { ok: true, status: "verified", reason: null, note: "pasted via Cmd+V" };
    } catch (err) {
      return { ok: false, status: "unverified", reason: "paste keystroke failed: " + String(err), note: null };
    }
  }

  return { ok: false, status: "unverified", reason: "unsupported live action kind: " + kind, note: null };
}

return JSON.stringify(executeRequest());
}
"#;

pub fn execute_live_action(
    app_selector: &str,
    resolved_app: Option<&AppSummary>,
    win: Option<&WindowSummary>,
    action: &LiveAction,
) -> Result<LiveActionOutcome, String> {
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some()
        && std::env::var_os("KRILL_COMPUTER_OSASCRIPT_BIN").is_none()
    {
        return Ok(LiveActionOutcome {
            ok: false,
            status: "unverified".to_owned(),
            reason: Some(
                "live OS action skipped because KRILL_TEST_OFFLINE is set without mock environment"
                    .to_owned(),
            ),
            note: None,
        });
    }
    let owned_app;
    let app = if let Some(a) = resolved_app {
        a
    } else {
        owned_app = resolve_live_app(app_selector)?;
        &owned_app
    };
    let payload = build_live_action_payload(app, win, action);
    let payload_str = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let raw = run_jxa(LIVE_ACTION_JXA_SCRIPT, &[&payload_str])?;
    serde_json::from_str(&raw).map_err(|e| format!("parse live action outcome: {e}"))
}

pub fn read_system_clipboard() -> Result<String, String> {
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        let Some(dir) = daemon::state_dir() else {
            return Ok(String::new());
        };
        let path = dir.join("computer-clipboard.txt");
        return match fs::read_to_string(path) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(format!("read offline clipboard: {e}")),
        };
    }
    let out = Command::new("/usr/bin/pbpaste")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("pbpaste: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn write_system_clipboard(text: &str) -> Result<(), String> {
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        let Some(dir) = daemon::state_dir() else {
            return Ok(());
        };
        fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        return fs::write(dir.join("computer-clipboard.txt"), text)
            .map_err(|e| format!("write offline clipboard: {e}"));
    }
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
    let bin = std::env::var_os("KRILL_COMPUTER_OSASCRIPT_BIN")
        .map_or_else(|| PathBuf::from("/usr/bin/osascript"), PathBuf::from);
    let mut cmd = Command::new(bin);
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
