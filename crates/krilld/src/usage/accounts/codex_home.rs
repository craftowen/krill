//! Imported Codex homes must be real, short directories: Codex canonicalizes `CODEX_HOME`.
//! The source is read only; only credentials/config are copied, conversation/resource directories
//! are shared. Reference: Orca codex runtime-home services, Copyright (c) 2026 Lovecast Inc., MIT.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use super::write_atomic_0600;
use super::{SAVE_LOCK, load_file_result, lock, save_inner};
use krill_proto::usage::{ProviderKind, profiles};

#[cfg(unix)]
pub(super) fn prepare(home: &Path, source: &Path) -> io::Result<PathBuf> {
    use sha2::{Digest as _, Sha256};
    use std::os::unix::fs::DirBuilderExt as _;
    let canonical = source.canonicalize()?;
    if canonical.as_os_str().len() <= profiles::MAX_CODEX_HOME_BYTES {
        return Ok(source.into());
    }
    let digest = format!(
        "{:x}",
        Sha256::digest(canonical.as_os_str().as_encoded_bytes())
    );
    let key = format!("i-{}", digest.get(..10).unwrap_or(&digest));
    let target = profiles::short_codex_profile_dir(home, &key);
    // Very long user homes need an explicit, private short location; never use a shared /tmp home.
    if target.as_os_str().len() > profiles::MAX_CODEX_HOME_BYTES {
        return Ok(source.into());
    }
    let marker = target.join(".krill-import-source");
    if let Ok(origin) = std::fs::read_to_string(&marker) {
        if Path::new(origin.trim()) != canonical {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
    } else if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Codex import home is owned by another profile",
        ));
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&target)?;
    write_atomic_0600(&marker, canonical.as_os_str().as_encoded_bytes())?;
    for name in ["auth.json", "config.toml", "AGENTS.md"] {
        let dest = target.join(name);
        if dest.exists() {
            continue;
        }
        let src = canonical.join(name);
        if !src.is_file() {
            continue;
        }
        let bytes = super::super::direct::read_file(&src).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Codex profile file is unreadable or too large",
            )
        })?;
        write_atomic_0600(&dest, &bytes)?;
    }
    for name in [
        "sessions",
        "archived_sessions",
        "skills",
        "prompts",
        "rules",
    ] {
        let src = canonical.join(name);
        let dest = target.join(name);
        if src.is_dir() && std::fs::symlink_metadata(&dest).is_err() {
            std::os::unix::fs::symlink(&src, &dest)?;
        }
    }
    Ok(target)
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "Preserve the fallible Unix prepare contract for shared callers"
)]
pub(super) fn prepare(_: &Path, source: &Path) -> io::Result<PathBuf> {
    Ok(source.into())
}

/// Upgrade older registrations without modifying the Orca profile they referenced.
pub(crate) fn migrate(home: &Path, state: Option<&Path>) -> io::Result<()> {
    let _guard = lock(&SAVE_LOCK);
    let mut file = match load_file_result(home, state) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut changed = false;
    for account in &mut file.accounts {
        if ProviderKind::parse(&account.provider) != Some(ProviderKind::Codex) {
            continue;
        }
        let source = Path::new(&account.profile_dir);
        if !source.is_dir() {
            continue;
        }
        let target = if source.starts_with(profiles::managed_root(home).join("codex")) {
            profiles::migrate_long_codex_profile_if_needed(
                home,
                account.id.strip_prefix("codex-").unwrap_or(&account.id),
                source,
            )
        } else {
            prepare(home, source)?
        };
        if target != source {
            account.profile_dir = target.display().to_string();
            changed = true;
        }
    }
    if changed {
        save_inner(home, state, &file)?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn imported_home_stays_short_after_canonicalization_and_preserves_source() {
        let root = PathBuf::from(format!("/tmp/kcx-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let source =
            root.join("Orca/Application Support/codex-accounts/very-long-source-account-name/home");
        std::fs::create_dir_all(source.join("sessions/2026/10/20")).unwrap();
        std::fs::write(
            source.join("auth.json"),
            r#"{"tokens":{"access_token":"fixture-only","account_id":"team"}}"#,
        )
        .unwrap();
        let before = std::fs::read(source.join("auth.json")).unwrap();
        let short = prepare(&root, &source).unwrap();
        assert!(short.canonicalize().unwrap().as_os_str().len() <= profiles::MAX_CODEX_HOME_BYTES);
        assert!(
            !std::fs::symlink_metadata(&short)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(short.join("auth.json")).unwrap(), before);
        std::fs::write(short.join("auth.json"), "refreshed Krill credentials").unwrap();
        assert_eq!(std::fs::read(source.join("auth.json")).unwrap(), before);
        assert_eq!(prepare(&root, &source).unwrap(), short);
        assert_eq!(
            std::fs::read_to_string(short.join("auth.json")).unwrap(),
            "refreshed Krill credentials"
        );
        assert_eq!(
            short.join("sessions").canonicalize().unwrap(),
            source.join("sessions").canonicalize().unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
