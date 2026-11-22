// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Cross-platform, symlink-safe external bot discovery for Orca, `OpenClaw`, Hermes, and `OpenBot`.
//!
//! Reads directory structures, `IDENTITY.md` name headers, `SOUL.md` previews, and non-secret
//! `bot.json`/`agent.json` metadata only; never reads `openclaw.json` or credential files.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::os::{canonicalize, home_dir};

use super::home::soul_description;
use super::types::{ExternalBotCandidate, ExternalBotSourceApp};

const MAX_BOTS_PER_APP: usize = 100;
const MAX_BOT_FILE_BYTES: usize = 512 * 1024;

/// Discovers external bots on this machine using the OS home directory and process environment.
#[must_use]
pub fn discover_external_bots(
    source_filter: Option<ExternalBotSourceApp>,
    custom_path: Option<&Path>,
) -> Vec<ExternalBotCandidate> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    discover_external_bots_filtered(&home, source_filter, custom_path, |key| {
        std::env::var(key).ok().filter(|v| !v.trim().is_empty())
    })
}

/// Deterministic, environment-injectable external bot discovery across all supported apps.
pub fn discover_external_bots_in<F>(
    home: &Path,
    custom_path: Option<&Path>,
    env_lookup: F,
) -> Vec<ExternalBotCandidate>
where
    F: FnMut(&str) -> Option<String>,
{
    discover_external_bots_filtered(home, None, custom_path, env_lookup)
}

/// Deterministic external bot discovery with an optional source-app filter.
pub fn discover_external_bots_filtered<F>(
    home: &Path,
    source_filter: Option<ExternalBotSourceApp>,
    custom_path: Option<&Path>,
    mut env_lookup: F,
) -> Vec<ExternalBotCandidate>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut out = Vec::new();
    let mut seen_paths = BTreeSet::new();

    if let Some(raw_custom) = custom_path {
        let raw_str = raw_custom.to_string_lossy();
        if raw_str.trim().is_empty() {
            return out;
        }
        let expanded = expand_tilde(home, &raw_str);
        if !is_real_dir(&expanded) {
            return out;
        }
        let app = source_filter.unwrap_or_else(|| infer_source_app(&expanded));
        scan_custom_root(app, &expanded, &mut seen_paths, &mut out);
        return out;
    }

    let want = |app: ExternalBotSourceApp| source_filter.is_none_or(|f| f == app);

    if want(ExternalBotSourceApp::Orca) {
        let mut orca_roots = vec![
            home.join(".orca").join("bots"),
            home.join("Library")
                .join("Application Support")
                .join("Orca")
                .join("bots"),
            home.join("Library")
                .join("Application Support")
                .join("orca")
                .join("bots"),
        ];
        if let Some(appdata) = env_non_empty(&mut env_lookup, "APPDATA") {
            let base = expand_tilde(home, &appdata);
            orca_roots.push(base.join("Orca").join("bots"));
            orca_roots.push(base.join("orca").join("bots"));
        } else {
            let fallback = home.join("AppData").join("Roaming");
            orca_roots.push(fallback.join("Orca").join("bots"));
        }
        if let Some(xdg) = env_non_empty(&mut env_lookup, "XDG_CONFIG_HOME") {
            let base = expand_tilde(home, &xdg);
            orca_roots.push(base.join("Orca").join("bots"));
            orca_roots.push(base.join("orca").join("bots"));
        }
        for root in orca_roots {
            discover_orca_root(&root, &mut seen_paths, &mut out);
        }
    }

    if want(ExternalBotSourceApp::OpenClaw) {
        let mut candidates = Vec::with_capacity(4);
        if let Some(state_dir) = env_non_empty(&mut env_lookup, "OPENCLAW_STATE_DIR") {
            candidates.push(expand_tilde(home, &state_dir));
        }
        for name in [".openclaw", ".clawdbot", ".moltbot"] {
            candidates.push(home.join(name));
        }
        if let Some(root) = candidates.into_iter().find(|p| is_real_dir(p)) {
            discover_openclaw_root(&root, &mut seen_paths, &mut out);
        }
    }

    if want(ExternalBotSourceApp::Hermes) {
        let root = match env_non_empty(&mut env_lookup, "HERMES_HOME") {
            Some(val) => strip_hermes_profile_suffix(&expand_tilde(home, &val)),
            None => home.join(".hermes"),
        };
        discover_hermes_root(&root, &mut seen_paths, &mut out);
    }

    if want(ExternalBotSourceApp::OpenBot) {
        let mut openbot_roots = vec![
            home.join("OpenBot").join("Agents"),
            home.join(".openbot"),
            home.join("Library")
                .join("Application Support")
                .join("OpenBot"),
        ];
        if let Some(appdata) = env_non_empty(&mut env_lookup, "APPDATA") {
            openbot_roots.push(expand_tilde(home, &appdata).join("OpenBot"));
        } else {
            openbot_roots.push(home.join("AppData").join("Roaming").join("OpenBot"));
        }
        if let Some(xdg) = env_non_empty(&mut env_lookup, "XDG_CONFIG_HOME") {
            openbot_roots.push(expand_tilde(home, &xdg).join("OpenBot"));
        }
        for root in openbot_roots {
            discover_openbot_root(&root, &mut seen_paths, &mut out);
        }
    }

    out
}

