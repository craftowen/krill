//! SQLite 대화 DB와 WAL·SHM은 같은 사본에서 한 묶음으로 게시한다.
//! 기본 저장소와 해당 프로필이 모두 유휴일 때만 이행하며 원본 파일을 삭제하지 않는다.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use krill_proto::os::fs::OpenOptionsExt as _;

use super::super::{
    Report,
    history_store::{self, ForeignLinks, Store},
};

const SUMMARY: &str = "conversation_summaries.db";
const SUFFIXES: &[&str] = &["", "-wal", "-shm"];

fn store<'a>(host: &'a Path, profile: &'a Path) -> Store<'a> {
    Store {
        host,
        profile,
        directories: &["conversations"],
        files: &[],
        backup_name: "history-merged-backup",
        foreign_links: ForeignLinks::Preserve,
        live: false,
    }
}

/// 이행이 옮기거나 게시하는 경로만 유휴 조회한다. 살아 있는 agy는 프로세스 조회가 따로 막는다.
/// 로그까지 포함한 저장소 전체를 훑으면 조회 시간 상한을 넘겨 이행이 영영 보류될 수 있다.
pub(super) fn probe_paths(store: &Path) -> Vec<PathBuf> {
    let summary = store.join(SUMMARY);
    let mut paths = vec![store.join("conversations")];
    paths.extend(SUFFIXES.iter().map(|suffix| sidecar(&summary, suffix)));
    // 링크는 기본 저장소 조회가 대상을 본다. 없는 경로는 lsof 오류라 사용 중으로 판정된다.
    paths.retain(|path| {
        path.symlink_metadata()
            .is_ok_and(|meta| !meta.file_type().is_symlink())
    });
    paths
}

pub(super) fn needs_merge(home: &Path, profile: &Path) -> bool {
    let local = profile.join(".gemini/antigravity-cli");
    store(&home.join(".gemini/antigravity-cli"), &local).needs_merge()
        || local
            .join(SUMMARY)
            .symlink_metadata()
            .is_ok_and(|m| !m.file_type().is_symlink())
        || ["-wal", "-shm"]
            .iter()
            .any(|suffix| sidecar(&local.join(SUMMARY), suffix).exists())
        || [
            ".gemini/config/plugins",
            ".gemini/skills",
            ".gemini/antigravity-cli/skills",
        ]
        .iter()
        .any(|rel| {
            home.join(rel).is_dir()
                && profile
                    .join(rel)
                    .symlink_metadata()
                    .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        })
}

