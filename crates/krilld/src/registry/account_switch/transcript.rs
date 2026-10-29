//! Only conversation data is bridged. Credentials stay in the selected account's home.
//! Reference: Orca src/main/codex/codex-legacy-session-resume.ts,
//! Copyright (c) 2026 Lovecast Inc., MIT License.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use krill_proto::os::ffi::OsStrExt as _;
use krill_proto::os::fs::OpenOptionsExt as _;
use krill_proto::usage::{AccountInfo, ProviderKind};

use crate::agent::control::{LaunchAccount, StoredLaunch, valid_session_id, valid_transcript_path};
use crate::usage::accounts;

mod agy;

/// A bound ID (launch/resume/hook) is authoritative. A hook-bound file can also
/// supply the ID, but cwd/mtime searches cannot prove which process owns a conversation.
pub(super) fn session_id(provider: ProviderKind, stored: &StoredLaunch) -> io::Result<String> {
    if let Some(id) = stored.session_id.as_deref() {
        return if valid_session_id(id) {
            Ok(id.to_owned())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid bound conversation identity",
            ))
        };
    }
    if let Some(path) = stored.transcript_path.as_deref() {
        if !valid_transcript_path(path) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if provider == ProviderKind::Antigravity
            && let Some((_, id)) =
                crate::agent_history::vault::agy_root_and_id_from_path(Path::new(path))
        {
            return Ok(id);
        }
        return file_session_id(Path::new(path), provider);
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "conversation identity is not bound",
    ))
}

/// The conversation as the selected profile will see it, re-checked before the original retires.
pub(super) struct PreparedTranscript {
    path: Option<PathBuf>,
    provider: ProviderKind,
    id: String,
    /// Antigravity's SQLite conversation, published as an independent copy (see `agy`).
    agy: Option<agy::Bridge>,
}

impl PreparedTranscript {
    pub(super) fn verify(&self) -> io::Result<()> {
        if self.provider == ProviderKind::Antigravity {
            // Only the conversation database makes an Antigravity conversation resumable.
            return self
                .agy
                .as_ref()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?
                .verify();
        }
        if let Some(path) = &self.path {
            if matches!(self.provider, ProviderKind::Claude | ProviderKind::Codex) {
                if file_session_id(path, self.provider)? != self.id {
                    return Err(io::ErrorKind::InvalidData.into());
                }
            } else if !path.exists() {
                return Err(io::ErrorKind::NotFound.into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn prepare(
    provider: ProviderKind,
    home: &Path,
    source: &LaunchAccount,
    target: &AccountInfo,
    id: &str,
    cwd: &[u8],
) -> io::Result<PreparedTranscript> {
    prepare_bound(provider, home, source, target, id, cwd, None)
}

pub(super) fn prepare_bound(
    provider: ProviderKind,
    home: &Path,
    source: &LaunchAccount,
    target: &AccountInfo,
    id: &str,
    cwd: &[u8],
    bound_path: Option<&str>,
) -> io::Result<PreparedTranscript> {
    let from = if source.dir.is_empty() {
        accounts::host_default_dir(home, provider)
    } else {
        PathBuf::from(&source.dir)
    };
    let to = Path::new(&target.profile_dir);
    // The same profile still has to hold the conversation; it only skips linking.
    let same_profile = from == to
        || from
            .canonicalize()
            .ok()
            .zip(to.canonicalize().ok())
            .is_some_and(|(a, b)| a == b);
    let mut agy_bridge = None;
    let path = match provider {
        ProviderKind::Claude | ProviderKind::Codex => {
            let base = if provider == ProviderKind::Claude {
                "projects"
            } else {
                "sessions"
            };
            let root = from.join(base);
            let bound = bound_path.map(Path::new).filter(|path| path.exists());
            let path = if let Some(path) = bound {
                let canonical = path.canonicalize()?;
                if !canonical.starts_with(root.canonicalize()?)
                    || file_session_id(path, provider)? != id
                {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                path.to_path_buf()
            } else {
                find_transcript(&root, provider, id)?.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "conversation transcript is not available yet",
                    )
                })?
            };
            // A renamed profile may retain a symlink alias, while its hook reports the
            // canonical path. Preserve the provider-relative layout in either spelling.
            let canonical = path.canonicalize()?;
            let provider_root = root.canonicalize()?;
            let relative = Path::new(base).join(
                canonical
                    .strip_prefix(&provider_root)
                    .map_err(io::Error::other)?,
            );
            let destination = to.join(&relative);
            if !same_profile {
                link_file(&path, &destination)?;
                if provider == ProviderKind::Claude {
                    let children = path.with_file_name(id);
                    if children.is_dir() {
                        link_tree(&children, &destination.with_file_name(id), &mut 4096)?;
                    }
                }
            }
            Some(destination)
        }
        ProviderKind::Antigravity => {
            let target_root = if target.host_default {
                to.to_path_buf()
            } else {
                to.join(".gemini/antigravity-cli")
            };
            // A process-attributed transcript path names the home that actually holds the
            // conversation. It must name this conversation; a different ID is a mismatch.
            let bound_root = match bound_path
                .and_then(|p| crate::agent_history::vault::agy_root_and_id_from_path(Path::new(p)))
            {
                Some((_, bound_id)) if bound_id != id => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "bound transcript names a different conversation",
                    ));
                }
                Some((root, _)) => Some(root),
                None => None,
            };
            let roots = crate::agent_history::vault::agy_candidate_dirs(&from, true);
            let bridged = agy::prepare(bound_root.as_deref(), &roots, &target_root, id)?;
            let path = bridged.target_db().to_path_buf();
            agy_bridge = Some(bridged);
            Some(path)
        }
        ProviderKind::OpenCode => {
            // Importing into the database it was exported from could duplicate the session.
            if !same_profile {
                opencode(source, target, id, cwd)?;
            }
            None
        }
        ProviderKind::Gemini => return Err(io::ErrorKind::InvalidInput.into()),
    };
    let prepared = PreparedTranscript {
        path,
        provider,
        id: id.to_owned(),
        agy: agy_bridge,
    };
    prepared.verify()?;
    Ok(prepared)
}