/// Expands leading `~/`, `~\`, or standalone `~` against `home`.
#[must_use]
pub fn expand_tilde(home: &Path, raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        let mut out = home.to_path_buf();
        for component in rest.split(['/', '\\']).filter(|s| !s.is_empty()) {
            out.push(component);
        }
        return out;
    }
    PathBuf::from(trimmed)
}

/// Formats `path` with a leading `~/` when it resides under `home`.
#[must_use]
pub fn tilde_display(path: &Path, home: &Path) -> String {
    if path == home {
        return "~".to_owned();
    }
    if let Ok(rel) = path.strip_prefix(home) {
        let joined = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        return format!("~/{joined}");
    }
    path.display().to_string()
}

/// Maps an Orca runtime agent ID or alias to a Krill canonical agent ID.
#[must_use]
pub fn map_orca_agent_id(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "opencode" => Some("opencode"),
        "cursor" | "cursor-agent" => Some("cursor"),
        "agy" | "antigravity" => Some("agy"),
        "grok" | "grok-cli" => Some("grok"),
        "muse" | "muse-cli" => Some("muse"),
        "copilot" | "github-copilot" => Some("copilot"),
        "pi" => Some("pi"),
        "omp" | "oh-my-pi" => Some("omp"),
        "hermes" => Some("hermes"),
        "kimi" | "kimi-cli" => Some("kimi"),
        "qwen" | "qwen-code" => Some("qwen"),
        "droid" => Some("droid"),
        "kiro" | "kiro-cli" => Some("kiro"),
        "aider" => Some("aider"),
        "goose" => Some("goose"),
        "amp" => Some("amp"),
        "mistral-vibe" | "vibe" => Some("mistral-vibe"),
        "crush" => Some("crush"),
        "cline" => Some("cline"),
        "kilo" | "kilocode" => Some("kilo"),
        "continue" | "cn" => Some("continue"),
        "devin" => Some("devin"),
        "trae" | "trae-cli" => Some("trae"),
        "zcode" => Some("zcode"),
        "dsh" | "deepagents" => Some("dsh"),
        "openclaude" | "open-claude" => Some("openclaude"),
        "opencode2" | "open-code-2" => Some("opencode2"),
        _ => None,
    }
}

