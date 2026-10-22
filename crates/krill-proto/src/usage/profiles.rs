//! Krill이 만든 계정 프로필 폴더(OWE-704). krilld와 앱의 로컬 경로가 같은 규칙을 쓴다.
//!
//! 지우는 대신 휴지통(`~/.Trash`)으로 옮긴다. 로그인 정보·대화 기록이 든 사용자 데이터라
//! 실수해도 Finder에서 되돌릴 수 있어야 한다. 관리 루트(`~/.config/krill/profiles/<provider>/<이름>`)
//! 밖의 폴더(사용자가 직접 지정한 `~/.claude` 등)는 절대 옮기지 않는다. 파일 탐색기의 휴지통 이동은
//! 워크트리 안으로 범위를 좁힌 별도 함수 [`move_path_to_trash`]를 쓴다.

use std::io;
use std::path::{Component, Path, PathBuf};

use super::{OrphanProfile, ProviderKind};

/// Codex가 `$CODEX_HOME` 내부에 최대 ~44바이트 접미사 Unix 도메인 소켓을 생성하므로,
/// macOS `SUN_PATH_MAX`(104바이트, 널 종료 제외 103바이트)를 넘지 않으려면
/// `$CODEX_HOME` 경로가 59바이트 이하여야 한다(#18566).
pub const MAX_CODEX_HOME_BYTES: usize = 59;

/// Krill 기본 프로필 루트. 계정 추가 시 경로를 비워 두면 `<루트>/<provider>/<slug>`에 만든다.
#[must_use]
pub fn managed_root(home: &Path) -> PathBuf {
    crate::user_dirs::config_dir(home).join("profiles")
}

/// Codex 소켓 길이 제한(`<= 59B`)을 맞추기 위한 단축 관리 루트(`~/.krill/p`).
#[must_use]
pub fn short_managed_root(home: &Path) -> PathBuf {
    home.join(".krill").join("p")
}

/// Codex 프로필 슬러그를 최대 12자 ASCII 키로 압축한다.
/// 12자 이하 영숫자·`-`·`_` 슬러그는 그대로 유지하고, 초과분은 앞 7자 + `-` + FNV-1a 4자리 hex로 축약한다.
#[must_use]
pub fn codex_short_key(key: &str) -> String {
    let mut clean: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    clean = clean.trim_matches(['-', '_']).to_owned();
    if clean.is_empty() {
        "default".clone_into(&mut clean);
    }
    if clean.len() <= 12 {
        return clean;
    }
    let mut hash: u32 = 0x811c_9dc5;
    for &b in key.as_bytes() {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let prefix = clean
        .get(..7)
        .unwrap_or(&clean)
        .trim_end_matches(['-', '_']);
    format!("{prefix}-{:04x}", hash & 0xffff)
}

/// Codex 단축 프로필 디렉터리(`~/.krill/p/codex/<short-key>`).
#[must_use]
pub fn short_codex_profile_dir(home: &Path, slug: &str) -> PathBuf {
    short_managed_root(home)
        .join(ProviderKind::Codex.as_str())
        .join(codex_short_key(slug))
}

/// 공급자별 기본 프로필 디렉터리. Codex는 소켓 경로 길이 제한을 위해 [`short_codex_profile_dir`]를 쓴다.
#[must_use]
pub fn default_profile_dir(home: &Path, provider: ProviderKind, slug: &str) -> PathBuf {
    if provider == ProviderKind::Codex {
        short_codex_profile_dir(home, slug)
    } else {
        managed_root(home).join(provider.as_str()).join(slug)
    }
}

/// 구 관리 루트(`~/.config/krill/profiles/codex/<slug>`)에 생성된 긴 Codex 프로필을
/// 단축 경로(`~/.krill/p/codex/<short-key>`)로 무손실 복사·이관한다.
/// 사용자가 직접 지정한 외부 경로는 건드리지 않는다.
#[must_use]
pub fn migrate_long_codex_profile_if_needed(
    home: &Path,
    slug: &str,
    current_dir: &Path,
) -> PathBuf {
    let legacy_codex_root = managed_root(home).join(ProviderKind::Codex.as_str());
    if current_dir.as_os_str().len() <= MAX_CODEX_HOME_BYTES
        || !current_dir.starts_with(&legacy_codex_root)
    {
        return current_dir.to_path_buf();
    }
    let target = short_codex_profile_dir(home, slug);
    if current_dir.is_dir() && std::fs::create_dir_all(&target).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).ok();
        }
        if let Ok(entries) = std::fs::read_dir(current_dir) {
            for entry in entries.flatten() {
                let src = entry.path();
                let dst = target.join(entry.file_name());
                if src.is_file() && !dst.exists() {
                    std::fs::copy(&src, &dst).ok();
                }
            }
        }
    }
    target
}

