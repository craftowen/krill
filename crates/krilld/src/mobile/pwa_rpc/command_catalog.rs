//! 선택한 CLI의 설치된 스킬·사용자 명령 메타데이터만 읽는다. 본문 실행·임의 경로 요청은 없다.
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 512;
const MAX_VISITS: usize = 4096;

#[derive(Debug, Serialize)]
pub(super) struct Item {
    name: String,
    text: String,
    description: String,
    kind: &'static str,
    scope: &'static str,
}

struct Scan {
    items: BTreeMap<String, Item>,
    visited: HashSet<PathBuf>,
    remaining: usize,
    truncated: bool,
}

/// 앞쪽(프로젝트)의 같은 호출명이 전역 항목보다 우선한다.
pub(super) fn load(
    home: &Path,
    cwd: &Path,
    agent: &str,
    config: Option<&Path>,
) -> serde_json::Value {
    let (directory, skill_prefix) = match agent {
        "claude" => (".claude", "/"),
        "codex" => (".codex", "$"),
        "agy" | "antigravity" => (".agent", "/"),
        "cursor" | "cursor-agent" => (".cursor", "/"),
        "opencode" => (".opencode", "/"),
        "pi" => (".pi", "/skill:"),
        _ => {
            return serde_json::json!({"agent":agent,"items":[],"supported":false,"truncated":false});
        }
    };
    let shared_skills = matches!(agent, "codex" | "opencode" | "pi");
    let mut scan = Scan {
        items: BTreeMap::new(),
        visited: HashSet::new(),
        remaining: MAX_VISITS,
        truncated: false,
    };
    // Codex 0.164's native empty-session welcome screen lists these local commands.
    // /model is consumed by the mobile composer chooser, which verifies the running CLI.
    if agent == "codex" {
        for (name, description) in [
            (
                "init",
                "Create an AGENTS.md file with instructions for Codex",
            ),
            ("status", "Show current session configuration"),
            ("permissions", "Choose what Codex is allowed to do"),
            ("model", "Choose the model for this chat session"),
            ("review", "Review changes and find issues"),
        ] {
            let text = format!("/{name}");
            scan.items.insert(
                text.clone(),
                Item {
                    name: name.into(),
                    text,
                    description: description.into(),
                    kind: "Command",
                    scope: "Built-in",
                },
            );
        }
    }
    // CLI가 부모 프로젝트의 지시도 읽으므로 가까운 작업 폴더부터 위로 찾는다.
    for parent in cwd.ancestors().take(32) {
        if parent == home {
            break;
        }
        scan.root(
            &parent.join(directory).join("skills"),
            true,
            skill_prefix,
            "Project",
        );
        if shared_skills {
            scan.root(
                &parent.join(".agents/skills"),
                true,
                skill_prefix,
                "Project",
            );
        }
        scan.root(
            &parent.join(directory).join("commands"),
            false,
            "/",
            "Project",
        );
        if matches!(agent, "agy" | "antigravity") {
            scan.root(&parent.join(".agent/workflows"), false, "/", "Project");
        }
        if parent.join(".git").exists() {
            break;
        }
    }
    let global = config.map_or_else(|| home.join(directory), Path::to_path_buf);
    scan.root(&global.join("skills"), true, skill_prefix, "Global");
    if shared_skills {
        scan.root(&home.join(".agents/skills"), true, skill_prefix, "Global");
    }
    if agent == "codex" {
        scan.root(&global.join("prompts"), false, "/prompts:", "Global");
    } else {
        scan.root(&global.join("commands"), false, "/", "Global");
    }
    if agent == "pi" {
        scan.root(&global.join("agent/skills"), true, skill_prefix, "Global");
        scan.root(&global.join("agent/prompts"), false, "/", "Global");
    }
    if agent == "opencode" {
        let global = config.map_or_else(|| home.join(".config/opencode"), Path::to_path_buf);
        scan.root(&global.join("skills"), true, skill_prefix, "Global");
        scan.root(&global.join("commands"), false, "/", "Global");
    }
    if matches!(agent, "agy" | "antigravity") {
        if let Some(config) = config {
            for rel in [
                ".gemini/antigravity-cli/skills",
                ".gemini/config/skills",
                ".gemini/antigravity/skills",
            ] {
                scan.root(&config.join(rel), true, skill_prefix, "Global");
            }
        }
        for rel in [
            ".gemini/antigravity-cli/skills",
            ".gemini/config/skills",
            ".gemini/antigravity/skills",
            ".agents/skills",
        ] {
            scan.root(&home.join(rel), true, skill_prefix, "Global");
        }
        scan.root(
            &home.join(".gemini/antigravity/global_workflows"),
            false,
            "/",
            "Global",
        );
    }
    serde_json::json!({"agent":agent,"items":scan.items.into_values().collect::<Vec<_>>(),"supported":true,"truncated":scan.truncated})
}