fn env_non_empty<F>(env_lookup: &mut F, key: &str) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    env_lookup(key).and_then(|v| {
        let trimmed = v.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn infer_source_app(path: &Path) -> ExternalBotSourceApp {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.contains("orca") {
        ExternalBotSourceApp::Orca
    } else if lower.contains("hermes") {
        ExternalBotSourceApp::Hermes
    } else if lower.contains("openbot") {
        ExternalBotSourceApp::OpenBot
    } else {
        ExternalBotSourceApp::OpenClaw
    }
}

fn scan_custom_root(
    app: ExternalBotSourceApp,
    root: &Path,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<ExternalBotCandidate>,
) {
    match app {
        ExternalBotSourceApp::Orca => {
            if is_orca_bot_dir(root)
                && let Some(cand) = read_orca_candidate(root)
            {
                push_unique_candidate(root, cand, seen, out);
            }
            let bots_sub = root.join("bots");
            if is_real_dir(&bots_sub) {
                discover_orca_root(&bots_sub, seen, out);
            }
            discover_orca_root(root, seen, out);
        }
        ExternalBotSourceApp::OpenClaw => {
            let before = out.len();
            discover_openclaw_root(root, seen, out);
            if out.len() == before {
                scan_generic_bot_root(ExternalBotSourceApp::OpenClaw, root, seen, out);
            }
        }
        ExternalBotSourceApp::Hermes => {
            let before = out.len();
            discover_hermes_root(root, seen, out);
            if out.len() == before {
                scan_generic_bot_root(ExternalBotSourceApp::Hermes, root, seen, out);
            }
        }
        ExternalBotSourceApp::OpenBot => {
            discover_openbot_root(root, seen, out);
            for child in non_hidden_child_dirs(root) {
                let dir = root.join(&child);
                if has_bot_marker_files(&dir)
                    && let Some(cand) = read_openbot_candidate(&dir, &child)
                {
                    push_unique_candidate(&dir, cand, seen, out);
                }
            }
        }
    }
}

fn scan_generic_bot_root(
    app: ExternalBotSourceApp,
    root: &Path,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<ExternalBotCandidate>,
) {
    if has_bot_marker_files(root)
        && let Some(cand) = read_generic_candidate(app, root)
    {
        push_unique_candidate(root, cand, seen, out);
    }
    for sub in ["bots", "agents", "Agents", "profiles", "workspaces"] {
        let container = root.join(sub);
        if is_real_dir(&container) {
            for child in non_hidden_child_dirs(&container)
                .into_iter()
                .take(MAX_BOTS_PER_APP)
            {
                let dir = container.join(&child);
                if has_bot_marker_files(&dir)
                    && let Some(cand) = read_generic_candidate(app, &dir)
                {
                    push_unique_candidate(&dir, cand, seen, out);
                }
            }
        }
    }
    for child in non_hidden_child_dirs(root)
        .into_iter()
        .take(MAX_BOTS_PER_APP)
    {
        let dir = root.join(&child);
        if has_bot_marker_files(&dir)
            && let Some(cand) = read_generic_candidate(app, &dir)
        {
            push_unique_candidate(&dir, cand, seen, out);
        }
    }
}

fn discover_orca_root(
    root: &Path,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<ExternalBotCandidate>,
) {
    if !is_real_dir(root) {
        return;
    }
    let mut added = 0usize;
    for folder_name in non_hidden_child_dirs(root) {
        if added >= MAX_BOTS_PER_APP {
            break;
        }
        let dir = root.join(&folder_name);
        if let Some(cand) = read_orca_candidate(&dir)
            && push_unique_candidate(&dir, cand, seen, out)
        {
            added = added.saturating_add(1);
        }
    }
}

fn is_orca_bot_dir(dir: &Path) -> bool {
    is_real_file(&dir.join("bot.json"))
        || is_real_file(&dir.join("SOUL.md"))
        || is_real_file(&dir.join("MEMORY.md"))
}

fn read_orca_candidate(dir: &Path) -> Option<ExternalBotCandidate> {
    if !is_orca_bot_dir(dir) {
        return None;
    }
    let folder_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("orca-bot");
    let manifest = read_bounded_utf8(&dir.join("bot.json"))
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    let soul = read_bounded_utf8(&dir.join("SOUL.md"));
    let name = manifest
        .as_ref()
        .and_then(|m| m.get("name").and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| bot_name_from_id(folder_name), str::to_owned);
    let agent = manifest
        .as_ref()
        .and_then(|m| {
            m.get("agent_id")
                .or_else(|| m.get("agent"))
                .and_then(serde_json::Value::as_str)
        })
        .and_then(map_orca_agent_id)
        .unwrap_or("claude")
        .to_owned();
    let soul_preview = soul.as_deref().and_then(preview_from_soul);
    Some(ExternalBotCandidate {
        app: ExternalBotSourceApp::Orca,
        agent,
        name,
        path: dir.to_string_lossy().into_owned(),
        soul_preview,
    })
}

fn discover_openclaw_root(
    root: &Path,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<ExternalBotCandidate>,
) {
    if !is_real_dir(root) {
        return;
    }
    let ids = non_hidden_child_dirs(&root.join("agents"));
    let agents: Vec<String> = if !ids.is_empty() {
        ids
    } else if is_real_dir(&root.join("workspace")) {
        vec!["main".to_owned()]
    } else {
        Vec::new()
    };

    for agent in agents.into_iter().take(MAX_BOTS_PER_APP) {
        let mut places = Vec::with_capacity(3);
        if agent == "main" {
            places.push(root.join("workspace"));
        }
        places.push(root.join(format!("workspace-{agent}")));
        places.push(root.join("workspace").join(&agent));

        let workspace = places.into_iter().find(|p| is_real_dir(p));
        let folder = workspace
            .clone()
            .unwrap_or_else(|| root.join("agents").join(&agent));
        if !is_real_dir(&folder) {
            continue;
        }
        // Never open `openclaw.json` because external installs may store user tokens inside it.
        let name = workspace
            .as_deref()
            .and_then(read_openclaw_identity_name)
            .or_else(|| read_openclaw_identity_name(&folder))
            .unwrap_or_else(|| bot_name_from_id(&agent));
        let soul_preview =
            read_bounded_utf8(&folder.join("SOUL.md")).and_then(|s| preview_from_soul(&s));
        let cand = ExternalBotCandidate {
            app: ExternalBotSourceApp::OpenClaw,
            agent,
            name,
            path: folder.to_string_lossy().into_owned(),
            soul_preview,
        };
        push_unique_candidate(&folder, cand, seen, out);
    }
}

/// Parses `OpenClaw`'s `IDENTITY.md` name line (`- **Name:** Molty`).
fn read_openclaw_identity_name(workspace: &Path) -> Option<String> {
    let text = read_bounded_utf8(&workspace.join("IDENTITY.md"))?;
    for line in text.lines() {
        let stripped = line.trim_start_matches([' ', '\t', '*', '-']);
        let cleaned_key = stripped.trim_start_matches('*');
        let Some((key, val)) = cleaned_key.split_once(':') else {
            continue;
        };
        let key_norm = key.trim().trim_end_matches('*').trim();
        if !key_norm.eq_ignore_ascii_case("name") {
            continue;
        }
        let name = val.trim().trim_matches('*').trim();
        if !name.is_empty() && name.len() <= 120 && !name.starts_with('(') {
            return Some(name.to_owned());
        }
    }
    None
}

fn discover_hermes_root(
    root: &Path,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<ExternalBotCandidate>,
) {
    if !is_real_dir(root) {
        return;
    }
    let mut added = 0usize;
    if is_hermes_profile_dir(root) {
        let soul_preview =
            read_bounded_utf8(&root.join("SOUL.md")).and_then(|s| preview_from_soul(&s));
        let cand = ExternalBotCandidate {
            app: ExternalBotSourceApp::Hermes,
            agent: "default".to_owned(),
            name: "Hermes".to_owned(),
            path: root.to_string_lossy().into_owned(),
            soul_preview,
        };
        if push_unique_candidate(root, cand, seen, out) {
            added = added.saturating_add(1);
        }
    }
    let profiles_dir = root.join("profiles");
    for profile in non_hidden_child_dirs(&profiles_dir) {
        if added >= MAX_BOTS_PER_APP {
            break;
        }
        let folder = profiles_dir.join(&profile);
        if is_hermes_profile_dir(&folder) {
            let soul_preview =
                read_bounded_utf8(&folder.join("SOUL.md")).and_then(|s| preview_from_soul(&s));
            let cand = ExternalBotCandidate {
                app: ExternalBotSourceApp::Hermes,
                agent: profile.clone(),
                name: bot_name_from_id(&profile),
                path: folder.to_string_lossy().into_owned(),
                soul_preview,
            };
            if push_unique_candidate(&folder, cand, seen, out) {
                added = added.saturating_add(1);
            }
        }
    }
}

fn strip_hermes_profile_suffix(path: &Path) -> PathBuf {
    if let Some(parent) = path.parent()
        && parent.file_name().and_then(|n| n.to_str()) == Some("profiles")
        && let Some(grandparent) = parent.parent()
    {
        return grandparent.to_path_buf();
    }
    path.to_path_buf()
}

fn is_hermes_profile_dir(dir: &Path) -> bool {
    is_real_file(&dir.join("SOUL.md")) || is_real_file(&dir.join("config.yaml"))
}

fn discover_openbot_root(
    root: &Path,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<ExternalBotCandidate>,
) {
    if !is_real_dir(root) {
        return;
    }
    let mut added = 0usize;
    let is_agents_dir = root
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("agents"));

    if is_agents_dir {
        for agent in non_hidden_child_dirs(root) {
            if added >= MAX_BOTS_PER_APP {
                break;
            }
            let dir = root.join(&agent);
            if let Some(cand) = read_openbot_candidate(&dir, &agent)
                && push_unique_candidate(&dir, cand, seen, out)
            {
                added = added.saturating_add(1);
            }
        }
        return;
    }

    if has_bot_marker_files(root)
        && let Some(folder_name) = root.file_name().and_then(|n| n.to_str())
        && let Some(cand) = read_openbot_candidate(root, folder_name)
        && push_unique_candidate(root, cand, seen, out)
    {
        added = added.saturating_add(1);
    }

    for sub in ["Agents", "agents", "bots"] {
        let container = root.join(sub);
        if is_real_dir(&container) {
            for agent in non_hidden_child_dirs(&container) {
                if added >= MAX_BOTS_PER_APP {
                    break;
                }
                let dir = container.join(&agent);
                if let Some(cand) = read_openbot_candidate(&dir, &agent)
                    && push_unique_candidate(&dir, cand, seen, out)
                {
                    added = added.saturating_add(1);
                }
            }
        }
    }

    for child in non_hidden_child_dirs(root) {
        if added >= MAX_BOTS_PER_APP {
            break;
        }
        if matches!(child.as_str(), "Agents" | "agents" | "bots") {
            continue;
        }
        let dir = root.join(&child);
        if has_bot_marker_files(&dir)
            && let Some(cand) = read_openbot_candidate(&dir, &child)
            && push_unique_candidate(&dir, cand, seen, out)
        {
            added = added.saturating_add(1);
        }
    }
}

fn read_openbot_candidate(dir: &Path, folder_name: &str) -> Option<ExternalBotCandidate> {
    if !is_real_dir(dir) {
        return None;
    }
    let manifest_name = read_json_name_field(dir);
    let stripped = folder_name
        .strip_prefix("agent-")
        .or_else(|| folder_name.strip_prefix("bot-"))
        .unwrap_or(folder_name);
    let name = manifest_name.unwrap_or_else(|| {
        if looks_like_uuid_prefix(stripped) {
            "OpenBot agent".to_owned()
        } else {
            bot_name_from_id(stripped)
        }
    });
    let soul_preview = read_bounded_utf8(&dir.join("SOUL.md"))
        .or_else(|| read_bounded_utf8(&dir.join("IDENTITY.md")))
        .and_then(|s| preview_from_soul(&s));
    Some(ExternalBotCandidate {
        app: ExternalBotSourceApp::OpenBot,
        agent: folder_name.to_owned(),
        name,
        path: dir.to_string_lossy().into_owned(),
        soul_preview,
    })
}

fn read_generic_candidate(app: ExternalBotSourceApp, dir: &Path) -> Option<ExternalBotCandidate> {
    if !is_real_dir(dir) {
        return None;
    }
    let folder_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.trim_start_matches('.'))
        .filter(|s| !s.is_empty())
        .unwrap_or("Imported Bot");
    let name = read_json_name_field(dir)
        .or_else(|| read_openclaw_identity_name(dir))
        .unwrap_or_else(|| bot_name_from_id(folder_name));
    let agent = match app {
        ExternalBotSourceApp::Hermes => "hermes".to_owned(),
        ExternalBotSourceApp::OpenClaw | ExternalBotSourceApp::OpenBot => folder_name.to_owned(),
        ExternalBotSourceApp::Orca => "claude".to_owned(),
    };
    let soul_preview = read_bounded_utf8(&dir.join("SOUL.md"))
        .or_else(|| read_bounded_utf8(&dir.join("IDENTITY.md")))
        .or_else(|| read_bounded_utf8(&dir.join("SYSTEM.md")))
        .and_then(|s| preview_from_soul(&s));
    Some(ExternalBotCandidate {
        app,
        agent,
        name,
        path: dir.to_string_lossy().into_owned(),
        soul_preview,
    })
}