/// `dir`이 관리 루트 바로 아래 `<provider>/<이름>` 폴더인가. `..`·심볼릭 링크는 아니다.
#[must_use]
pub fn is_managed(home: &Path, dir: &Path) -> bool {
    let Ok(rel) = dir
        .strip_prefix(managed_root(home))
        .or_else(|_| dir.strip_prefix(short_managed_root(home)))
    else {
        return false;
    };
    let mut parts = rel.components();
    let shape = matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(Component::Normal(provider)), Some(Component::Normal(_)), None)
            if ProviderKind::ALL.iter().any(|p| provider == p.as_str())
    );
    shape && dir.symlink_metadata().is_ok_and(|m| m.file_type().is_dir())
}

/// 관리 루트에 있지만 `keep`(등록된 프로필·세션이 쓰는 폴더)에 없는 폴더. 경로순.
#[must_use]
pub fn orphans(home: &Path, keep: &[PathBuf]) -> Vec<OrphanProfile> {
    let roots = [managed_root(home), short_managed_root(home)];
    let mut out = Vec::new();
    for root in &roots {
        for provider in ProviderKind::ALL {
            let Ok(entries) = std::fs::read_dir(root.join(provider.as_str())) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().is_ok_and(|t| t.is_dir()) && !keep.iter().any(|k| k == &path) {
                    out.push(OrphanProfile {
                        provider,
                        dir: path.display().to_string(),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out.dedup_by(|a, b| a.dir == b.dir);
    out
}

/// 관리 폴더를 `~/.Trash/krill-<provider>-<이름>`(겹치면 ` 2`, ` 3` …)으로 옮긴다.
/// 관리 폴더가 아니면 `InvalidInput`으로 거부하고 아무것도 바꾸지 않는다.
pub fn move_to_trash(home: &Path, dir: &Path) -> io::Result<PathBuf> {
    if !is_managed(home, dir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a krill-managed profile dir",
        ));
    }
    let provider = dir
        .parent()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    rename_into_trash(home, dir, &format!("krill-{provider}-{name}"))
}

/// 파일 탐색기 전용: 워크트리 `root` 안의 파일·폴더를 `~/.Trash/<이름>`(겹치면 ` 2`, ` 3` …)으로 옮긴다.
/// 루트 자체, 루트 밖(`..`·심볼릭 링크 부모로 빠져나가는 경로 포함)은 `InvalidInput`으로 거부한다.
/// 계정 프로필 폴더는 [`move_to_trash`]만 쓴다.
pub fn move_path_to_trash(home: &Path, root: &Path, path: &Path) -> io::Result<PathBuf> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "path outside worktree");
    if !path.is_absolute()
        || path == root
        || !path.starts_with(root)
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid());
    }
    let parent = path.parent().ok_or_else(invalid)?.canonicalize()?;
    if !parent.starts_with(root.canonicalize()?) {
        return Err(invalid());
    }
    path.symlink_metadata()?;
    let name = path.file_name().ok_or_else(invalid)?.to_string_lossy();
    rename_into_trash(home, path, &name)
}

