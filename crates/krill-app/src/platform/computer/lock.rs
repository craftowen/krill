//! 데스크톱 전역 입력 잠금: `/Users/Shared/.krill-computer-<euid>/input.lock`의 BSD `flock`.
//!
//! - CLI 실행기(`krill computer`의 lifecycle)와 같은 경로·같은 규칙이다. 경로는 환경변수(`KRILL_STATE_DIR`·`HOME`·
//!   `TMPDIR`)를 읽지 않는다. `/tmp`는 periodic 정리가 오래 쥔 잠금 inode를 지울 수 있어 쓰지 않는다.
//!   `/Users/Shared`는 sticky라 다른 사용자가 이 사용자의 디렉터리를 지우거나 바꾸지 못한다.
//! - 잠금은 열린 파일 설명에 붙어 실행기 프로세스가 닫거나 죽을 때만 풀린다. TTL·삭제·재생성·잘라내기를 하지 않는다.
//! - 매 시도 전에 취소·기한을 먼저 본다. 기한이 지난 요청은 잠금이 비어 있어도 얻지 못한다.
//! - 잠근 뒤 fd와 경로의 (dev, ino)가 같은지 확인해, 교체된 파일에 잠금 두 개가 공존하지 않게 한다.
//! - 디렉터리는 실제 디렉터리·소유자 euid·그룹/기타 권한 없음, 파일은 심볼릭 링크 아님·일반 파일·소유자 euid·
//!   그룹/기타 권한 없음·링크 1개여야 한다. 어긋나면 잠그지 않고 거절한다(입력을 내지 않는다).
//!
//! 협력 실행기 사이의 발행 직렬화이며, 사용자·다른 도구·옛 바이너리를 막는 보안 경계가 아니다.

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{FileType, FlockOperation, Mode, OFlags};
use rustix::io::Errno;

/// 잠금 대기 중 다시 시도하는 간격.
// ponytail: 10ms 폴링(공정성·FIFO 없음). 대기자가 많아 지연·기아가 문제면 차단 flock을 별도 스레드에서 기다리게 바꾼다.
const POLL: Duration = Duration::from_millis(10);

pub fn lock_path_for(euid: u32) -> PathBuf {
    PathBuf::from(format!("/Users/Shared/.krill-computer-{euid}/input.lock"))
}

pub fn default_lock_path() -> PathBuf {
    lock_path_for(rustix::process::geteuid().as_raw())
}

/// 잡고 있는 동안만 입력을 발행할 수 있다. 드롭하면 fd가 닫히며 풀린다.
#[derive(Debug)]
pub struct InputLock {
    _fd: OwnedFd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockError {
    /// 경로 소유자·종류·권한이 안전하지 않다.
    Unsafe(String),
    Io(String),
    /// 기한까지 잡지 못했거나 시작할 때 이미 기한이 지났다. 미실행이다.
    Timeout,
    /// 대기 중 취소·종료됐다. 미실행이다.
    Abandoned,
}

impl LockError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unsafe(_) => "lock_unsafe",
            Self::Io(_) => "lock_unavailable",
            Self::Timeout => "lock_timeout",
            Self::Abandoned => "cancelled",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Unsafe(why) | Self::Io(why) => why.clone(),
            Self::Timeout => "input lock wait exceeded the request deadline".to_owned(),
            Self::Abandoned => "request was cancelled while waiting for the input lock".to_owned(),
        }
    }
}

fn io_error(what: &str, errno: Errno) -> LockError {
    LockError::Io(format!("{what}: {errno}"))
}

fn open_lock_file(path: &Path) -> Result<OwnedFd, LockError> {
    let euid = rustix::process::geteuid().as_raw();
    let dir = path
        .parent()
        .ok_or_else(|| LockError::Unsafe("lock path has no parent".to_owned()))?;
    match rustix::fs::mkdir(dir, Mode::RWXU) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(errno) => return Err(io_error("create lock dir", errno)),
    }
    let dir_stat = rustix::fs::lstat(dir).map_err(|e| io_error("stat lock dir", e))?;
    if FileType::from_raw_mode(dir_stat.st_mode) != FileType::Directory {
        return Err(LockError::Unsafe(
            "lock dir is not a real directory".to_owned(),
        ));
    }
    if dir_stat.st_uid != euid || dir_stat.st_mode & 0o077 != 0 {
        return Err(LockError::Unsafe(
            "lock dir must be owned by this user with mode 0700".to_owned(),
        ));
    }
    let fd = rustix::fs::open(
        path,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|errno| {
        if errno == Errno::LOOP {
            LockError::Unsafe("lock file is a symlink".to_owned())
        } else {
            io_error("open lock file", errno)
        }
    })?;
    let file_stat = rustix::fs::fstat(&fd).map_err(|e| io_error("stat lock file", e))?;
    if FileType::from_raw_mode(file_stat.st_mode) != FileType::RegularFile
        || file_stat.st_uid != euid
        || file_stat.st_mode & 0o077 != 0
        || file_stat.st_nlink != 1
    {
        return Err(LockError::Unsafe(
            "lock file must be a private regular single-link file owned by this user".to_owned(),
        ));
    }
    Ok(fd)
}

