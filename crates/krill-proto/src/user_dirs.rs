//! Homedir-managed roots with one-time migration from `minikrill` names.
//!
//! - `~/.config/krill` (accounts/hosts/profiles/API keys), from `~/.config/minikrill`
//! - `~/.local/share/krill` (staged helper binaries), from `~/.local/share/minikrill`
//!
//! Unix migration moves the old dir to the new name and leaves a symlink behind so
//! still-running old binaries keep working. It is idempotent and safe under
//! concurrent processes and `kill -9` (a missing compat link is recreated on
//! the next call). Existing data is never merged or deleted: if both names exist
//! as real dirs, the new one wins and the old one is left alone. Other platforms
//! retain existing legacy roots so active profile paths remain reachable.

use std::fs;
use std::path::{Path, PathBuf};

/// New config root name (`~/.config/krill`).
pub const CONFIG_DIR_NAME: &str = "krill";
/// Previous config root name (migrated on first use).
const LEGACY_CONFIG_DIR_NAME: &str = "minikrill";
/// New share root name (`~/.local/share/krill`).
pub const SHARE_DIR_NAME: &str = "krill";
/// Previous share root name (migrated on first use).
const LEGACY_SHARE_DIR_NAME: &str = "minikrill";

/// Config root, migrating the previous name on first use.
#[must_use]
pub fn config_dir(home: &Path) -> PathBuf {
    migrated(
        &home.join(".config"),
        CONFIG_DIR_NAME,
        LEGACY_CONFIG_DIR_NAME,
    )
}

/// Repository config: the new name wins, including malformed/unreadable files.
/// Only an absent new file permits reading the legacy name.
#[must_use]
pub fn repo_config(repo: &Path) -> PathBuf {
    preferred_path(repo, "krill.toml", "minikrill.toml")
}

/// Reuse an installed legacy plugin rather than loading duplicate callbacks.
#[must_use]
pub fn opencode_plugin(config: &Path) -> PathBuf {
    preferred_path(&config.join("plugins"), "krill.js", "minikrill.js")
}

fn preferred_path(parent: &Path, name: &str, legacy: &str) -> PathBuf {
    let new = parent.join(name);
    if fs::symlink_metadata(&new).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        let old = parent.join(legacy);
        if fs::symlink_metadata(&old).is_ok() {
            old
        } else {
            new
        }
    } else {
        new
    }
}

/// Worktree default root. Existing trees stay in place so Git metadata and
/// running agents keep their paths. A new-name alias exposes the same root.
#[must_use]
pub fn worktrees_root(home: &Path) -> PathBuf {
    let new = home.join("Krill/worktrees");
    let old = home.join("Minikrill/worktrees");
    if new.exists() || !old.exists() {
        new
    } else {
        #[cfg(unix)]
        if fs::create_dir_all(home.join("Krill")).is_ok() {
            std::os::unix::fs::symlink(&old, &new).ok();
            if new.exists() {
                return new;
            }
        }
        old
    }
}

/// Share root, migrating the previous name on first use.
#[must_use]
pub fn share_dir(home: &Path) -> PathBuf {
    migrated(
        &home.join(".local/share"),
        SHARE_DIR_NAME,
        LEGACY_SHARE_DIR_NAME,
    )
}

fn migrated(parent: &Path, name: &str, legacy: &str) -> PathBuf {
    let new = parent.join(name);
    if !fs::symlink_metadata(&new).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        if fs::symlink_metadata(parent.join(legacy)).is_err() {
            leave_compat_link(&parent.join(legacy), name);
        }
        return new;
    }
    let old = parent.join(legacy);
    match fs::symlink_metadata(&old) {
        Ok(m) if m.is_dir() && !m.is_symlink() => {
            // Windows cannot leave the Unix compatibility link. Keep active old
            // profiles reachable there until a junction migration is available.
            #[cfg(not(unix))]
            return old;
            #[cfg(unix)]
            {
                fs::rename(&old, &new).ok();
                if new.exists() {
                    leave_compat_link(&old, name);
                    new
                } else {
                    old
                }
            }
        }
        // Preserve custom redirects and dangling links rather than deleting them.
        Ok(m) if m.is_symlink() => old,
        _ => new,
    }
}

#[cfg(unix)]
fn leave_compat_link(old: &Path, new_name: &str) {
    std::os::unix::fs::symlink(new_name, old).ok();
}

#[cfg(not(unix))]
fn leave_compat_link(_old: &Path, _new_name: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "krill-user-dirs-{tag}-{pid}",
            pid = std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::remove_file(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fresh_home_uses_new_names() {
        let home = sandbox("fresh");
        assert_eq!(config_dir(&home), home.join(".config").join("krill"));
        assert_eq!(share_dir(&home), home.join(".local/share").join("krill"));
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    #[cfg(unix)]
    fn legacy_dir_moves_and_stays_reachable() {
        let home = sandbox("legacy");
        let old = home.join(".config").join("minikrill");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("accounts.toml"), "legacy = true\n").unwrap();
        let resolved = config_dir(&home);
        assert_eq!(resolved, home.join(".config").join("krill"));
        assert_eq!(
            fs::read_to_string(resolved.join("accounts.toml")).unwrap(),
            "legacy = true\n"
        );
        assert_eq!(
            fs::read_to_string(old.join("accounts.toml")).unwrap(),
            "legacy = true\n"
        );
        assert_eq!(config_dir(&home), resolved);
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn worktrees_root_prefers_existing_old_and_new_for_fresh() {
        let home = sandbox("worktrees");
        assert_eq!(worktrees_root(&home), home.join("Krill/worktrees"));
        let old = home.join("Minikrill/worktrees");
        fs::create_dir_all(&old).unwrap();
        #[cfg(unix)]
        assert_eq!(worktrees_root(&home), home.join("Krill/worktrees"));
        #[cfg(not(unix))]
        assert_eq!(worktrees_root(&home), old);
        fs::create_dir_all(home.join("Krill/worktrees")).unwrap();
        assert_eq!(worktrees_root(&home), home.join("Krill/worktrees"));
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn both_real_dirs_keep_the_new_one_untouched() {
        let home = sandbox("both");
        let (old, new) = (
            home.join(".config").join("minikrill"),
            home.join(".config").join("krill"),
        );
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("old.toml"), "x\n").unwrap();
        assert_eq!(config_dir(&home), new);
        assert!(old.join("old.toml").is_file());
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn repo_config_new_name_wins_without_merging() {
        let home = sandbox("repo-config");
        let legacy = home.join("minikrill.toml");
        fs::write(&legacy, "[worktree]\nsetup = 'old'\n").unwrap();
        assert_eq!(repo_config(&home), legacy);
        let new = home.join("krill.toml");
        fs::write(&new, "invalid TOML is still authoritative").unwrap();
        assert_eq!(repo_config(&home), new);
        assert!(legacy.is_file());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn custom_legacy_symlink_is_preserved() {
        let home = sandbox("symlink");
        fs::create_dir_all(home.join(".config")).unwrap();
        let old = home.join(".config/minikrill");
        std::os::unix::fs::symlink("missing-custom-root", &old).unwrap();
        assert_eq!(config_dir(&home), old);
        assert_eq!(
            fs::read_link(&old).unwrap(),
            Path::new("missing-custom-root")
        );
        fs::remove_dir_all(home).unwrap();
    }
}
