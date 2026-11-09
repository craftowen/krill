//! Krill Drop: file transfer between a paired phone and this PC.
//!
//! Layout under the state directory, shared by `krill drop` and krilld so both read one truth:
//! - `drop/outbox/<id>/<name>`: PC→phone items. `<id>` is 32 lowercase hex. Writers fill
//!   `drop/outbox/.tmp-<id>/` and rename it into place, so a listed item is always complete.
//!   The item directory's mtime is the time it was added (the copied file keeps its own mtime).
//! - `drop/incoming/<upload>.partial`: phone→PC upload in progress. `<upload>.done` holds the saved
//!   path so a retried final chunk answers the same result instead of starting over.
//!
//! Finished phone→PC files land in [`inbox_dir`] (`~/Downloads/Krill Drop`) and are never overwritten.

use crate::os::fs::DirBuilderExt as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Largest file either direction accepts.
pub const MAX_DROP_BYTES: u64 = 2 << 30;
/// Bytes per mobile RPC chunk. Its base64 stays under the 700,000-char decode cap and the 1 MiB frame.
pub const CHUNK_BYTES: usize = 384 * 1024;
/// Outbox items the phone never removed are deleted after this long.
pub const OUTBOX_TTL: Duration = Duration::from_hours(7 * 24);
/// Abandoned uploads and unfinished outbox copies are deleted after this long.
pub const PARTIAL_TTL: Duration = Duration::from_hours(24);

const NAME_MAX: usize = 255;
const TMP_PREFIX: &str = ".tmp-";

#[must_use]
pub fn outbox_dir(state: &Path) -> PathBuf {
    state.join("drop").join("outbox")
}

#[must_use]
pub fn incoming_dir(state: &Path) -> PathBuf {
    state.join("drop").join("incoming")
}

/// Where finished phone→PC files are saved.
#[must_use]
pub fn inbox_dir(home: &Path) -> PathBuf {
    home.join("Downloads").join("Krill Drop")
}

#[must_use]
pub fn partial_path(state: &Path, upload: &str) -> PathBuf {
    incoming_dir(state).join(format!("{upload}.partial"))
}

#[must_use]
pub fn done_path(state: &Path, upload: &str) -> PathBuf {
    incoming_dir(state).join(format!("{upload}.done"))
}

/// Outbox item and upload ids: exactly 32 lowercase hex characters.
#[must_use]
pub fn is_id(id: &str) -> bool {
    crate::error_reporting::is_hex32(id)
}

/// A file name safe to create inside one directory: the last path component, without control
/// characters or leading dots (no hidden files), at most 255 bytes with the extension kept.
#[must_use]
pub fn sanitize_name(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = last.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim_start_matches('.');
    (!clean.is_empty()).then(|| numbered_name(clean, 0))
}

/// `name` for `n == 0`, else `stem (n).ext`, trimmed to 255 bytes on a char boundary.
#[must_use]
pub fn numbered_name(name: &str, n: u32) -> String {
    let suffix = if n == 0 {
        String::new()
    } else {
        format!(" ({n})")
    };
    // Keep at least 8 stem bytes so truncation never leaves a bare ".ext" (a hidden file).
    let budget = NAME_MAX.saturating_sub(suffix.len()).saturating_sub(8);
    let (stem, ext) = match name.rfind('.') {
        Some(at) if at > 0 && name.len().saturating_sub(at) <= budget => name.split_at(at),
        _ => (name, ""),
    };
    let mut end = stem.len().min(
        NAME_MAX
            .saturating_sub(suffix.len())
            .saturating_sub(ext.len()),
    );
    while !stem.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{suffix}{ext}", stem.get(..end).unwrap_or_default())
}

pub fn new_id() -> io::Result<String> {
    crate::error_reporting::random_id()
}

/// One complete outbox item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub added: SystemTime,
    pub path: PathBuf,
}

