// Portions of the computer-use runtime and macOS execution backend are adapted from Orca
// (.ref/orca/src/cli/handlers/computer.ts, .ref/orca/native/computer-use-macos/):
// Copyright (c) 2026 Lovecast Inc. Licensed under the MIT License (.ref/orca/LICENSE).

//! `krill computer` 스냅샷 상태 캐시, 모의 환경, macOS 실행 백엔드.

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::Command;

use krill_proto::daemon;

pub use super::model::*;

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
    list_windows_for_app(&app)
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
                    let shot_path = write_mock_screenshot(&win.id)?;
                    Some(ScreenshotMetadata {
                        path: shot_path.display().to_string(),
                        width: clamp_pixel_dim(win.bounds.width),
                        height: clamp_pixel_dim(win.bounds.height),
                        scale_factor: 2.0,
                        window_id: Some(win.id.clone()),
                        logical_bounds: win.bounds,
                        measured: false,
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
            snapshot_id: format!("snap-{}", unique_token()),
            process_start: format!("mock:{}", app_summary.pid),
            caller_scope: caller_scope(),
            captured_at_ms: now_ms(),
            coverage_truncated: false,
            captured_at: now_ms().to_string(),
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

    if restore_window {
        verify_target_app(&app)?;
    }
    let (mut root, process_start, coverage_truncated) =
        query_live_window_ax_tree(&app, selected_window.as_ref(), restore_window)?;
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
    let screenshot = if no_screenshot || !sc {
        if !no_screenshot && !sc {
            notes.push("screenshot permission was not confirmed; capture skipped".to_owned());
        }
        None
    } else if let Some(win) = &selected_window {
        match capture_live_window_screenshot(win) {
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

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "window bounds dimensions are clamped to >= 1.0 before converting to u32 pixels"
)]
fn clamp_pixel_dim(v: f64) -> u32 {
    v.max(1.0) as u32
}

fn write_mock_screenshot(window_id: &str) -> Result<PathBuf, String> {
    let shots = capture_directory()?;
    let path = shots.join(format!("window-{window_id}-{}.png", unique_token()));
    write_private_new(&path, b"\x89PNG\r\n\x1a\n")?;
    retention::prune(&shots, &path).map_err(|e| format!("capture retention: {e}"))?;
    Ok(path)
}

fn capture_directory() -> Result<PathBuf, String> {
    let dir = daemon::state_dir().ok_or("cannot resolve private capture state directory")?;
    let shots = dir.join("computer-captures-v2");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(&shots).map_err(|e| e.to_string())?;
    let metadata = fs::symlink_metadata(&shots).map_err(|e| e.to_string())?;
    if !metadata.is_dir() {
        return Err("capture directory is not a real directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err("capture directory must be private and owned by this user".to_owned());
        }
    }
    Ok(shots)
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
            logical_bounds: win.bounds,
            measured: false,
        });
    }
    let shots = capture_directory()?;
    let path = shots.join(format!("window-{}-{}.png", win.id, unique_token()));
    let mut cmd = Command::new("/usr/sbin/screencapture");
    cmd.args(["-x", "-o", "-l", &win.id]).arg(&path);
    let finished = run_managed(cmd)?;
    if !matches!(finished.end, super::lifecycle::End::Exited(status) if status.success())
        || !finished.reaped
    {
        if finished.reaped {
            fs::remove_file(&path).ok();
            retention::prune(&shots, &path).ok();
        }
        return Err(format!(
            "screencapture failed: {:?}; reaped={}",
            finished.end, finished.reaped
        ));
    }
    let capture = (|| -> Result<ScreenshotMetadata, String> {
        let mut file = fs::File::open(&path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        // Dimensions need only the PNG signature and IHDR, never the full pixel file.
        let mut header = [0_u8; 24];
        file.read_exact(&mut header).map_err(|e| e.to_string())?;
        let (width, height) = png_dimensions(&header)?;
        let scale = f64::from(width) / win.bounds.width;
        if !scale.is_finite()
            || scale <= 0.0
            || (f64::from(height) / win.bounds.height - scale).abs() > 0.05
        {
            return Err("screenshot transform is not a uniform measured window crop".to_owned());
        }
        retention::prune(&shots, &path).map_err(|e| format!("capture retention: {e}"))?;
        Ok(ScreenshotMetadata {
            path: path.display().to_string(),
            width,
            height,
            scale_factor: scale,
            window_id: Some(win.id.clone()),
            logical_bounds: win.bounds,
            measured: true,
        })
    })();
    if capture.is_err() {
        fs::remove_file(&path).ok();
        retention::prune(&shots, &path).ok();
    }
    capture
}

