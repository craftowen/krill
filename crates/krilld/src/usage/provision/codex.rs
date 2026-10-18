//! Codex 계정별 `CODEX_HOME` 준비(OWE-721, MK-158).
//! 설계 참고: Orca codex-accounts/runtime-home-service-managed-home.ts·codex-config-mirror.ts,
//! codex/codex-account-session-bridge.ts (MIT, Copyright (c) 2026 Lovecast Inc.). 코드 복사 없음.
//! 인증·SQLite 인덱스는 계정별로 유지하고, 이미 있는 원본 기록만 링크한다.

use std::io;
use std::path::Path;

use super::Report;

const LINKED: &[&str] = &["sessions", "archived_sessions"];
const COPIED: &[&str] = &["config.toml", "AGENTS.md"];

pub(super) fn provision(home: &Path, profile: &Path) -> io::Result<Report> {
    let mut report = Report::default();
    let profile = profile.canonicalize()?;
    let host = match home.join(".codex").canonicalize() {
        Ok(host) => host,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(error),
    };
    if profile.starts_with(&host) || host.starts_with(&profile) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile overlaps the host codex folder",
        ));
    }
    // 원본 기록이 프로필을 가리키는 링크여도 순환 링크·원본 쓰기를 만들지 않는다.
    for name in LINKED {
        if let Ok(source) = host.join(name).canonicalize()
            && (source.starts_with(&profile) || profile.starts_with(&source))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "codex history overlaps the profile",
            ));
        }
    }
    for &name in LINKED {
        let src = host.join(name);
        if src.is_dir() {
            super::link(&src, &profile.join(name), name, &mut report)?;
        }
    }
    for &name in COPIED {
        let src = host.join(name);
        if src.is_file() {
            copy_once(&src, &profile.join(name), name, &mut report)?;
        }
    }
    Ok(report)
}