pub(super) fn provision(
    home: &Path,
    profile: &Path,
    idle: &dyn Fn() -> bool,
) -> io::Result<Report> {
    let profile = profile.canonicalize()?;
    let home = home.canonicalize()?;
    let local = profile.join(".gemini/antigravity-cli");
    let host = home.join(".gemini/antigravity-cli");
    if profile.starts_with(&host) || host.starts_with(&profile) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // 링크로 외부 홈 안에 쓰지 않는다. 프로필 전체를 링크한 경우도 위 canonicalize로 검증한다.
    for path in [&host, &local] {
        history_store::real_dir(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
        history_store::real_dir(path)?;
    }
    let mut report = Report::default();
    // 새 프로필에는 옮길 기록이 없다. 기존 기본 저장소를 바꾸지 않고 링크만 만든다.
    let needs_idle = store(&host, &local).needs_merge()
        || local
            .join(SUMMARY)
            .symlink_metadata()
            .is_ok_and(|m| !m.file_type().is_symlink())
        || ["-wal", "-shm"]
            .iter()
            .any(|suffix| sidecar(&local.join(SUMMARY), suffix).exists());
    if needs_idle && !idle() {
        report.conflicts.extend(["conversations", SUMMARY]);
        return Ok(report);
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(host.join(".krill-agy-history.lock"))?;
    lock.try_lock().map_err(io::Error::from)?;
    store(&host, &local).share_using(idle, &mut report, &merge_conversations)?;
    if !local.join("conversations").is_symlink()
        || local.join("conversations").canonicalize()?
            != host.join("conversations").canonicalize()?
        || (needs_idle && !idle())
    {
        report.conflicts.push(SUMMARY);
        return Ok(report);
    }
    share_summary(&host, &local, &mut report)?;
    share_gemini_extensions(&home, &profile, &host, &local, &mut report)?;
    Ok(report)
}

fn share_gemini_extensions(
    home: &Path,
    profile: &Path,
    host: &Path,
    local: &Path,
    report: &mut Report,
) -> io::Result<()> {
    for (rel, label) in [
        (".gemini/config/plugins", "plugins"),
        (
            ".gemini/config/import_manifest.json",
            "import_manifest.json",
        ),
        (".gemini/config/config.json", "config.json"),
        (".gemini/skills", "skills"),
        (".gemini/GEMINI.md", "GEMINI.md"),
        (".gemini/antigravity-cli/skills", "antigravity-cli/skills"),
        (".gemini/antigravity-cli/mcp_config.json", "mcp_config.json"),
    ] {
        let target = home.join(rel);
        let Ok(target_meta) = target.symlink_metadata() else {
            continue;
        };
        let source = profile.join(rel);
        match source.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                if fs::read_link(&source)? != target {
                    report.conflicts.push(label);
                }
                continue;
            }
            Ok(meta) if meta.is_dir() && fs::remove_dir(&source).is_ok() => {}
            Ok(meta) if meta.is_dir() && target_meta.is_dir() => {
                absorb_extension_dir(host, local, &source, &target, label)?;
                report.changed.push(label);
                continue;
            }
            Ok(_) => {
                report.conflicts.push(label);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(parent) = source.parent() {
            fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(&target, &source)?;
        report.changed.push(label);
    }
    Ok(())
}

/// 비어 있지 않은 프로필 확장 디렉터리는 기본 폴더에 없는 항목만 옮기고 백업 후 심볼릭 링크로 교체한다.
fn absorb_extension_dir(
    host: &Path,
    local: &Path,
    source: &Path,
    target: &Path,
    label: &str,
) -> io::Result<()> {
    copy_missing_extension(source, target, 0)?;
    let backup = history_store::archive_root(host, local, "history-merged-backup")
        .join("extensions")
        .join(label);
    history_store::make_archive(host, &backup)?;
    let original = history_store::unused_path(&backup.join("original"));
    history_store::replace_with_link(target, source, &original)?;
    copy_missing_extension(&original, target, 0)
}

fn copy_missing_extension(source: &Path, target: &Path, depth: usize) -> io::Result<()> {
    if depth > 64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let metadata = source.symlink_metadata()?;
    if metadata.is_dir() {
        if depth > 0 {
            match history_store::real_dir(target) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let name = entry.file_name();
            copy_missing_extension(&entry.path(), &target.join(&name), depth + 1)?;
        }
        return Ok(());
    }
    let result = if metadata.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(source)?, target)
    } else if metadata.is_file() {
        match fs::hard_link(source, target) {
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                if target.symlink_metadata().is_ok() {
                    return Ok(());
                }
                fs::copy(source, target).map(drop)
            }
            result => result,
        }
    } else {
        return Ok(());
    };
    match result {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => Err(error),
        _ => Ok(()),
    }
}

