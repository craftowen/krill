//! Private backups, durable receipts, same-directory atomic replacement and optimistic conflict checks.

use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const LIMIT: u64 = 4 * 1024 * 1024;
static SERIAL: AtomicU64 = AtomicU64::new(0);

pub fn error(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

/// Refuse links and non-regular files. Check parents before creating directories or replacing files.
pub fn safe_path(path: &Path) -> io::Result<()> {
    for p in path.ancestors() {
        match fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(error(format!("refusing symlink: {}", p.display())));
            }
            Ok(m) if p == path && !m.is_file() => {
                return Err(error(format!("not a regular file: {}", p.display())));
            }
            Ok(m) if p != path && !m.is_dir() => return Err(error("parent is not a directory")),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

pub fn read(path: &Path) -> io::Result<Option<String>> {
    safe_path(path)?;
    match File::open(path) {
        Ok(f) => {
            let mut text = String::new();
            f.take(LIMIT + 1).read_to_string(&mut text)?;
            if u64::try_from(text.len()).unwrap_or(u64::MAX) > LIMIT {
                return Err(error("hook configuration exceeds 4 MiB"));
            }
            Ok(Some(text))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn directory(path: &Path) -> io::Result<()> {
    safe_path(&path.join(".minikrill-path-check"))?;
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

pub fn lock(dir: &Path) -> io::Result<File> {
    directory(dir)?;
    let path = dir.join("install.lock");
    safe_path(&path)?;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    f.try_lock()
        .map_err(|e| error(format!("another hook operation is running: {e}")))?;
    Ok(f)
}

fn sibling(path: &Path, kind: &str) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| error("missing file name"))?
        .to_string_lossy();
    Ok(path.with_file_name(format!(
        "{name}.minikrill-{kind}-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )))
}

fn exclusive(path: &Path, text: &str, mode: u32) -> io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

pub fn backup(path: &Path, text: &str) -> io::Result<PathBuf> {
    let b = sibling(path, "bak")?;
    exclusive(&b, text, 0o600)?;
    Ok(b)
}

/// Keep the current file's access mode. New configs, backups and receipts are private.
pub fn replace(path: &Path, expected: Option<&str>, next: Option<&str>) -> io::Result<()> {
    safe_path(path)?;
    let parent = path.parent().ok_or_else(|| error("missing parent"))?;
    directory(parent)?;
    if read(path)?.as_deref() != expected {
        return Err(error(format!(
            "configuration changed; rerun to review: {}",
            path.display()
        )));
    }
    if expected == next {
        return Ok(());
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt as _;
        fs::metadata(path).map_or(0o600, |m| m.permissions().mode() & 0o777)
    };
    #[cfg(windows)]
    let mode = 0o600; // 쓰기 측 mode는 Windows에서 무시된다(crate::os).
    if let Some(text) = expected {
        backup(path, text)?;
    }
    if let Some(text) = next {
        let temp = sibling(path, "tmp")?;
        let result = (|| {
            exclusive(&temp, text, mode)?;
            if read(path)?.as_deref() != expected {
                return Err(error("configuration changed during write"));
            }
            fs::rename(&temp, path)
        })();
        if result.is_err() {
            fs::remove_file(temp).ok();
        }
        result?;
    } else {
        if read(path)?.as_deref() != expected {
            return Err(error("configuration changed during removal"));
        }
        fs::remove_file(path)?;
    }
    // Windows는 디렉터리를 `File::open`으로 열 수 없다.
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}