pub const LIVE_WINDOW_AX_JXA_SCRIPT: &str = r#"
ObjC.import("AppKit");
ObjC.import("ApplicationServices");
ObjC.import("CoreGraphics");
ObjC.import("Foundation");
function run(argv) {
let req;
if(String(argv[0]) === "--request-file") {
  const data=$.NSData.dataWithContentsOfFile(String(argv[1] || ""));
  if(!data || data.isNil() || Number(data.length)>4194304) throw Error("private request file unavailable or exceeds bound");
  req=JSON.parse(ObjC.unwrap($.NSString.alloc.initWithDataEncoding(data,$.NSUTF8StringEncoding)));
} else { req=JSON.parse(String(argv[0] || "{}")); }
const se = Application("System Events");
function processStart() {
  const running = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(req.pid));
  if (!running || running.isNil()) throw Error("target process exited");
  if (Number(running.processIdentifier) === Number($.NSProcessInfo.processInfo.processIdentifier)) throw Error("executor self target");
  const bundle = ObjC.unwrap(running.bundleIdentifier) || "";
  if (bundle === "dev.krill.app" || bundle === "com.krill.app") throw Error("Krill self target");
  const executable=running.executableURL ? ObjC.unwrap(running.executableURL.lastPathComponent) : "";
  if(executable === "krill-app" || executable === "Krill") throw Error("Krill executable self target");
  if (req.bundleId && bundle !== req.bundleId) throw Error("process bundle changed");
  if (!running.launchDate || running.launchDate.isNil()) throw Error("process start witness unavailable");
  return String(Number(running.launchDate.timeIntervalSince1970));
}
function getProcess() {
  const targetPid = Number(req.pid);
  if (!(targetPid > 0)) throw Error("missing process PID");
  const proc = se.processes.whose({ unixId: targetPid }).first;
  if (Number(proc.unixId()) !== targetPid) throw Error("process PID mismatch");
  return proc;
}
function rect(el) {
  const p = el.position(), z = el.size();
  return {x:Number(p[0]),y:Number(p[1]),width:Number(z[0]),height:Number(z[1])};
}
function canonical(value) {
  if(Array.isArray(value)) return value.map(canonical);
  if(value !== null && typeof value === "object") {
    const result={};Object.keys(value).sort().forEach(key=>{result[key]=canonical(value[key]);});return result;
  }
  return value;
}
function equal(a,b) { return JSON.stringify(canonical(a)) === JSON.stringify(canonical(b)); }
function selectTargetWindow(proc) {
  const wins = proc.windows;
  const names = wins.name();
  if (!Array.isArray(names) || names.length === 0) throw Error("AX windows unavailable; observation failed");
  const matches = [], geometric = [];
  for (let i=0;i<Math.min(names.length,128);i++) {
    const w=wins[i];
    let n=null;
    try { n=w.attributes.byName("AXWindowNumber").value(); } catch (_) {}
    if (n !== null && String(n) === String(req.winId)) matches.push(w);
    try {
      if ((!req.winTitle || String(names[i] || "") === req.winTitle) && equal(rect(w),req.winBounds)) geometric.push(w);
    } catch (_) {}
  }
  if (matches.length === 1) return matches[0];
  if (matches.length > 1 || geometric.length !== 1) throw Error("window correspondence missing or ambiguous");
  return geometric[0];
}
function checkCGWindow() {
  const raw = $.CGWindowListCopyWindowInfo(1,0);
  const rows = raw ? ObjC.deepUnwrap(ObjC.castRefToObject(raw)) : [];
  const matches = rows.filter(w => String(w.kCGWindowNumber) === String(req.winId) && Number(w.kCGWindowOwnerPID) === Number(req.pid));
  if (matches.length !== 1) throw Error("window identity expired");
  const b=matches[0].kCGWindowBounds;
  if (!equal({x:Number(b.X),y:Number(b.Y),width:Number(b.Width),height:Number(b.Height)},req.winBounds)) throw Error("window geometry changed");
}
const rolesInteractive={AXButton:1,AXTextField:1,AXTextArea:1,AXCheckBox:1,AXRadioButton:1,AXPopUpButton:1,AXComboBox:1,AXMenuItem:1,AXLink:1,AXTab:1,AXSlider:1,AXRow:1,AXCell:1};
let truncated=false, budget=160, nextRef=1, entries={};
function readNode(el, depth, isRoot) {
  function read(f, fallback) { try { const v=f(); return v === undefined ? fallback : v; } catch (_) { return fallback; } }
  const role=String(read(()=>el.role(),isRoot ? "AXWindow" : "AXGroup"));
  const actions=read(()=>el.actions.name(),[]).map(String);
  const raw=read(()=>el.value(),null);
  let value=(raw !== null && ["string","number","boolean"].indexOf(typeof raw)>=0) ? String(raw) : null;
  if (value !== null && value.length > 65536) throw Error("AX value exceeds bounded full observation; no lossy identity witness");
  const node={referenceId:null,role,subrole:read(()=>el.subrole(),null),title:read(()=>el.title(),null),description:read(()=>el.description(),null),value,
    bounds:read(()=>rect(el),null),enabled:Boolean(read(()=>el.enabled(),false)),focused:Boolean(read(()=>el.focused(),false)),selected:false,actions,children:[]};
  if (!isRoot && (rolesInteractive[role] || actions.length)) {node.referenceId=nextRef++;entries[node.referenceId]=el;}
  const children=el.uiElements;
  let childRoles;
  try { childRoles=children.role(); } catch (_) {truncated=true;return node;}
  if (!Array.isArray(childRoles)) {truncated=true;return node;}
  if(isRoot && childRoles.length===0) truncated=true;
  if (depth >= 4 || budget <= 0) {if (childRoles.length) truncated=true;return node;}
  const count=Math.min(childRoles.length,40);
  if (count<childRoles.length) truncated=true;
  for(let i=0;i<count;i++) {
    if(budget--<=0) {truncated=true;break;}
    node.children.push(readNode(children[i],depth+1,false));
  }
  return node;
}
function identity(n, recursive) {
  const v=[n.role,n.subrole,n.title,n.description,n.bounds,n.actions];
  if(recursive) v.push(n.children.map(c=>identity(c,true)));
  return v;
}
function validateTree(root) {
  if (!req.baseRoot || !equal(identity(root,true),identity(req.baseRoot,true))) throw Error("snapshot structure changed or reordered");
  const refs=(Array.isArray(req.targetRefs)?req.targetRefs:[]).concat(req.targetRef?[req.targetRef]:[]);
  refs.forEach(ref=>{
    let target=null,duplicates=0;
    function original(n) {if(n.referenceId===Number(ref)) target=n;n.children.forEach(original);}
    original(req.baseRoot);
    if(!target) throw Error("reference absent from immutable snapshot");
    function count(n) {if(equal(identity(n,false),identity(target,false))) duplicates++;n.children.forEach(count);}
    count(root);
    if(duplicates!==1 || !entries[ref]) throw Error("element identity missing or ambiguous");
    if(!Boolean(entries[ref].enabled())) throw Error("target element disabled");
    let actualValue=null;try {actualValue=entries[ref].value();} catch(_) {}
    const normalizedValue=actualValue!==null && ["string","number","boolean"].indexOf(typeof actualValue)>=0?String(actualValue):null;
    if(normalizedValue!==target.value) throw Error("target value changed since observation; possible semantic reuse");
  });
  return entries[req.targetRef];
}
function foreground(proc, win) {
  const front=$.NSWorkspace.sharedWorkspace.frontmostApplication;
  if(!front || Number(front.processIdentifier)!==Number(req.pid) || !Boolean(proc.frontmost())) throw Error("foreground application changed");
  const focused=proc.attributes.byName("AXFocusedWindow").value();
  if(!focused || !equal(rect(focused),req.winBounds)) throw Error("focused window changed");
}

