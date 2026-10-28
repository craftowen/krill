//! Krill이 만든 계정 프로필 폴더(OWE-704). krilld와 앱의 로컬 경로가 같은 규칙을 쓴다.
//!
//! 지우는 대신 휴지통(`~/.Trash`)으로 옮긴다. 로그인 정보·대화 기록이 든 사용자 데이터라
//! macOS에서는 Finder로 되돌리고, Windows에서는 이 사설 보관 경로에서 원래 위치로 옮겨 복원한다. 관리 루트(`~/.config/krill/profiles/<provider>/<이름>`)
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
    // Read-only callers keep the original, working home when migration fails. Startup
    // and registration use the fallible variant and report the error to their caller.
    try_migrate_long_codex_profile(home, slug, current_dir)
        .unwrap_or_else(|_| current_dir.to_path_buf())
}

/// Preserve all legacy content and publish the short home only after a verified copy.
/// The original is never renamed or deleted. Existing unrelated targets are rejected.
pub fn try_migrate_long_codex_profile(
    home: &Path,
    slug: &str,
    current_dir: &Path,
) -> io::Result<PathBuf> {
    let legacy_codex_root = managed_root(home).join(ProviderKind::Codex.as_str());
    if !cfg!(unix)
        || current_dir.as_os_str().len() <= MAX_CODEX_HOME_BYTES
        || !current_dir.starts_with(&legacy_codex_root)
    {
        return Ok(current_dir.to_path_buf());
    }
    if !is_managed(home, current_dir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid legacy Codex home",
        ));
    }
    let source = current_dir.canonicalize()?;
    let target = short_codex_profile_dir(home, slug);
    let marker_name = ".krill-legacy-source";
    if source.join(marker_name).symlink_metadata().is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "legacy Codex home contains a reserved migration marker",
        ));
    }
    let marker = target.join(marker_name);
    if target.symlink_metadata().is_ok() {
        if !target.symlink_metadata()?.is_dir()
            || std::fs::read(&marker)? != source.as_os_str().as_encoded_bytes()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Codex home belongs to another profile",
            ));
        }
        return Ok(target);
    }
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("missing Codex parent"))?;
    std::fs::create_dir_all(parent)?;
    let migration_lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(target.with_extension("migration-lock"))?;
    migration_lock.try_lock().map_err(io::Error::other)?;
    // Another participating process may have completed before this lock was acquired.
    if target.symlink_metadata().is_ok() {
        return try_migrate_long_codex_profile(home, slug, current_dir);
    }
    // create_dir deliberately rejects stale staging directories, so retries cannot silently
    // reuse a partial copy. The user's original home stays usable on every failure.
    let staging = target.with_extension(format!("migrating-{}", std::process::id()));
    std::fs::create_dir(&staging)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))?;
    }
    let result = (|| {
        let before = codex_tree_stamp(&source, 0)?;
        copy_codex_tree(&source, &staging, &source, 0)?;
        if before != codex_tree_stamp(&source, 0)? {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Codex home changed during migration; retry after the agent stops",
            ));
        }
        std::fs::write(
            staging.join(marker_name),
            source.as_os_str().as_encoded_bytes(),
        )?;
        // A concurrently created target must never be replaced, including an empty directory.
        if target.symlink_metadata().is_ok() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        std::fs::rename(&staging, &target)?;
        Ok(target)
    })();
    if result.is_err() {
        std::fs::remove_dir_all(&staging).ok();
    }
    result
}

type CodexTreeStamp = Vec<(PathBuf, u64, std::time::SystemTime)>;

fn codex_tree_stamp(source: &Path, depth: usize) -> io::Result<CodexTreeStamp> {
    if depth > 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex home nesting too deep",
        ));
    }
    let mut entries = vec![(
        source.to_path_buf(),
        0,
        source.symlink_metadata()?.modified()?,
    )];
    for entry in std::fs::read_dir(source)? {
        let path = entry?.path();
        let meta = path.symlink_metadata()?;
        if meta.is_dir() {
            entries.extend(codex_tree_stamp(&path, depth.saturating_add(1))?);
        } else {
            entries.push((path, meta.len(), meta.modified()?));
        }
        if entries.len() > 100_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Codex migration entry budget exceeded",
            ));
        }
    }
    entries.sort();
    Ok(entries)
}

