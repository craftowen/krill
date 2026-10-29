//! Strict archive-hook inspection before an offline CLI deletion.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::Path;

use super::{MAX_COMMENTS_BYTES, NativeTomlDoc, OrcaYamlDoc, WorktreeStore, worktree_key};

/// Whether offline removal must defer to the daemon's archive-hook implementation.
///
/// # Errors
/// Refuses unreadable, oversized, or malformed hook configuration instead of treating it as empty.
pub fn offline_archive_configured(repo: &Path, state_dir: Option<&Path>) -> io::Result<bool> {
    let key = worktree_key(repo);
    let root = Path::new(&key);
    let native: NativeTomlDoc = read(&crate::user_dirs::repo_config(root))?
        .map(|body| toml::from_str(&body).map_err(io::Error::other))
        .transpose()?
        .unwrap_or_default();
    let orca: OrcaYamlDoc = read(&root.join("orca.yaml"))?
        .map(|body| serde_saphyr::from_str(&body).map_err(io::Error::other))
        .transpose()?
        .unwrap_or_default();
    let store = state_dir
        .map(WorktreeStore::load)
        .transpose()?
        .unwrap_or_default();
    // An explicit empty native value disables inherited archive hooks, just as in krilld.
    let archive = native
        .worktree
        .archive
        .or_else(|| store.archive_script_for(&key))
        .or(orca.scripts.archive);
    Ok(archive.is_some_and(|script| !script.trim().is_empty()))
}

fn read(path: &Path) -> io::Result<Option<String>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut body = String::new();
    file.take(MAX_COMMENTS_BYTES + 1)
        .read_to_string(&mut body)?;
    if u64::try_from(body.len()).unwrap_or(u64::MAX) > MAX_COMMENTS_BYTES {
        return Err(io::Error::other("archive configuration exceeds size limit"));
    }
    Ok(Some(body))
}
