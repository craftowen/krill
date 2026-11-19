//! Antigravity keeps each conversation in `conversations/<id>.db` (SQLite in WAL mode).
//! SQLite derives `-wal`/`-shm` from the name it opened, so a second hard-link name gives one
//! database two WAL files: frames written through one name are invisible to, or replayed over,
//! the other. The bridge therefore never aliases the database. It publishes an independent copy
//! of the database and its `-wal`, taken while no rollback journal exists and unchanged across
//! the copy. macOS SQLite keeps `-wal`/`-shm` after a clean close and a live agy holds them, so
//! their presence never means busy. A brain folder without its database is never resumable.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::os::fs::OpenOptionsExt as _;
use sha2::{Digest as _, Sha256};

use crate::agent::control::valid_session_id;

const MAGIC: &[u8] = b"SQLite format 3\0";
/// Moved with a parked database so a new file never inherits another file's WAL frames.
const SIDECARS: [&str; 2] = ["-wal", "-shm"];
const MAX_DB_BYTES: u64 = 4 << 30;
const MAX_AUX_ENTRIES: usize = 4096;
/// Krill-owned bookkeeping inside an Antigravity home. Antigravity does not read it.
const BRIDGE: &str = ".krill-bridge";

/// In use (or changed during the switch). The original keeps running; retry when it is idle.
fn busy() -> io::Error {
    io::Error::from_raw_os_error(libc::EBUSY)
}

/// The database and its WAL. A turn or checkpoint by the original changes one of them.
#[derive(PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
    handle: same_file::Handle,
    wal: Option<(u64, SystemTime)>,
}

impl Stamp {
    fn of(path: &Path) -> io::Result<Self> {
        no_journal(path)?;
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_file() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let wal = match fs::symlink_metadata(sidecar(path, "-wal")) {
            Ok(wal) if wal.is_file() => Some((wal.len(), wal.modified()?)),
            Ok(_) => return Err(io::ErrorKind::InvalidInput.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(Self {
            len: meta.len(),
            modified: meta.modified()?,
            handle: same_file::Handle::from_path(path)?,
            wal,
        })
    }
}

/// The conversation database as the selected home will open it.
pub(super) struct Bridge {
    target_db: PathBuf,
    target: same_file::Handle,
    /// `None` when source and target are already one path.
    source: Option<(PathBuf, Stamp)>,
}

impl Bridge {
    pub(super) fn target_db(&self) -> &Path {
        &self.target_db
    }

    /// Re-checked before the original retires: the published file is still ours and the
    /// original has not taken a turn the copy lacks.
    pub(super) fn verify(&self) -> io::Result<()> {
        if !fs::symlink_metadata(&self.target_db)?.is_file()
            || same_file::Handle::from_path(&self.target_db)? != self.target
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "selected conversation database was replaced",
            ));
        }
        check_header(&self.target_db)?;
        if let Some((path, stamp)) = &self.source
            && Stamp::of(path)? != *stamp
        {
            return Err(busy());
        }
        Ok(())
    }
}