/// Read only provider identity metadata, never infer an ID from an arbitrary rollout filename.
pub(super) fn file_session_id(path: &Path, provider: ProviderKind) -> io::Result<String> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut head = Vec::new();
    File::open(path)?.take(128 * 1024).read_to_end(&mut head)?;
    // A newly opened rollout may be visible before its metadata write completes.
    // Only empty/incomplete JSON is retryable; complete invalid metadata stays fatal.
    let pending_header = head.is_empty()
        || serde_json::from_slice::<serde_json::Value>(&head).is_err_and(|e| e.is_eof());
    let mut identity: Option<String> = None;
    for record in String::from_utf8_lossy(&head)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
    {
        let id = match provider {
            ProviderKind::Codex
                if record.get("type").and_then(serde_json::Value::as_str)
                    == Some("session_meta") =>
            {
                // A subagent rollout is a valid file, just never a parent conversation.
                // `Unsupported` lets callers skip it without ignoring corrupt metadata.
                if record.pointer("/payload/source/subagent").is_some() {
                    return Err(io::ErrorKind::Unsupported.into());
                }
                record.pointer("/payload/id")
            }
            ProviderKind::Claude => {
                if record
                    .get("isSidechain")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    return Err(io::ErrorKind::Unsupported.into());
                }
                record.get("sessionId")
            }
            _ => None,
        }
        .and_then(serde_json::Value::as_str);
        if let Some(id) = id {
            if !valid_session_id(id) || identity.as_deref().is_some_and(|previous| previous != id) {
                return Err(io::ErrorKind::InvalidData.into());
            }
            identity = Some(id.to_owned());
        }
    }
    identity.ok_or_else(|| {
        io::Error::new(
            if pending_header {
                io::ErrorKind::WouldBlock
            } else {
                io::ErrorKind::InvalidData
            },
            "conversation transcript has no verified identity",
        )
    })
}

pub(super) fn find_transcript(
    root: &Path,
    provider: ProviderKind,
    id: &str,
) -> io::Result<Option<PathBuf>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    // Only search provider-owned transcript levels. Claude subagent/workflow trees
    // can exceed the whole search budget and cannot contain the parent conversation.
    let max_depth = if provider == ProviderKind::Claude {
        1
    } else {
        3
    };
    let mut pending = vec![(root.to_path_buf(), 0_u8)];
    let mut nodes = 0_u32;
    let mut found = None;
    while let Some((dir, depth)) = pending.pop() {
        if nodes >= 20_000 || Instant::now() > deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            nodes = nodes.saturating_add(1);
            if nodes > 20_000 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                if depth < max_depth {
                    pending.push((path, depth.saturating_add(1)));
                }
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let matches = if provider == ProviderKind::Claude {
                name == format!("{id}.jsonl")
            } else {
                name.starts_with("rollout-") && name.ends_with(&format!("-{id}.jsonl"))
            };
            if !matches || !entry.file_type()?.is_file() {
                continue;
            }
            if file_session_id(&path, provider)? != id || found.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "conversation transcript identity is mismatched or ambiguous",
                ));
            }
            found = Some(path);
        }
    }
    Ok(found)
}

