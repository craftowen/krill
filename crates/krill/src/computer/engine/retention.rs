//! Bounded retention for immutable capture files, independent of snapshot history.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

pub const MAX_IMAGES: usize = 32;
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_AGE: Duration = Duration::from_secs(300);

/// Prune only regular capture files created by this backend. Never follow links.
/// `keep` is the capture being returned; an oversized capture is rejected and removed.
pub fn prune(directory: &Path, keep: &Path) -> io::Result<()> {
    prune_with_limits(directory, keep, MAX_IMAGES, MAX_BYTES, MAX_AGE)
}

fn prune_with_limits(
    directory: &Path,
    keep: &Path,
    max_images: usize,
    max_bytes: u64,
    max_age: Duration,
) -> io::Result<()> {
    let now = SystemTime::now();
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("window-") || !name.ends_with(".png") {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified()?;
        if path == keep && metadata.len() > max_bytes {
            fs::remove_file(path)?;
            return Err(io::Error::other(
                "capture exceeds the retained-image byte budget",
            ));
        }
        if path != keep && now.duration_since(modified).is_ok_and(|age| age > max_age) {
            fs::remove_file(path)?;
            continue;
        }
        entries.push((modified, path, metadata.len()));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    let mut bytes = entries
        .iter()
        .map(|entry| entry.2)
        .fold(0_u64, u64::saturating_add);
    let mut count = entries.len();
    for (_, path, length) in entries {
        if count <= max_images && bytes <= max_bytes {
            break;
        }
        if path == keep {
            continue;
        }
        fs::remove_file(path)?;
        count = count.saturating_sub(1);
        bytes = bytes.saturating_sub(length);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn directory() -> std::path::PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "krill-image-retention-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn count_and_byte_budget_keep_the_newest_capture() {
        let dir = directory();
        for n in 0..5 {
            fs::write(dir.join(format!("window-{n}.png")), [0; 8]).unwrap();
        }
        let keep = dir.join("window-4.png");
        prune_with_limits(&dir, &keep, 3, 16, Duration::from_secs(300)).unwrap();
        assert!(keep.is_file());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn expired_files_are_removed_but_unrelated_files_survive() {
        let dir = directory();
        let old = dir.join("window-old.png");
        let keep = dir.join("window-new.png");
        fs::write(&old, []).unwrap();
        fs::File::open(&old)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();
        fs::write(&keep, []).unwrap();
        fs::write(dir.join("user-image.png"), b"retain").unwrap();
        prune(&dir, &keep).unwrap();
        assert!(!old.exists());
        assert_eq!(fs::read(dir.join("user-image.png")).unwrap(), b"retain");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_capture_is_rejected_without_deleting_other_images() {
        let dir = directory();
        let keep = dir.join("window-large.png");
        fs::write(&keep, [0; 20]).unwrap();
        fs::write(dir.join("window-other.png"), [0; 4]).unwrap();
        assert!(prune_with_limits(&dir, &keep, 3, 16, MAX_AGE).is_err());
        assert!(!keep.exists());
        assert!(dir.join("window-other.png").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_never_pruned() {
        let dir = directory();
        let outside = dir.with_extension("outside");
        fs::write(&outside, b"private outside data").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("window-link.png")).unwrap();
        let keep = dir.join("window-current.png");
        fs::write(&keep, []).unwrap();
        prune_with_limits(&dir, &keep, 1, 1, MAX_AGE).unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"private outside data");
        assert!(
            fs::symlink_metadata(dir.join("window-link.png"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(dir).unwrap();
        fs::remove_file(outside).unwrap();
    }
}
