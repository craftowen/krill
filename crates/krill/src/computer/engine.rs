// Portions of the computer-use runtime and macOS execution backend are adapted from Orca
// (.ref/orca/src/cli/handlers/computer.ts, .ref/orca/native/computer-use-macos/):
// Copyright (c) 2026 Lovecast Inc. Licensed under the MIT License (.ref/orca/LICENSE).

//! `krill computer` 스냅샷 상태 캐시, 모의 환경, macOS 실행 백엔드.

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;

use krill_proto::daemon;

pub use super::model::*;

mod capture;
mod native;
mod retention;

const MOCK_FILE_NAME: &str = "computer-mock.json";
const SNAPSHOT_CACHE_FILE_NAME: &str = "computer-state.json";

pub fn mock_file_path() -> Option<PathBuf> {
    daemon::state_dir().map(|dir| dir.join(MOCK_FILE_NAME))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn unique_token() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        now_ms(),
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Scope is derived from the actual parent process, never --session or environment claims.
pub fn caller_scope() -> String {
    static SCOPE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SCOPE.get_or_init(compute_caller_scope).clone()
}

fn compute_caller_scope() -> String {
    use std::hash::{Hash, Hasher};
    #[cfg(unix)]
    let parent = rustix::process::Pid::as_raw(rustix::process::getppid());
    #[cfg(not(unix))]
    let parent = std::process::id();
    let mut command = Command::new("/bin/ps");
    command.args(["-p", &parent.to_string(), "-o", "lstart="]);
    let witness = run_managed(command)
        .ok()
        .filter(|out| matches!(out.end, super::lifecycle::End::Exited(status) if status.success()))
        .map(|out| out.stdout.bytes);
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    parent.hash(&mut hash);
    witness.hash(&mut hash);
    format!("caller-{:016x}", hash.finish())
}

fn snapshot_dir() -> Option<PathBuf> {
    daemon::state_dir().map(|dir| dir.join("computer-snapshots").join(caller_scope()))
}

pub fn snapshot_cache_path() -> Option<PathBuf> {
    snapshot_dir().map(|dir| dir.join(SNAPSHOT_CACHE_FILE_NAME))
}

fn write_private_new(path: &std::path::Path, body: &[u8]) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    file.write_all(body)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())
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
    let Some(dir) = snapshot_dir() else {
        return Err("cannot resolve snapshot cache".to_owned());
    };
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let body = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
    write_private_new(&dir.join(format!("{}.json", snapshot.snapshot_id)), &body)?;
    let temporary = dir.join(format!("latest-{}.tmp", unique_token()));
    write_private_new(&temporary, &body)?;
    fs::rename(&temporary, dir.join(SNAPSHOT_CACHE_FILE_NAME)).map_err(|e| e.to_string())?;
    // Expired immutable observations cannot be used for input. Bound retained history.
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("snap-"))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let remove_count = entries.len().saturating_sub(128);
    for entry in entries.into_iter().take(remove_count) {
        fs::remove_file(entry.path()).ok();
    }
    Ok(())
}

pub fn load_snapshot(id: Option<&str>) -> Result<Option<AppStateSnapshot>, String> {
    let path = if let Some(id) = id {
        if !id.starts_with("snap-") || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err("invalid snapshot token".to_owned());
        }
        snapshot_dir().map(|dir| dir.join(format!("{id}.json")))
    } else {
        snapshot_cache_path()
    };
    let Some(path) = path else {
        return Ok(None);
    };
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let snapshot: AppStateSnapshot =
                serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            if snapshot.caller_scope != caller_scope() {
                return Err("snapshot caller scope mismatch".to_owned());
            }
            Ok(Some(snapshot))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("read {}: {err}", path.display())),
    }
}

