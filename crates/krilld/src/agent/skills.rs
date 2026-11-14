//! Synchronize bundled instructions separately from opt-in credential provisioning.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use krill_proto::agent::Profile;
use krill_proto::orchestration::{
    AgentDesktopSource, ensure_agent_desktop_installed, skill_directories, stage_agent_desktop,
    sync_builtin_skills,
};

use super::{Launch, LaunchAccount};

#[derive(Default)]
struct PreparedState {
    staged_homes: HashSet<PathBuf>,
    desktop_homes: HashSet<PathBuf>,
    /// 동기화 완료한 스킬 경로와 동기화 직후 존재 여부(`true`면 디렉터리가 지워졌을 때 다시 복구한다).
    synced_dirs: HashMap<PathBuf, bool>,
}

fn prepared_state() -> &'static Mutex<PreparedState> {
    static STATE: OnceLock<Mutex<PreparedState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(PreparedState::default()))
}

fn lock_state(mutex: &Mutex<PreparedState>) -> std::sync::MutexGuard<'_, PreparedState> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Match app/CLI isolation: an explicit state directory never prepares host skills.
pub(super) fn prepare(profile: Option<&Profile>, launch: &Launch, account: Option<&LaunchAccount>) {
    if cfg!(test) || krill_proto::daemon::state_dir_isolated() {
        return;
    }
    let home = super::accounts::default_home();
    prepare_with_state(prepared_state(), home, profile, launch, account);
}

fn prepare_with_state(
    cache: &'static Mutex<PreparedState>,
    home: PathBuf,
    profile: Option<&Profile>,
    launch: &Launch,
    account: Option<&LaunchAccount>,
) {
    let dirs = directories(&home, profile, launch, account);
    let (need_stage, pending_dirs, spawn_desktop) = {
        let mut st = lock_state(cache);
        let need_stage = !st.staged_homes.contains(&home);
        let pending_dirs: Vec<PathBuf> = dirs
            .into_iter()
            .filter(|dir| match st.synced_dirs.get(dir) {
                None => true,
                Some(true) => !dir.exists(),
                Some(false) => false,
            })
            .collect();
        let spawn_desktop = st.desktop_homes.insert(home.clone());
        (need_stage, pending_dirs, spawn_desktop)
    };

    if profile.is_some() {
        if need_stage || !pending_dirs.is_empty() {
            synchronize_pending(cache, &home, need_stage, &pending_dirs);
        }
        let home_for_err = home.clone();
        if spawn_desktop
            && std::thread::Builder::new()
                .name("krilld-agent-desktop".into())
                .spawn(move || install_desktop_in(cache, &home))
                .is_err()
        {
            lock_state(cache).desktop_homes.remove(&home_for_err);
        }
        return;
    }

    // 일반 로그인 셸(`Program::Login`)은 `Create` 중요 경로를 막지 않고 백그라운드에서 한 번만 준비한다.
    if !need_stage && pending_dirs.is_empty() && !spawn_desktop {
        return;
    }
    let home_for_err = home.clone();
    if std::thread::Builder::new()
        .name("krilld-agent-desktop".into())
        .spawn(move || {
            if need_stage || !pending_dirs.is_empty() {
                synchronize_pending(cache, &home, need_stage, &pending_dirs);
            }
            if spawn_desktop {
                install_desktop_in(cache, &home);
            }
        })
        .is_err()
        && spawn_desktop
    {
        lock_state(cache).desktop_homes.remove(&home_for_err);
    }
}

fn install_desktop_in(cache: &Mutex<PreparedState>, home: &Path) {
    let retry = match ensure_agent_desktop_installed(home, None) {
        Ok(report) => report.source == AgentDesktopSource::Busy,
        Err(_) => true,
    };
    if retry {
        lock_state(cache).desktop_homes.remove(home);
    }
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

#[cfg(test)]
fn synchronize(
    home: &Path,
    profile: Option<&Profile>,
    launch: &Launch,
    account: Option<&LaunchAccount>,
) {
    let cache = Mutex::new(PreparedState::default());
    let dirs = directories(home, profile, launch, account);
    synchronize_pending(&cache, home, true, &dirs);
}

fn synchronize_pending(
    cache: &Mutex<PreparedState>,
    home: &Path,
    need_stage: bool,
    dirs: &[PathBuf],
) {
    if need_stage && stage_agent_desktop(None, home).is_ok() {
        lock_state(cache).staged_homes.insert(home.to_path_buf());
    }
    if dirs.is_empty() {
        return;
    }
    let report = sync_builtin_skills(dirs);
    for _dir in &report.busy {
        // 앱·CLI·다른 세션 시작이 같은 폴더를 동기화하는 중이다. 같은 묶음을 쓰고 다음 세션 시작에 다시 맞춘다.
        tracing::debug!("built-in skill synchronization deferred to the running installer");
    }
    for error in &report.failures {
        // 프로필 skills가 사용자 스킬 폴더로 가는 심볼릭 링크면 그 폴더가 따로 동기화된다. 의도된 구성이라 경고하지 않는다
        // (2026-10-26부터 codex 프로필에서 매 동기화마다 800여 줄).
        if error.contains("must not traverse a symlink") {
            tracing::debug!("built-in skill synchronization skipped a linked agent target");
        } else {
            tracing::warn!("built-in skill synchronization skipped an agent target");
        }
    }
    let mut st = lock_state(cache);
    for dir in dirs {
        if !report.busy.contains(dir) {
            st.synced_dirs.insert(dir.clone(), dir.exists());
        }
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
            assert!(dir.join("artifacts/SKILL.md").is_file());
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
        assert!(account_root.join("skills/artifacts/SKILL.md").is_file());
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

    #[test]
    fn prepared_state_skips_already_synced_dirs_and_heals_deleted_target() {
        let home = std::env::temp_dir().join(format!("krill-skills-cache-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let cache = Mutex::new(PreparedState::default());
        let target = home.join(".claude/skills");
        let dirs = vec![target.clone()];

        synchronize_pending(&cache, &home, false, &dirs);
        assert!(target.join("artifacts/SKILL.md").is_file());
        assert_eq!(lock_state(&cache).synced_dirs.get(&target), Some(&true));

        let pending_second: Vec<PathBuf> = dirs
            .iter()
            .filter(|dir| match lock_state(&cache).synced_dirs.get(*dir) {
                None => true,
                Some(true) => !dir.exists(),
                Some(false) => false,
            })
            .cloned()
            .collect();
        assert!(pending_second.is_empty());

        std::fs::remove_dir_all(&target).unwrap();
        let pending_after_delete: Vec<PathBuf> = dirs
            .iter()
            .filter(|dir| match lock_state(&cache).synced_dirs.get(*dir) {
                None => true,
                Some(true) => !dir.exists(),
                Some(false) => false,
            })
            .cloned()
            .collect();
        assert_eq!(pending_after_delete, dirs);

        std::fs::remove_dir_all(home).ok();
    }
}