/// Publish conversation `id` into `target_root`. `bound_root` is the home named by the
/// process-attributed transcript path; `source_roots` are the resolved source account's homes.
pub(super) fn prepare(
    bound_root: Option<&Path>,
    source_roots: &[PathBuf],
    target_root: &Path,
    id: &str,
) -> io::Result<Bridge> {
    if !valid_session_id(id) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let (source_root, source_db) = locate(bound_root, source_roots, id)?;
    check_header(&source_db)?;
    let target_db = target_root.join("conversations").join(format!("{id}.db"));
    if regular(&target_db)? && same_file::is_same_file(&source_db, &target_db)? {
        if source_db.canonicalize()? == target_db.canonicalize()? {
            // One path: the conversation already lives in the selected home.
            return Ok(Bridge {
                target: same_file::Handle::from_path(&target_db)?,
                target_db,
                source: None,
            });
        }
        // A hard-link alias left by an earlier bridge. Drop only the extra name while neither
        // name holds unmerged frames; the inode keeps its source name, so nothing is lost.
        settled(&source_db)?;
        settled(&target_db)?;
        fs::remove_file(&target_db)?;
        park_sidecars(
            &target_db,
            &target_root
                .join(BRIDGE)
                .join("archive")
                .join(unique_suffix()),
        )?;
    }
    let before = Stamp::of(&source_db)?;
    let scratch = target_root.join(BRIDGE).join("tmp");
    let staged = Staged::copy_db(&source_db, &scratch)?;
    let hash = digest(staged.path())?;
    if Stamp::of(&source_db)? != before || digest(&source_db)? != hash {
        return Err(busy());
    }
    let archive = target_root
        .join(BRIDGE)
        .join("archive")
        .join(format!("{id}-{}", unique_suffix()));
    let mut displaced = None;
    match fs::symlink_metadata(&target_db) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(meta) => {
            if !meta.is_file() {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            no_journal(&target_db)?;
            let existing = digest(&target_db)?;
            if existing == hash {
                // An identical independent copy is already in place.
                write_seed(target_root, id, &hash)?;
                copy_aux(&source_root, target_root, id, false, &archive, &scratch)?;
                return Ok(Bridge {
                    target: same_file::Handle::from_path(&target_db)?,
                    target_db,
                    source: Some((source_db, before)),
                });
            }
            // Replace only bytes this source is known to have been seeded from: such a copy has
            // no turns of its own. Anything else is a divergent conversation; keep both.
            if read_seed(&source_root, id).as_deref() != Some(existing.as_str()) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "selected profile holds a conflicting conversation copy",
                ));
            }
            let parked = archive.join("conversations").join(format!("{id}.db"));
            fs::create_dir_all(parked.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
            // ponytail: an agy already running this conversation in the selected home keeps
            // writing to the parked file; refuse here if Krill ever tracks such live readers.
            fs::rename(&target_db, &parked)?;
            park_sidecars(&target_db, &parked)?;
            displaced = Some(parked);
        }
    }
    if let Err(error) = staged.publish_db(&target_db) {
        if let Some(parked) = displaced {
            fs::rename(&parked, &target_db).ok();
            for suffix in SIDECARS {
                fs::rename(sidecar(&parked, suffix), sidecar(&target_db, suffix)).ok();
            }
        }
        return Err(error);
    }
    write_seed(target_root, id, &hash)?;
    copy_aux(&source_root, target_root, id, true, &archive, &scratch)?;
    Ok(Bridge {
        target: same_file::Handle::from_path(&target_db)?,
        target_db,
        source: Some((source_db, before)),
    })
}

/// The bound home wins. Otherwise exactly one database among the account's candidate homes.
fn locate(
    bound_root: Option<&Path>,
    source_roots: &[PathBuf],
    id: &str,
) -> io::Result<(PathBuf, PathBuf)> {
    let name = format!("{id}.db");
    if let Some(root) = bound_root {
        let db = root.join("conversations").join(&name);
        if regular(&db)? {
            return Ok((root.to_path_buf(), db));
        }
    }
    let mut found: Option<(PathBuf, PathBuf)> = None;
    for root in source_roots {
        let db = root.join("conversations").join(&name);
        if !regular(&db)? {
            continue;
        }
        match &found {
            Some((_, previous)) if same_file::is_same_file(previous, &db)? => {}
            Some(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "conversation database is ambiguous",
                ));
            }
            None => found = Some((root.clone(), db)),
        }
    }
    found.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "conversation database is missing; a brain folder alone cannot be resumed",
        )
    })
}

fn regular(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(suffix);
    name.into()
}

/// A rollback journal means a write transaction is open or must be recovered first.
fn no_journal(db: &Path) -> io::Result<()> {
    match fs::symlink_metadata(sidecar(db, "-journal")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(busy()),
    }
}