pub fn load_cached_snapshot() -> Result<Option<AppStateSnapshot>, String> {
    load_snapshot(None)
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

/// Missing JXA preflight symbols cannot prove a permission grant. Fail closed;
/// an actual capture failure is separately reported in observation notes.
pub const PERMISSION_PROBE_JXA_SCRIPT: &str = r#"
ObjC.import("ApplicationServices");
ObjC.import("CoreGraphics");
let accessibility = false;
try {
  if (typeof $.AXIsProcessTrusted === "function") {
    accessibility = Boolean($.AXIsProcessTrusted());
  }
} catch (_) {}
let screenshots = false;
try {
  // BridgeSupport omits this newer symbol on some macOS versions. Bind its
  // documented no-prompt signature instead of mistaking missing metadata for denial.
  if (typeof $.CGPreflightScreenCaptureAccess !== "function") {
    ObjC.bindFunction("CGPreflightScreenCaptureAccess", ["bool", []]);
  }
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
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg(id.settings_url());
    let finished = run_managed(cmd)?;
    if matches!(finished.end, super::lifecycle::End::Exited(status) if status.success()) {
        Ok(())
    } else {
        Err(format!(
            "open {} failed: {:?}",
            id.settings_url(),
            finished.end
        ))
    }
}

pub fn capabilities_payload() -> Result<CapabilitiesPayload, String> {
    let (ax, sc) = query_permissions()?;
    let supported = cfg!(target_os = "macos");
    let guide = krill_proto::orchestration::builtin_skills()
        .iter()
        .find(|skill| skill.name == "computer-use")
        .map_or("", |skill| skill.content);
    // Version freshness only, not an authentication/signature primitive.
    let hash = guide.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    Ok(CapabilitiesPayload {
        contract: "computer/2".to_owned(), backend: "jxa".to_owned(),
        guide_hash: format!("fnv1a64:{hash:016x}"), batch_supported: false,
        request_dedup_supported: false, status_supported: false, cancel_supported: false,
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
            "References are immutable: pass --snapshot <snapshotId>; input never retries an uncertain mutation.".to_owned(),
            "Paste refuses when full clipboard preservation or receiver confirmation is unavailable.".to_owned(),
            "Experimental persistent MCP: krill computer serve --experimental (MCP 2025-11-25); native features are reported by computer_session, not these JXA flags.".to_owned(),
            format!("Completed captures are immutable private artifacts, pruned to {} files / {} bytes / {} seconds; an evicted image must be captured again.", retention::MAX_IMAGES, retention::MAX_BYTES, retention::MAX_AGE.as_secs()),
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
    list_windows_for_app(&app, false)
}

pub fn resolve_live_app(app_selector: &str) -> Result<AppSummary, String> {
    let apps = list_apps()?;
    let mut matches = apps
        .into_iter()
        .filter(|app| matches_app_selector(app, app_selector));
    let app = matches
        .next()
        .ok_or_else(|| format!("application not found: {app_selector}"))?;
    if matches.next().is_some() {
        return Err("application selector is ambiguous; use pid:N".to_owned());
    }
    Ok(app)
}

/// `include_offscreen` (restore only) lists hidden/minimized windows so they can be restored.
fn list_windows_for_app(
    app: &AppSummary,
    include_offscreen: bool,
) -> Result<Vec<WindowSummary>, String> {
    let script = r#"
ObjC.import("CoreGraphics");
function run(argv) {
  const targetPid = Number(argv[0] || 0);
  const targetName = String(argv[1] || "");
  const targetBundle = argv[2] ? String(argv[2]) : null;
  const option = argv[3] === "all" ? 0 : 1;
  let list = [];
  try {
    const rawRef = $.CGWindowListCopyWindowInfo(option, 0);
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
      title: String(w.kCGWindowName || ""),
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
            if include_offscreen { "all" } else { "onscreen" },
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
    let snapshot = observe_app_state(
        app_selector,
        window_id,
        window_index,
        restore_window,
        no_screenshot,
        context_note,
    )?;
    save_cached_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub fn observe_app_state(
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
            verify_target_app(&app_entry.summary())?;
        }
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
                    Some(capture::mock_screenshot(&summary)?)
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
            snapshot_id: format!("snap-{}", unique_token()),
            process_start: format!("mock:{}", app_summary.pid),
            caller_scope: caller_scope(),
            captured_at_ms: now_ms(),
            coverage_truncated: false,
            captured_at: now_ms().to_string(),
            ax_observation_timing: None,
            geometry: None,
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
        return Ok(snapshot);
    }

    let (ax, sc) = query_permissions()?;
    if !ax {
        return Err(format!(
            "accessibility permission denied; enable in System Settings ({ACCESSIBILITY_SETTINGS_URL})"
        ));
    }
    let app = resolve_live_app(app_selector)?;
    // The native worker has no window restore or capture geometry; those requests stay on JXA.
    let use_native = native::enabled()
        && window_index.is_none()
        && !restore_window
        && (no_screenshot || !sc)
        && window_id.is_none_or(native::is_native_window);
    let (selected_window, observation, ax_observation_timing) = if use_native {
        let ax_started = capture::clock();
        let (window, observation) = native::observe(&app, window_id)?;
        (Some(window), observation, Some(capture::timing(ax_started)))
    } else {
        let windows = list_windows_for_app(&app, restore_window)?;
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

        if restore_window {
            verify_target_app(&app)?;
        }
        let ax_started = capture::clock();
        let observation =
            query_live_window_ax_tree(&app, selected_window.as_ref(), restore_window)?;
        (
            selected_window,
            observation,
            Some(capture::timing(ax_started)),
        )
    };
    let LiveObservation {
        mut root,
        process_start,
        coverage_truncated,
        geometry,
        geometry_error,
    } = observation;
    let (flat, focused_id, total_count, tree_text) = index_and_render_tree(&mut root);
    let mut notes = Vec::new();
    if coverage_truncated {
        notes.push(
            "AX coverage is partial; absent elements are not proof of disappearance".to_owned(),
        );
    }
    if let Some(note) = context_note {
        notes.push(note);
    }
    if let Some(error) = geometry_error {
        notes.push(format!(
            "geometry witness unavailable: {error}; image mapping unavailable"
        ));
    }
    let screenshot = if no_screenshot || !sc {
        if !no_screenshot && !sc {
            notes.push("screenshot permission was not confirmed; capture skipped".to_owned());
        }
        None
    } else if let (Some(win), Some(geometry)) = (&selected_window, &geometry) {
        match capture::live_screenshot(win, geometry) {
            Ok(shot) => Some(shot),
            Err(error) => {
                notes.push(format!("screenshot failed: {error}"));
                None
            }
        }
    } else {
        None
    };
    let snapshot = AppStateSnapshot {
        snapshot_id: format!("snap-{}", unique_token()),
        process_start,
        caller_scope: caller_scope(),
        captured_at_ms: now_ms(),
        coverage_truncated,
        captured_at: now_ms().to_string(),
        ax_observation_timing,
        geometry,
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
    Ok(snapshot)
}

/// Read-only unless `restore` is set; then restore is a leased, postcondition-checked mutation.
pub const LIVE_WINDOW_AX_JXA_SCRIPT: &str = concat!(
    include_str!("engine/scripts/common.js"),
    include_str!("engine/capture/geometry.js"),
    include_str!("engine/scripts/observe.js")
);

struct LiveObservation {
    root: ElementNode,
    process_start: String,
    coverage_truncated: bool,
    geometry: Option<GeometryWitness>,
    geometry_error: Option<String>,
}

fn query_live_window_ax_tree(
    app: &AppSummary,
    win: Option<&WindowSummary>,
    restore_window: bool,
) -> Result<LiveObservation, String> {
    let win = win.ok_or("application has no observed window")?;
    let payload = serde_json::json!({"pid":app.pid,"bundleId":app.bundle_id,"winId":win.id,
        "winTitle":win.title,"winBounds":win.bounds,"restore":restore_window});
    let raw = run_jxa(LIVE_WINDOW_AX_JXA_SCRIPT, &[&payload.to_string()])?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse AX observation: {e}"))?;
    let node: ElementNode = serde_json::from_value(
        value
            .get("root")
            .cloned()
            .ok_or("AX observation missing root")?,
    )
    .map_err(|e| format!("parse AX root: {e}"))?;
    let start = value
        .get("processStart")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("process start witness unavailable")?
        .to_owned();
    let (geometry, geometry_error) = match capture::observation_geometry(&value, win) {
        Ok(geometry) => (Some(geometry), None),
        Err(error) => (None, Some(error)),
    };
    Ok(LiveObservation {
        root: node,
        process_start: start,
        coverage_truncated: value
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        geometry,
        geometry_error,
    })
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
        target_ref: Option<usize>,
        point: Option<Point>,
        delta_x: f64,
        delta_y: f64,
    },
    Drag {
        from_ref: Option<usize>,
        to_ref: Option<usize>,
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
        text: String,
        target_ref: Option<usize>,
        focus_point: Option<Point>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveActionOutcome {
    #[serde(default)]
    pub issued_units: Option<u32>,
    #[serde(default)]
    pub delivery: Delivery,
    #[serde(default)]
    pub cleanup: Cleanup,
    #[serde(default)]
    pub clipboard: Option<ClipboardMetadata>,
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
            target_ref,
            point,
            delta_x,
            delta_y,
        } => serde_json::json!({
            "kind": "scroll",
            "targetRefs": target_ref.iter().collect::<Vec<_>>(),
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
        LiveAction::Drag {
            from_ref,
            to_ref,
            from,
            to,
        } => serde_json::json!({
            "kind": "drag",
            "targetRefs": ([from_ref, to_ref].into_iter().flatten().collect::<Vec<_>>()),
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
            text: _,
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

/// Shares `common.js` with observation so identity/value validation cannot drift.
pub const LIVE_ACTION_JXA_SCRIPT: &str = concat!(
    include_str!("engine/scripts/common.js"),
    include_str!("engine/scripts/action.js")
);

fn focused_receiver(node: &ElementNode) -> Option<&ElementNode> {
    node.children
        .iter()
        .find_map(focused_receiver)
        .or_else(|| node.focused.then_some(node))
}

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
            issued_units: Some(0),
            delivery: Delivery::NotDispatched,
            cleanup: Cleanup::NotNeeded,
            clipboard: None,
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
    verify_target_app(app)?;
    let base = action_snapshot().ok_or("no immutable action snapshot")?;
    if native::is_native_snapshot(&base) {
        // The worker takes this same session lock around its own dispatch and cannot inherit our
        // lease (its stdin is the protocol pipe). Nothing on this path issues input from the CLI.
        EXECUTION.with_borrow_mut(|slot| {
            if let Some(execution) = slot {
                execution.lease = None;
            }
        });
        return Ok(native::execute(&base, app, action));
    }
    let mut payload = build_live_action_payload(app, win, action);
    payload["processStart"] = serde_json::json!(base.process_start);
    payload["receiver"] = serde_json::json!(base.root.as_ref().and_then(focused_receiver));
    payload["baseRoot"] = serde_json::json!(base.root);
    payload["winBounds"] = serde_json::json!(win.map(|w| w.bounds));
    if let LiveAction::PasteText { text, .. } = action {
        payload["text"] = serde_json::json!(text);
    }
    let payload_str = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let raw = match run_private_action(LIVE_ACTION_JXA_SCRIPT, &payload_str) {
        Ok(raw) => raw,
        Err(error) => return Ok(LiveActionOutcome::unknown(error)),
    };
    let mut outcome: LiveActionOutcome = match serde_json::from_str(&raw) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Ok(LiveActionOutcome::unknown(format!(
                "action started but response invalid: {error}"
            )));
        }
    };
    if outcome.delivery != Delivery::Submitted {
        outcome.ok = false;
        "unverified".clone_into(&mut outcome.status);
    }
    if !matches!(
        action,
        LiveAction::SetValue { .. } | LiveAction::PasteText { .. }
    ) && outcome.status == "verified"
    {
        "unverified".clone_into(&mut outcome.status);
        outcome.reason = Some("dispatch is not a verified business predicate".to_owned());
    }
    Ok(outcome)
}

/// Input text and private AX values never appear in child argv, even for *-stdin flags.
fn run_private_action(script: &str, payload: &str) -> Result<String, String> {
    if payload.len() > 4 * 1024 * 1024 {
        return Err("not_dispatched: action payload exceeds bound".to_owned());
    }
    let dir = snapshot_dir()
        .ok_or("cannot resolve private operation directory")?
        .join("operations");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("operation-{}.json", unique_token()));
    write_private_new(&path, payload.as_bytes())?;
    let bin = std::env::var_os("KRILL_COMPUTER_OSASCRIPT_BIN")
        .map_or_else(|| PathBuf::from("/usr/bin/osascript"), PathBuf::from);
    let mut command = Command::new(bin);
    command
        // `--` ends osascript's getopt parsing; otherwise `--request-file` is read as an option.
        .args(["-l", "JavaScript", "-e", script, "--", "--request-file"])
        .arg(&path);
    match run_managed(command) {
        Err(error) => {
            fs::remove_file(&path).ok();
            Err(error)
        }
        Ok(finished) => {
            // Do not erase an operation file while an un-reaped child may still need it.
            if finished.reaped {
                fs::remove_file(&path).ok();
            }
            decode_jxa(finished)
        }
    }
}

pub fn run_jxa(script: &str, args: &[&str]) -> Result<String, String> {
    let bin = std::env::var_os("KRILL_COMPUTER_OSASCRIPT_BIN")
        .map_or_else(|| PathBuf::from("/usr/bin/osascript"), PathBuf::from);
    let mut cmd = Command::new(bin);
    cmd.args(["-l", "JavaScript", "-e", script, "--"])
        .args(args);
    decode_jxa(run_managed(cmd)?)
}

fn decode_jxa(finished: super::lifecycle::Finished) -> Result<String, String> {
    match finished.end {
        super::lifecycle::End::Exited(status)
            if status.success() && !finished.stdout.truncated && finished.reaped =>
        {
            Ok(String::from_utf8_lossy(&finished.stdout.bytes)
                .trim()
                .to_owned())
        }
        end => Err(format!(
            "executor ended {end:?}; outputTruncated={}; outcome unknown; do not replay: {}",
            finished.stdout.truncated,
            String::from_utf8_lossy(&finished.stderr.bytes)
        )),
    }
}

struct Execution {
    lease: Option<super::lifecycle::InputLease>,
    deadline: std::time::Instant,
}

thread_local! {
    static EXECUTION: std::cell::RefCell<Option<Execution>> = const { std::cell::RefCell::new(None) };
    static ACTION_SNAPSHOT: std::cell::RefCell<Option<AppStateSnapshot>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn with_execution(
    sub: &str,
    args: &[String],
    f: impl FnOnce() -> Result<(), super::CliError>,
) -> Result<(), super::CliError> {
    use super::lifecycle::{Access, InputLease, access_for, default_lease_path};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let lease = if access_for(sub, args) == Access::Mutate {
        Some(
            InputLease::acquire(&default_lease_path(), deadline, None)
                .map_err(|e| super::CliError::Runtime(format!("not_dispatched: {e}")))?,
        )
    } else {
        None
    };
    EXECUTION.with_borrow_mut(|slot| *slot = Some(Execution { lease, deadline }));
    let result = f();
    ACTION_SNAPSHOT.with_borrow_mut(|slot| *slot = None);
    EXECUTION.with_borrow_mut(|slot| *slot = None);
    result
}

fn run_managed(cmd: Command) -> Result<super::lifecycle::Finished, String> {
    use super::lifecycle::{self, LeaseSlot, RunOpts};
    EXECUTION.with_borrow(|context| {
        let opts = RunOpts {
            lease: context
                .as_ref()
                .and_then(|ctx| ctx.lease.as_ref())
                .map(|lease| (lease, LeaseSlot::Stdin)),
            stdin: None,
            deadline: context.as_ref().map_or_else(
                || std::time::Instant::now() + std::time::Duration::from_secs(5),
                |ctx| ctx.deadline,
            ),
            cancel: None,
            max_output: 4 * 1024 * 1024,
        };
        lifecycle::run(cmd, &opts).map_err(|e| format!("not_dispatched: {e}"))
    })
}

pub fn is_native_snapshot(snapshot: &AppStateSnapshot) -> bool {
    native::is_native_snapshot(snapshot)
}

pub fn set_action_snapshot(snapshot: AppStateSnapshot) {
    ACTION_SNAPSHOT.with_borrow_mut(|slot| *slot = Some(snapshot));
}

fn action_snapshot() -> Option<AppStateSnapshot> {
    ACTION_SNAPSHOT.with_borrow(Clone::clone)
}

pub fn validate_snapshot_identity(
    before: &AppStateSnapshot,
    fresh: &AppStateSnapshot,
) -> Result<(), String> {
    if before.app.pid != fresh.app.pid
        || before.app.bundle_id != fresh.app.bundle_id
        || before.process_start.is_empty()
        || before.process_start != fresh.process_start
    {
        return Err("process instance changed; observe again".to_owned());
    }
    // A previously measured topology must be positively re-observed. Compare full
    // facts as well as the diagnostic revision; the revision is not authority.
    if let Some(original) = &before.geometry
        && fresh.geometry.as_ref() != Some(original)
    {
        return Err(
            "display topology or geometry witness changed/unavailable; observe again".to_owned(),
        );
    }
    match (&before.window, &fresh.window) {
        (Some(old), Some(new))
            if old.id == new.id && old.bounds == new.bounds && old.title == new.title => {}
        _ => return Err("window identity or geometry changed; observe again".to_owned()),
    }
    Ok(())
}

pub fn validate_mock_target(
    app: &MockApp,
    win: &MockWindow,
    target: Option<usize>,
) -> Result<(), String> {
    let base = action_snapshot().ok_or("no original snapshot")?;
    verify_target_app(&app.summary())?;
    if !app.is_active || !win.is_focused {
        return Err("foreground receiver changed".to_owned());
    }
    if base.app.pid != app.pid
        || base
            .window
            .as_ref()
            .is_none_or(|w| w.id != win.id || w.bounds != win.bounds)
    {
        return Err("original window no longer exists".to_owned());
    }
    if let Some(target) = target {
        let original = base.root.as_ref().ok_or("original AX root unavailable")?;
        validate_reference(original, &win.root, target)?;
    }
    Ok(())
}

impl LiveActionOutcome {
    pub fn unknown(reason: String) -> Self {
        Self {
            ok: false,
            status: "unverified".to_owned(),
            reason: Some(reason),
            note: None,
            issued_units: None,
            delivery: Delivery::Unknown,
            cleanup: Cleanup::Incomplete,
            clipboard: None,
        }
    }
    pub fn submitted() -> Self {
        Self {
            ok: true,
            status: "unverified".to_owned(),
            reason: Some("submitted; no predicate requested".to_owned()),
            note: None,
            issued_units: Some(1),
            delivery: Delivery::Submitted,
            cleanup: Cleanup::NotNeeded,
            clipboard: None,
        }
    }
}

pub fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n")
        || bytes.get(8..12) != Some(&13_u32.to_be_bytes())
        || bytes.get(12..16) != Some(b"IHDR")
    {
        return Err("capture did not produce a PNG with an IHDR".to_owned());
    }
    let width = bytes
        .get(16..20)
        .and_then(|v| v.try_into().ok())
        .map(u32::from_be_bytes);
    let height = bytes
        .get(20..24)
        .and_then(|v| v.try_into().ok())
        .map(u32::from_be_bytes);
    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err("invalid PNG dimensions".to_owned()),
    }
}

pub fn verify_target_app(app: &AppSummary) -> Result<(), String> {
    verify_not_self_targeted(&app.name, app.bundle_id.as_deref())?;
    if app.pid == 0 || app.pid == std::process::id() {
        return Err("self-targeting guard: invalid or executor PID".to_owned());
    }
    Ok(())
}