impl Scan {
    fn root(&mut self, root: &Path, skills: bool, prefix: &str, scope: &'static str) {
        self.walk(root, root, skills, prefix, scope, 0);
    }

    fn walk(
        &mut self,
        root: &Path,
        path: &Path,
        skills: bool,
        prefix: &str,
        scope: &'static str,
        depth: usize,
    ) {
        if self.remaining == 0 || self.items.len() >= MAX_ENTRIES {
            self.truncated = true;
            return;
        }
        self.remaining -= 1;
        let Ok(canonical) = fs::canonicalize(path) else {
            return;
        };
        if !self.visited.insert(canonical) {
            return;
        }
        if skills && path.join("SKILL.md").is_file() {
            self.add(
                &path.join("SKILL.md"),
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default(),
                prefix,
                "Skill",
                scope,
            );
            return;
        }
        if !skills && path.extension().is_some_and(|ext| ext == "md") {
            let name = path.strip_prefix(root).ok().map(|p| {
                p.with_extension("")
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(":")
            });
            if let Some(name) = name {
                self.add(path, &name, prefix, "Command", scope);
            }
            return;
        }
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut paths: Vec<_> = entries
            .take(self.remaining.saturating_add(1))
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        paths.sort();
        if depth >= 4 {
            self.truncated |= !paths.is_empty();
            return;
        }
        for child in paths {
            if child
                .file_name()
                .is_some_and(|name| name == ".git" || name == "node_modules")
            {
                continue;
            }
            self.walk(root, &child, skills, prefix, scope, depth + 1);
        }
    }

    fn add(
        &mut self,
        path: &Path,
        fallback: &str,
        prefix: &str,
        kind: &'static str,
        scope: &'static str,
    ) {
        let Ok(metadata) = fs::metadata(path) else {
            return;
        };
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            return;
        }
        let Ok(file) = fs::File::open(path) else {
            return;
        };
        let mut bytes = Vec::new();
        if file.take(8192).read_to_end(&mut bytes).is_err() {
            return;
        }
        let content = String::from_utf8_lossy(&bytes);
        if kind == "Skill" && field(&content, "user-invocable").as_deref() == Some("false") {
            return;
        }
        let name = if kind == "Skill" {
            field(&content, "name").unwrap_or_else(|| fallback.to_owned())
        } else {
            fallback.to_owned()
        };
        if name.is_empty()
            || name.len() > 100
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || "-_.:".contains(c))
        {
            return;
        }
        let text = format!("{prefix}{name}");
        let description = field(&content, "description")
            .unwrap_or_default()
            .chars()
            .take(320)
            .collect();
        self.items.entry(text.clone()).or_insert(Item {
            name,
            text,
            description,
            kind,
            scope,
        });
    }
}

fn field(content: &str, key: &str) -> Option<String> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut lines = lines.take_while(|line| line.trim() != "---").peekable();
    while let Some(line) = lines.next() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((name, raw)) = line.split_once(':') else {
            continue;
        };
        if name != key {
            continue;
        }
        let raw = raw.trim();
        if matches!(raw, ">" | "|" | ">-" | "|-") {
            let mut value = String::new();
            while lines
                .peek()
                .is_some_and(|line| line.starts_with(char::is_whitespace))
            {
                if let Some(line) = lines.next() {
                    value.push_str(line.trim());
                    value.push(' ');
                }
            }
            return Some(value.trim().to_owned());
        }
        return Some(raw.trim_matches(['\'', '"']).to_owned());
    }
    None
}

#[cfg(test)]
mod tests;
