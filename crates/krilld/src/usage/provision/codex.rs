//! Codex 계정별 `CODEX_HOME` 준비(OWE-721, MK-158).
//! 설계 참고: Orca codex-accounts/runtime-home-service-managed-home.ts·codex-config-mirror.ts,
//! codex/codex-account-session-bridge.ts (MIT, Copyright (c) 2026 Lovecast Inc.). 코드 복사 없음.
//! 인증·SQLite 인덱스는 계정별로 유지하고, 이미 있는 원본 기록만 링크한다.

use std::io;
use std::path::Path;

use super::Report;

const LINKED: &[&str] = &[
    "sessions",
    "archived_sessions",
    "shell_snapshots",
    "thread-writer-locks",
];
#[cfg(unix)]
const EXTENSIONS: &[&str] = &["skills", "plugins", "rules", "prompts", "memories"];
#[cfg(unix)]
const FILES: &[&str] = &["history.jsonl", "session_index.jsonl"];

#[cfg(unix)]
fn store<'a>(host: &'a Path, profile: &'a Path) -> super::history_store::Store<'a> {
    super::history_store::Store {
        host,
        profile,
        directories: LINKED,
        files: FILES,
        backup_name: "history-merged-backup",
        foreign_links: super::history_store::ForeignLinks::Copy,
        live: false,
    }
}

#[cfg(unix)]
fn extension_store<'a>(
    host: &'a Path,
    profile: &'a Path,
    dir: &'static [&'static str],
) -> super::history_store::Store<'a> {
    super::history_store::Store {
        host,
        profile,
        directories: dir,
        files: &[],
        backup_name: "history-merged-backup",
        foreign_links: super::history_store::ForeignLinks::Copy,
        live: true,
    }
}

#[cfg(unix)]
pub(super) fn needs_merge(home: &Path, profile: &Path) -> bool {
    let host = home.join(".codex");
    store(&host, profile).needs_merge()
        || EXTENSIONS.iter().any(|name| {
            host.join(name).is_dir()
                && extension_store(&host, profile, std::slice::from_ref(name)).needs_merge()
        })
        || profile.join("config.toml").is_file()
            && std::fs::read_to_string(profile.join("config.toml"))
                .ok()
                .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
                .is_some_and(|config| config.get("sqlite_home").is_none())
}

const COPIED: &[&str] = &["config.toml", "AGENTS.md"];

pub(super) fn provision(home: &Path, profile: &Path) -> io::Result<Report> {
    provision_inner(home, profile, &|| {
        #[cfg(unix)]
        {
            super::history_probe::quiescent(profile, "CODEX_HOME")
        }
        #[cfg(not(unix))]
        {
            false
        }
    })
}

#[cfg_attr(
    not(unix),
    expect(unused_variables, reason = "기록 이행 프로브는 Unix에서만 사용한다")
)]
fn provision_inner(home: &Path, profile: &Path, idle: &dyn Fn() -> bool) -> io::Result<Report> {
    let mut report = Report::default();
    let profile = profile.canonicalize()?;
    let host = match home.join(".codex").canonicalize() {
        Ok(host) => host,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            home.canonicalize()?.join(".codex")
        }
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
    #[cfg(unix)]
    {
        store(&host, &profile).share(idle, &mut report)?;
        for name in EXTENSIONS {
            if host.join(name).is_dir() {
                extension_store(&host, &profile, std::slice::from_ref(name))
                    .share(idle, &mut report)?;
            }
        }
    }
    #[cfg(not(unix))]
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
    #[cfg(unix)]
    configure_sqlite_home(&host, &profile, idle, &mut report)?;
    Ok(report)
}

/// 세션 원본과 보관 기록이 기본 저장소를 가리킬 때만 인덱스 위치를 전환한다.
/// Store가 사용 중인 프로필·이해할 수 없는 링크를 보류했으면 기존 `SQLite`를 계속 쓴다.
#[cfg(unix)]
fn history_shared(host: &Path, profile: &Path) -> bool {
    ["sessions", "archived_sessions"].iter().all(|name| {
        profile.join(name).is_symlink()
            && profile
                .join(name)
                .canonicalize()
                .ok()
                .zip(host.join(name).canonicalize().ok())
                .is_some_and(|(a, b)| a == b)
    })
}