fn share_summary(host: &Path, local: &Path, report: &mut Report) -> io::Result<()> {
    let source = local.join(SUMMARY);
    let target = host.join(SUMMARY);
    if source.is_symlink() {
        if fs::read_link(&source)? != target {
            report.conflicts.push(SUMMARY);
        }
        return Ok(());
    }
    let backup =
        history_store::archive_root(host, local, "history-merged-backup").join("summaries");
    history_store::make_archive(host, &backup)?;
    // SQLite를 열거나 수정하지 않는다. WAL을 포함한 원본 전체를 그대로 보존한다.
    for suffix in SUFFIXES {
        let src = sidecar(&source, suffix);
        match src.symlink_metadata() {
            Ok(meta) if meta.is_file() => {
                let dst =
                    history_store::unused_path(&backup.join(src.file_name().unwrap_or_default()));
                fs::rename(src, dst)?;
            }
            Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    // 기본 DB가 없으면 SQLite가 초기화할 빈 파일을 만든다. 기존 기본 DB는 그대로 둔다.
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&target)
    {
        Ok(_) => {}
        Err(error)
            if error.kind() == io::ErrorKind::AlreadyExists
                && target.symlink_metadata()?.is_file() => {}
        Err(error) => return Err(error),
    }
    std::os::unix::fs::symlink(target, source)?;
    report.changed.push(SUMMARY);
    Ok(())
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    name.into()
}

fn group_rank(path: &Path) -> io::Result<(u64, SystemTime)> {
    let mut bytes = 0_u64;
    let mut modified = SystemTime::UNIX_EPOCH;
    for suffix in ["", "-wal"] {
        match sidecar(path, suffix).symlink_metadata() {
            Ok(meta) if meta.is_file() => {
                bytes = bytes.saturating_add(meta.len());
                modified = modified.max(meta.modified()?);
            }
            Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok((bytes, modified))
}

fn merge_database(source: &Path, target: &Path, displaced: &Path, depth: usize) -> io::Result<()> {
    for path in [source, target] {
        for suffix in SUFFIXES {
            match sidecar(path, suffix).symlink_metadata() {
                Ok(meta) if meta.is_file() => {}
                Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    if target.exists() && group_rank(source)? <= group_rank(target)? {
        return Ok(());
    }
    // sidecar만 게시된 중단 상태도 먼저 백업한 뒤 전체 incoming 묶음을 재게시한다.
    let parent = displaced.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let root = displaced
        .ancestors()
        .nth(depth + 1)
        .ok_or(io::ErrorKind::InvalidInput)?;
    history_store::make_archive(root, parent)?;
    let backup = history_store::unused_path(displaced);
    history_store::real_dir(&backup)?;
    for suffix in SUFFIXES {
        let path = sidecar(target, suffix);
        if path.symlink_metadata().is_ok() {
            fs::rename(&path, backup.join(path.file_name().unwrap_or_default()))?;
        }
    }
    // 본체를 마지막에 게시한다. 이 도중 죽어도 original과 displaced의 사본은 남는다.
    for suffix in ["-wal", "-shm", ""] {
        let src = sidecar(source, suffix);
        if src.is_file() {
            fs::hard_link(src, sidecar(target, suffix))?;
        }
    }
    Ok(())
}

fn merge_conversations(
    source: &Path,
    target: &Path,
    displaced: &Path,
    depth: usize,
) -> io::Result<()> {
    if depth > 64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    if !source.is_dir() {
        return history_store::merge(source, target, displaced, depth);
    }
    history_store::real_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if let Some(stem) = text
            .strip_suffix("-wal")
            .or_else(|| text.strip_suffix("-shm"))
            && Path::new(stem).extension().is_some_and(|ext| ext == "db")
        {
            if !source.join(stem).is_file() {
                return Err(io::ErrorKind::InvalidData.into());
            }
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "db") && path.is_file() {
            merge_database(
                &path,
                &target.join(&name),
                &displaced.join(&name),
                depth + 1,
            )?;
        } else {
            merge_conversations(
                &path,
                &target.join(&name),
                &displaced.join(&name),
                depth + 1,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn non_empty_agy_skills_directory_is_absorbed_and_lock_contention_returns_would_block() {
        let root = std::env::temp_dir().join(format!("krill-agy-skills-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let home = root.join("home");
        let host = home.join(".gemini/antigravity-cli");
        let profile = root.join("profile");
        let local = profile.join(".gemini/antigravity-cli");
        fs::create_dir_all(host.join("skills/host-skill")).unwrap();
        fs::write(host.join("skills/host-skill/SKILL.md"), b"host").unwrap();
        fs::create_dir_all(local.join("skills/profile-skill")).unwrap();
        fs::write(local.join("skills/profile-skill/SKILL.md"), b"profile").unwrap();
        std::os::unix::fs::symlink("/tmp/external-skill", local.join("skills/symlinked-skill"))
            .unwrap();

        assert!(needs_merge(&home, &profile));
        let report = provision(&home, &profile, &|| true).unwrap();
        assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);
        assert!(report.changed.contains(&"antigravity-cli/skills"));
        assert_eq!(
            fs::read_link(local.join("skills")).unwrap(),
            host.canonicalize().unwrap().join("skills")
        );
        assert_eq!(
            fs::read(host.join("skills/profile-skill/SKILL.md")).unwrap(),
            b"profile"
        );
        assert_eq!(
            fs::read_link(host.join("skills/symlinked-skill")).unwrap(),
            Path::new("/tmp/external-skill")
        );
        assert_eq!(
            provision(&home, &profile, &|| true).unwrap(),
            Report::default()
        );

        let held = OpenOptions::new()
            .read(true)
            .write(true)
            .open(host.canonicalize().unwrap().join(".krill-agy-history.lock"))
            .unwrap();
        held.try_lock().unwrap();
        let err = provision(&home, &profile, &|| true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        drop(held);
        fs::remove_dir_all(root).unwrap();
    }
}
