//! Session-scoped Claude statusline observations for the mobile composer.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Write as _;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionContext {
    profile_scope: ProfileScope,
    pub session_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub percent: Option<f64>,
    pub recorded_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct ProfileScope(String);

impl ProfileScope {
    fn from_profile(profile: &Path) -> Option<Self> {
        let profile = profile.canonicalize().ok().filter(|path| path.is_dir())?;
        let digest = ring::digest::digest(
            &ring::digest::SHA256,
            profile.as_os_str().as_encoded_bytes(),
        );
        let mut key = String::with_capacity(64);
        for byte in digest.as_ref() {
            write!(&mut key, "{byte:02x}").ok()?;
        }
        Some(Self(key))
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn path(state: &Path, scope: &ProfileScope, session_id: &str) -> Option<PathBuf> {
    let state = state.canonicalize().ok()?;
    (!session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)))
    .then(|| {
        state
            .join("session-context")
            .join(&scope.0)
            .join(format!("{session_id}.json"))
    })
}

pub fn record(state: &Path, profile: &Path, payload: &Value) -> io::Result<()> {
    let Some(session_id) = payload.get("session_id").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(scope) = ProfileScope::from_profile(profile) else {
        return Ok(());
    };
    let Some(path) = path(state, &scope, session_id).filter(|_| state.is_dir()) else {
        return Ok(());
    };
    let observation = SessionContext {
        profile_scope: scope,
        session_id: session_id.to_owned(),
        recorded_at: now(),
        model: payload
            .pointer("/model/id")
            .and_then(Value::as_str)
            .filter(|s| s.len() <= 128)
            .map(str::to_owned),
        effort: payload
            .get("effort")
            .and_then(Value::as_str)
            .filter(|s| matches!(*s, "low" | "medium" | "high" | "xhigh" | "max"))
            .map(str::to_owned),
        percent: payload
            .pointer("/context_window/used_percentage")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && (0.0..=100.0).contains(n)),
    };
    if observation.model.is_none() && observation.percent.is_none() && observation.effort.is_none()
    {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing context directory"))?;
    crate::hook_install::store::safe_path(&path)?;
    crate::hook_install::store::directory(parent)?;
    let text = serde_json::to_string(&observation)?;
    if std::fs::read_to_string(&path).ok().as_deref() == Some(&text) {
        return Ok(());
    }
    super::write_named_cache_file(parent, &format!("{session_id}.json"), &text)?;
    Ok(())
}

pub fn read(state: &Path, profile: &Path, session_id: &str) -> Option<SessionContext> {
    let scope = ProfileScope::from_profile(profile)?;
    let path = path(state, &scope, session_id)?;
    crate::hook_install::store::safe_path(&path).ok()?;
    let file = std::fs::File::open(path).ok()?;
    let observation: SessionContext = serde_json::from_reader(file.take(4096)).ok()?;
    (observation.session_id == session_id
        && observation.profile_scope == scope
        && now()
            .checked_sub(observation.recorded_at)
            .is_some_and(|age| age < 300))
    .then_some(observation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statusline_context_is_private_scoped_and_excludes_unrelated_payload() {
        let dir = std::env::temp_dir().join(format!("krill-context-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let profile = dir.join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let scope = ProfileScope::from_profile(&profile).unwrap();
        let payload = serde_json::json!({"session_id":"context-a", "model":{"id":"observed-claude"}, "context_window":{"used_percentage":37.5}, "transcript":"private conversation", "cwd":"private path"});
        record(&dir, &profile, &payload).unwrap();
        let observed = read(&dir, &profile, "context-a").unwrap();
        assert_eq!(observed.model.as_deref(), Some("observed-claude"));
        assert_eq!(observed.percent, Some(37.5));
        assert!(read(&dir, &profile, "context-b").is_none());
        assert!(read(&dir, &profile, "../context-a").is_none());
        let text = std::fs::read_to_string(path(&dir, &scope, "context-a").unwrap()).unwrap();
        assert!(!text.contains("private"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(path(&dir, &scope, "context-a").unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        record(&dir, &profile, &serde_json::json!({"session_id":"context-a","model":{"id":"observed-claude"},"context_window":{"used_percentage":0}})).unwrap();
        assert_eq!(
            read(&dir, &profile, "context-a").unwrap().percent,
            Some(0.0)
        );
        let mut stale = read(&dir, &profile, "context-a").unwrap();
        stale.recorded_at = now().saturating_sub(300);
        std::fs::write(
            path(&dir, &scope, "context-a").unwrap(),
            serde_json::to_vec(&stale).unwrap(),
        )
        .unwrap();
        assert!(read(&dir, &profile, "context-a").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn copied_session_ids_are_isolated_by_canonical_profile_scope() {
        let dir =
            std::env::temp_dir().join(format!("krill-context-accounts-{}", std::process::id()));
        let first = dir.join("private-account-first");
        let second = dir.join("private-account-second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let payload = |percent| serde_json::json!({"session_id":"copied-session","model":{"id":"same-model"},"context_window":{"used_percentage":percent}});
        record(&dir, &first, &payload(25)).unwrap();
        record(&dir, &second, &payload(90)).unwrap();
        assert_eq!(
            read(&dir, &first, "copied-session").unwrap().percent,
            Some(25.0)
        );
        assert_eq!(
            read(&dir, &second, "copied-session").unwrap().percent,
            Some(90.0)
        );
        assert!(read(&dir, &dir.join("missing-profile"), "copied-session").is_none());
        let scope = ProfileScope::from_profile(&first).unwrap();
        let text = std::fs::read_to_string(path(&dir, &scope, "copied-session").unwrap()).unwrap();
        assert!(!text.contains("private-account"));
        #[cfg(unix)]
        {
            let alias = dir.join("profile-alias");
            std::os::unix::fs::symlink(&first, &alias).unwrap();
            assert_eq!(
                read(&dir, &alias, "copied-session").unwrap().percent,
                Some(25.0)
            );
            record(&dir, &alias, &payload(30)).unwrap();
            assert_eq!(
                read(&dir, &first, "copied-session").unwrap().percent,
                Some(30.0)
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