/// No journal and no WAL frames that are not yet in the database file.
fn settled(db: &Path) -> io::Result<()> {
    no_journal(db)?;
    match fs::symlink_metadata(sidecar(db, "-wal")) {
        Ok(wal) if wal.len() > 0 => Err(busy()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn park_sidecars(db: &Path, parked: &Path) -> io::Result<()> {
    for suffix in SIDECARS {
        let from = sidecar(db, suffix);
        match fs::symlink_metadata(&from) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => {
                let to = sidecar(parked, suffix);
                fs::create_dir_all(to.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
                fs::rename(from, to)?;
            }
        }
    }
    Ok(())
}

fn check_header(path: &Path) -> io::Result<()> {
    let mut head = [0_u8; 16];
    let read = File::open(path)?.read_exact(&mut head);
    if matches!(&read, Err(error) if error.kind() == io::ErrorKind::UnexpectedEof) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "conversation file is too short for an SQLite database",
        ));
    }
    read?;
    if head != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "conversation file is not an SQLite database",
        ));
    }
    Ok(())
}

/// Hash of a database together with its WAL frames: the conversation SQLite will read.
fn digest(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    hash_file(File::open(path)?, &mut hasher, &mut total)?;
    match File::open(sidecar(path, "-wal")) {
        Ok(wal) if wal.metadata()?.len() > 0 => {
            hasher.update(b"\0wal\0");
            hash_file(wal, &mut hasher, &mut total)?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let bytes: [u8; 32] = hasher.finalize().into();
    let mut hex = String::with_capacity(64);
    for byte in bytes {
        write!(hex, "{byte:02x}").ok();
    }
    Ok(hex)
}

fn hash_file(file: File, hasher: &mut Sha256, total: &mut u64) -> io::Result<()> {
    let mut reader = file.take(MAX_DB_BYTES + 1);
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        *total = total.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        if *total > MAX_DB_BYTES {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        hasher.update(buf.get(..n).ok_or(io::ErrorKind::InvalidData)?);
    }
    Ok(())
}

fn seed_path(root: &Path, id: &str) -> PathBuf {
    root.join(BRIDGE).join(format!("{id}.json"))
}

/// Hash of the database bytes the last bridge published into `root`.
fn read_seed(root: &Path, id: &str) -> Option<String> {
    let mut raw = Vec::new();
    File::open(seed_path(root, id))
        .ok()?
        .take(4096)
        .read_to_end(&mut raw)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let seed = value.get("seeded_from")?.as_str()?;
    (seed.len() == 64 && seed.bytes().all(|b| b.is_ascii_hexdigit())).then(|| seed.to_owned())
}

fn write_seed(root: &Path, id: &str, hash: &str) -> io::Result<()> {
    let path = seed_path(root, id);
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{id}-{}.tmp", unique_suffix()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(
            serde_json::json!({ "seeded_from": hash })
                .to_string()
                .as_bytes(),
        )?;
        file.sync_all()?;
        fs::rename(&temp, &path)
    })();
    if result.is_err() {
        fs::remove_file(&temp).ok();
    }
    result
}

fn unique_suffix() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// An independent copy (an APFS clone on macOS) removed unless published.
struct Staged(PathBuf);

impl Staged {
    fn copy(source: &Path, scratch: &Path) -> io::Result<Self> {
        fs::create_dir_all(scratch)?;
        let staged = Self(scratch.join(format!("{}.tmp", unique_suffix())));
        copy_file(source, &staged.0)?;
        Ok(staged)
    }