fn link_tree(from: &Path, to: &Path, budget: &mut usize) -> io::Result<()> {
    if *budget == 0 {
        return Err(io::ErrorKind::FileTooLarge.into());
    }
    if fs::symlink_metadata(from)?.file_type().is_symlink() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    if to.exists() {
        if same_file::is_same_file(from, to).unwrap_or(false) {
            return Ok(());
        }
        if fs::symlink_metadata(to)?.file_type().is_symlink() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        *budget = budget.checked_sub(1).ok_or(io::ErrorKind::FileTooLarge)?;
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            link_tree(&entry.path(), &dest, budget)?;
        } else {
            link_file(&entry.path(), &dest)?;
        }
    }
    Ok(())
}

fn link_file(from: &Path, to: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(from)?.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let parent = to.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(parent)?;
    match fs::hard_link(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(to)?.is_file() {
                return Err(e);
            }
            // Equal bytes in separate files do not share the original's subsequent writes.
            if same_file::is_same_file(from, to)? {
                Ok(())
            } else {
                Err(e)
            }
        }
        Err(e) => Err(e),
    }
}

fn opencode(source: &LaunchAccount, target: &AccountInfo, id: &str, cwd: &[u8]) -> io::Result<()> {
    let path = std::ffi::OsStr::from_bytes(crate::shell::login_path());
    let exe = krill_proto::agent::which("opencode", path).ok_or(io::ErrorKind::NotFound)?;
    opencode_with_exe(&exe, path, source, target, id, cwd)
}

fn opencode_with_exe(
    exe: &Path,
    path: &std::ffi::OsStr,
    source: &LaunchAccount,
    target: &AccountInfo,
    id: &str,
    cwd: &[u8],
) -> io::Result<()> {
    let command = |dir: Option<&Path>| {
        let mut command = Command::new(exe);
        command
            .env("PATH", path)
            .env_remove("OPENCODE_CONFIG_DIR")
            .env_remove("OPENCODE_AUTH_CONTENT")
            .env_remove("OPENCODE_DB");
        if let Some(dir) = dir {
            command
                .env("XDG_DATA_HOME", dir.join("data"))
                .env("XDG_STATE_HOME", dir.join("state"))
                .env("OPENCODE_DB", "opencode.db");
        }
        if !cwd.is_empty() {
            command.current_dir(Path::new(std::ffi::OsStr::from_bytes(cwd)));
        }
        command
    };
    let mut export = command((!source.dir.is_empty()).then(|| Path::new(&source.dir)));
    export.args(["export", id]);
    let raw = capture(export)?;
    let json: serde_json::Value = serde_json::from_slice(&raw).map_err(io::Error::other)?;
    if json.pointer("/info/id").and_then(serde_json::Value::as_str) != Some(id) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let temp =
        std::env::temp_dir().join(format!("krill-session-{}-{nonce}.json", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&raw)?;
        drop(file);
        let mut import = command((!target.host_default).then(|| Path::new(&target.profile_dir)));
        import.arg("import").arg(&temp);
        capture(import)?;
        Ok(())
    })();
    fs::remove_file(temp).ok();
    result
}