/// `create_new`로 임시 경로의 기존 링크를 거부하고 `hard_link`로 기존 설정을 덮어쓰지 않고 게시한다.
fn copy_once(src: &Path, dst: &Path, name: &'static str, report: &mut Report) -> io::Result<()> {
    use krill_proto::os::fs::OpenOptionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    match dst.symlink_metadata() {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let tmp = dst.with_file_name(format!(
        ".codex-copy-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let result = (|| {
        let mut input = std::fs::File::open(src)?;
        io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        std::fs::hard_link(&tmp, dst)
    })();
    drop(output);
    std::fs::remove_file(tmp)?;
    match result {
        Ok(()) => report.changed.push(name),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::usage::provision::provision_if_enabled;
    use krill_proto::usage::ProviderKind;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let home = std::env::temp_dir().join(format!("owe721-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let host = home.join(".codex");
        let profile = home.join("profile");
        std::fs::create_dir_all(host.join("sessions/2026/10/17")).unwrap();
        std::fs::create_dir_all(host.join("archived_sessions")).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        for (name, text) in [
            ("sessions/2026/10/17/rollout-test.jsonl", "host transcript"),
            ("config.toml", "model = \"fixture\"\n"),
            ("AGENTS.md", "# fixture\n"),
            ("auth.json", "host-secret"),
            ("history.jsonl", "host inputs"),
            ("session_index.jsonl", "host index"),
            ("state_5.sqlite", "host database"),
        ] {
            std::fs::write(host.join(name), text).unwrap();
        }
        (home, host, profile)
    }

    // 변경 시각·링크 대상을 포함한다. 링크는 따라가지 않아 원본/프로필 양쪽을 안전하게 비교한다.
    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let meta = path.symlink_metadata().unwrap();
            let bytes = if meta.file_type().is_symlink() {
                std::fs::read_link(&path)
                    .unwrap()
                    .into_os_string()
                    .into_encoded_bytes()
            } else if meta.is_dir() {
                stack.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
                Vec::new()
            } else {
                std::fs::read(&path).unwrap()
            };
            out.push((path, bytes, meta.modified().unwrap()));
        }
        out.sort();
        out
    }

    #[test]
    fn shares_history_and_copies_settings_once_without_auth_or_host_writes() {
        let (home, host, profile) = fixture("history");
        std::fs::write(profile.join("auth.json"), "account-secret").unwrap();
        let before = snapshot(&host);
        let report = provision(&home, &profile).unwrap();
        assert_eq!(
            report.changed,
            ["sessions", "archived_sessions", "config.toml", "AGENTS.md"]
        );
        assert!(report.conflicts.is_empty());
        for name in LINKED {
            assert_eq!(
                std::fs::read_link(profile.join(name)).unwrap(),
                host.canonicalize().unwrap().join(name)
            );
        }
        for name in COPIED {
            assert!(
                !profile
                    .join(name)
                    .symlink_metadata()
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                std::fs::read(profile.join(name)).unwrap(),
                std::fs::read(host.join(name)).unwrap()
            );
        }
        assert_eq!(
            std::fs::read(profile.join("auth.json")).unwrap(),
            b"account-secret"
        );
        for name in ["history.jsonl", "session_index.jsonl", "state_5.sqlite"] {
            assert!(!profile.join(name).exists());
        }
        std::fs::write(profile.join("config.toml"), "model = \"mine\"\n").unwrap();
        let profile_before = snapshot(&profile);
        assert_eq!(provision(&home, &profile).unwrap(), Report::default());
        assert_eq!(snapshot(&profile), profile_before);
        assert_eq!(snapshot(&host), before);
        std::fs::remove_dir_all(&profile).unwrap();
        assert_eq!(snapshot(&host), before, "프로필 삭제도 원본 기록 보존");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn preserves_existing_history_and_settings_and_dangling_links() {
        let (home, host, profile) = fixture("conflicts");
        std::fs::create_dir_all(profile.join("sessions")).unwrap();
        std::fs::write(profile.join("sessions/mine.jsonl"), "mine").unwrap();
        std::os::unix::fs::symlink("/missing-owe721", profile.join("archived_sessions")).unwrap();
        std::fs::write(profile.join("config.toml"), "mine").unwrap();
        let before = snapshot(&host);
        let report = provision(&home, &profile).unwrap();
        assert_eq!(report.conflicts, ["sessions", "archived_sessions"]);
        assert_eq!(report.changed, ["AGENTS.md"]);
        assert_eq!(
            std::fs::read(profile.join("sessions/mine.jsonl")).unwrap(),
            b"mine"
        );
        assert_eq!(std::fs::read(profile.join("config.toml")).unwrap(), b"mine");
        assert_eq!(snapshot(&host), before);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn absent_source_is_not_created_and_overlapping_profiles_are_rejected() {
        let (home, host, profile) = fixture("overlap");
        let inner = host.join("profiles/a");
        std::fs::create_dir_all(&inner).unwrap();
        let alias = home.join("alias");
        std::os::unix::fs::symlink(&host, &alias).unwrap();
        let before = snapshot(&host);
        for p in [&home, &host, &inner, &alias] {
            assert!(provision(&home, p).is_err());
        }
        assert_eq!(snapshot(&host), before);
        std::fs::remove_dir_all(&host).unwrap();
        assert_eq!(provision(&home, &profile).unwrap(), Report::default());
        assert!(!host.exists());
        assert_eq!(std::fs::read_dir(&profile).unwrap().count(), 0);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn provisioning_gate_is_write_free_for_all_providers() {
        let (home, _, profile) = fixture("gate");
        let state = home.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let dir = profile.to_str().unwrap();
        let path = state.join("accounts.toml");
        let before = snapshot(&home);
        for provider in ProviderKind::ALL {
            provision_if_enabled(&home, Some(&state), provider, dir);
        }
        assert_eq!(snapshot(&home), before, "설정 파일 없음도 쓰기 0");
        for config in [
            String::new(),
            format!(
                "provisioning = false\n[[accounts]]\nid = \"a\"\nprovider = \"codex\"\nlabel = \"a\"\nprofile_dir = \"{dir}\"\n"
            ),
            "provisioning = true\n".into(),
        ] {
            std::fs::write(&path, config).unwrap();
            let before = snapshot(&home);
            for provider in ProviderKind::ALL {
                provision_if_enabled(&home, Some(&state), provider, dir);
            }
            assert_eq!(snapshot(&home), before);
        }
        std::fs::write(&path, format!("provisioning = true\n[[accounts]]\nid = \"a\"\nprovider = \"codex\"\nlabel = \"a\"\nprofile_dir = \"{dir}\"\n")).unwrap();
        provision_if_enabled(&home, Some(&state), ProviderKind::Codex, dir);
        assert!(
            profile.join("sessions").is_symlink(),
            "Codex 분기를 통과한다"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn second_account_uses_same_history_without_copying_auth() {
        use std::os::unix::fs::PermissionsExt as _;
        let (home, host, profile) = fixture("second-account");
        let other = home.join("account-b");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(other.join("sessions")).unwrap();
        let before = snapshot(&host);
        provision(&home, &profile).unwrap();
        provision(&home, &other).unwrap();
        assert!(!profile.join("auth.json").exists());
        assert!(!other.join("auth.json").exists());
        assert_eq!(
            profile.join("sessions").canonicalize().unwrap(),
            other.join("sessions").canonicalize().unwrap()
        );
        assert!(
            other
                .join("sessions/2026/10/17/rollout-test.jsonl")
                .is_file()
        );
        assert_eq!(
            other
                .join("config.toml")
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(snapshot(&host), before);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn refuses_source_history_pointing_back_into_profile() {
        let (home, host, profile) = fixture("cycle");
        std::fs::remove_dir_all(host.join("sessions")).unwrap();
        std::os::unix::fs::symlink(&profile, host.join("sessions")).unwrap();
        let before = snapshot(&home);
        assert_eq!(
            provision(&home, &profile).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(snapshot(&home), before);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn agy_opt_in_still_cannot_touch_native_stores() {
        let (home, _, profile) = fixture("agy-native-gate");
        let state = home.join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(home.join(".gemini/antigravity-cli")).unwrap();
        std::fs::write(
            home.join(".gemini/antigravity-cli/antigravity-oauth-token"),
            "fake native credential",
        )
        .unwrap();
        let dir = profile.to_str().unwrap();
        std::fs::write(state.join("accounts.toml"), format!("provisioning = true\n[[accounts]]\nid = \"agy\"\nprovider = \"agy\"\nlabel = \"agy\"\nprofile_dir = \"{dir}\"\n")).unwrap();
        let before = snapshot(&home);
        provision_if_enabled(&home, Some(&state), ProviderKind::Antigravity, dir);
        assert_eq!(snapshot(&home), before);
        std::fs::remove_dir_all(home).unwrap();
    }
}
