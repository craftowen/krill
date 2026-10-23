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
    }
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
    lock.try_lock().map_err(io::Error::other)?;
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
    Ok(report)
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