    /// The database and its non-empty WAL; SQLite rebuilds `-shm` when the copy is opened.
    fn copy_db(source: &Path, scratch: &Path) -> io::Result<Self> {
        let staged = Self::copy(source, scratch)?;
        let wal = sidecar(source, "-wal");
        if fs::symlink_metadata(&wal).is_ok_and(|m| m.is_file() && m.len() > 0) {
            copy_file(&wal, &sidecar(&staged.0, "-wal"))?;
        }
        Ok(staged)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// `hard_link` refuses an existing destination, so a racing writer is never overwritten.
    /// The staged name is dropped afterwards; the published file keeps a single name.
    fn publish(self, destination: &Path) -> io::Result<()> {
        fs::create_dir_all(destination.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
        fs::hard_link(&self.0, destination)
    }

    /// The WAL lands first so the database never appears without the frames it needs.
    fn publish_db(self, destination: &Path) -> io::Result<()> {
        fs::create_dir_all(destination.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
        let staged_wal = sidecar(&self.0, "-wal");
        let wal = sidecar(destination, "-wal");
        let linked_wal = staged_wal.is_file();
        if linked_wal {
            fs::hard_link(&staged_wal, &wal)?;
        }
        fs::hard_link(&self.0, destination).inspect_err(|_| {
            if linked_wal {
                fs::remove_file(&wal).ok();
            }
        })
    }
}

fn copy_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::copy(source, destination)?;
    #[cfg(windows)]
    {
        // Only this private copy becomes writable, so the selected account
        // can append turns and failed staging can be removed on Windows.
        let mut permissions = fs::metadata(destination)?.permissions();
        if permissions.readonly() {
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "Windows attribute on our owned staging copy; Unix permissions are preserved"
            )]
            permissions.set_readonly(false);
            fs::set_permissions(destination, permissions)?;
        }
    }
    // FlushFileBuffers requires write access on Windows. Unix also permits
    // syncing a read-only copy, whose permissions fs::copy preserves.
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    options.write(true);
    options.open(destination)?.sync_all()?;
    Ok(())
}

impl Drop for Staged {
    fn drop(&mut self) {
        fs::remove_file(&self.0).ok();
        fs::remove_file(sidecar(&self.0, "-wal")).ok();
    }
}

/// Brain folder, title and legacy session log follow the database. Missing files are copied.
/// A differing target file is archived and replaced only when the database itself was
/// (re)published; otherwise the selected home's file is kept.
fn copy_aux(
    source_root: &Path,
    target_root: &Path,
    id: &str,
    replace: bool,
    archive: &Path,
    scratch: &Path,
) -> io::Result<()> {
    if same_file::is_same_file(source_root, target_root).unwrap_or(false) {
        return Ok(());
    }
    let mut budget = MAX_AUX_ENTRIES;
    for relative in [
        PathBuf::from("brain").join(id),
        PathBuf::from("annotations").join(format!("{id}.pbtxt")),
        PathBuf::from("sessions").join(format!("{id}.jsonl")),
    ] {
        copy_entry(
            &source_root.join(&relative),
            &target_root.join(&relative),
            &archive.join(&relative),
            replace,
            &mut budget,
            scratch,
        )?;
    }
    Ok(())
}

fn copy_entry(
    source: &Path,
    target: &Path,
    archive: &Path,
    replace: bool,
    budget: &mut usize,
    scratch: &Path,
) -> io::Result<()> {
    let meta = match fs::symlink_metadata(source) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    // Never follow links out of the source home.
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    *budget = budget.checked_sub(1).ok_or(io::ErrorKind::FileTooLarge)?;
    let existing = match fs::symlink_metadata(target) {
        Ok(meta) => Some(meta),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if meta.is_dir() {
        if existing.as_ref().is_some_and(|m| !m.is_dir()) {
            if !replace {
                return Ok(());
            }
            park(target, archive)?;
        }
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let name = entry.file_name();
            copy_entry(
                &entry.path(),
                &target.join(&name),
                &archive.join(&name),
                replace,
                budget,
                scratch,
            )?;
        }
        return Ok(());
    }
    if !meta.is_file() {
        return Ok(());
    }
    if let Some(existing) = existing {
        if existing.is_file()
            && (same_file::is_same_file(source, target)?
                || (existing.len() == meta.len() && digest(source)? == digest(target)?))
        {
            return Ok(());
        }
        if !replace {
            return Ok(());
        }
        park(target, archive)?;
    }
    Staged::copy(source, scratch)?.publish(target)
}