fn read_json_name_field(dir: &Path) -> Option<String> {
    for manifest_name in ["bot.json", "agent.json"] {
        if let Some(raw) = read_bounded_utf8(&dir.join(manifest_name))
            && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
            && let Some(n) = val.get("name").and_then(serde_json::Value::as_str)
        {
            let trimmed = n.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

fn has_bot_marker_files(dir: &Path) -> bool {
    is_real_file(&dir.join("SOUL.md"))
        || is_real_file(&dir.join("IDENTITY.md"))
        || is_real_file(&dir.join("bot.json"))
        || is_real_file(&dir.join("agent.json"))
}

fn preview_from_soul(soul: &str) -> Option<String> {
    soul_description(soul).or_else(|| {
        soul.lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|s| s.chars().take(160).collect())
    })
}

fn push_unique_candidate(
    dir: &Path,
    cand: ExternalBotCandidate,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<ExternalBotCandidate>,
) -> bool {
    let key = canonicalize(dir).map_or_else(
        |_| dir.to_string_lossy().into_owned(),
        |p| p.to_string_lossy().into_owned(),
    );
    if !seen.insert(key) {
        return false;
    }
    out.push(cand);
    true
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_symlink() && m.is_dir())
}

fn is_real_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_symlink() && m.is_file())
}