/// The item for `id`, or `None` when `id` is malformed or no complete item has it.
pub fn item(state: &Path, id: &str) -> io::Result<Option<Item>> {
    if !is_id(id) {
        return Ok(None);
    }
    let dir = outbox_dir(state).join(id);
    let meta = match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => meta,
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut files = fs::read_dir(&dir)?.filter_map(Result::ok);
    let (Some(entry), None) = (files.next(), files.next()) else {
        return Ok(None);
    };
    let file = entry.metadata()?;
    if !file.is_file() {
        return Ok(None);
    }
    Ok(Some(Item {
        id: id.to_owned(),
        name: entry.file_name().to_string_lossy().into_owned(),
        size: file.len(),
        added: meta.modified()?,
        path: entry.path(),
    }))
}

/// Complete outbox items, newest first.
pub fn list(state: &Path) -> io::Result<Vec<Item>> {
    let entries = match fs::read_dir(outbox_dir(state)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut items = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        if let Some(found) = item(state, &entry.file_name().to_string_lossy())? {
            items.push(found);
        }
    }
    items.sort_by(|a, b| b.added.cmp(&a.added).then_with(|| a.id.cmp(&b.id)));
    Ok(items)
}

/// Copy a regular file into the outbox and publish it atomically.
pub fn add(state: &Path, source: &Path) -> io::Result<Item> {
    let meta = fs::metadata(source)?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    if meta.len() > MAX_DROP_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "larger than 2 GiB",
        ));
    }
    let name = source
        .file_name()
        .and_then(|name| sanitize_name(&name.to_string_lossy()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unusable file name"))?;
    let outbox = outbox_dir(state);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&outbox)?;
    let id = new_id()?;
    let tmp = outbox.join(format!("{TMP_PREFIX}{id}"));
    fs::DirBuilder::new().mode(0o700).create(&tmp)?;
    let published =
        fs::copy(source, tmp.join(&name)).and_then(|_| fs::rename(&tmp, outbox.join(&id)));
    if let Err(e) = published {
        fs::remove_dir_all(&tmp).ok();
        return Err(e);
    }
    item(state, &id)?.ok_or_else(|| io::Error::other("drop item disappeared"))
}

