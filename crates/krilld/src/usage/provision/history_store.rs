//! 프로바이더 기록을 기본 저장소로 모은다. 파일은 삭제하지 않고 원본 트리와 충돌 정본을 보존한다.
//! 프로세스 전역 잠금은 `provision::LOCK`, 데몬 간 잠금은 `File::try_lock`으로 직렬화한다.
//! `live` 저장소는 세션이 쓰는 중에도 옮긴다. 파일을 하드 링크로 게시한 뒤 경로만 원자 교체하므로
//! 열린 fd와 경로 쓰기가 같은 inode에 남고, 교체 전에 생긴 파일은 교체 뒤 한 번 더 모은다.

use std::collections::HashSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use krill_proto::os::fs::OpenOptionsExt as _;
use sha2::{Digest as _, Sha256};

use super::Report;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ForeignLinks {
    Preserve,
    Copy,
}

pub(super) struct Store<'a> {
    pub host: &'a Path,
    pub profile: &'a Path,
    pub directories: &'static [&'static str],
    pub files: &'static [&'static str],
    pub backup_name: &'static str,
    pub foreign_links: ForeignLinks,
    /// 사용 중인 프로필도 기다리지 않고 원자 교체로 옮긴다.
    pub live: bool,
}

impl Store<'_> {
    fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.directories.iter().chain(self.files).copied()
    }

    pub(super) fn needs_merge(&self) -> bool {
        let (host, profile) = (self.host, self.profile);
        let archive = archive_root(host, profile, self.backup_name);
        self.names().any(|name| {
            if profile.join(name).is_symlink() {
                return self.foreign_links == ForeignLinks::Copy
                    && fs::read_link(profile.join(name)).is_ok_and(|path| path != host.join(name))
                    && profile.join(name).canonicalize().ok()
                        != host.join(name).canonicalize().ok();
            }
            !profile.join(name).is_symlink()
                && (profile.join(name).symlink_metadata().is_ok()
                    || archive
                        .join(name)
                        .join("original")
                        .symlink_metadata()
                        .is_ok())
        })
    }

    pub(super) fn share(&self, idle: &dyn Fn() -> bool, report: &mut Report) -> io::Result<()> {
        self.share_using(idle, report, &merge)
    }

    pub(super) fn share_using(
        &self,
        idle: &dyn Fn() -> bool,
        report: &mut Report,
        merge_tree: &dyn Fn(&Path, &Path, &Path, usize) -> io::Result<()>,
    ) -> io::Result<()> {
        let (host, profile) = (self.host, self.profile);
        // 호출자가 host/profile의 경로 겹침을 먼저 거부한 뒤에만 쓰기 시작한다.
        real_dir(host)?;
        let archive = archive_root(host, profile, self.backup_name);
        let _lock = if self.needs_merge() {
            if !self.live && !idle() {
                report.conflicts.extend(self.names());
                return Ok(());
            }
            Some(migration_lock(host)?)
        } else {
            None
        };
        for name in self.names() {
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
                    ensure_target(&target, name, self.files)?;
                } else if self.foreign_links == ForeignLinks::Copy && source.exists() {
                    if !idle() {
                        report.conflicts.push(name);
                        continue;
                    }
                    self.import_link(name, &source, &target, &archive)?;
                    report.changed.push(name);
                } else {
                    report.conflicts.push(name);
                }
                continue;
            }
            let backup = archive.join(name);
            let original = backup.join("original");
            let private = source.symlink_metadata().is_ok();
            if private && self.live {
                validate_tree(&source, 0)?;
                make_archive(host, &backup)?;
                share_live(&source, &target, &backup, merge_tree)?;
            } else if private || original.symlink_metadata().is_ok() {
                // 검사 후 생긴 프로세스도 디렉터리를 옮기기 직전에 다시 확인한다.
                if !self.live && !idle() {
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
                let result = merge_tree(&original, &target, &backup.join("displaced"), 0)
                    .and_then(|()| std::os::unix::fs::symlink(&target, &source));
                if let Err(error) = result {
                    // 부분 게시된 공용 파일은 보존하고 프로필의 원본 경로를 복구한다.
                    if source.symlink_metadata().is_err() {
                        fs::rename(&original, &source)?;
                    }
                    return Err(error);
                }
            } else {
                ensure_target(&target, name, self.files)?;
                std::os::unix::fs::symlink(&target, &source)?;
            }
            report.changed.push(name);
        }
        Ok(())
    }
    /// 외부 저장소는 읽기만 한다. 사본을 병합한 뒤 Krill 쪽 링크만 바꾼다.
    fn import_link(
        &self,
        name: &str,
        source: &Path,
        target: &Path,
        archive: &Path,
    ) -> io::Result<()> {
        let foreign = source.canonicalize()?;
        let profile = self.profile.canonicalize()?;
        let host = self.host.canonicalize()?;
        if foreign.starts_with(&profile)
            || profile.starts_with(&foreign)
            || foreign.starts_with(&host)
            || host.starts_with(&foreign)
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let backup = archive.join(name);
        make_archive(self.host, &backup)?;
        let copy = unused_path(&backup.join("foreign-copy"));
        copy_tree(&foreign, &copy, 0)?;
        merge(&copy, target, &backup.join("displaced"), 0)?;
        let prior = unused_path(&backup.join("previous-link"));
        fs::rename(source, &prior)?;
        if let Err(error) = std::os::unix::fs::symlink(target, source) {
            fs::rename(&prior, source)?;
            return Err(error);
        }
        Ok(())
    }
}