fn capture(mut command: Command) -> io::Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let pipe = child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut raw = Vec::new();
        let result = pipe
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut raw)
            .map(|_| raw);
        tx.send(result).ok();
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            None => {
                child.kill().ok();
                child.wait().ok();
                return Err(io::ErrorKind::TimedOut.into());
            }
        }
    };
    if !status.success() {
        return Err(io::Error::other("OpenCode conversation transfer failed"));
    }
    let raw = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(io::Error::other)??;
    if raw.len() > 32 * 1024 * 1024 {
        return Err(io::ErrorKind::FileTooLarge.into());
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn opencode_export_import_uses_each_accounts_data_home_and_orcas_database_name() {
        use std::os::unix::fs::PermissionsExt as _;
        let root =
            std::env::temp_dir().join(format!("krill-opencode-handoff-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let from = root.join("source account");
        let to = root.join("target account");
        for dir in [&from, &to] {
            fs::create_dir_all(dir.join("data/opencode")).unwrap();
        }
        fs::write(from.join("data/opencode/auth.json"), "source credentials").unwrap();
        fs::write(to.join("data/opencode/auth.json"), "selected credentials").unwrap();
        let exe = root.join("opencode");
        fs::write(
            &exe,
            r#"#!/bin/sh
[ "$OPENCODE_DB" = opencode.db ] || exit 3
case "$1" in
export)
  [ "$(cat "$XDG_DATA_HOME/opencode/auth.json")" = 'source credentials' ] || exit 4
  printf '{"info":{"id":"ses_fixture"},"messages":[]}'
  ;;
import)
  [ "$(cat "$XDG_DATA_HOME/opencode/auth.json")" = 'selected credentials' ] || exit 5
  cp "$2" "$XDG_DATA_HOME/imported.json"
  ;;
*) exit 6;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        let source = LaunchAccount {
            id: "opencode-source".into(),
            env_key: "OPENCODE_CONFIG_DIR".into(),
            dir: from.display().to_string(),
        };
        let target = AccountInfo {
            id: "opencode-target".into(),
            provider: ProviderKind::OpenCode,
            label: "Target".into(),
            email: None,
            profile_dir: to.display().to_string(),
            active: true,
            host_default: false,
            stale: false,
            usage: krill_proto::usage::Snapshot::default(),
        };
        opencode_with_exe(
            &exe,
            std::ffi::OsStr::new("/usr/bin:/bin"),
            &source,
            &target,
            "ses_fixture",
            b"",
        )
        .unwrap();
        let imported: serde_json::Value =
            serde_json::from_slice(&fs::read(to.join("data/imported.json")).unwrap()).unwrap();
        assert_eq!(
            imported
                .pointer("/info/id")
                .and_then(serde_json::Value::as_str),
            Some("ses_fixture")
        );
        assert_eq!(
            fs::read_to_string(to.join("data/opencode/auth.json")).unwrap(),
            "selected credentials"
        );
        assert!(
            opencode_with_exe(
                &exe,
                std::ffi::OsStr::new("/usr/bin:/bin"),
                &source,
                &target,
                "different_session",
                b""
            )
            .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_bridge_verifies_identity_preserves_auth_and_rejects_conflicts() {
        let root = std::env::temp_dir().join(format!("krill-handoff-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let from = root.join("from");
        let to = root.join("to");
        fs::create_dir_all(from.join("sessions/2026/10/20")).unwrap();
        fs::create_dir_all(&to).unwrap();
        let id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let name = format!("sessions/2026/10/20/rollout-date-{id}.jsonl");
        fs::write(
            from.join(&name),
            format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\"}}}}\n"),
        )
        .unwrap();
        fs::write(from.join("auth.json"), "old credentials").unwrap();
        fs::write(to.join("auth.json"), "selected credentials").unwrap();
        let source = LaunchAccount {
            id: "codex-old".into(),
            env_key: "CODEX_HOME".into(),
            dir: from.display().to_string(),
        };
        let target = AccountInfo {
            id: "codex-new".into(),
            provider: ProviderKind::Codex,
            label: "New".into(),
            email: None,
            profile_dir: to.display().to_string(),
            active: true,
            host_default: false,
            stale: false,
            usage: krill_proto::usage::Snapshot::default(),
        };
        prepare(ProviderKind::Codex, &root, &source, &target, id, b"").unwrap();
        assert_eq!(
            fs::read(to.join(&name)).unwrap(),
            fs::read(from.join(&name)).unwrap()
        );
        assert_eq!(
            fs::read_to_string(to.join("auth.json")).unwrap(),
            "selected credentials"
        );
        prepare(ProviderKind::Codex, &root, &source, &target, id, b"").unwrap();
        fs::remove_file(to.join(&name)).unwrap();
        fs::copy(from.join(&name), to.join(&name)).unwrap();
        assert!(
            matches!(prepare(ProviderKind::Codex, &root, &source, &target, id, b""), Err(error) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        fs::remove_file(to.join(&name)).unwrap();
        fs::write(to.join(&name), "conflicting conversation").unwrap();
        assert!(prepare(ProviderKind::Codex, &root, &source, &target, id, b"").is_err());
        assert!(fs::read_to_string(from.join(&name)).unwrap().contains(id));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_transcript_search_skips_nested_subagent_workflow_dirs() {
        // Claude Code workflow subagents leave `<project>/<sid>/subagents/workflows/wf_*`, one level
        // below the search depth. Siblings on both sides keep the test order-independent.
        let root =
            std::env::temp_dir().join(format!("krill-deep-transcript-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let projects = root.join("projects");
        for project in ["a-deep", "z-deep"] {
            let deep = projects.join(project).join("sid/subagents/workflows/wf_1");
            fs::create_dir_all(&deep).unwrap();
            fs::write(deep.join("agent.jsonl"), "{}").unwrap();
        }
        let id = "3b71560c-6c71-4dce-86d0-5ce025eac068";
        let wanted = projects.join("m-target").join(format!("{id}.jsonl"));
        fs::create_dir_all(wanted.parent().unwrap()).unwrap();
        fs::write(&wanted, format!("{{\"sessionId\":\"{id}\"}}\n")).unwrap();
        assert_eq!(
            find_transcript(&projects, ProviderKind::Claude, id).unwrap(),
            Some(wanted)
        );
        assert_eq!(
            find_transcript(&projects, ProviderKind::Claude, "missing").unwrap(),
            None
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn claude_transcript_search_does_not_open_unrelated_subagent_dirs() {
        use std::os::unix::fs::PermissionsExt as _;

        let root =
            std::env::temp_dir().join(format!("krill-unreadable-subagent-{}", std::process::id()));
        let subagents = root.join("project/sid/subagents");
        fs::create_dir_all(&subagents).unwrap();
        fs::set_permissions(&subagents, fs::Permissions::from_mode(0o000)).unwrap();
        let result = find_transcript(&root, ProviderKind::Claude, "missing");
        // Restore permissions even when the search regresses, so fixture cleanup succeeds.
        fs::set_permissions(&subagents, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(root).unwrap();
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn same_cwd_sessions_keep_distinct_bound_ids_and_bound_rollout_path_uses_metadata() {
        use krill_proto::agent::Permission;
        let root = std::env::temp_dir().join(format!("kas-identity-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let from = root.join("from");
        let to = root.join("to");
        fs::create_dir_all(from.join("sessions/2026/10/20")).unwrap();
        fs::create_dir_all(&to).unwrap();
        let source = LaunchAccount {
            id: "codex-source".into(),
            env_key: "CODEX_HOME".into(),
            dir: from.display().to_string(),
        };
        let target = AccountInfo {
            id: "codex-target".into(),
            provider: ProviderKind::Codex,
            label: "Target".into(),
            email: None,
            profile_dir: to.display().to_string(),
            active: true,
            host_default: false,
            stale: false,
            usage: krill_proto::usage::Snapshot::default(),
        };
        let mut sessions = Vec::new();
        for id in ["conversation-one", "conversation-two"] {
            let path = from.join(format!("sessions/2026/10/20/rollout-date-{id}.jsonl"));
            fs::write(&path, format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/same/project\"}}}}\n")).unwrap();
            sessions.push(StoredLaunch {
                profile: "codex".into(),
                permission: Permission::Manual,
                session_id: Some(id.into()),
                transcript_path: Some(path.display().to_string()),
                cwd: b"/same/project".to_vec(),
                hibernation_blocked: false,
                account: Some(source.clone()),
            });
        }
        for stored in &sessions {
            let id = session_id(ProviderKind::Codex, stored).unwrap();
            assert_eq!(Some(&id), stored.session_id.as_ref());
            let prepared = prepare(
                ProviderKind::Codex,
                &root,
                &source,
                &target,
                &id,
                &stored.cwd,
            )
            .unwrap();
            prepared.verify().unwrap();
            let path = prepared.path.as_ref().unwrap();
            assert_eq!(file_session_id(path, ProviderKind::Codex).unwrap(), id);
            fs::remove_file(path).unwrap();
            assert_eq!(
                prepared.verify().unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
        }
        let mut path_only = sessions[0].clone();
        path_only.session_id = None;
        assert_eq!(
            session_id(ProviderKind::Codex, &path_only).unwrap(),
            "conversation-one"
        );
        path_only.transcript_path = None;
        assert_eq!(
            session_id(ProviderKind::Codex, &path_only)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bound_transcript_rejects_conflicting_metadata_and_same_profile_requires_file() {
        use krill_proto::agent::Permission;
        let root = std::env::temp_dir().join(format!("kas-bound-file-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        let path = root.join("bound.jsonl");
        let stored = StoredLaunch {
            profile: "codex".into(),
            permission: Permission::Manual,
            session_id: None,
            transcript_path: Some(path.display().to_string()),
            cwd: b"/same/project".to_vec(),
            hibernation_blocked: false,
            account: None,
        };
        assert_eq!(
            session_id(ProviderKind::Codex, &stored).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        fs::write(&path, "{\"type\":\"session_meta\",\"payload\":{\"id\":\"one\"}}\n{\"type\":\"session_meta\",\"payload\":{\"id\":\"two\"}}\n").unwrap();
        assert_eq!(
            session_id(ProviderKind::Codex, &stored).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::write(&path, "{\"sessionId\":\"claude-one\"}\n").unwrap();
        assert_eq!(
            session_id(ProviderKind::Claude, &stored).unwrap(),
            "claude-one"
        );
        let source = LaunchAccount {
            id: "codex-source".into(),
            env_key: "CODEX_HOME".into(),
            dir: root.display().to_string(),
        };
        let target = AccountInfo {
            id: "codex-target".into(),
            provider: ProviderKind::Codex,
            label: "Target".into(),
            email: None,
            profile_dir: root.display().to_string(),
            active: true,
            host_default: false,
            stale: false,
            usage: krill_proto::usage::Snapshot::default(),
        };
        assert!(
            prepare(
                ProviderKind::Codex,
                &root,
                &source,
                &target,
                "missing",
                b"/same/project"
            )
            .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod bound_path_tests {
    use super::*;

    #[test]
    fn bound_path_preserves_relative_layout_and_rejects_other_profiles() {
        let root = std::env::temp_dir().join(format!("kas-bound-path-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let from = root.join("from");
        let to = root.join("to");
        let path = from.join("sessions/2026/10/23/rollout-known.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(&path, r#"{"type":"session_meta","payload":{"id":"known"}}"#).unwrap();
        // The bound path works without a filename-derived ID and avoids scanning unrelated trees.
        let source = LaunchAccount {
            id: "source".into(),
            env_key: "CODEX_HOME".into(),
            dir: from.display().to_string(),
        };
        let target = AccountInfo {
            id: "target".into(),
            provider: ProviderKind::Codex,
            label: "Target".into(),
            email: None,
            profile_dir: to.display().to_string(),
            active: true,
            host_default: false,
            stale: false,
            usage: krill_proto::usage::Snapshot::default(),
        };
        prepare_bound(
            ProviderKind::Codex,
            &root,
            &source,
            &target,
            "known",
            b"",
            path.to_str(),
        )
        .unwrap()
        .verify()
        .unwrap();
        let relative = path.strip_prefix(&from).unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            fs::read(to.join(relative)).unwrap()
        );
        #[cfg(unix)]
        {
            let alias = root.join("source-alias");
            std::os::unix::fs::symlink(&from, &alias).unwrap();
            let alias_source = LaunchAccount {
                dir: alias.display().to_string(),
                ..source.clone()
            };
            prepare_bound(
                ProviderKind::Codex,
                &root,
                &alias_source,
                &target,
                "known",
                b"",
                path.to_str(),
            )
            .unwrap()
            .verify()
            .unwrap();
        }
        let foreign = root.join("foreign.jsonl");
        fs::write(&foreign, fs::read(&path).unwrap()).unwrap();
        assert!(
            matches!(prepare_bound(ProviderKind::Codex, &root, &source, &target, "known", b"", foreign.to_str()), Err(e) if e.kind() == io::ErrorKind::InvalidData)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subagent_transcripts_are_unsupported_but_corrupt_metadata_stays_invalid() {
        let root = std::env::temp_dir().join(format!("kas-subagent-kind-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir_all(&root).unwrap();
        let path = root.join("t.jsonl");
        for (provider, body, kind) in [
            (
                ProviderKind::Codex,
                r#"{"type":"session_meta","payload":{"id":"child","source":{"subagent":{}}}}"#,
                io::ErrorKind::Unsupported,
            ),
            (
                ProviderKind::Claude,
                r#"{"sessionId":"child","isSidechain":true}"#,
                io::ErrorKind::Unsupported,
            ),
            (
                ProviderKind::Claude,
                "{\"sessionId\":\"one\"}\n{\"sessionId\":\"two\"}",
                io::ErrorKind::InvalidData,
            ),
        ] {
            fs::write(&path, body).unwrap();
            assert_eq!(
                file_session_id(&path, provider).unwrap_err().kind(),
                kind,
                "{body}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