fn same_inode(fd: &OwnedFd, path: &Path) -> Result<bool, LockError> {
    let held = rustix::fs::fstat(fd).map_err(|e| io_error("stat lock file", e))?;
    match rustix::fs::lstat(path) {
        Ok(current) => Ok(held.st_dev == current.st_dev && held.st_ino == current.st_ino),
        Err(Errno::NOENT) => Ok(false),
        Err(errno) => Err(io_error("stat lock path", errno)),
    }
}

/// `deadline`까지 잠금을 기다린다. `keep_waiting`이 `false`를 돌려주면 즉시 포기한다.
/// 매 시도 전에 취소·기한을 먼저 확인하므로, 이미 끝난 요청은 비어 있는 잠금도 얻지 않는다.
pub fn acquire(
    path: &Path,
    deadline: Instant,
    mut keep_waiting: impl FnMut() -> bool,
) -> Result<InputLock, LockError> {
    let mut fd = None;
    loop {
        if !keep_waiting() {
            return Err(LockError::Abandoned);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(LockError::Timeout);
        }
        let held = match fd.take() {
            Some(held) => held,
            None => open_lock_file(path)?,
        };
        match rustix::fs::flock(&held, FlockOperation::NonBlockingLockExclusive) {
            // 잠근 inode가 지금 경로의 inode인지 확인한다. 바뀌었으면(교체·이름 변경) 놓고 다음 시도에서 다시 연다.
            Ok(()) if same_inode(&held, path)? => return Ok(InputLock { _fd: held }),
            Ok(()) => drop(held),
            Err(Errno::WOULDBLOCK | Errno::INTR) => fd = Some(held),
            Err(errno) => return Err(io_error("flock", errno)),
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "krill-computer-lock-test-{}-{name}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn soon(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn default_path_ignores_state_and_tmpdir() {
        // Must stay byte-identical to the CLI lifecycle's `default_lease_path()`.
        assert_eq!(
            lock_path_for(501),
            PathBuf::from("/Users/Shared/.krill-computer-501/input.lock")
        );
        assert_eq!(
            default_lock_path(),
            lock_path_for(rustix::process::geteuid().as_raw())
        );
    }

    #[test]
    fn expired_or_cancelled_request_never_takes_a_free_lock() {
        let dir = scratch("expired");
        let path = dir.join("input.lock");
        assert_eq!(
            acquire(&path, Instant::now(), || true).unwrap_err(),
            LockError::Timeout
        );
        assert_eq!(
            acquire(&path, soon(5_000), || false).unwrap_err(),
            LockError::Abandoned
        );
        // Neither attempt held it: a live request still gets it at once.
        drop(acquire(&path, soon(100), || true).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn second_holder_times_out_until_first_drops() {
        let dir = scratch("contend");
        let path = dir.join("input.lock");
        let held = acquire(&path, soon(100), || true).unwrap();
        assert_eq!(dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        // flock binds to the open file description: a second open in the same process contends.
        assert_eq!(
            acquire(&path, soon(30), || true).unwrap_err(),
            LockError::Timeout
        );
        let inode = path.metadata().unwrap();
        drop(held);
        let again = acquire(&path, soon(100), || true).unwrap();
        // Same inode: never unlinked or recreated.
        assert_eq!(path.metadata().unwrap().ino(), inode.ino());
        drop(again);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancelled_wait_is_abandoned() {
        let dir = scratch("abandon");
        let path = dir.join("input.lock");
        let _held = acquire(&path, soon(100), || true).unwrap();
        assert_eq!(
            acquire(&path, soon(5_000), || false).unwrap_err(),
            LockError::Abandoned
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_symlinks_and_loose_permissions() {
        let dir = scratch("unsafe");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = dir.join("elsewhere");
        std::fs::write(&target, b"").unwrap();
        let link = dir.join("input.lock");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(
            acquire(&link, soon(10), || true),
            Err(LockError::Unsafe(_))
        ));

        let hard = dir.join("hard.lock");
        std::fs::hard_link(&target, &hard).unwrap();
        assert!(matches!(
            acquire(&hard, soon(10), || true),
            Err(LockError::Unsafe(_))
        ));

        let loose = dir.join("loose.lock");
        std::fs::write(&loose, b"").unwrap();
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            acquire(&loose, soon(10), || true),
            Err(LockError::Unsafe(_))
        ));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(matches!(
            acquire(&dir.join("other.lock"), soon(10), || true),
            Err(LockError::Unsafe(_))
        ));

        let dir_link = scratch("dirlink");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&dir, &dir_link).unwrap();
        assert!(matches!(
            acquire(&dir_link.join("input.lock"), soon(10), || true),
            Err(LockError::Unsafe(_))
        ));
        std::fs::remove_file(&dir_link).ok();
        std::fs::remove_dir_all(&dir).ok();
    }
}
