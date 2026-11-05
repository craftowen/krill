#[cfg(unix)]
use std::fs::Permissions;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::{Component, FILES, RECORD_BYTES};

/// Decimal bytes: 4 components × 5 files × 1,000,000 = 20,000,000.
pub const FILE_BYTES: u64 = 1_000_000;

/// Longest wait for another writer before an event is dropped.
pub const LOCK_WAIT: Duration = Duration::from_millis(50);

/// Serializes this process's writers so they never contend on flock, which macOS applies per
/// open file description: polling threads could each wait past `LOCK_WAIT` and drop events.
/// ponytail: one lock for every family; key it by directory and component if appends get slow.
static IN_PROCESS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Stateless file writer: every append and rotation takes the same cross-process family lock.
/// The lock file is permanently empty and is never renamed/deleted.
#[derive(Debug)]
pub struct RollingLog {
    directory: PathBuf,
    component: Component,
}

impl RollingLog {
    pub fn open(directory: impl Into<PathBuf>, component: Component) -> io::Result<Self> {
        let directory = directory.into();
        let mut builder = DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&directory)?;
        // Windows has no equivalent private-mode guarantee yet; OWE-833 adds explicit ACLs.
        #[cfg(unix)]
        fs::set_permissions(&directory, Permissions::from_mode(0o700))?;
        let log = Self {
            directory,
            component,
        };
        // Recover oversized files left by an older implementation or interrupted partial writes.
        // If another writer is active it will maintain the same invariant on its next append.
        log.with_lock(|| {
            for index in 0..FILES {
                let path = log.path(index);
                // Repair only existing files: creating them here left empty `.log.1`..`.log.4` behind.
                match open_file_with_access(&path, false) {
                    Ok(file) => {
                        if file.metadata()?.len() > FILE_BYTES {
                            file.set_len(FILE_BYTES)?;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        })?;
        Ok(log)
    }

    fn path(&self, index: usize) -> PathBuf {
        let name = self.component.name();
        self.directory.join(if index == 0 {
            format!("{name}.log")
        } else {
            format!("{name}.log.{index}")
        })
    }

    fn with_lock(&self, action: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        let _serialized = IN_PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lock = open_file(
            &self
                .directory
                .join(format!("{}.lock", self.component.name())),
        )?;
        // Other processes hold the lock for one append, so wait briefly instead of dropping the
        // event. Only a stuck holder costs the event, never an unbounded stall of the logging thread.
        // A hold usually lasts one append, so the first retries only yield: Windows rounds a 1ms
        // sleep up to its ~15ms tick. A longer hold then sleeps so its holder gets the CPU back.
        let deadline = Instant::now() + LOCK_WAIT;
        for attempt in 0u32.. {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    if attempt < 32 {
                        std::thread::yield_now();
                    } else {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                Err(TryLockError::WouldBlock) => return Ok(()),
                Err(TryLockError::Error(e)) => return Err(e),
            }
        }
        let result = action();
        // Explicit unlock also releases a lock briefly inherited by another
        // thread's fork before exec; relying only on close can prolong it.
        lock.unlock()?;
        result
    }

    /// One bounded UTF-8 event. Too-long records end at a character boundary.
    /// Failure never falls back to stderr or another unbounded file.
    pub fn append(&self, record: &str) -> io::Result<()> {
        let end = record.floor_char_boundary(record.len().min(RECORD_BYTES - 1));
        let record = record.get(..end).unwrap_or_default();
        self.with_lock(|| {
            let mut active = open_file(&self.path(0))?;
            let bytes = u64::try_from(record.len() + 1).map_err(io::Error::other)?;
            if active.metadata()?.len().saturating_add(bytes) > FILE_BYTES {
                drop(active);
                remove_if_present(&self.path(FILES - 1))?;
                for index in (0..FILES - 1).rev() {
                    match fs::rename(self.path(index), self.path(index + 1)) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e),
                    }
                }
                active = open_file(&self.path(0))?;
            }
            active.write_all(record.as_bytes())?;
            active.write_all(b"\n")
        })
    }
}

fn open_file(path: &Path) -> io::Result<File> {
    open_file_with_access(path, true)
}

/// `append` is the event path; otherwise this is the startup repair, which never creates files.
fn open_file_with_access(path: &Path, append: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    // Windows append-only handles lack FILE_WRITE_DATA, which set_len needs
    // when repairing an oversized log. Normal events keep append semantics.
    options.create(append).write(true).append(append).read(true);
    // Windows has no equivalent private-mode or no-follow guarantee yet; OWE-833
    // restores these with explicit ACLs and reparse-point handling.
    #[cfg(unix)]
    {
        let flags = i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).map_err(io::Error::other)?;
        options.mode(0o600).custom_flags(flags);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    file.set_permissions(Permissions::from_mode(0o600))?;
    Ok(file)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
