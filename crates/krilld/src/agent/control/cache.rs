//! Metadata identities for atomic file replacement and same-size in-place edits.
use super::{Path, RulesPayload, fs};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Stamp {
    len: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

pub(crate) fn stamp(path: &Path) -> Option<Stamp> {
    let meta = fs::metadata(path).ok()?;
    Some(Stamp {
        len: meta.len(),
        modified: meta.modified().ok()?,
        created: meta.created().ok(),
        #[cfg(unix)]
        identity: (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
    })
}

#[derive(Debug)]
pub(super) struct Signed {
    pub stamps: (Option<Stamp>, Option<Stamp>),
    pub payload: Option<RulesPayload>,
}