#[cfg(unix)]
fn configure_sqlite_home(
    host: &Path,
    profile: &Path,
    idle: &dyn Fn() -> bool,
    report: &mut Report,
) -> io::Result<()> {
    let config = profile.join("config.toml");
    let original = match std::fs::read_to_string(&config) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let parsed: toml::Value = toml::from_str(&original).map_err(io::Error::other)?;
    if let Some(existing) = parsed.get("sqlite_home") {
        if existing.as_str().is_none_or(|path| {
            Path::new(path) != host
                && Path::new(path).canonicalize().ok() != host.canonicalize().ok()
        }) {
            report.conflicts.push("sqlite_home");
        }
        return Ok(());
    }
    if !history_shared(host, profile) || !idle() {
        report.conflicts.push("sqlite_home");
        return Ok(());
    }
    if config.is_symlink() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let path = toml::Value::String(host.to_string_lossy().into_owned()).to_string();
    let updated = format!("sqlite_home = {path}\n{original}");
    // 기존 바이트·주석은 그대로 뒤에 둔다. 원본 설정은 별도 사본으로도 보존한다.
    if !original.is_empty() {
        let backup = profile.join("config.toml.krill-history-backup");
        if !backup.exists() {
            std::fs::hard_link(&config, backup)?;
        }
    }
    super::super::accounts::write_atomic_0600(&config, updated.as_bytes())?;
    report.changed.push("sqlite_home");
    Ok(())
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

    fn provision(home: &Path, profile: &Path) -> io::Result<Report> {
        super::provision_inner(home, profile, &|| true)
    }

    fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let home = std::env::temp_dir().join(format!("owe721-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let host = home.join(".codex");
        let profile = home.join("profile");
        std::fs::create_dir_all(host.join("sessions/2026/10/17")).unwrap();
        std::fs::create_dir_all(host.join("archived_sessions")).unwrap();
        std::fs::create_dir_all(host.join("shell_snapshots")).unwrap();
        std::fs::create_dir_all(host.join("thread-writer-locks")).unwrap();
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
            LINKED
                .iter()
                .chain(FILES)
                .chain(COPIED)
                .copied()
                .chain(["sqlite_home"])
                .collect::<Vec<_>>()
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
            if *name != "config.toml" {
                assert_eq!(
                    std::fs::read(profile.join(name)).unwrap(),
                    std::fs::read(host.join(name)).unwrap()
                );
            }
        }
        assert_eq!(
            std::fs::read(profile.join("auth.json")).unwrap(),
            b"account-secret"
        );
        assert!(!profile.join("state_5.sqlite").exists());
        std::fs::write(
            profile.join("config.toml"),
            format!(
                "sqlite_home = {:?}\nmodel = \"mine\"\n",
                host.to_string_lossy()
            ),
        )
        .unwrap();
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
        std::fs::write(profile.join("config.toml"), "model = \"mine\"\n").unwrap();
        let before = snapshot(&host);
        let report = provision(&home, &profile).unwrap();
        assert_eq!(report.conflicts, ["archived_sessions", "sqlite_home"]);
        assert!(report.changed.contains(&"sessions"));
        assert_eq!(
            std::fs::read(profile.join("sessions/mine.jsonl")).unwrap(),
            b"mine"
        );
        assert!(
            std::fs::read_to_string(profile.join("config.toml"))
                .unwrap()
                .contains("model = \"mine\"")
        );
        for (path, bytes, _) in before {
            if path.is_file() {
                assert_eq!(std::fs::read(path).unwrap(), bytes);
            }
        }
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
        provision(&home, &profile).unwrap();
        assert!(host.is_dir());
        assert!(profile.join("sessions").is_symlink());
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
        for (path, bytes, _) in before {
            if path.is_file() {
                assert_eq!(std::fs::read(path).unwrap(), bytes);
            }
        }
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
    fn agy_history_opt_in_keeps_native_credentials_untouched() {
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
        provision_if_enabled(&home, Some(&state), ProviderKind::Antigravity, dir);
        assert_eq!(
            std::fs::read(home.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
            b"fake native credential"
        );
        assert!(
            !profile
                .join(".gemini/antigravity-cli/antigravity-oauth-token")
                .exists()
        );
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn redirects_orca_history_by_copying_without_modifying_the_orca_source() {
        let (home, host, profile) = fixture("orca-copy");
        let source = home.join("orca/codex-accounts/account/home/sessions");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("krill-session.jsonl"),
            b"Orca source remains intact",
        )
        .unwrap();
        std::os::unix::fs::symlink(&source, profile.join("sessions")).unwrap();
        let before = snapshot(&home.join("orca"));
        provision(&home, &profile).unwrap();
        assert_eq!(
            std::fs::read_link(profile.join("sessions")).unwrap(),
            host.canonicalize().unwrap().join("sessions")
        );
        assert_eq!(
            std::fs::read(host.join("sessions/krill-session.jsonl")).unwrap(),
            b"Orca source remains intact"
        );
        assert_eq!(snapshot(&home.join("orca")), before);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn new_codex_profile_shares_history_and_sqlite_home_without_copying_auth() {
        let (home, host, profile) = fixture("shared-first");
        std::fs::remove_dir_all(&host).unwrap();
        provision(&home, &profile).unwrap();
        assert_eq!(
            std::fs::read_link(profile.join("sessions")).unwrap(),
            host.canonicalize().unwrap().join("sessions")
        );
        let config = std::fs::read_to_string(profile.join("config.toml")).unwrap();
        let config: toml::Value = toml::from_str(&config).unwrap();
        assert_eq!(
            config.get("sqlite_home").and_then(toml::Value::as_str),
            host.canonicalize().unwrap().to_str()
        );
        assert!(!profile.join("auth.json").exists());
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn sqlite_setting_preserves_comments_custom_overrides_and_existing_databases() {
        let (home, host, profile) = fixture("sqlite-policy");
        let original = "# account comments\nmodel = \"fixture\"\n[features]\nexample = true\n";
        std::fs::write(profile.join("config.toml"), original).unwrap();
        provision(&home, &profile).unwrap();
        let updated = std::fs::read_to_string(profile.join("config.toml")).unwrap();
        assert!(updated.ends_with(original));
        assert_eq!(
            std::fs::read_to_string(profile.join("config.toml.krill-history-backup")).unwrap(),
            original
        );
        assert_eq!(provision(&home, &profile).unwrap(), Report::default());
        let custom = "# custom home\nsqlite_home = \"/custom/index\"\n";
        std::fs::write(profile.join("config.toml"), custom).unwrap();
        assert!(
            provision(&home, &profile)
                .unwrap()
                .conflicts
                .contains(&"sqlite_home")
        );
        assert_eq!(
            std::fs::read_to_string(profile.join("config.toml")).unwrap(),
            custom
        );
        std::fs::write(profile.join("config.toml"), original).unwrap();
        std::fs::write(profile.join("state_5.sqlite"), b"private paginated index").unwrap();
        let report = provision(&home, &profile).unwrap();
        assert!(report.changed.contains(&"sqlite_home"));
        assert!(
            std::fs::read_to_string(profile.join("config.toml"))
                .unwrap()
                .ends_with(original)
        );
        assert_eq!(
            std::fs::read(profile.join("state_5.sqlite")).unwrap(),
            b"private paginated index"
        );
        assert!(!host.join("state_5.sqlite").is_symlink());
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn existing_sqlite_transitions_only_after_history_is_shared_and_keeps_the_database() {
        let (home, host, profile) = fixture("sqlite-existing");
        let original = "# account preferences\nmodel = \"fixture\"\n";
        std::fs::write(profile.join("config.toml"), original).unwrap();
        std::fs::write(profile.join("state_5.sqlite"), b"private database retained").unwrap();
        std::fs::create_dir_all(profile.join("sessions")).unwrap();
        std::fs::write(profile.join("sessions/private.jsonl"), b"private rollout").unwrap();
        let report = provision(&home, &profile).unwrap();
        assert!(report.changed.contains(&"sqlite_home"));
        assert_eq!(
            std::fs::read(host.join("sessions/private.jsonl")).unwrap(),
            b"private rollout"
        );
        assert_eq!(
            std::fs::read(profile.join("state_5.sqlite")).unwrap(),
            b"private database retained"
        );
        assert!(
            std::fs::read_to_string(profile.join("config.toml"))
                .unwrap()
                .ends_with(original)
        );
        assert_eq!(provision(&home, &profile).unwrap(), Report::default());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn deferred_history_cannot_switch_sqlite_even_if_a_later_probe_is_idle() {
        use std::cell::Cell;
        let (home, _, profile) = fixture("sqlite-deferred");
        let original = "# keep until history was imported\n";
        std::fs::write(profile.join("config.toml"), original).unwrap();
        std::fs::create_dir_all(profile.join("sessions")).unwrap();
        std::fs::write(profile.join("sessions/private.jsonl"), b"still in use").unwrap();
        let probes = Cell::new(0);
        let report = super::provision_inner(&home, &profile, &|| {
            let prior = probes.get();
            probes.set(prior + 1);
            prior > 0
        })
        .unwrap();
        assert!(report.conflicts.contains(&"sqlite_home"));
        assert_eq!(
            std::fs::read_to_string(profile.join("config.toml")).unwrap(),
            original
        );
        assert!(!profile.join("sessions").is_symlink());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn shares_codex_extensions_and_merges_profile_extensions_into_host() {
        let (home, host, profile) = fixture("extensions");
        for name in EXTENSIONS {
            std::fs::create_dir_all(host.join(name)).unwrap();
        }
        std::fs::create_dir_all(host.join("skills/pstack")).unwrap();
        std::fs::write(host.join("skills/pstack/SKILL.md"), b"host pstack").unwrap();
        std::fs::create_dir_all(profile.join("rules")).unwrap();
        std::fs::write(profile.join("rules/custom.mdc"), b"profile rule").unwrap();

        let report = provision(&home, &profile).unwrap();
        for name in EXTENSIONS {
            assert!(report.changed.contains(name), "missing {name}");
            assert_eq!(
                std::fs::read_link(profile.join(name)).unwrap(),
                host.canonicalize().unwrap().join(name)
            );
        }
        assert_eq!(
            std::fs::read(profile.join("skills/pstack/SKILL.md")).unwrap(),
            b"host pstack"
        );
        assert_eq!(
            std::fs::read(host.join("rules/custom.mdc")).unwrap(),
            b"profile rule"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