fn park(target: &Path, archive: &Path) -> io::Result<()> {
    fs::create_dir_all(archive.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
    fs::rename(target, archive)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";

    struct Fixture {
        root: PathBuf,
        source: PathBuf,
        target: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("krill-agy-bridge-{name}-{}", std::process::id()));
            fs::remove_dir_all(&root).ok();
            let source = root.join("source/.gemini/antigravity-cli");
            let target = root.join("target/.gemini/antigravity-cli");
            for dir in [&source, &target] {
                fs::create_dir_all(dir.join("conversations")).unwrap();
            }
            Self {
                root,
                source,
                target,
            }
        }

        fn roots(&self) -> Vec<PathBuf> {
            vec![self.source.clone()]
        }

        fn db(root: &Path) -> PathBuf {
            root.join("conversations").join(format!("{ID}.db"))
        }

        fn write_db(root: &Path, body: &[u8]) {
            let mut bytes = MAGIC.to_vec();
            bytes.extend_from_slice(body);
            fs::write(Self::db(root), bytes).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
    }

    fn os_error(result: io::Result<Bridge>) -> Option<i32> {
        result.err().and_then(|e| e.raw_os_error())
    }

    #[test]
    fn readonly_source_is_preserved_and_its_staged_copy_is_removed() {
        let f = Fixture::new("readonly");
        Fixture::write_db(&f.source, b"readonly source");
        let source = Fixture::db(&f.source);
        let mut permissions = fs::metadata(&source).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&source, permissions).unwrap();
        let staged = Staged::copy(&source, &f.target.join("scratch")).unwrap();
        let scratch = staged.path().to_owned();
        assert_eq!(fs::read(&source).unwrap(), fs::read(&scratch).unwrap());
        assert!(fs::metadata(&source).unwrap().permissions().readonly());
        #[cfg(windows)]
        assert!(!fs::metadata(&scratch).unwrap().permissions().readonly());
        #[cfg(unix)]
        assert!(fs::metadata(&scratch).unwrap().permissions().readonly());
        drop(staged);
        assert!(!scratch.exists());
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(&source).unwrap().permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "Windows-only cleanup of this test's source file"
            )]
            permissions.set_readonly(false);
            fs::set_permissions(source, permissions).unwrap();
        }
    }

    #[test]
    fn publishes_independent_snapshot_and_keeps_source() {
        let f = Fixture::new("snapshot");
        Fixture::write_db(&f.source, b"turns");
        let logs = f
            .source
            .join("brain")
            .join(ID)
            .join(".system_generated/logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join("transcript.jsonl"), "{}\n").unwrap();
        fs::create_dir_all(f.source.join("annotations")).unwrap();
        fs::write(
            f.source.join("annotations").join(format!("{ID}.pbtxt")),
            "title: \"t\"",
        )
        .unwrap();
        fs::write(
            f.source.join("antigravity-oauth-token"),
            "source credentials",
        )
        .unwrap();

        let bridge = prepare(None, &f.roots(), &f.target, ID).unwrap();
        let (source_db, target_db) = (Fixture::db(&f.source), Fixture::db(&f.target));
        assert_eq!(bridge.target_db(), target_db);
        assert_eq!(fs::read(&target_db).unwrap(), fs::read(&source_db).unwrap());
        // No second name for one database: SQLite would pair it with a second WAL.
        assert!(!same_file::is_same_file(&source_db, &target_db).unwrap());
        let copied_log = f
            .target
            .join("brain")
            .join(ID)
            .join(".system_generated/logs/transcript.jsonl");
        assert!(copied_log.is_file());
        assert!(!same_file::is_same_file(logs.join("transcript.jsonl"), &copied_log).unwrap());
        assert!(
            f.target
                .join("annotations")
                .join(format!("{ID}.pbtxt"))
                .is_file()
        );
        assert!(!f.target.join("antigravity-oauth-token").exists());
        assert!(source_db.is_file());
        assert!(read_seed(&f.target, ID).is_some());
        assert!(
            !f.target
                .join(BRIDGE)
                .join("tmp")
                .read_dir()
                .unwrap()
                .any(|_| true)
        );
        bridge.verify().unwrap();
    }

    #[test]
    fn rollback_journal_source_is_not_copied() {
        let f = Fixture::new("journal");
        Fixture::write_db(&f.source, b"turns");
        fs::write(sidecar(&Fixture::db(&f.source), "-journal"), b"pages").unwrap();
        assert_eq!(
            os_error(prepare(None, &f.roots(), &f.target, ID)),
            Some(libc::EBUSY)
        );
        assert!(!Fixture::db(&f.target).exists());
    }

    #[test]
    fn live_source_wal_travels_with_the_copy_and_a_later_turn_blocks_commit() {
        let f = Fixture::new("live-wal");
        Fixture::write_db(&f.source, b"turns");
        let source_wal = sidecar(&Fixture::db(&f.source), "-wal");
        fs::write(&source_wal, b"frames").unwrap();
        fs::write(sidecar(&Fixture::db(&f.source), "-shm"), b"index").unwrap();
        let bridge = prepare(None, &f.roots(), &f.target, ID).unwrap();
        let target_db = Fixture::db(&f.target);
        assert_eq!(fs::read(sidecar(&target_db, "-wal")).unwrap(), b"frames");
        assert!(!sidecar(&target_db, "-shm").exists());
        bridge.verify().unwrap();
        fs::write(&source_wal, b"frames and a new turn").unwrap();
        assert_eq!(
            bridge.verify().err().and_then(|e| e.raw_os_error()),
            Some(libc::EBUSY)
        );
    }

    #[test]
    fn closed_source_leftover_sidecars_do_not_block_the_copy() {
        let f = Fixture::new("leftover");
        Fixture::write_db(&f.source, b"turns");
        fs::write(sidecar(&Fixture::db(&f.source), "-wal"), b"").unwrap();
        fs::write(sidecar(&Fixture::db(&f.source), "-shm"), b"index").unwrap();
        prepare(None, &f.roots(), &f.target, ID)
            .unwrap()
            .verify()
            .unwrap();
        assert!(!sidecar(&Fixture::db(&f.target), "-wal").exists());
    }

    #[test]
    fn brain_without_database_is_not_resumable() {
        let f = Fixture::new("brain-only");
        fs::create_dir_all(f.source.join("brain").join(ID)).unwrap();
        fs::write(f.source.join("brain").join(ID).join("notes.md"), "x").unwrap();
        let error = prepare(None, &f.roots(), &f.target, ID).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!f.target.join("brain").join(ID).exists());
        assert!(!Fixture::db(&f.target).exists());
    }

    #[test]
    fn non_sqlite_database_is_rejected() {
        let f = Fixture::new("header");
        fs::write(Fixture::db(&f.source), b"not a database").unwrap();
        let error = prepare(None, &f.roots(), &f.target, ID).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!Fixture::db(&f.target).exists());
    }

    #[test]
    fn divergent_target_copy_is_kept_and_rejected() {
        let f = Fixture::new("conflict");
        Fixture::write_db(&f.source, b"source turns");
        Fixture::write_db(&f.target, b"target turns");
        let error = prepare(None, &f.roots(), &f.target, ID).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            fs::read(Fixture::db(&f.target))
                .unwrap()
                .ends_with(b"target turns")
        );
    }

    #[test]
    fn target_wal_frames_count_as_divergent_turns() {
        let f = Fixture::new("target-wal");
        Fixture::write_db(&f.source, b"a1");
        drop(prepare(None, &f.roots(), &f.target, ID).unwrap());
        fs::write(sidecar(&Fixture::db(&f.source), "-wal"), b"a2").unwrap();
        fs::write(sidecar(&Fixture::db(&f.target), "-wal"), b"b2").unwrap();
        let error = prepare(None, &f.roots(), &f.target, ID).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(sidecar(&Fixture::db(&f.target), "-wal")).unwrap(),
            b"b2"
        );
    }

    #[test]
    fn round_trip_replaces_only_the_stale_seed_and_archives_it() {
        let f = Fixture::new("round-trip");
        Fixture::write_db(&f.source, b"a1");
        drop(prepare(None, &f.roots(), &f.target, ID).unwrap());
        // The selected home takes more turns, then the user switches back.
        Fixture::write_db(&f.target, b"a1 b2");
        // The retired original left its persistent index behind.
        fs::write(sidecar(&Fixture::db(&f.source), "-shm"), b"stale index").unwrap();
        let back = prepare(None, std::slice::from_ref(&f.target), &f.source, ID).unwrap();
        assert!(
            fs::read(Fixture::db(&f.source))
                .unwrap()
                .ends_with(b"a1 b2")
        );
        back.verify().unwrap();
        let archived: Vec<_> = fs::read_dir(f.source.join(BRIDGE).join("archive"))
            .unwrap()
            .flatten()
            .map(|e| e.path().join("conversations").join(format!("{ID}.db")))
            .collect();
        assert_eq!(archived.len(), 1);
        assert!(fs::read(&archived[0]).unwrap().ends_with(b"a1"));
        assert_eq!(
            fs::read(sidecar(&archived[0], "-shm")).unwrap(),
            b"stale index"
        );
        assert!(!sidecar(&Fixture::db(&f.source), "-shm").exists());
        // And forward again without either side diverging.
        drop(prepare(None, std::slice::from_ref(&f.source), &f.target, ID).unwrap());
    }

    #[test]
    fn hard_link_alias_from_an_earlier_bridge_becomes_an_independent_copy() {
        let f = Fixture::new("alias");
        Fixture::write_db(&f.source, b"turns");
        fs::hard_link(Fixture::db(&f.source), Fixture::db(&f.target)).unwrap();
        let bridge = prepare(None, &f.roots(), &f.target, ID).unwrap();
        assert!(!same_file::is_same_file(Fixture::db(&f.source), Fixture::db(&f.target)).unwrap());
        assert!(
            fs::read(Fixture::db(&f.source))
                .unwrap()
                .ends_with(b"turns")
        );
        bridge.verify().unwrap();
    }

    #[test]
    fn source_turn_after_snapshot_fails_verification() {
        let f = Fixture::new("diverged");
        Fixture::write_db(&f.source, b"turns");
        let bridge = prepare(None, &f.roots(), &f.target, ID).unwrap();
        fs::write(sidecar(&Fixture::db(&f.source), "-wal"), b"frames").unwrap();
        assert_eq!(
            bridge.verify().err().and_then(|e| e.raw_os_error()),
            Some(libc::EBUSY)
        );
        fs::remove_file(sidecar(&Fixture::db(&f.source), "-wal")).unwrap();
        Fixture::write_db(&f.source, b"turns and more");
        assert_eq!(
            bridge.verify().err().and_then(|e| e.raw_os_error()),
            Some(libc::EBUSY)
        );
    }

    #[test]
    fn bound_home_wins_and_candidates_must_be_unambiguous() {
        let f = Fixture::new("locate");
        let other = f.root.join("other/.gemini/antigravity-cli");
        fs::create_dir_all(other.join("conversations")).unwrap();
        Fixture::write_db(&f.source, b"source");
        Fixture::write_db(&other, b"other");
        let roots = vec![f.source.clone(), other.clone()];
        assert_eq!(
            prepare(None, &roots, &f.target, ID).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        drop(prepare(Some(&other), &roots, &f.target, ID).unwrap());
        assert!(
            fs::read(Fixture::db(&f.target))
                .unwrap()
                .ends_with(b"other")
        );
    }

    #[test]
    fn same_home_is_verified_without_copying() {
        let f = Fixture::new("same");
        Fixture::write_db(&f.source, b"turns");
        let bridge = prepare(None, &f.roots(), &f.source, ID).unwrap();
        assert_eq!(bridge.target_db(), Fixture::db(&f.source));
        assert!(!f.source.join(BRIDGE).exists());
        bridge.verify().unwrap();
    }
}