fn copy_codex_tree(source: &Path, target: &Path, original: &Path, depth: usize) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Codex home nesting too deep",
        ));
    }
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let src = entry.path();
        let dst = target.join(entry.file_name());
        let meta = src.symlink_metadata()?;
        if meta.is_dir() {
            std::fs::create_dir(&dst)?;
            copy_codex_tree(&src, &dst, original, depth.saturating_add(1))?;
            std::fs::set_permissions(&dst, meta.permissions())?;
        } else if meta.is_file() {
            std::fs::copy(&src, &dst)?;
            if !same_file_contents(&src, &dst)? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Codex source changed during copy",
                ));
            }
        } else if meta.file_type().is_symlink() {
            // External shared stores keep their identity. Internal absolute links would
            // depend on an orphaned legacy home, so leave that migration to the user.
            let link = std::fs::read_link(&src)?;
            let resolved = if link.is_absolute() {
                link.clone()
            } else {
                src.parent()
                    .ok_or_else(|| io::Error::other("invalid Codex link"))?
                    .join(&link)
            }
            .canonicalize()?;
            if resolved.starts_with(original) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "legacy Codex home has an internal absolute link",
                ));
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(resolved, dst)?;
            #[cfg(not(unix))]
            return Err(io::ErrorKind::Unsupported.into());
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported Codex home entry",
            ));
        }
    }
    Ok(())
}

fn same_file_contents(source: &Path, target: &Path) -> io::Result<bool> {
    use std::io::Read as _;
    let mut a = std::fs::File::open(source)?;
    let mut b = std::fs::File::open(target)?;
    let mut left = [0u8; 8192];
    let mut right = [0u8; 8192];
    loop {
        let n = a.read(&mut left)?;
        b.read_exact(
            right
                .get_mut(..n)
                .ok_or_else(|| io::Error::other("invalid copy size"))?,
        )?;
        if left.get(..n) != right.get(..n) {
            return Ok(false);
        }
        if n == 0 {
            return Ok(b.read(&mut right)? == 0);
        }
    }
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

/// 파일 탐색기 전용: macOS는 `~/.Trash/<이름>`, Windows는 시스템 Recycle Bin으로 옮긴다.
/// Windows 성공 반환값은 복구 UI 위치인 `shell:RecycleBinFolder`다. UNC/reparse point는 거부한다.
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
    #[cfg(windows)]
    {
        let _ = (home, name);
        recycle_windows_path(path)
    }
    #[cfg(not(windows))]
    rename_into_trash(home, path, &name)
}