/// Remove an outbox item. A missing item is already removed.
pub fn remove(state: &Path, id: &str) -> io::Result<()> {
    if !is_id(id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid id"));
    }
    match fs::remove_dir_all(outbox_dir(state).join(id)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Best-effort removal of expired outbox items and stale temporary files, as of `now`.
pub fn prune(state: &Path, now: SystemTime) {
    let older = |path: &Path, ttl: Duration| {
        fs::symlink_metadata(path)
            .and_then(|meta| meta.modified())
            .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > ttl))
    };
    for entry in fs::read_dir(outbox_dir(state))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ttl = if name.starts_with(TMP_PREFIX) {
            PARTIAL_TTL
        } else if is_id(&name) {
            OUTBOX_TTL
        } else {
            continue;
        };
        if older(&entry.path(), ttl) {
            fs::remove_dir_all(entry.path()).ok();
        }
    }
    for entry in fs::read_dir(incoming_dir(state))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
    {
        if older(&entry.path(), PARTIAL_TTL) {
            fs::remove_file(entry.path()).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "krill-drop-{name}-{}-{}",
            std::process::id(),
            new_id().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_keeps_only_a_visible_last_component() {
        assert_eq!(sanitize_name("report.pdf").as_deref(), Some("report.pdf"));
        assert_eq!(sanitize_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(
            sanitize_name("C:\\Users\\a\\사진 1.jpg").as_deref(),
            Some("사진 1.jpg")
        );
        assert_eq!(sanitize_name("a\u{0}b\nc.txt").as_deref(), Some("abc.txt"));
        assert_eq!(sanitize_name("..env").as_deref(), Some("env"));
        for rejected in ["", ".", "..", "dir/", "a/..", "\u{7}", "..."] {
            assert_eq!(sanitize_name(rejected), None, "{rejected:?}");
        }
    }

    #[test]
    fn long_names_keep_their_extension_within_255_bytes() {
        let long = format!("{}.tar.gz", "가".repeat(200));
        let name = sanitize_name(&long).unwrap();
        assert!(name.len() <= 255);
        assert_eq!(name.get(name.len() - 3..), Some(".gz"));
        assert!(name.starts_with('가'));
        let numbered = numbered_name(&name, 12);
        assert!(numbered.len() <= 255);
        assert!(numbered.ends_with(" (12).gz"), "{numbered}");
        let huge_ext = format!("a.{}", "x".repeat(300));
        assert_eq!(sanitize_name(&huge_ext).unwrap().len(), 255);
    }

    #[test]
    fn clash_names_number_before_the_extension() {
        assert_eq!(numbered_name("report.pdf", 0), "report.pdf");
        assert_eq!(numbered_name("report.pdf", 1), "report (1).pdf");
        assert_eq!(numbered_name("archive.tar.gz", 2), "archive.tar (2).gz");
        assert_eq!(numbered_name("README", 3), "README (3)");
    }

    proptest! {
        #[test]
        fn sanitize_never_yields_a_path_or_hidden_name(raw in ".{0,400}") {
            if let Some(name) = sanitize_name(&raw) {
                prop_assert!(!name.is_empty() && name.len() <= 255);
                prop_assert!(!name.contains(['/', '\\']));
                prop_assert!(!name.starts_with('.'));
                prop_assert!(!name.chars().any(char::is_control));
                for n in [1, 999, u32::MAX] {
                    let numbered = numbered_name(&name, n);
                    prop_assert!(numbered.len() <= 255 && !numbered.starts_with('.'));
                }
            }
        }
    }

    #[test]
    fn add_list_remove_round_trip() {
        let dir = scratch("roundtrip");
        let state = dir.join("state");
        let source = dir.join("report.md");
        fs::write(&source, b"# done").unwrap();
        let added = add(&state, &source).unwrap();
        assert!(is_id(&added.id));
        assert_eq!(added.name, "report.md");
        assert_eq!(added.size, 6);
        assert_eq!(fs::read(&added.path).unwrap(), b"# done");
        assert_eq!(list(&state).unwrap(), vec![added.clone()]);
        assert_eq!(item(&state, "../outbox").unwrap(), None);
        remove(&state, &added.id).unwrap();
        remove(&state, &added.id).unwrap();
        assert!(remove(&state, "../../x").is_err());
        assert!(list(&state).unwrap().is_empty());
        assert!(add(&state, &dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn listing_ignores_unfinished_and_malformed_entries() {
        let dir = scratch("listing");
        let outbox = outbox_dir(&dir);
        let tmp = outbox.join(format!(".tmp-{}", new_id().unwrap()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("half.bin"), b"x").unwrap();
        let two = outbox.join(new_id().unwrap());
        fs::create_dir_all(&two).unwrap();
        fs::write(two.join("a"), b"a").unwrap();
        fs::write(two.join("b"), b"b").unwrap();
        fs::create_dir_all(outbox.join("not-an-id")).unwrap();
        assert!(list(&dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn prune_removes_only_expired_entries() {
        let dir = scratch("prune");
        let source = dir.join("keep.txt");
        fs::write(&source, b"k").unwrap();
        let kept = add(&dir, &source).unwrap();
        let tmp = outbox_dir(&dir).join(format!(".tmp-{}", new_id().unwrap()));
        fs::create_dir_all(&tmp).unwrap();
        let partial = partial_path(&dir, &new_id().unwrap());
        fs::create_dir_all(incoming_dir(&dir)).unwrap();
        fs::write(&partial, b"p").unwrap();
        let now = SystemTime::now();
        prune(&dir, now);
        assert_eq!(list(&dir).unwrap(), vec![kept.clone()]);
        assert!(tmp.exists() && partial.exists());
        prune(&dir, now + PARTIAL_TTL + Duration::from_secs(60));
        assert_eq!(list(&dir).unwrap(), vec![kept]);
        assert!(!tmp.exists() && !partial.exists());
        prune(&dir, now + OUTBOX_TTL + Duration::from_secs(60));
        assert!(list(&dir).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
