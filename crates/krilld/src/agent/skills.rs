//! Synchronize bundled instructions separately from opt-in credential provisioning.

use std::path::{Path, PathBuf};

use krill_proto::agent::Profile;
use krill_proto::orchestration::{skill_directories, sync_builtin_skills};

use super::{Launch, LaunchAccount};

/// Match app/CLI isolation: an explicit state directory never prepares host skills.
pub(super) fn prepare(profile: Option<&Profile>, launch: &Launch, account: Option<&LaunchAccount>) {
    if cfg!(test) || krill_proto::daemon::state_dir_override().is_some() {
        return;
    }
    synchronize(&super::accounts::default_home(), profile, launch, account);
}

fn env_path(launch: &Launch, key: &str) -> Option<PathBuf> {
    use krill_proto::os::ffi::OsStrExt as _;
    launch
        .env
        .iter()
        .rev()
        .find(|(k, v)| k == key.as_bytes() && !v.is_empty())
        .map(|(_, value)| PathBuf::from(std::ffi::OsStr::from_bytes(value)))
}

fn directories(
    home: &Path,
    profile: Option<&Profile>,
    launch: &Launch,
    account: Option<&LaunchAccount>,
) -> Vec<PathBuf> {
    let agent = profile
        .and_then(Profile::program)
        .and_then(|p| Path::new(p).file_stem())
        .and_then(|p| p.to_str())
        .map_or("all", krill_proto::agent::canonical_id);
    let mut dirs = skill_directories(home, agent);
    for (provider, key) in [
        ("claude", "CLAUDE_CONFIG_DIR"),
        ("codex", "CODEX_HOME"),
        ("opencode", "OPENCODE_CONFIG_DIR"),
        ("pi", "PI_CODING_AGENT_DIR"),
    ] {
        if (agent == "all" || agent == provider)
            && let Some(root) = env_path(launch, key)
        {
            dirs.push(root.join("skills"));
        }
    }
    if let Some(account) = account.filter(|a| !a.dir.is_empty()) {
        match agent {
            "claude" | "codex" => dirs.push(Path::new(&account.dir).join("skills")),
            "agy" => dirs.extend(skill_directories(Path::new(&account.dir), "agy")),
            _ => {}
        }
    }
    if agent == "all"
        && let Some(root) = env_path(launch, "ANTIGRAVITY_CLI_HOME")
    {
        dirs.extend(skill_directories(&root, "agy"));
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

fn synchronize(
    home: &Path,
    profile: Option<&Profile>,
    launch: &Launch,
    account: Option<&LaunchAccount>,
) {
    let report = sync_builtin_skills(&directories(home, profile, launch, account));
    for error in report.failures {
        tracing::warn!(%error, "built-in skill synchronization skipped an agent target");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_profiles_receive_bundled_references_without_credentials_or_orca_changes() {
        let home = std::env::temp_dir().join(format!("krill-launch-skills-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let profiles = krill_proto::agent::Agents::load(None).unwrap();
        let account_root = home.join("account with spaces");
        let account = LaunchAccount {
            id: "test".into(),
            env_key: "ANTIGRAVITY_CLI_HOME".into(),
            dir: account_root.display().to_string(),
        };
        let launch = Launch {
            argv: Vec::new(),
            env: Vec::new(),
        };
        let orca = home.join(".gemini/antigravity-cli/skills/orchestration/SKILL.md");
        std::fs::create_dir_all(orca.parent().unwrap()).unwrap();
        std::fs::write(&orca, "external Orca skill").unwrap();
        synchronize(
            &home,
            Some(&profiles.profiles["agy"]),
            &launch,
            Some(&account),
        );
        for dir in skill_directories(&account_root, "agy") {
            assert!(
                dir.join("krill-orchestration/references/worker-contract.md")
                    .is_file()
            );
        }
        assert_eq!(
            std::fs::read_to_string(orca).unwrap(),
            "external Orca skill"
        );
        assert!(!account_root.join(".gemini/config/settings.json").exists());
        assert!(!account_root.join(".gemini/config/hooks.json").exists());
        let codex = LaunchAccount {
            env_key: "CODEX_HOME".into(),
            ..account
        };
        synchronize(
            &home,
            Some(&profiles.profiles["codex"]),
            &launch,
            Some(&codex),
        );
        assert!(
            account_root
                .join("skills/krill-orchestration/SKILL.md")
                .is_file()
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn configured_provider_roots_are_included() {
        let profiles = krill_proto::agent::Agents::load(None).unwrap();
        let launch = Launch {
            argv: Vec::new(),
            env: vec![(b"CODEX_HOME".to_vec(), b"/isolated/codex".to_vec())],
        };
        let dirs = directories(
            Path::new("/fake/home"),
            Some(&profiles.profiles["codex"]),
            &launch,
            None,
        );
        assert!(dirs.contains(&PathBuf::from("/isolated/codex/skills")));
        assert!(!dirs.contains(&PathBuf::from("/isolated/claude/skills")));
        // Claude's Settings wrapper passes no account object, retaining its effective env.
        let claude = Launch {
            argv: Vec::new(),
            env: vec![(b"CLAUDE_CONFIG_DIR".to_vec(), b"/isolated/claude".to_vec())],
        };
        assert!(
            directories(
                Path::new("/fake/home"),
                Some(&profiles.profiles["claude"]),
                &claude,
                None,
            )
            .contains(&PathBuf::from("/isolated/claude/skills"))
        );
    }
}
