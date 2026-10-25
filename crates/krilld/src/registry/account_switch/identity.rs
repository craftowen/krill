//! Recover identity from files held by this pane's process, never from project recency.

use std::io;
use std::path::{Path, PathBuf};

use krill_proto::usage::ProviderKind;

use super::transcript;

#[cfg(unix)]
mod claude;

#[cfg_attr(
    not(unix),
    expect(
        clippy::unnecessary_wraps,
        reason = "Unix process discovery may fail; other platforms keep the same interface"
    )
)]
pub(super) fn discover_claude(
    homes: &[PathBuf],
    root: u32,
    foreground: Option<u32>,
) -> io::Result<Option<Identity>> {
    #[cfg(unix)]
    {
        claude::discover(homes, root, foreground)
    }
    #[cfg(not(unix))]
    {
        let _ = (homes, root, foreground);
        Ok(None)
    }
}

fn process_pids(root: u32, foreground: Option<u32>) -> Vec<u32> {
    let mut pids = Vec::new();
    let mut current = foreground.unwrap_or(root);
    for _ in 0..16 {
        if current <= 1 || pids.contains(&current) {
            break;
        }
        pids.push(current);
        if current == root {
            break;
        }
        let Ok(parent) = crate::sys::parent_pid(current) else {
            break;
        };
        current = parent;
    }
    if pids.last() != Some(&root) {
        pids.clear();
        pids.push(root);
    }
    pids
}

pub(super) fn descendant_pids(root: u32, foreground: Option<u32>) -> Vec<u32> {
    let seeds = process_pids(root, foreground);
    let Ok(all) = crate::sys::all_pids() else {
        return Vec::new();
    };
    let mut seen = seeds;
    let mut frontier = seen.clone();
    let mut out = Vec::new();
    for _ in 0..3 {
        let mut next = Vec::new();
        for &pid in &all {
            if pid <= 1 || seen.contains(&pid) {
                continue;
            }
            if crate::sys::parent_pid(pid).is_ok_and(|ppid| frontier.contains(&ppid)) {
                seen.push(pid);
                out.push(pid);
                next.push(pid);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    out
}

pub(in crate::registry) struct Identity {
    pub id: String,
    pub path: PathBuf,
    pub home: PathBuf,
}

/// Inspect only the foreground ancestry belonging to this PTY and its root process.
/// Files opened by background helpers or sibling panes cannot bind the conversation.
pub(in crate::registry) fn discover(
    provider: ProviderKind,
    root: u32,
    foreground: Option<u32>,
) -> io::Result<Option<Identity>> {
    if !matches!(
        provider,
        ProviderKind::Claude | ProviderKind::Codex | ProviderKind::Antigravity
    ) || root <= 1
    {
        return Ok(None);
    }
    let Ok(root_start) = crate::sys::start_us(root) else {
        return Ok(None);
    };
    let mut paths = Vec::new();
    for pid in process_pids(root, foreground) {
        let Ok(start) = crate::sys::start_us(pid) else {
            continue;
        };
        let observed = crate::sys::open_vnode_paths(pid);
        if crate::sys::start_us(pid).ok() == Some(start) {
            paths.extend(observed);
        }
    }
    if crate::sys::start_us(root).ok() != Some(root_start) {
        return Ok(None);
    }
    let found = from_paths(provider, paths)?;
    if found.is_some() {
        return Ok(found);
    }
    // Configured launchers and Node/shell wrappers often keep a shell or wrapper as group
    // leader while a descendant process in this PTY tree holds the conversation files.
    let mut paths = Vec::new();
    for pid in descendant_pids(root, foreground) {
        let Ok(start) = crate::sys::start_us(pid) else {
            continue;
        };
        let observed = crate::sys::open_vnode_paths(pid);
        if crate::sys::start_us(pid).ok() == Some(start) {
            paths.extend(observed);
        }
    }
    if crate::sys::start_us(root).ok() != Some(root_start) {
        return Ok(None);
    }
    from_paths(provider, paths)
}

fn from_paths(
    provider: ProviderKind,
    paths: impl IntoIterator<Item = PathBuf>,
) -> io::Result<Option<Identity>> {
    if provider == ProviderKind::Antigravity {
        return agy_from_paths(paths);
    }
    let mut found: Option<Identity> = None;
    for path in paths {
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let Some(home) = profile_home(provider, &path) else {
            continue;
        };
        let id = match transcript::file_session_id(&path, provider) {
            Ok(id) => id,
            Err(error) if error.kind() == io::ErrorKind::Unsupported => continue,
            Err(error) => return Err(error),
        };
        if let Some(previous) = &found {
            if previous.id != id || !same_file::is_same_file(&previous.path, &path)? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "process holds multiple conversation transcripts",
                ));
            }
        } else {
            found = Some(Identity { id, path, home });
        }
    }
    Ok(found)
}

fn agy_from_paths(paths: impl IntoIterator<Item = PathBuf>) -> io::Result<Option<Identity>> {
    let paths: Vec<_> = paths.into_iter().collect();
    // agy also opens old brain directories. Its held presence lock identifies the live
    // conversation; otherwise require one unambiguous open conversation database.
    let presence = paths.iter().any(|p| {
        p.parent().is_some_and(|d| d.ends_with("presence"))
            && p.extension().is_some_and(|e| e == "lock")
    });
    let mut found: Option<Identity> = None;
    for path in paths.iter().filter(|p| {
        if presence {
            p.parent().is_some_and(|d| d.ends_with("presence"))
                && p.extension().is_some_and(|e| e == "lock")
        } else {
            p.parent().is_some_and(|d| d.ends_with("conversations"))
                && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    p.extension().and_then(|e| e.to_str()).is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("db")
                            || ext.eq_ignore_ascii_case("db-wal")
                            || ext.eq_ignore_ascii_case("db-shm")
                    }) && !n.is_empty()
                })
        }
    }) {
        let Some((home, id)) = crate::agent_history::vault::agy_root_and_id_from_path(path) else {
            continue;
        };
        if let Some(previous) = &found {
            if previous.id != id || previous.home != home {
                return Err(io::ErrorKind::InvalidData.into());
            }
        } else {
            let conv_dir = home.join("brain").join(&id);
            let logs = conv_dir.join(".system_generated/logs");
            let transcript = [
                logs.join("transcript.jsonl"),
                logs.join("transcript_full.jsonl"),
                conv_dir.join("transcript.jsonl"),
                home.join("sessions").join(format!("{id}.jsonl")),
            ]
            .into_iter()
            .find(|p| p.is_file());
            found = Some(Identity {
                path: transcript
                    .unwrap_or_else(|| home.join("conversations").join(format!("{id}.db"))),
                home,
                id,
            });
        }
    }
    Ok(found)
}

fn profile_home(provider: ProviderKind, path: &Path) -> Option<PathBuf> {
    match provider {
        ProviderKind::Claude => {
            let project = path.parent()?;
            let projects = project.parent()?;
            (projects.file_name()? == "projects")
                .then(|| projects.parent().map(Path::to_path_buf))?
        }
        ProviderKind::Codex => {
            // sessions/YYYY/MM/DD/rollout-*.jsonl; nested subagent files are rejected below.
            if !path.file_name()?.to_str()?.starts_with("rollout-") {
                return None;
            }
            let sessions = path.ancestors().nth(4)?;
            (sessions.file_name()? == "sessions")
                .then(|| sessions.parent().map(Path::to_path_buf))?
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
