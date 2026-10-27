//! 오류 보고 파일 열기. panic hook·UI 경로에서도 막히지 않게 일반 파일만 연다: Unix는
//! `O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC`로 열어 FIFO·장치·심볼릭 링크에서 기다리지 않고, 연 뒤 종류를 다시 본다.
//! Windows는 링크가 아닌 일반 파일인지 먼저 확인한다.

use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::Path;

fn not_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "error reporting: not a regular file",
    )
}

/// 읽기 전용으로 일반 파일만 연다. 없으면 `NotFound`, 일반 파일이 아니면(FIFO·장치·디렉터리·링크) `InvalidData`.
pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let file = File::from(open(
        path,
        rustix::fs::OFlags::RDONLY,
        rustix::fs::Mode::empty(),
    )?);
    #[cfg(not(unix))]
    let file = {
        if !fs::symlink_metadata(path)?.is_file() {
            return Err(not_regular());
        }
        File::open(path)?
    };
    if !file.metadata()?.is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

/// 잠금 파일(0600)을 만들거나 연다. 일반 파일이 아니면 `InvalidData`.
pub(crate) fn open_lock(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags};
        File::from(open(
            path,
            OFlags::RDWR | OFlags::CREATE,
            Mode::RUSR | Mode::WUSR,
        )?)
    };
    #[cfg(not(unix))]
    let file = {
        if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) {
            return Err(not_regular());
        }
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?
    };
    if !file.metadata()?.is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

#[cfg(unix)]
fn open(
    path: &Path,
    flags: rustix::fs::OFlags,
    mode: rustix::fs::Mode,
) -> io::Result<rustix::fd::OwnedFd> {
    use rustix::fs::OFlags;
    rustix::fs::open(
        path,
        flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        mode,
    )
    .map_err(|errno| {
        let error = io::Error::from(errno);
        // 링크(ELOOP)·소켓(ENXIO) 등 일반 파일이 아닌 자리는 한 종류로 돌려준다.
        if error.kind() != io::ErrorKind::NotFound
            && fs::symlink_metadata(path).is_ok_and(|m| !m.is_file())
        {
            not_regular()
        } else {
            error
        }
    })
}

/// 일반 파일을 `limit` 바이트까지 읽는다. 넘으면 `Ok(None)`.
pub(crate) fn read_bounded(path: &Path, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let cap = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut raw = Vec::new();
    open_regular(path)?.take(cap).read_to_end(&mut raw)?;
    Ok((raw.len() <= limit).then_some(raw))
}

/// 링크가 아닌 실제 디렉터리.
pub(crate) fn real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}
