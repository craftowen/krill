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

use crate::agent::control::LaunchAccount;
use crate::usage::accounts;

pub(super) fn prepare(
    provider: ProviderKind,
    home: &Path,
    source: &LaunchAccount,
    target: &AccountInfo,
    id: &str,
    cwd: &[u8],
) -> io::Result<()> {
    let from = if source.dir.is_empty() {
        accounts::host_default_dir(home, provider)
    } else {
        PathBuf::from(&source.dir)
    };
    let to = Path::new(&target.profile_dir);
    if from == to
        || from
            .canonicalize()
            .ok()
            .zip(to.canonicalize().ok())
            .is_some_and(|(a, b)| a == b)
    {
        return Ok(());
    }
    match provider {
        ProviderKind::Claude | ProviderKind::Codex => {
            let base = if provider == ProviderKind::Claude {
                "projects"
            } else {
                "sessions"
            };
            let root = from.join(base);
            let path = find_transcript(&root, provider, id)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "conversation transcript is not available yet",
                )
            })?;
            let relative = path.strip_prefix(&from).map_err(io::Error::other)?;
            link_file(&path, &to.join(relative))?;
            if provider == ProviderKind::Claude {
                let children = path.with_file_name(id);
                if children.is_dir() {
                    link_tree(&children, &to.join(relative).with_file_name(id), &mut 4096)?;
                }
            }
            Ok(())
        }
        ProviderKind::Antigravity => {
            let target_root = if target.host_default {
                to.to_path_buf()
            } else {
                to.join(".gemini/antigravity-cli")
            };
            for root in crate::agent_history::vault::agy_candidate_dirs(&from, true) {
                let brain = root.join("brain").join(id);
                if brain.is_dir() {
                    return link_tree(&brain, &target_root.join("brain").join(id), &mut 4096);
                }
                let session = root.join("sessions").join(format!("{id}.jsonl"));
                if session.is_file() {
                    return link_file(
                        &session,
                        &target_root.join("sessions").join(format!("{id}.jsonl")),
                    );
                }
            }
            Err(io::ErrorKind::NotFound.into())
        }
        ProviderKind::OpenCode => opencode(source, target, id, cwd),
        ProviderKind::Gemini => Err(io::ErrorKind::InvalidInput.into()),
    }
}

fn find_transcript(root: &Path, provider: ProviderKind, id: &str) -> io::Result<Option<PathBuf>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut pending = vec![(root.to_path_buf(), 0_u8)];
    let mut nodes = 0_u32;
    while let Some((dir, depth)) = pending.pop() {
        if depth > 4 || nodes >= 20_000 || Instant::now() > deadline {
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
                pending.push((path, depth.saturating_add(1)));
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
            if provider == ProviderKind::Codex {
                let mut head = Vec::new();
                File::open(&path)?.take(128 * 1024).read_to_end(&mut head)?;
                let valid = String::from_utf8_lossy(&head)
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .any(|v| {
                        v.get("type").and_then(serde_json::Value::as_str) == Some("session_meta")
                            && v.pointer("/payload/id").and_then(serde_json::Value::as_str)
                                == Some(id)
                    });
                if !valid {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "rollout identity mismatch",
                    ));
                }
            }
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn link_tree(from: &Path, to: &Path, budget: &mut usize) -> io::Result<()> {
    if *budget == 0 {
        return Err(io::ErrorKind::FileTooLarge.into());
    }
    if fs::symlink_metadata(from)?.file_type().is_symlink() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    if to.exists() && fs::symlink_metadata(to)?.file_type().is_symlink() {
        return Err(io::ErrorKind::AlreadyExists.into());
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
            let (mut a, mut b) = (File::open(from)?, File::open(to)?);
            if a.metadata()?.len() != b.metadata()?.len() {
                return Err(e);
            }
            let (mut left, mut right) = ([0; 8192], [0; 8192]);
            loop {
                let an = a.read(&mut left)?;
                let bn = b.read(&mut right)?;
                if an != bn || left.get(..an) != right.get(..bn) {
                    return Err(e);
                }
                if an == 0 {
                    return Ok(());
                }
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
        fs::write(to.join(&name), "conflicting conversation").unwrap();
        assert!(prepare(ProviderKind::Codex, &root, &source, &target, id, b"").is_err());
        assert!(fs::read_to_string(from.join(&name)).unwrap().contains(id));
        fs::remove_dir_all(root).unwrap();
    }
}