fn non_hidden_child_dirs(dir: &Path) -> Vec<String> {
    if !is_real_dir(dir) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            is_real_dir(&e.path()).then_some(name)
        })
        .collect();
    names.sort();
    names
}

fn read_bounded_utf8(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    let max_u64 = u64::try_from(MAX_BOT_FILE_BYTES).unwrap_or(u64::MAX);
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > max_u64 {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(max_u64.saturating_add(1))
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() > MAX_BOT_FILE_BYTES {
        return None;
    }
    String::from_utf8(buf).ok()
}

fn bot_name_from_id(id: &str) -> String {
    let words: String = id
        .chars()
        .map(|c| if c == '-' || c == '_' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = words.chars();
    let Some(first) = chars.next() else {
        return id.to_owned();
    };
    let mut out = String::new();
    for upper in first.to_uppercase() {
        out.push(upper);
    }
    out.push_str(chars.as_str());
    out
}

fn looks_like_uuid_prefix(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 14
        && bytes
            .get(0..8)
            .is_some_and(|p| p.iter().all(u8::is_ascii_hexdigit))
        && bytes.get(8) == Some(&b'-')
        && bytes
            .get(9..13)
            .is_some_and(|p| p.iter().all(u8::is_ascii_hexdigit))
        && bytes.get(13) == Some(&b'-')
}

/// Prompt the owner copies into an external bot whose files cannot be read directly.
pub const BOT_IMPORT_EXPORT_PROMPT: &str = "I'm moving to another assistant and need a complete copy of your setup. Don't change, pause or delete anything. If you keep your setup in files (persona or soul, memory, user notes, skills, routines), save copies of those files, unchanged, into a folder in my Downloads called <your name>-export, then list what you saved. Write out in full in your reply anything that isn't in a file. Never include passwords, API keys, tokens or cookies.";

/// Maximum length of the old bot's reply pasted into Export import.
pub const BOT_IMPORT_REPLY_MAX: usize = 200_000;

/// Home-relative file where an exported bot's reply is saved.
pub const BOT_IMPORT_REPLY_FILE: &str = "import/old-bot-reply.md";

/// Summary for the first-bot import nudge inside New bot: `OpenClaw` count, else `Hermes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotImportNudgeInfo {
    pub app: ExternalBotSourceApp,
    pub label: &'static str,
    pub count: usize,
}

/// Human-facing app label used in import rows and nudges.
#[must_use]
pub const fn bot_import_app_label(app: ExternalBotSourceApp) -> &'static str {
    match app {
        ExternalBotSourceApp::OpenClaw => "OpenClaw",
        ExternalBotSourceApp::Hermes => "Hermes",
        ExternalBotSourceApp::OpenBot => "OpenBot",
        ExternalBotSourceApp::Orca => "Orca",
    }
}

