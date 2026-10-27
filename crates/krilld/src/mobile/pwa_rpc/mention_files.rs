//! Bounded filename discovery rooted at the selected session's cwd. No file bodies are read.
use super::{Value, bounded_command, invalid, json, mobile_git_command};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

const MAX_ITEMS: usize = 80;
const MAX_VISITS: usize = 4096;
const MAX_PATH: usize = 512;

fn safe_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !name.starts_with('.')
        && !name.chars().any(char::is_control)
        && !matches!(
            lower.as_str(),
            "target"
                | "build"
                | "dist"
                | "node_modules"
                | "vendor"
                | "coverage"
                | "secrets"
                | "credentials"
                | "id_rsa"
                | "id_ed25519"
        )
        && !lower.starts_with("credentials.")
        && !lower.starts_with("secrets.")
        && !Path::new(name).extension().is_some_and(|ext| {
            ["pem", "key", "p12", "pfx"]
                .iter()
                .any(|blocked| ext.eq_ignore_ascii_case(blocked))
        })
        && lower != "auth.json"
        && lower != "accounts.toml"
}

fn relative_file(root: &Path, path: &Path) -> Option<String> {
    let text = path.to_str()?;
    if text.len() > MAX_PATH || text.contains(['\\', ':']) || path.as_os_str().is_empty() {
        return None;
    }
    let mut candidate = root.to_owned();
    for component in path.components() {
        let Component::Normal(name) = component else {
            return None;
        };
        if !safe_name(name.to_str()?) {
            return None;
        }
        candidate.push(name);
        // Reject every symlink, including intermediate directories, before returning a relative name.
        if fs::symlink_metadata(&candidate)
            .ok()?
            .file_type()
            .is_symlink()
        {
            return None;
        }
    }
    let metadata = fs::symlink_metadata(&candidate).ok()?;
    if !metadata.is_file() || !candidate.canonicalize().ok()?.starts_with(root) {
        return None;
    }
    Some(text.replace(std::path::MAIN_SEPARATOR, "/"))
}

pub(super) fn load(cwd: &Path, query: &str) -> io::Result<Value> {
    if query.len() > 256
        || query.chars().any(char::is_control)
        || query.contains(['\\', ':'])
        || Path::new(query).is_absolute()
        || Path::new(query)
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(invalid());
    }
    let root = cwd.canonicalize()?;
    let query = query.to_lowercase();
    let mut scan = Scan {
        root: &root,
        query: &query,
        paths: BTreeSet::new(),
        remaining: MAX_VISITS,
        truncated: false,
        deadline: Instant::now() + Duration::from_millis(150),
    };
    // Git handles nested ignore rules; `ls-files` paths are relative to the invocation cwd.
    let git = bounded_command(
        mobile_git_command(&root).args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ]),
        Duration::from_secs(2),
        1024 * 1024,
    );
    scan.deadline = Instant::now() + Duration::from_millis(150);
    if let Ok(bytes) = git {
        for raw in bytes.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            if !scan.visit() {
                break;
            }
            if let Ok(path) = std::str::from_utf8(raw) {
                scan.add(Path::new(path));
            }
        }
    } else if root.ancestors().any(|p| p.join(".git").exists()) {
        // A failed/oversized Git listing must not bypass its ignore rules via the fallback walk.
        scan.truncated = true;
    } else {
        scan.walk(Path::new(""), 0);
    }
    Ok(
        json!({"items":scan.paths.into_iter().map(|path| json!({"path":path})).collect::<Vec<_>>(),"truncated":scan.truncated}),
    )
}

struct Scan<'a> {
    root: &'a Path,
    query: &'a str,
    paths: BTreeSet<String>,
    remaining: usize,
    truncated: bool,
    deadline: Instant,
}

impl Scan<'_> {
    fn visit(&mut self) -> bool {
        if self.remaining == 0 || self.paths.len() >= MAX_ITEMS || Instant::now() >= self.deadline {
            self.truncated = true;
            return false;
        }
        self.remaining -= 1;
        true
    }

    fn add(&mut self, path: &Path) {
        if path
            .to_str()
            .is_some_and(|p| p.to_lowercase().contains(self.query))
            && let Some(path) = relative_file(self.root, path)
        {
            self.paths.insert(path);
        }
    }

    fn walk(&mut self, relative: &Path, depth: usize) {
        let Ok(entries) = fs::read_dir(self.root.join(relative)) else {
            return;
        };
        let mut children = Vec::new();
        for entry in entries {
            if !self.visit() {
                return;
            }
            if let Ok(entry) = entry {
                children.push(entry);
            }
        }
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            if self.paths.len() >= MAX_ITEMS || Instant::now() >= self.deadline {
                self.truncated = true;
                return;
            }
            if !child.file_name().to_str().is_some_and(safe_name) {
                continue;
            }
            let Ok(kind) = child.file_type() else {
                continue;
            };
            let path = relative.join(child.file_name());
            if kind.is_dir() {
                if depth < 12 {
                    self.walk(&path, depth + 1);
                } else {
                    self.truncated = true;
                }
            } else if kind.is_file() {
                self.add(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests;