/// 공용 저장소로 먼저 게시하고 경로를 교체한 뒤, 그사이 생긴 파일까지 다시 모은다.
fn share_live(
    source: &Path,
    target: &Path,
    backup: &Path,
    merge_tree: &dyn Fn(&Path, &Path, &Path, usize) -> io::Result<()>,
) -> io::Result<()> {
    let displaced = backup.join("displaced");
    if source.symlink_metadata()?.is_file() {
        append_missing_lines(source, target)?;
    }
    merge_tree(source, target, &displaced, 0)?;
    let original = unused_path(&backup.join("original"));
    replace_with_link(target, source, &original)?;
    merge_tree(&original, target, &displaced, 0)
}

/// 프롬프트 기록은 크기로 고르지 않고 없는 줄만 덧붙인다. 한 번의 append라 동시 기록과 섞이지 않는다.
fn append_missing_lines(source: &Path, target: &Path) -> io::Result<()> {
    let current = match fs::read(target) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let incoming = fs::read(source)?;
    let seen: HashSet<&[u8]> = current.split(|byte| *byte == b'\n').collect();
    let mut missing = Vec::new();
    for line in incoming.split(|byte| *byte == b'\n') {
        if !line.is_empty() && !seen.contains(line) {
            missing.extend_from_slice(line);
            missing.push(b'\n');
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    if current.last().is_some_and(|byte| *byte != b'\n') {
        missing.insert(0, b'\n');
    }
    OpenOptions::new()
        .append(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(target)?
        .write_all(&missing)
}

pub(super) fn migration_lock(host: &Path) -> io::Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(host.join(".krill-history-migration.lock"))?;
    lock.try_lock().map_err(io::Error::other)?;
    Ok(lock)
}

/// `source`를 `target` 링크로 원자 교체하고 이전 항목을 `parked`로 옮긴다.
pub(super) fn replace_with_link(target: &Path, source: &Path, parked: &Path) -> io::Result<()> {
    let name = source.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let staged = unused_path(&source.with_file_name(format!(
        ".krill-link-{}-{}",
        std::process::id(),
        name.to_string_lossy()
    )));
    std::os::unix::fs::symlink(target, &staged)?;
    match exchange(&staged, source) {
        Ok(()) => fs::rename(&staged, parked),
        // 교체를 지원하지 않는 파일시스템은 잠깐의 공백을 감수하고 되돌릴 수 있게 옮긴다.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
            ) =>
        {
            fs::remove_file(&staged)?;
            fs::rename(source, parked)?;
            std::os::unix::fs::symlink(target, source).or_else(|error| {
                fs::rename(parked, source)?;
                Err(error)
            })
        }
        Err(error) => {
            fs::remove_file(&staged).ok();
            Err(error)
        }
    }
}

#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn exchange(first: &Path, second: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, first, CWD, second, RenameFlags::EXCHANGE).map_err(io::Error::from)
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn exchange(_first: &Path, _second: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

fn ensure_target(target: &Path, name: &str, files: &[&str]) -> io::Result<()> {
    if !files.contains(&name) {
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

pub(super) fn archive_root(host: &Path, profile: &Path, backup_name: &str) -> PathBuf {
    let profile = profile
        .canonicalize()
        .unwrap_or_else(|_| profile.to_path_buf());
    let digest = format!(
        "{:x}",
        Sha256::digest(profile.as_os_str().as_encoded_bytes())
    );
    let slug = profile.file_name().unwrap_or_default().to_string_lossy();
    host.join(backup_name).join(format!("{slug}-{digest}"))
}

pub(super) fn real_dir(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && path.symlink_metadata()?.is_dir() => {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

pub(super) fn make_archive(host: &Path, path: &Path) -> io::Result<()> {
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

pub(super) fn merge(
    source: &Path,
    target: &Path,
    displaced: &Path,
    depth: usize,
) -> io::Result<()> {
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

pub(super) fn unused_path(base: &Path) -> PathBuf {
    let mut candidate = base.to_path_buf();
    let mut sequence = 0_u64;
    while candidate.symlink_metadata().is_ok() {
        sequence += 1;
        candidate = base.with_file_name(format!(
            "{}.{}",
            base.file_name().unwrap_or_default().to_string_lossy(),
            sequence
        ));
    }
    candidate
}

/// 외부 원본과 inode를 공유하지 않는다. 사본에 쓰더라도 Orca 원본은 바뀌지 않는다.
fn copy_tree(source: &Path, target: &Path, depth: usize) -> io::Result<()> {
    if depth > 64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let metadata = source.symlink_metadata()?;
    if metadata.is_dir() {
        real_dir(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()), depth + 1)?;
        }
    } else if metadata.is_file() {
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source)?;
        let before = input.metadata()?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(target)?;
        io::copy(&mut input, &mut output)?;
        let after = input.metadata()?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "source history changed while copying",
            ));
        }
        output.set_times(std::fs::FileTimes::new().set_modified(before.modified()?))?;
        output.sync_all()?;
    } else {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}