/// Returns the first-bot import nudge line (`OpenClaw` bots first, else `Hermes` bots).
#[must_use]
pub fn bot_import_nudge(candidates: &[ExternalBotCandidate]) -> Option<BotImportNudgeInfo> {
    for app in [ExternalBotSourceApp::OpenClaw, ExternalBotSourceApp::Hermes] {
        let count = candidates.iter().filter(|c| c.app == app).count();
        if count > 0 {
            return Some(BotImportNudgeInfo {
                app,
                label: bot_import_app_label(app),
                count,
            });
        }
    }
    None
}

/// Suggests a bot name from an export folder like `~/Downloads/sage-export` -> `Sage`.
#[must_use]
pub fn bot_name_from_export_folder(raw_path: &str) -> Option<String> {
    let trimmed = raw_path.trim().trim_end_matches(['/', '\\']);
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    let tail_idx = base.len().saturating_sub(7);
    let stripped = base
        .strip_suffix("-export")
        .or_else(|| base.strip_suffix("_export"))
        .or_else(|| base.strip_suffix(" export"))
        .or_else(|| base.strip_suffix("-EXPORT"))
        .or_else(|| base.strip_suffix("_EXPORT"))
        .or_else(|| {
            base.get(..tail_idx).filter(|_| {
                base.len() > 7
                    && base.get(tail_idx..).is_some_and(|s| {
                        s.eq_ignore_ascii_case("-export")
                            || s.eq_ignore_ascii_case("_export")
                            || s.eq_ignore_ascii_case(" export")
                    })
            })
        })
        .unwrap_or(base);
    if stripped
        .chars()
        .all(|ch| ch == '-' || ch == '_' || ch.is_whitespace())
    {
        return None;
    }
    Some(bot_name_from_id(stripped))
}