const start=processStart();
checkCGWindow();
const proc=getProcess();
const win=selectTargetWindow(proc);
if(req.restore) {
  proc.visible=true;
  win.attributes.byName("AXMinimized").value=false;
}
const root=readNode(win,0,true);
if(root.role!=="AXWindow") throw Error("AX window not ready");
return JSON.stringify({root,processStart:start,truncated});
}
"#;

fn query_live_window_ax_tree(
    app: &AppSummary,
    win: Option<&WindowSummary>,
    restore_window: bool,
) -> Result<(ElementNode, String, bool), String> {
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
    Ok((
        node,
        start,
        value
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
    ))
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

pub const LIVE_ACTION_JXA_SCRIPT: &str = r#"
ObjC.import("AppKit");
ObjC.import("ApplicationServices");
ObjC.import("CoreGraphics");
ObjC.import("Foundation");
function run(argv) {
let req;
if(String(argv[0]) === "--request-file") {
  const data=$.NSData.dataWithContentsOfFile(String(argv[1] || ""));
  if(!data || data.isNil() || Number(data.length)>4194304) throw Error("private request file unavailable or exceeds bound");
  req=JSON.parse(ObjC.unwrap($.NSString.alloc.initWithDataEncoding(data,$.NSUTF8StringEncoding)));
} else { req=JSON.parse(String(argv[0] || "{}")); }
const se = Application("System Events");
function processStart() {
  const running = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(req.pid));
  if (!running || running.isNil()) throw Error("target process exited");
  if (Number(running.processIdentifier) === Number($.NSProcessInfo.processInfo.processIdentifier)) throw Error("executor self target");
  const bundle = ObjC.unwrap(running.bundleIdentifier) || "";
  if (bundle === "dev.krill.app" || bundle === "com.krill.app") throw Error("Krill self target");
  const executable=running.executableURL ? ObjC.unwrap(running.executableURL.lastPathComponent) : "";
  if(executable === "krill-app" || executable === "Krill") throw Error("Krill executable self target");
  if (req.bundleId && bundle !== req.bundleId) throw Error("process bundle changed");
  if (!running.launchDate || running.launchDate.isNil()) throw Error("process start witness unavailable");
  return String(Number(running.launchDate.timeIntervalSince1970));
}
function getProcess() {
  const targetPid = Number(req.pid);
  if (!(targetPid > 0)) throw Error("missing process PID");
  const proc = se.processes.whose({ unixId: targetPid }).first;
  if (Number(proc.unixId()) !== targetPid) throw Error("process PID mismatch");
  return proc;
}
function rect(el) {
  const p = el.position(), z = el.size();
  return {x:Number(p[0]),y:Number(p[1]),width:Number(z[0]),height:Number(z[1])};
}
function canonical(value) {
  if(Array.isArray(value)) return value.map(canonical);
  if(value !== null && typeof value === "object") {
    const result={};Object.keys(value).sort().forEach(key=>{result[key]=canonical(value[key]);});return result;
  }
  return value;
}
function equal(a,b) { return JSON.stringify(canonical(a)) === JSON.stringify(canonical(b)); }
function selectTargetWindow(proc) {
  const wins = proc.windows;
  const names = wins.name();
  if (!Array.isArray(names) || names.length === 0) throw Error("AX windows unavailable; observation failed");
  const matches = [], geometric = [];
  for (let i=0;i<Math.min(names.length,128);i++) {
    const w=wins[i];
    let n=null;
    try { n=w.attributes.byName("AXWindowNumber").value(); } catch (_) {}
    if (n !== null && String(n) === String(req.winId)) matches.push(w);
    try {
      if ((!req.winTitle || String(names[i] || "") === req.winTitle) && equal(rect(w),req.winBounds)) geometric.push(w);
    } catch (_) {}
  }
  if (matches.length === 1) return matches[0];
  if (matches.length > 1 || geometric.length !== 1) throw Error("window correspondence missing or ambiguous");
  return geometric[0];
}
function checkCGWindow() {
  const raw = $.CGWindowListCopyWindowInfo(1,0);
  const rows = raw ? ObjC.deepUnwrap(ObjC.castRefToObject(raw)) : [];
  const matches = rows.filter(w => String(w.kCGWindowNumber) === String(req.winId) && Number(w.kCGWindowOwnerPID) === Number(req.pid));
  if (matches.length !== 1) throw Error("window identity expired");
  const b=matches[0].kCGWindowBounds;
  if (!equal({x:Number(b.X),y:Number(b.Y),width:Number(b.Width),height:Number(b.Height)},req.winBounds)) throw Error("window geometry changed");
}
const rolesInteractive={AXButton:1,AXTextField:1,AXTextArea:1,AXCheckBox:1,AXRadioButton:1,AXPopUpButton:1,AXComboBox:1,AXMenuItem:1,AXLink:1,AXTab:1,AXSlider:1,AXRow:1,AXCell:1};
let truncated=false, budget=160, nextRef=1, entries={};
function readNode(el, depth, isRoot) {
  function read(f, fallback) { try { const v=f(); return v === undefined ? fallback : v; } catch (_) { return fallback; } }
  const role=String(read(()=>el.role(),isRoot ? "AXWindow" : "AXGroup"));
  const actions=read(()=>el.actions.name(),[]).map(String);
  const raw=read(()=>el.value(),null);
  let value=(raw !== null && ["string","number","boolean"].indexOf(typeof raw)>=0) ? String(raw) : null;
  if (value !== null && value.length > 65536) throw Error("AX value exceeds bounded full observation; no lossy identity witness");
  const node={referenceId:null,role,subrole:read(()=>el.subrole(),null),title:read(()=>el.title(),null),description:read(()=>el.description(),null),value,
    bounds:read(()=>rect(el),null),enabled:Boolean(read(()=>el.enabled(),false)),focused:Boolean(read(()=>el.focused(),false)),selected:false,actions,children:[]};
  if (!isRoot && (rolesInteractive[role] || actions.length)) {node.referenceId=nextRef++;entries[node.referenceId]=el;}
  const children=el.uiElements;
  let childRoles;
  try { childRoles=children.role(); } catch (_) {truncated=true;return node;}
  if (!Array.isArray(childRoles)) {truncated=true;return node;}
  if(isRoot && childRoles.length===0) truncated=true;
  if (depth >= 4 || budget <= 0) {if (childRoles.length) truncated=true;return node;}
  const count=Math.min(childRoles.length,40);
  if (count<childRoles.length) truncated=true;
  for(let i=0;i<count;i++) {
    if(budget--<=0) {truncated=true;break;}
    node.children.push(readNode(children[i],depth+1,false));
  }
  return node;
}
function identity(n, recursive) {
  const v=[n.role,n.subrole,n.title,n.description,n.bounds,n.actions];
  if(recursive) v.push(n.children.map(c=>identity(c,true)));
  return v;
}
function validateTree(root) {
  if (!req.baseRoot || !equal(identity(root,true),identity(req.baseRoot,true))) throw Error("snapshot structure changed or reordered");
  if (!req.targetRef) return null;
  let target=null, duplicates=0;
  function original(n) {if(n.referenceId===Number(req.targetRef)) target=n; n.children.forEach(original);}
  original(req.baseRoot);
  if(!target) throw Error("reference absent from immutable snapshot");
  function count(n) {if(equal(identity(n,false),identity(target,false))) duplicates++;n.children.forEach(count);}
  count(root);
  if(duplicates!==1 || !entries[req.targetRef]) throw Error("element identity missing or ambiguous");
  return entries[req.targetRef];
}
function foreground(proc, win) {
  const front=$.NSWorkspace.sharedWorkspace.frontmostApplication;
  if(!front || Number(front.processIdentifier)!==Number(req.pid) || !Boolean(proc.frontmost())) throw Error("foreground application changed");
  const focused=proc.attributes.byName("AXFocusedWindow").value();
  if(!focused || !equal(rect(focused),req.winBounds)) throw Error("focused window changed");
}

let issued=0, mayHaveEffect=false, cleanup="not_needed", clipboard=null;
function result(ok,status,reason,note,delivery) {
  const state=delivery || (issued ? "submitted" : "not_dispatched");
  return {ok,status,reason,note,delivery:state,issuedUnits:state==="unknown"?null:issued,cleanup,clipboard};
}
function guard(proc,win) {
  if(processStart()!==req.processStart) throw Error("process start witness changed");
  checkCGWindow();foreground(proc,win);
  if(["pressKey","hotkey","typeText","pasteText"].indexOf(String(req.kind))>=0) {
    if(req.targetRef && el) {
      if(!Boolean(el.focused())) throw Error("text receiver focus changed");
    } else {
      if(!req.receiver) throw Error("keyboard receiver was not observed");
      const receiver=proc.attributes.byName("AXFocusedUIElement").value();
      if(!receiver || String(receiver.role())!==req.receiver.role || !equal(rect(receiver),req.receiver.bounds)) throw Error("keyboard receiver changed");
      let title=null;try {title=receiver.title();} catch(_) {}
      if(title!==req.receiver.title) throw Error("keyboard receiver label changed");
    }
  }
  if(typeof $.IsSecureEventInputEnabled === "function" && $.IsSecureEventInputEnabled()) throw Error("Secure Event Input is active");
}
function issue(f) {mayHaveEffect=true;f();issued++;}
function post(event) {if(!event) throw Error("event allocation failed");issue(()=>$.CGEventPost(0,event));}
function assertPoint(pt) {
  const b=req.winBounds;
  if(!pt || !Number.isFinite(pt.x) || !Number.isFinite(pt.y) || pt.x<b.x || pt.y<b.y || pt.x>b.x+b.width || pt.y>b.y+b.height) throw Error("point outside observed window");
  // CG's front-to-back list proves the pixel receiver belongs to the selected window.
  const raw=$.CGWindowListCopyWindowInfo(1,0), rows=ObjC.deepUnwrap(ObjC.castRefToObject(raw));
  const hit=rows.find(w=> {const z=w.kCGWindowBounds;return z && Number(w.kCGWindowLayer||0)===0 && pt.x>=z.X && pt.y>=z.Y && pt.x<=z.X+z.Width && pt.y<=z.Y+z.Height;});
  if(!hit || String(hit.kCGWindowNumber)!==String(req.winId)) throw Error("pixel receiver is occluded or changed");
  // A layer-0 window census alone misses overlays, sheets and menus. Require the
  // actual AX hit to belong to the focused window already checked against our snapshot.
  const system=$.AXUIElementCreateSystemWide(), actual=Ref(), owner=Ref(), focused=Ref();
  if(Number($.AXUIElementCopyElementAtPosition(system,pt.x,pt.y,actual))!==0 ||
     Number($.AXUIElementGetPid(actual[0],owner))!==0 || Number(owner[0])!==Number(req.pid)) {
    throw Error("pixel receiver hit-test is unavailable or belongs to another application");
  }
  const app=$.AXUIElementCreateApplication(Number(req.pid));
  if(Number($.AXUIElementCopyAttributeValue(app,$("AXFocusedWindow"),focused))!==0) throw Error("focused AX window unavailable");
  let cursor=actual[0], belongs=false;
  for(let depth=0;depth<64;depth++) {
    if($.CFEqual(cursor,focused[0])) {belongs=true;break;}
    const parent=Ref();
    if(Number($.AXUIElementCopyAttributeValue(cursor,$("AXParent"),parent))!==0) break;
    if($.CFEqual(cursor,parent[0])) break;
    cursor=parent[0];
  }
  if(!belongs) throw Error("pixel receiver is outside the observed focused window");
}
function mouse(pt,button,count) {
  assertPoint(pt);
  const n=button==="right" ? 1 : button==="middle" ? 2 : 0;
  const down=n===1?3:n===2?25:1, up=n===1?4:n===2?26:2;
  for(let i=1;i<=count;i++) {
    guard(proc,win);assertPoint(pt);
    const d=$.CGEventCreateMouseEvent(null,down,pt,n),u=$.CGEventCreateMouseEvent(null,up,pt,n);
    if(!d||!u) throw Error("mouse pair allocation failed");
    $.CGEventSetIntegerValueField(d,1,i);$.CGEventSetIntegerValueField(u,1,i);
    $.CGEventSetFlags(d,Number(req.cgFlags||0));$.CGEventSetFlags(u,Number(req.cgFlags||0));
    try {post(d);} finally {post(u);}
  }
}
let proc,win,el;
try {
  if(!req.processStart || processStart()!==req.processStart) throw Error("process instance changed");
  checkCGWindow();proc=getProcess();win=selectTargetWindow(proc);
  const root=readNode(win,0,true);
  el=validateTree(root);
  if(el && !Boolean(el.enabled())) throw Error("element disabled");
  guard(proc,win);
  const kind=String(req.kind||"");
  if(kind==="click" || kind==="secondaryAction") {
    const simple=kind==="secondaryAction" || ((!req.button || req.button==="left") && Number(req.clickCount||1)===1 && (!req.modifiers||!req.modifiers.length));
    if(el && simple) {
      const name=kind==="secondaryAction" ? String(req.actionName||"AXShowMenu") : "AXPress";
      if(el.actions.name().indexOf(name)<0) throw Error("requested AX action unavailable; no coordinate fallback");
      issue(()=>el.actions.byName(name).perform());
    } else {
      if(kind==="secondaryAction") throw Error("secondary action requires original reference");
      mouse(req.point,req.button||"left",Math.min(16,Number(req.clickCount||1)));
    }
    return JSON.stringify(result(true,"unverified","action submitted; no business predicate requested","single dispatch; no fallback"));
  }
  if(kind==="setValue") {
    if(!el) throw Error("set-value requires original reference");
    issue(()=>{el.attributes.byName("AXValue").value=String(req.value);});
    // Full readback on the SAME resolved object; never use the 500-character preview.
    guard(proc,win);
    const actual=String(el.attributes.byName("AXValue").value());
    if(actual!==String(req.value)) return JSON.stringify(result(false,"unverified","full target value readback differs",null));
    return JSON.stringify(result(true,"verified",null,"full AXValue equality on resolved target"));
  }
  if(kind==="typeText" || kind==="pasteText") {
    if(!el) throw Error("text input requires an observed editable receiver");
    const role=String(el.role());
    if(["AXTextField","AXTextArea","AXComboBox"].indexOf(role)<0) throw Error("receiver is not an editable text control");
    if(!Boolean(el.focused())) throw Error("text receiver focus changed; observe or explicitly focus first");
    const beforeValue=String(el.value()||"");
    if(kind==="typeText") {
      // One System Events dispatch, no newline/tab sub-actions with unchecked receiver changes.
      if(/[\r\n\t]/.test(String(req.text))) throw Error("multiline/tab input requires set-value or paste-text");
      issue(()=>se.keystroke(String(req.text||"")));
      return JSON.stringify(result(true,"unverified","keystroke submitted; inserted text not asserted",null));
    }
    const pb=$.NSPasteboard.generalPasteboard;
    const initial=Number(pb.changeCount);
    const items=pb.pasteboardItems, saved=[];
    let size=0;
    for(let i=0;i<Number(items.count);i++) {
      const item=items.objectAtIndex(i), copy=$.NSPasteboardItem.alloc.init, types=item.types;
      for(let j=0;j<Number(types.count);j++) {
        const type=types.objectAtIndex(j), data=item.dataForType(type);
        if(!data || data.isNil()) throw Error("clipboard promised type cannot be preserved");
        size+=Number(data.length);
        if(size>4194304) throw Error("clipboard preservation exceeds bound; unchanged");
        if(!copy.setDataForType(data,type)) throw Error("clipboard type copy failed");
      }
      saved.push(copy);
    }
    if(Number(pb.changeCount)!==initial) throw Error("clipboard changed during preservation");
    guard(proc,win);
    clipboard={preserved:true,restored:false};cleanup="incomplete";mayHaveEffect=true;
    pb.clearContents;
    if(!pb.setStringForType(String(req.text||""),$.NSPasteboardTypeString)) throw Error("clipboard write failed");
    const owned=Number(pb.changeCount);
    try {
      guard(proc,win);
      if(Number(pb.changeCount)!==owned || !Boolean(el.focused())) throw Error("clipboard or receiver changed before paste");
      issue(()=>se.keystroke("v",{using:"command down"}));
      const deadline=Date.now()+750;
      let consumed=false;
      while(Date.now()<deadline) {
        if(Number(pb.changeCount)!==owned) throw Error("clipboard conflict; user contents retained");
        guard(proc,win);
        const value=String(el.value()||"");
        if(value!==beforeValue && value.indexOf(String(req.text))>=0) {consumed=true;break;}
        $.NSThread.sleepForTimeInterval(0.025);
      }
      if(!consumed) throw Error("paste consumption unconfirmed; clipboard retained for delayed receiver");
      if(Number(pb.changeCount)!==owned) throw Error("clipboard conflict before restoration");
      pb.clearContents;
      if(saved.length && !pb.writeObjects($(saved))) throw Error("clipboard restoration failed");
      clipboard.restored=true;cleanup="completed";
      return JSON.stringify(result(true,"verified",null,"receiver value changed and contains pasted text"));
    } catch(error) {return JSON.stringify(result(false,"unverified",String(error),null,issued?"submitted":"unknown"));}
  }
  if(kind==="pressKey" || kind==="hotkey") {
    const opts=Array.isArray(req.usingMods)&&req.usingMods.length ? {using:req.usingMods} : null;
    for(let i=0;i<Math.min(32,Math.max(1,Number(req.repeat||1)));i++) {
      guard(proc,win);
      if(req.keyCode!==null && req.keyCode!==undefined) issue(()=>opts ? se.keyCode(Number(req.keyCode),opts) : se.keyCode(Number(req.keyCode)));
      else issue(()=>opts ? se.keystroke(String(req.key),opts) : se.keystroke(String(req.key)));
    }
    return JSON.stringify(result(true,"unverified","key events submitted; no predicate requested",null));
  }
  if(kind==="scroll") {
    assertPoint(req.point);guard(proc,win);
    const move=$.CGEventCreateMouseEvent(null,5,req.point,0);
    const scroll=$.CGEventCreateScrollWheelEvent(null,0,2,Math.round(req.deltaY||0),Math.round(req.deltaX||0));
    if(!move||!scroll) throw Error("scroll event allocation failed");
    post(move);guard(proc,win);post(scroll);
    return JSON.stringify(result(true,"unverified","scroll submitted; no predicate requested",null));
  }
  if(kind==="drag") {
    assertPoint(req.from);assertPoint(req.to);
    const down=$.CGEventCreateMouseEvent(null,1,req.from,0), up=$.CGEventCreateMouseEvent(null,2,req.to,0);
    if(!down||!up) throw Error("drag pair allocation failed");
    cleanup="incomplete";
    try {
      post(down);
      for(let i=1;i<=5;i++) {
        guard(proc,win);
        const point={x:req.from.x+(req.to.x-req.from.x)*i/5,y:req.from.y+(req.to.y-req.from.y)*i/5};
        assertPoint(point);post($.CGEventCreateMouseEvent(null,6,point,0));
        $.NSThread.sleepForTimeInterval(0.01);
      }
    } finally {post(up);cleanup="completed";}
    return JSON.stringify(result(true,"unverified","drag submitted; no predicate requested",null));
  }
  throw Error("unsupported action");
} catch(error) {
  return JSON.stringify(result(false,"unverified",String(error),"no mutation replay or backend fallback",mayHaveEffect?"unknown":"not_dispatched"));
}
}
"#;

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
        .args(["-l", "JavaScript", "-e", script, "--request-file"])
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
    cmd.args(["-l", "JavaScript", "-e", script]).args(args);
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
    if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") || bytes.get(12..16) != Some(b"IHDR") {
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
