// Portions of the computer-use command surface, key normalization, and runtime payload
// schema are adapted from Orca (.ref/orca/src/shared/computer-use-*.ts,
// .ref/orca/native/computer-use-macos/):
// Copyright (c) 2026 Lovecast Inc. Licensed under the MIT License (.ref/orca/LICENSE).

//! `krill computer` 런타임 데이터 모델, 키/핫키 정규화, AX 트리 인덱싱.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

pub const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
pub const SCREENSHOTS_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionId {
    Accessibility,
    Screenshots,
}

impl PermissionId {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "accessibility" => Some(Self::Accessibility),
            "screenshots" | "screen_recording" | "screen-recording" => Some(Self::Screenshots),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accessibility => "accessibility",
            Self::Screenshots => "screenshots",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Accessibility => "Accessibility",
            Self::Screenshots => "Screen Recording",
        }
    }

    pub const fn settings_url(self) -> &'static str {
        match self {
            Self::Accessibility => ACCESSIBILITY_SETTINGS_URL,
            Self::Screenshots => SCREENSHOTS_SETTINGS_URL,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionCapability {
    pub id: String,
    pub label: String,
    pub required_for: Vec<String>,
    pub granted: bool,
    pub can_prompt: bool,
    pub status: String,
    pub instructions: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesPayload {
    pub platform: String,
    pub supported: bool,
    pub permissions: Vec<PermissionCapability>,
    pub actions: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsPayload {
    pub platform: String,
    pub supported: bool,
    pub all_granted: bool,
    pub permissions: Vec<PermissionCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opened_settings_for: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn center(self) -> Point {
        Point {
            x: self.x + (self.width / 2.0),
            y: self.y + (self.height / 2.0),
        }
    }

    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.x + self.width
            && point.y >= self.y
            && point.y <= self.y + self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSummary {
    pub name: String,
    pub bundle_id: Option<String>,
    pub pid: u32,
    pub is_active: bool,
    pub is_hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowSummary {
    pub id: String,
    pub index: usize,
    pub title: String,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub pid: u32,
    pub bounds: Rect,
    pub is_focused: bool,
    pub is_main: bool,
    pub is_minimized: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementNode {
    #[serde(default)]
    pub reference_id: Option<usize>,
    pub role: String,
    #[serde(default)]
    pub subrole: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub bounds: Option<Rect>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub children: Vec<ElementNode>,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatElement {
    pub reference_id: usize,
    pub role: String,
    pub label: Option<String>,
    pub value: Option<String>,
    pub bounds: Option<Rect>,
    pub enabled: bool,
    pub focused: bool,
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotMetadata {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub window_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStateSnapshot {
    pub snapshot_id: String,
    pub captured_at: String,
    pub app: AppSummary,
    pub window: Option<WindowSummary>,
    pub focused_element_id: Option<usize>,
    pub element_count: usize,
    pub interactive_count: usize,
    pub tree_text: String,
    pub elements: Vec<FlatElement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<ElementNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<ScreenshotMetadata>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationMetadata {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub retried: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardMetadata {
    pub preserved: bool,
    pub restored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionResultPayload {
    pub action: String,
    pub ok: bool,
    pub verification: VerificationMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_reference_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_point: Option<Point>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_state: Option<AppStateSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<ClipboardMetadata>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockWindow {
    pub id: String,
    pub title: String,
    pub bounds: Rect,
    #[serde(default = "default_true")]
    pub is_focused: bool,
    #[serde(default = "default_true")]
    pub is_main: bool,
    #[serde(default)]
    pub is_minimized: bool,
    pub root: ElementNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockApp {
    pub name: String,
    pub bundle_id: Option<String>,
    pub pid: u32,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_hidden: bool,
    #[serde(default)]
    pub windows: Vec<MockWindow>,
}

impl MockApp {
    pub fn summary(&self) -> AppSummary {
        AppSummary {
            name: self.name.clone(),
            bundle_id: self.bundle_id.clone(),
            pid: self.pid,
            is_active: self.is_active,
            is_hidden: self.is_hidden,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockEnvironment {
    #[serde(default = "default_true")]
    pub accessibility_granted: bool,
    #[serde(default = "default_true")]
    pub screenshots_granted: bool,
    #[serde(default)]
    pub clipboard: String,
    #[serde(default)]
    pub clipboard_restore_count: u32,
    #[serde(default)]
    pub apps: Vec<MockApp>,
    #[serde(default)]
    pub last_action: Option<String>,
}

pub fn normalize_key_name(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("key must not be empty".to_owned());
    }
    let lower = trimmed.to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "enter" | "return" => "return",
        "tab" => "tab",
        "space" | "spacebar" => "space",
        "backspace" | "delete" => "delete",
        "forwarddelete" | "forward-delete" | "del" => "forwarddelete",
        "esc" | "escape" => "escape",
        "up" | "arrowup" => "up",
        "down" | "arrowdown" => "down",
        "left" | "arrowleft" => "left",
        "right" | "arrowright" => "right",
        "home" => "home",
        "end" => "end",
        "pageup" | "page-up" => "pageup",
        "pagedown" | "page-down" => "pagedown",
        "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => {
            lower.as_str()
        }
        _ => {
            let mut chars = trimmed.chars();
            if let (Some(ch), None) = (chars.next(), chars.next())
                && !ch.is_control()
            {
                return Ok(ch.to_ascii_lowercase().to_string());
            }
            return Err(format!("unsupported key: {raw}"));
        }
    };
    Ok(canonical.to_owned())
}

pub fn normalize_modifier_name(raw: &str) -> Result<&'static str, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "cmd" | "command" | "meta" | "super" => Ok("command"),
        "ctrl" | "control" => Ok("control"),
        "alt" | "opt" | "option" => Ok("option"),
        "shift" => Ok("shift"),
        "fn" | "function" => Ok("fn"),
        _ => Err(format!("unsupported modifier: {raw}")),
    }
}

pub fn parse_hotkey_chord(raw: &str) -> Result<(Vec<String>, String), String> {
    let parts: Vec<&str> = raw
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let Some((&last, mod_parts)) = parts.split_last() else {
        return Err("hotkey chord must not be empty".to_owned());
    };
    if mod_parts.is_empty() {
        return Err(
            "hotkey chord requires at least one modifier (e.g. Command+Shift+P)".to_owned(),
        );
    }
    let mut modifiers = Vec::with_capacity(mod_parts.len());
    for part in mod_parts {
        let normalized = normalize_modifier_name(part)?.to_owned();
        if !modifiers.contains(&normalized) {
            modifiers.push(normalized);
        }
    }
    let key = normalize_key_name(last)?;
    Ok((modifiers, key))
}

pub fn key_to_macos_keycode(normalized_key: &str) -> Option<u16> {
    match normalized_key {
        "return" => Some(36),
        "tab" => Some(48),
        "space" => Some(49),
        "delete" => Some(51),
        "forwarddelete" => Some(117),
        "escape" => Some(53),
        "up" => Some(126),
        "down" => Some(125),
        "left" => Some(123),
        "right" => Some(124),
        "home" => Some(115),
        "end" => Some(119),
        "pageup" => Some(116),
        "pagedown" => Some(121),
        "f1" => Some(122),
        "f2" => Some(120),
        "f3" => Some(99),
        "f4" => Some(118),
        "f5" => Some(96),
        "f6" => Some(97),
        "f7" => Some(98),
        "f8" => Some(100),
        "f9" => Some(101),
        "f10" => Some(109),
        "f11" => Some(103),
        "f12" => Some(111),
        _ => None,
    }
}

pub fn modifier_to_system_events_using(normalized_mod: &str) -> Option<&'static str> {
    match normalized_mod {
        "command" => Some("command down"),
        "control" => Some("control down"),
        "option" => Some("option down"),
        "shift" => Some("shift down"),
        _ => None,
    }
}

pub fn modifiers_to_cg_flags(modifiers: &[String]) -> u64 {
    let mut flags = 0u64;
    for m in modifiers {
        match m.as_str() {
            "command" => flags |= 0x0010_0000,
            "shift" => flags |= 0x0002_0000,
            "option" => flags |= 0x0008_0000,
            "control" => flags |= 0x0004_0000,
            "fn" => flags |= 0x0080_0000,
            _ => {}
        }
    }
    flags
}

fn is_interactive_node(node: &ElementNode) -> bool {
    if !node.actions.is_empty() {
        return true;
    }
    matches!(
        node.role.as_str(),
        "AXButton"
            | "AXTextField"
            | "AXTextArea"
            | "AXCheckBox"
            | "AXRadioButton"
            | "AXPopUpButton"
            | "AXComboBox"
            | "AXMenuItem"
            | "AXLink"
            | "AXTab"
            | "AXSlider"
            | "AXRow"
            | "AXCell"
    )
}

fn node_label(node: &ElementNode) -> Option<String> {
    node.title
        .as_deref()
        .or(node.description.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn visit_ax_node(
    node: &mut ElementNode,
    depth: usize,
    next_ref: &mut usize,
    total_count: &mut usize,
    focused_id: &mut Option<usize>,
    flat: &mut Vec<FlatElement>,
    out: &mut String,
) {
    *total_count += 1;
    let interactive = is_interactive_node(node);
    let ref_id = if interactive {
        let assigned = node.reference_id.unwrap_or(*next_ref);
        *next_ref = assigned.saturating_add(1);
        node.reference_id = Some(assigned);
        if node.focused && focused_id.is_none() {
            *focused_id = Some(assigned);
        }
        flat.push(FlatElement {
            reference_id: assigned,
            role: node.role.clone(),
            label: node_label(node),
            value: node.value.clone(),
            bounds: node.bounds,
            enabled: node.enabled,
            focused: node.focused,
            actions: node.actions.clone(),
        });
        Some(assigned)
    } else {
        node.reference_id = None;
        None
    };

    let indent = "  ".repeat(depth);
    write!(out, "{indent}").ok();
    if let Some(id) = ref_id {
        write!(out, "[#{id}] ").ok();
    }
    write!(out, "{}", node.role).ok();
    if let Some(label) = node_label(node) {
        write!(out, " \"{label}\"").ok();
    }
    if let Some(val) = node.value.as_deref().filter(|v| !v.is_empty()) {
        write!(out, " value=\"{val}\"").ok();
    }
    if let Some(b) = node.bounds {
        write!(
            out,
            " @({:.0},{:.0} {:.0}x{:.0})",
            b.x, b.y, b.width, b.height
        )
        .ok();
    }
    if node.focused {
        write!(out, " [focused]").ok();
    }
    if !node.enabled {
        write!(out, " [disabled]").ok();
    }
    writeln!(out).ok();

    for child in &mut node.children {
        visit_ax_node(
            child,
            depth + 1,
            next_ref,
            total_count,
            focused_id,
            flat,
            out,
        );
    }
}

pub fn index_and_render_tree(
    root: &mut ElementNode,
) -> (Vec<FlatElement>, Option<usize>, usize, String) {
    let mut flat = Vec::new();
    let mut focused_id = None;
    let mut total_count = 0usize;
    let mut next_ref = 1usize;
    let mut tree_text = String::new();
    visit_ax_node(
        root,
        0,
        &mut next_ref,
        &mut total_count,
        &mut focused_id,
        &mut flat,
        &mut tree_text,
    );
    (flat, focused_id, total_count, tree_text)
}

pub fn mutate_element_by_ref(
    node: &mut ElementNode,
    target_ref: usize,
    new_value: Option<&str>,
    append_text: Option<&str>,
    focus: bool,
) -> Option<(Option<Rect>, bool)> {
    fn clear_focus(n: &mut ElementNode) {
        n.focused = false;
        for c in &mut n.children {
            clear_focus(c);
        }
    }
    fn find_and_apply(
        n: &mut ElementNode,
        target_ref: usize,
        new_value: Option<&str>,
        append_text: Option<&str>,
        focus: bool,
    ) -> Option<(Option<Rect>, bool)> {
        if n.reference_id == Some(target_ref) {
            if !n.enabled {
                return Some((n.bounds, false));
            }
            if focus {
                n.focused = true;
            }
            if let Some(v) = new_value {
                n.value = Some(v.to_owned());
            }
            if let Some(append) = append_text {
                let mut cur = n.value.clone().unwrap_or_default();
                cur.push_str(append);
                n.value = Some(cur);
            }
            return Some((n.bounds, true));
        }
        for child in &mut n.children {
            if let Some(found) = find_and_apply(child, target_ref, new_value, append_text, focus) {
                return Some(found);
            }
        }
        None
    }
    if focus {
        clear_focus(node);
    }
    find_and_apply(node, target_ref, new_value, append_text, focus)
}

pub fn mutate_focused_element(
    node: &mut ElementNode,
    new_value: Option<&str>,
    append_text: Option<&str>,
) -> Option<(Option<usize>, Option<Rect>)> {
    if node.focused && node.reference_id.is_some() && node.enabled {
        if let Some(v) = new_value {
            node.value = Some(v.to_owned());
        }
        if let Some(append) = append_text {
            let mut cur = node.value.clone().unwrap_or_default();
            cur.push_str(append);
            node.value = Some(cur);
        }
        return Some((node.reference_id, node.bounds));
    }
    for child in &mut node.children {
        if let Some(found) = mutate_focused_element(child, new_value, append_text) {
            return Some(found);
        }
    }
    None
}

pub fn matches_app_selector(app: &AppSummary, selector: &str) -> bool {
    let trimmed = selector.trim();
    if let Some(pid_raw) = trimmed.strip_prefix("pid:")
        && let Ok(pid) = pid_raw.trim().parse::<u32>()
    {
        return app.pid == pid;
    }
    if let Ok(pid) = trimmed.parse::<u32>()
        && app.pid == pid
    {
        return true;
    }
    let needle = trimmed.to_ascii_lowercase();
    app.name.to_ascii_lowercase() == needle
        || app
            .bundle_id
            .as_deref()
            .is_some_and(|b| b.to_ascii_lowercase() == needle)
        || app.name.to_ascii_lowercase().contains(&needle)
}

pub fn verify_not_self_targeted(app_name: &str, bundle_id: Option<&str>) -> Result<(), String> {
    let lower = app_name.to_ascii_lowercase();
    let bundle_lower = bundle_id.unwrap_or_default().to_ascii_lowercase();
    if lower == "krill"
        || lower == "krill-app"
        || bundle_lower == "dev.krill.app"
        || bundle_lower == "com.krill.app"
    {
        return Err(
            "self-targeting guard: krill computer refuses to drive the Krill host window itself"
                .to_owned(),
        );
    }
    Ok(())
}