#[cfg(test)]
mod nudge_tests {
    use super::*;

    #[test]
    fn nudge_prefers_openclaw_then_hermes_and_ignores_others() {
        let openbot = ExternalBotCandidate {
            app: ExternalBotSourceApp::OpenBot,
            agent: "ob".to_owned(),
            name: "Ob".to_owned(),
            path: "/tmp/ob".to_owned(),
            soul_preview: None,
        };
        let hermes = ExternalBotCandidate {
            app: ExternalBotSourceApp::Hermes,
            agent: "default".to_owned(),
            name: "Hermes".to_owned(),
            path: "/tmp/hermes".to_owned(),
            soul_preview: None,
        };
        let openclaw = ExternalBotCandidate {
            app: ExternalBotSourceApp::OpenClaw,
            agent: "research".to_owned(),
            name: "Research".to_owned(),
            path: "/tmp/openclaw".to_owned(),
            soul_preview: None,
        };
        assert_eq!(bot_import_nudge(std::slice::from_ref(&openbot)), None);
        assert_eq!(
            bot_import_nudge(&[openbot.clone(), hermes.clone()]),
            Some(BotImportNudgeInfo {
                app: ExternalBotSourceApp::Hermes,
                label: "Hermes",
                count: 1,
            })
        );
        assert_eq!(
            bot_import_nudge(&[hermes, openclaw]),
            Some(BotImportNudgeInfo {
                app: ExternalBotSourceApp::OpenClaw,
                label: "OpenClaw",
                count: 1,
            })
        );
    }

    #[test]
    fn export_folder_suggests_clean_bot_name() {
        assert_eq!(
            bot_name_from_export_folder("/Users/a/Downloads/sage-export").as_deref(),
            Some("Sage")
        );
        assert_eq!(
            bot_name_from_export_folder("/Users/a/Downloads/research_desk_export").as_deref(),
            Some("Research desk")
        );
        assert_eq!(
            bot_name_from_export_folder("/Users/a/Downloads/Custom-Export/").as_deref(),
            Some("Custom")
        );
        assert_eq!(bot_name_from_export_folder("-export"), None);
    }
}
