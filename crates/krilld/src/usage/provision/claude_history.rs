//! Claude 기록을 공용 루트로 모은다. 파일은 삭제하지 않고 원본 트리와 충돌 정본을 보존한다.
//! 프로세스 전역 잠금은 `provision::LOCK`, 데몬 간 잠금은 `File::try_lock`으로 직렬화한다.

use std::fs::{self, Metadata, OpenOptions};
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use krill_proto::os::fs::OpenOptionsExt as _;
use sha2::{Digest as _, Sha256};

use super::{LINKED, Report};

const HISTORY: &str = "history.jsonl";

pub(super) fn quiescent(profile: &Path) -> bool {
    super::history_probe::quiescent(profile, "CLAUDE_CONFIG_DIR")
}

pub(super) fn needs_merge(host: &Path, profile: &Path) -> bool {
    let archive = archive_root(host, profile);
    LINKED.iter().copied().chain([HISTORY]).any(|name| {
        !profile.join(name).is_symlink()
            && (profile.join(name).symlink_metadata().is_ok()
                || archive
                    .join(name)
                    .join("original")
                    .symlink_metadata()
                    .is_ok())
    })
}

pub(super) fn share(
    host: &Path,
    profile: &Path,
    idle: &dyn Fn() -> bool,
    report: &mut Report,
) -> io::Result<()> {
    // 호출자가 host/profile의 경로 겹침을 먼저 거부한 뒤에만 쓰기 시작한다.
    real_dir(host)?;
    let archive = archive_root(host, profile);
    let _lock = if needs_merge(host, profile) {
        if !idle() {
            report
                .conflicts
                .extend(LINKED.iter().copied().chain([HISTORY]));
            return Ok(());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(host.join(".krill-history-migration.lock"))?;
        lock.try_lock().map_err(io::Error::other)?;
        Some(lock)
    } else {
        None
    };
    for name in LINKED.iter().copied().chain([HISTORY]) {
        let source = profile.join(name);
        let target = host.join(name);
        if source.is_symlink() {
            if fs::read_link(&source)? == target
                || source
                    .canonicalize()
                    .ok()
                    .zip(target.canonicalize().ok())
                    .is_some_and(|(a, b)| a == b)
            {
                ensure_target(&target, name)?;
            } else {
                // 외부/Orca 소유 링크의 대상을 이행하지 않는다.
                report.conflicts.push(name);
            }
            continue;
        }
        let backup = archive.join(name);
        let original = backup.join("original");
        let private = source.symlink_metadata().is_ok();
        if private || original.symlink_metadata().is_ok() {
            // 검사 후 생긴 프로세스도 디렉터리를 옮기기 직전에 다시 확인한다.
            if !idle() {
                report.conflicts.push(name);
                continue;
            }
            if private {
                validate_tree(&source, 0)?;
                if original.symlink_metadata().is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "unfinished history migration has another source",
                    ));
                }
                make_archive(host, &backup)?;
                // 같은 파일시스템만 허용한다. EXDEV면 원본을 남기고 안전하게 실패한다.
                fs::rename(&source, &original)?;
            }
            // 중단 후 original만 남아도 동일 경로에서 다시 병합한다.
            let result = merge(&original, &target, &backup.join("displaced"), 0)
                .and_then(|()| std::os::unix::fs::symlink(&target, &source));
            if let Err(error) = result {
                // 부분 게시된 공용 파일은 보존하고 프로필의 원본 경로를 복구한다.
                if source.symlink_metadata().is_err() {
                    fs::rename(&original, &source)?;
                }
                return Err(error);
            }
        } else {
            ensure_target(&target, name)?;
            std::os::unix::fs::symlink(&target, &source)?;
        }
        report.changed.push(name);
    }
    Ok(())
}

fn ensure_target(target: &Path, name: &str) -> io::Result<()> {
    if name != HISTORY {
        return real_dir(target);
    }
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)
    {
        Ok(_) => Ok(()),
        Err(e)
            if e.kind() == io::ErrorKind::AlreadyExists && target.symlink_metadata()?.is_file() =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn archive_root(host: &Path, profile: &Path) -> PathBuf {
    let digest = format!(
        "{:x}",
        Sha256::digest(profile.as_os_str().as_encoded_bytes())
    );
    let slug = profile.file_name().unwrap_or_default().to_string_lossy();
    host.join("projects-merged-backup")
        .join(format!("{slug}-{digest}"))
}

fn real_dir(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.symlink_metadata()?.is_dir() => {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn make_archive(host: &Path, path: &Path) -> io::Result<()> {
    let mut parent = host.to_path_buf();
    for part in path
        .strip_prefix(host)
        .map_err(io::Error::other)?
        .components()
    {
        parent.push(part);
        real_dir(&parent)?;
    }
    Ok(())
}

/// 링크·특수 파일은 추적하지 않는다. 예상 밖 데이터는 원래 프로필에 남긴다.
fn validate_tree(path: &Path, depth: usize) -> io::Result<()> {
    if depth > 64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let meta = path.symlink_metadata()?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            validate_tree(&entry?.path(), depth + 1)?;
        }
    } else if !meta.is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}

fn preferred(incoming: &Metadata, current: &Metadata) -> io::Result<bool> {
    Ok(incoming.len() > current.len()
        || (incoming.len() == current.len() && incoming.modified()? > current.modified()?))
}

fn merge(source: &Path, target: &Path, displaced: &Path, depth: usize) -> io::Result<()> {
    if depth > 64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let incoming = source.symlink_metadata()?;
    if incoming.is_dir() {
        real_dir(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            merge(
                &entry.path(),
                &target.join(entry.file_name()),
                &displaced.join(entry.file_name()),
                depth + 1,
            )?;
        }
        return Ok(());
    }
    if !incoming.is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    // create-new와 같은 hard_link로 공용에 없는 파일만 원자적으로 게시한다.
    match fs::hard_link(source, target) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let current = target.symlink_metadata()?;
    if !current.is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    if preferred(&incoming, &current)? {
        let parent = displaced.parent().ok_or(io::ErrorKind::InvalidInput)?;
        // displaced 아래도 링크를 따라 쓰지 않는다.
        let root = displaced
            .ancestors()
            .nth(depth + 1)
            .ok_or(io::ErrorKind::InvalidInput)?;
        make_archive(root, parent)?;
        let mut backup = displaced.to_path_buf();
        let mut suffix = 0_u64;
        while backup.symlink_metadata().is_ok() {
            suffix += 1;
            backup = displaced.with_file_name(format!(
                "{}.{}",
                displaced.file_name().unwrap_or_default().to_string_lossy(),
                suffix
            ));
        }
        fs::rename(target, backup)?;
        fs::hard_link(source, target)?;
    }
    // 선택되지 않은 incoming도 original 트리에 그대로 보존한다.
    Ok(())
}

#[cfg(test)]
#[path = "claude_history_tests.rs"]
mod tests;
