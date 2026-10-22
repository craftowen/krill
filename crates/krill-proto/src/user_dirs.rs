//! Homedir-managed roots.
//!
//! - `~/.config/krill` (accounts/hosts/profiles/API keys)
//! - `~/.local/share/krill` (staged helper binaries)
//! - `~/Krill/worktrees` (default worktree root)

use std::path::{Path, PathBuf};

/// Config root name (`~/.config/krill`).
pub const CONFIG_DIR_NAME: &str = "krill";
/// Share root name (`~/.local/share/krill`).
pub const SHARE_DIR_NAME: &str = "krill";

/// Config root.
#[must_use]
pub fn config_dir(home: &Path) -> PathBuf {
    home.join(".config").join(CONFIG_DIR_NAME)
}

/// Repository config (`krill.toml`).
#[must_use]
pub fn repo_config(repo: &Path) -> PathBuf {
    repo.join("krill.toml")
}

/// Krill plugin file for `opencode`.
#[must_use]
pub fn opencode_plugin(config: &Path) -> PathBuf {
    config.join("plugins/krill.js")
}

/// Worktree default root.
#[must_use]
pub fn worktrees_root(home: &Path) -> PathBuf {
    home.join("Krill/worktrees")
}

/// Share root.
#[must_use]
pub fn share_dir(home: &Path) -> PathBuf {
    home.join(".local/share").join(SHARE_DIR_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_use_krill_names() {
        let home = Path::new("/home/u");
        assert_eq!(config_dir(home), home.join(".config/krill"));
        assert_eq!(share_dir(home), home.join(".local/share/krill"));
        assert_eq!(worktrees_root(home), home.join("Krill/worktrees"));
        assert_eq!(repo_config(home), home.join("krill.toml"));
        assert_eq!(
            opencode_plugin(&home.join(".config/opencode")),
            home.join(".config/opencode/plugins/krill.js")
        );
    }
}