fn rename_into_trash(home: &Path, src: &Path, base: &str) -> io::Result<PathBuf> {
    let trash = home.join(".Trash");
    std::fs::create_dir_all(&trash)?;
    let mut target = trash.join(base);
    let mut n = 2_u32;
    while target.symlink_metadata().is_ok() {
        target = trash.join(format!("{base} {n}"));
        n = n.saturating_add(1);
    }
    std::fs::rename(src, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(name: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("krill-profiles-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&h).ok();
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    #[test]
    fn only_provider_child_dirs_under_root_are_managed() {
        let h = home("managed");
        let root = managed_root(&h);
        let ok = root.join("claude").join("work");
        std::fs::create_dir_all(&ok).unwrap();
        std::fs::create_dir_all(root.join("unknown").join("x")).unwrap();
        std::fs::create_dir_all(h.join(".claude")).unwrap();
        assert!(is_managed(&h, &ok));
        assert!(!is_managed(&h, &root.join("claude")));
        assert!(!is_managed(&h, &root.join("unknown").join("x")));
        assert!(!is_managed(&h, &h.join(".claude")));
        assert!(!is_managed(
            &h,
            &root.join("claude").join("..").join("claude")
        ));
        assert!(!is_managed(&h, &root.join("claude").join("missing")));
        // 심볼릭 링크로 바깥 폴더를 가리키면 옮기지 않는다.
        #[cfg(unix)]
        {
            let link = root.join("claude").join("link");
            std::os::unix::fs::symlink(h.join(".claude"), &link).unwrap();
            assert!(!is_managed(&h, &link));
        }
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    fn orphans_skip_kept_dirs_and_trash_keeps_unique_names() {
        let h = home("orphans");
        let root = managed_root(&h);
        let kept = root.join("claude").join("work");
        let agy = root.join("agy").join("antigravity-profile-1");
        let oc = root.join("opencode").join("opencode-profile-2");
        for d in [&kept, &agy, &oc] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(agy.join("state.json"), "{}").unwrap();
        let found = orphans(&h, std::slice::from_ref(&kept));
        assert_eq!(
            found,
            vec![
                OrphanProfile {
                    provider: ProviderKind::Antigravity,
                    dir: agy.display().to_string(),
                },
                OrphanProfile {
                    provider: ProviderKind::OpenCode,
                    dir: oc.display().to_string(),
                },
            ]
        );
        let first = move_to_trash(&h, &agy).unwrap();
        assert_eq!(
            first,
            h.join(".Trash").join("krill-agy-antigravity-profile-1")
        );
        assert!(
            first.join("state.json").is_file(),
            "contents are kept in the trash"
        );
        assert!(!agy.exists());
        std::fs::create_dir_all(&agy).unwrap();
        assert_eq!(
            move_to_trash(&h, &agy).unwrap(),
            h.join(".Trash").join("krill-agy-antigravity-profile-1 2")
        );
        assert_eq!(
            move_to_trash(&h, &h.join(".Trash")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(orphans(&h, std::slice::from_ref(&kept)).len(), 1);
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    fn account_trash_rejects_user_dirs() {
        let h = home("user-dirs");
        let user = h.join(".claude");
        let project = h.join("work").join("notes.txt");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(project.parent().unwrap()).unwrap();
        std::fs::write(&project, "x").unwrap();
        for dir in [&user, &project, &h, &managed_root(&h)] {
            assert_eq!(
                move_to_trash(&h, dir).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{}",
                dir.display()
            );
        }
        assert!(user.is_dir() && project.is_file());
        assert!(!h.join(".Trash").exists());
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn worktree_trash_only_moves_paths_inside_root() {
        let h = home("worktree");
        let root = h.join("repo");
        let file = root.join("src").join("a.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "x").unwrap();
        let outside = h.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        for path in [
            root.clone(),
            outside.join("secret"),
            root.join("..").join("outside").join("secret"),
            root.join("link").join("secret"),
            PathBuf::from("repo/src/a.rs"),
        ] {
            assert_eq!(
                move_path_to_trash(&h, &root, &path).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{}",
                path.display()
            );
        }
        assert!(outside.join("secret").is_file() && root.is_dir());

        let trashed = move_path_to_trash(&h, &root, &file).unwrap();
        assert_eq!(trashed, h.join(".Trash").join("a.rs"));
        assert!(trashed.is_file() && !file.exists());
        std::fs::write(&file, "y").unwrap();
        assert_eq!(
            move_path_to_trash(&h, &root, &file).unwrap(),
            h.join(".Trash").join("a.rs 2")
        );
        // 심볼릭 링크 자체는 워크트리 안이라 링크만 옮기고 대상은 남긴다.
        assert_eq!(
            move_path_to_trash(&h, &root, &root.join("link")).unwrap(),
            h.join(".Trash").join("link")
        );
        assert!(outside.join("secret").is_file());
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    fn codex_short_profile_dir_stays_within_socket_budget_and_migrates() {
        let realistic_home = Path::new("/Users/alexanderhamilton");
        let dir = default_profile_dir(
            realistic_home,
            ProviderKind::Codex,
            "enterprise-workspace-production-account-2",
        );
        assert!(
            dir.as_os_str().len() <= MAX_CODEX_HOME_BYTES,
            "codex profile path {} ({} bytes) exceeds {MAX_CODEX_HOME_BYTES}",
            dir.display(),
            dir.as_os_str().len()
        );
        let h = home("codex-short");
        let legacy = managed_root(&h)
            .join("codex")
            .join("very-long-codex-account-slug-that-exceeds-socket-limit");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("auth.json"), r#"{"token":"ok"}"#).unwrap();
        let migrated = migrate_long_codex_profile_if_needed(
            &h,
            "very-long-codex-account-slug-that-exceeds-socket-limit",
            &legacy,
        );
        assert_eq!(
            migrated,
            short_codex_profile_dir(&h, "very-long-codex-account-slug-that-exceeds-socket-limit")
        );
        assert!(migrated.join("auth.json").is_file());
        assert!(is_managed(&h, &migrated));
        assert!(
            orphans(&h, std::slice::from_ref(&legacy))
                .iter()
                .any(|o| o.dir == migrated.display().to_string())
        );
        std::fs::remove_dir_all(h).unwrap();
    }
}
