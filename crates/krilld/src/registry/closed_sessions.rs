use std::fs;
use std::io::{self, BufReader, Read as _};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use krill_proto::SessionId;
use krill_proto::control::ClosedSessionRanges;

use crate::registry::persistence::StateWriter;
use crate::session::lock;
use crate::usage::accounts::write_atomic_0600;

pub(crate) const FILE_NAME: &str = "closed-sessions.json";
const MAX_BYTES: u64 = 512 * 1024;
const VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskFile {
    version: u32,
    ranges: ClosedSessionRanges,
}

fn read_disk(path: &Path) -> io::Result<Option<ClosedSessionRanges>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let reader = BufReader::new(file.take(MAX_BYTES + 1));
    let disk: DiskFile =
        serde_json::from_reader(reader).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    if disk.version != VERSION {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(Some(disk.ranges))
}

fn encode_disk(ranges: &ClosedSessionRanges) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&DiskFile {
        version: VERSION,
        ranges: ranges.clone(),
    })
    .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
}

fn write_disk(path: &Path, ranges: &ClosedSessionRanges) -> io::Result<()> {
    let raw = encode_disk(ranges)?;
    write_atomic_0600(path, &raw)
}

fn scan_legacy_closed_markers(root: &Path, ranges: &mut ClosedSessionRanges) {
    let Ok(entries) = fs::read_dir(root.join("sessions")) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(raw_id) = name
            .to_str()
            .and_then(|s| s.strip_suffix(".noindex"))
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&n| n > 0)
        else {
            continue;
        };
        if fs::symlink_metadata(entry.path().join("closed")).is_ok_and(|m| m.is_file()) {
            ranges.insert(SessionId(raw_id));
        }
    }
}

pub(crate) fn known_floor(root: &Path) -> io::Result<u64> {
    Ok(read_disk(&root.join(FILE_NAME))?
        .and_then(|ranges| ranges.max_session())
        .map_or(0, |id| id.0))
}

pub(crate) struct ClosedLedger {
    writer: Option<StateWriter>,
    corrupt: AtomicBool,
    ranges: Mutex<ClosedSessionRanges>,
}

impl ClosedLedger {
    pub(crate) fn open(dir: Option<&Path>, live_or_kept: &[SessionId]) -> Self {
        let Some(root) = dir else {
            return Self {
                writer: None,
                corrupt: AtomicBool::new(false),
                ranges: Mutex::default(),
            };
        };
        let path = root.join(FILE_NAME);
        let (on_disk, mut ranges, corrupt) = match read_disk(&path) {
            Ok(Some(ranges)) => (Some(ranges.clone()), ranges, false),
            Ok(None) => (None, ClosedSessionRanges::default(), false),
            Err(error) => {
                // Never overwrite an unreadable ledger; session_ids::known_floor relies on the
                // error to block ID reuse until the file is repaired.
                tracing::warn!(kind = ?error.kind(), "closed session ledger unreadable");
                (None, ClosedSessionRanges::default(), true)
            }
        };
        scan_legacy_closed_markers(root, &mut ranges);
        for &id in live_or_kept {
            ranges.remove(id);
        }
        let writer = if corrupt {
            None
        } else {
            let needs_migration =
                on_disk.as_ref() != Some(&ranges) && (!ranges.is_empty() || on_disk.is_some());
            if needs_migration && let Err(error) = write_disk(&path, &ranges) {
                tracing::warn!(kind = ?error.kind(), "closed session ledger migration write failed");
            }
            let w = StateWriter::new("closed-sessions", path.clone()).ok();
            if needs_migration
                && let Some(ref writer) = w
                && let Ok(raw) = encode_disk(&ranges)
            {
                // Enqueue on the resilient writer so an initial ENOSPC retries until disk space returns.
                writer.save(raw);
            }
            w
        };
        Self {
            writer,
            corrupt: AtomicBool::new(corrupt),
            ranges: Mutex::new(ranges),
        }
    }

    pub(crate) fn record_closed(&self, id: SessionId) {
        let saved = {
            let mut guard = lock(&self.ranges);
            if !guard.insert(id) || self.corrupt.load(Ordering::Relaxed) {
                return;
            }
            match encode_disk(&guard) {
                Ok(raw) => {
                    if let Some(writer) = &self.writer {
                        writer.save(raw);
                        true
                    } else {
                        false
                    }
                }
                Err(error) => {
                    tracing::warn!(kind = ?error.kind(), "closed session ledger encode failed");
                    false
                }
            }
        };
        if saved && let Some(writer) = &self.writer {
            writer.flush_observed(Duration::from_millis(200)).ok();
        }
    }

    pub(crate) fn flush(&self, timeout: Duration) {
        if let Some(writer) = &self.writer {
            writer.shutdown(timeout);
        }
    }

    pub(crate) fn contains(&self, id: SessionId) -> bool {
        lock(&self.ranges).contains(id)
    }

    pub(crate) fn snapshot(&self) -> ClosedSessionRanges {
        lock(&self.ranges).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_closed_markers_excludes_kept_and_preserves_corrupt_floor() {
        let dir = std::env::temp_dir().join(format!("closed-ledger-unit-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let s41 = dir.join("sessions/41.noindex");
        let s42 = dir.join("sessions/42.noindex");
        fs::create_dir_all(&s41).unwrap();
        fs::create_dir_all(&s42).unwrap();
        fs::write(s41.join("closed"), b"").unwrap();
        fs::write(s42.join("closed"), b"").unwrap();

        let ledger = ClosedLedger::open(Some(&dir), &[SessionId(42)]);
        assert!(ledger.contains(SessionId(41)));
        assert!(!ledger.contains(SessionId(42)));
        assert_eq!(known_floor(&dir).unwrap(), 41);

        ledger.record_closed(SessionId(90));
        assert_eq!(known_floor(&dir).unwrap(), 90);

        fs::write(dir.join(FILE_NAME), b"{corrupt").unwrap();
        assert_eq!(
            known_floor(&dir).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let corrupt_ledger = ClosedLedger::open(Some(&dir), &[]);
        corrupt_ledger.record_closed(SessionId(91));
        assert_eq!(
            known_floor(&dir).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_record_closed_preserves_union_of_all_session_ids() {
        let dir = std::env::temp_dir().join(format!("closed-ledger-conc-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        let ledger = std::sync::Arc::new(ClosedLedger::open(Some(&dir), &[]));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let mut threads = Vec::new();
        for t in 0_u64..4 {
            let ledger = std::sync::Arc::clone(&ledger);
            let barrier = std::sync::Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                for i in 0_u64..25 {
                    ledger.record_closed(SessionId(1 + t * 25 + i));
                }
            }));
        }
        for handle in threads {
            handle.join().unwrap();
        }
        ledger.flush(Duration::from_secs(2));

        let reloaded = ClosedLedger::open(Some(&dir), &[]);
        for id in 1_u64..=100 {
            assert!(
                reloaded.contains(SessionId(id)),
                "closed session {id} lost under concurrent record_closed"
            );
        }
        fs::remove_dir_all(&dir).ok();
    }
}
