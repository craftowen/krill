#[cfg(unix)]
use std::fs::Permissions;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::{Component, FILES, RECORD_BYTES};

/// Decimal bytes: 4 components × 5 files × 1,000,000 = 20,000,000.
pub const FILE_BYTES: u64 = 1_000_000;

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
                match open_file_with_access(&path, false) {
                    Ok(file) => {
                        if file.metadata()?.len() > FILE_BYTES {
                            file.set_len(FILE_BYTES)?;
                        }
                    }
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
        let lock = open_file(
            &self
                .directory
                .join(format!("{}.lock", self.component.name())),
        )?;
        match lock.try_lock() {
            Ok(()) => {
                let result = action();
                // Explicit unlock also releases a lock briefly inherited by another
                // thread's fork before exec; relying only on close can prolong it.
                lock.unlock()?;
                result
            }
            Err(TryLockError::WouldBlock) => Ok(()),
            Err(TryLockError::Error(e)) => Err(e),
        }
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

fn open_file_with_access(path: &Path, append: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    // Windows append-only handles lack FILE_WRITE_DATA, which set_len needs
    // when repairing an oversized log. Normal events keep append semantics.
    options.create(true).write(true).append(append).read(true);
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