/// File explorer deletion uses the OS recovery surface on Windows. Account data is
/// deliberately retained in the private ~/.Trash archive by `move_to_trash` instead.
#[cfg(windows)]
fn recycle_windows_path(path: &Path) -> io::Result<PathBuf> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let system_root =
        std::env::var_os("SystemRoot").ok_or_else(|| io::Error::other("SystemRoot is not set"))?;
    let powershell =
        PathBuf::from(system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let script = r"$ErrorActionPreference='Stop'
try {
  Add-Type -AssemblyName Microsoft.VisualBasic
  $p=$env:KRILL_RECYCLE_PATH
  if ($p.StartsWith('\\') -or ([IO.File]::GetAttributes($p) -band [IO.FileAttributes]::ReparsePoint)) {
    throw 'Recycle Bin is not available for network paths or reparse points'
  }
  if ([IO.Directory]::Exists($p)) {
    [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($p, [Microsoft.VisualBasic.FileIO.UIOption]::OnlyErrorDialogs, [Microsoft.VisualBasic.FileIO.RecycleOption]::SendToRecycleBin, [Microsoft.VisualBasic.FileIO.UICancelOption]::ThrowException)
  } else {
    [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($p, [Microsoft.VisualBasic.FileIO.UIOption]::OnlyErrorDialogs, [Microsoft.VisualBasic.FileIO.RecycleOption]::SendToRecycleBin, [Microsoft.VisualBasic.FileIO.UICancelOption]::ThrowException)
  }
  exit 0
} catch { exit 1 }
";
    let mut child = crate::os::background_command(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .env("KRILL_RECYCLE_PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() || path.symlink_metadata().is_ok() {
                    return Err(io::Error::other(
                        "Windows Recycle Bin operation failed; no permanent-delete fallback",
                    ));
                }
                return Ok(PathBuf::from("shell:RecycleBinFolder"));
            }
            Ok(None) if started.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => {
                child.kill().ok();
                child.wait().ok();
                return Err(result.err().unwrap_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Windows Recycle Bin operation timed out",
                    )
                }));
            }
        }
    }
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

    #[cfg(unix)]
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

#[cfg(all(test, unix))]
mod migration_regression_tests {
    use super::*;

    #[test]
    fn legacy_migration_preserves_nested_data_and_fails_without_switching() {
        let home = std::env::temp_dir().join(format!("krill-legacy-copy-{}", std::process::id()));
        std::fs::remove_dir_all(&home).ok();
        let source = managed_root(&home).join("codex/account-with-a-very-long-legacy-home-name");
        std::fs::create_dir_all(source.join("sessions/day")).unwrap();
        std::fs::create_dir_all(source.join("rules")).unwrap();
        std::fs::write(source.join("auth.json"), "original credentials").unwrap();
        std::fs::write(
            source.join("sessions/day/conversation.jsonl"),
            "original conversation",
        )
        .unwrap();
        std::fs::write(source.join("rules/private.md"), "private rules").unwrap();
        let target = try_migrate_long_codex_profile(&home, "nested", &source).unwrap();
        assert_eq!(
            std::fs::read(target.join("rules/private.md")).unwrap(),
            b"private rules"
        );
        assert_eq!(
            std::fs::read(target.join("sessions/day/conversation.jsonl")).unwrap(),
            b"original conversation"
        );
        std::fs::write(target.join("auth.json"), "new credentials").unwrap();
        assert_eq!(
            try_migrate_long_codex_profile(&home, "nested", &source).unwrap(),
            target
        );
        assert_eq!(
            std::fs::read(target.join("auth.json")).unwrap(),
            b"new credentials"
        );
        assert_eq!(
            std::fs::read(source.join("auth.json")).unwrap(),
            b"original credentials"
        );
        let external = home.join("shared-history");
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("record"), "shared data").unwrap();
        std::os::unix::fs::symlink("../../../../../shared-history", source.join("external"))
            .unwrap();
        let with_link = try_migrate_long_codex_profile(&home, "links", &source).unwrap();
        assert_eq!(
            with_link.join("external").canonicalize().unwrap(),
            external.canonicalize().unwrap()
        );
        let blocked = short_codex_profile_dir(&home, "blocked");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::write(blocked.join("auth.json"), "someone else").unwrap();
        assert!(try_migrate_long_codex_profile(&home, "blocked", &source).is_err());
        assert_eq!(
            migrate_long_codex_profile_if_needed(&home, "blocked", &source),
            source
        );
        let socket_path = PathBuf::from(format!("/tmp/kcs-{}", std::process::id()));
        std::fs::remove_file(&socket_path).ok();
        let socket = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
        std::fs::rename(socket_path, source.join("live.sock")).unwrap();
        assert!(try_migrate_long_codex_profile(&home, "incomplete", &source).is_err());
        assert!(!short_codex_profile_dir(&home, "incomplete").exists());
        drop(socket);
        std::fs::remove_dir_all(home).unwrap();
    }
}
