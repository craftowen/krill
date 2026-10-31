//! Account recovery records reserve both ends even after terminal history retention.

use std::fmt;
use std::fs;
use std::io::{self, BufReader, Read as _};
use std::path::Path;

use krill_proto::SessionId;
use serde::de::{DeserializeOwned, SeqAccess, Visitor};

#[derive(serde::Deserialize)]
struct Prepared {
    from: SessionId,
    to: SessionId,
}

#[derive(serde::Deserialize)]
struct Reference {
    from: SessionId,
    to: Option<SessionId>,
}

struct HandoffFloor(u64);

impl<'de> serde::Deserialize<'de> for HandoffFloor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FloorVisitor;
        impl<'de> Visitor<'de> for FloorVisitor {
            type Value = HandoffFloor;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an array of account handoffs")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut floor = 0;
                while let Some(reference) = seq.next_element::<Reference>()? {
                    floor = floor
                        .max(reference.from.0)
                        .max(reference.to.map_or(0, |to| to.0));
                }
                Ok(HandoffFloor(floor))
            }
        }
        deserializer.deserialize_seq(FloorVisitor)
    }
}

fn read<T: DeserializeOwned>(path: &Path, limit: Option<u64>) -> io::Result<Option<T>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || limit.is_some_and(|limit| metadata.len() > limit) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // A replaced FIFO must not block initialization between metadata and open.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || limit.is_some_and(|limit| metadata.len() > limit) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    // The ledger writer has no size limit. Stream IDs instead of imposing a new
    // limit on valid history or allocating its account names and error messages.
    // Bound reads to this regular file's observed length even if it grows later.
    let reader = BufReader::new(file.take(limit.unwrap_or(metadata.len()).saturating_add(1)));
    serde_json::from_reader(reader)
        .map(Some)
        .map_err(|_| io::ErrorKind::InvalidData.into())
}

fn prepared_floor(path: &Path) -> io::Result<u64> {
    Ok(read::<Prepared>(path, Some(4096))?.map_or(0, |p| p.from.0.max(p.to.0)))
}

pub(super) fn known_floor(root: &Path) -> io::Result<u64> {
    let mut floor =
        read::<HandoffFloor>(&root.join("account-handoffs.json"), None)?.map_or(0, |floor| floor.0);
    floor = floor.max(prepared_floor(&root.join("account-replacement.json"))?);
    let entries = match fs::read_dir(root.join("account-replacements")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(floor),
        Err(error) => return Err(error),
    };
    for (index, entry) in entries.enumerate() {
        if index >= krill_proto::holder::MAX_SESSIONS {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            floor = floor.max(prepared_floor(&path)?);
        }
    }
    Ok(floor)
}
