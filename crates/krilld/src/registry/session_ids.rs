//! Durable ID reservations are independent of terminal recording and history retention.
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use krill_proto::SessionId;

mod handoffs;

const FILE: &str = "session-id-sequence.json";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Sequence {
    version: u8,
    last_reserved: u64,
}

pub(super) struct SessionIds {
    root: Option<PathBuf>,
    floor: u64,
    unavailable: Option<io::ErrorKind>,
}

impl SessionIds {
    pub(super) fn open(root: Option<&Path>, kept: impl Iterator<Item = SessionId>) -> Self {
        let mut ids = Self {
            root: root.map(Path::to_path_buf),
            floor: kept.map(|id| id.0).max().unwrap_or_default(),
            unavailable: None,
        };
        if let Err(error) = ids.bootstrap() {
            tracing::warn!(kind = ?error.kind(), "session ID reservation unavailable; new sessions withheld, kept sessions preserved");
            ids.unavailable = Some(error.kind());
        }
        ids
    }

    pub(super) fn ready(&self) -> bool {
        self.unavailable.is_none()
    }

    pub(super) fn ensure_ready(&mut self) -> bool {
        if self.unavailable.is_some() {
            self.unavailable = self.bootstrap().err().map(|error| error.kind());
        }
        self.ready()
    }

    fn bootstrap(&mut self) -> io::Result<()> {
        let root = self.root.as_deref().ok_or(io::ErrorKind::NotFound)?;
        let path = root.join(FILE);
        let _lock = krill_proto::config_document::lock_toml(&path)?;
        let saved = match read_sequence(&path) {
            Ok(sequence) => sequence.last_reserved,
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        self.floor = self.floor.max(saved).max(known_floor(root)?);
        persist(&path, self.floor)
    }

    pub(super) fn reserve_after(
        &mut self,
        prior: Option<SessionId>,
        live_floor: u64,
    ) -> io::Result<SessionId> {
        self.reserve_using(prior, live_floor, persist)
    }

    fn reserve_using(
        &mut self,
        prior: Option<SessionId>,
        live_floor: u64,
        write: impl FnOnce(&Path, u64) -> io::Result<()>,
    ) -> io::Result<SessionId> {
        if !self.ensure_ready() {
            return Err(self.unavailable.unwrap_or(io::ErrorKind::Other).into());
        }
        let root = self.root.as_deref().ok_or(io::ErrorKind::NotFound)?;
        let path = root.join(FILE);
        let _lock = krill_proto::config_document::lock_toml(&path)?;
        // Disappearance after bootstrap is an error, never permission to reset to zero.
        let saved = read_sequence(&path)?.last_reserved;
        let floor = self
            .floor
            .max(saved)
            .max(live_floor)
            .max(prior.map_or(0, |id| id.0));
        let mut candidate = floor.checked_add(1).ok_or(io::ErrorKind::InvalidData)?;
        loop {
            let recorded = root.join("sessions").join(format!("{candidate}.noindex"));
            let launch = root
                .join("agent-launches")
                .join(format!("{candidate}.json"));
            if !recorded.try_exists()? && !launch.try_exists()? {
                break;
            }
            candidate = candidate.checked_add(1).ok_or(io::ErrorKind::InvalidData)?;
        }
        // Even a failed directory sync after rename must never lower the in-memory floor.
        self.floor = candidate;
        write(&path, candidate)?;
        Ok(SessionId(candidate))
    }
}

fn read_sequence(path: &Path) -> io::Result<Sequence> {
    let mut raw = Vec::new();
    fs::File::open(path)?.take(4097).read_to_end(&mut raw)?;
    let value: Sequence = serde_json::from_slice(&raw).map_err(|_| io::ErrorKind::InvalidData)?;
    if raw.len() > 4096 || value.version != 1 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(value)
}

fn persist(path: &Path, last_reserved: u64) -> io::Result<()> {
    let raw = serde_json::to_vec(&Sequence {
        version: 1,
        last_reserved,
    })
    .map_err(io::Error::other)?;
    crate::usage::accounts::write_atomic_0600(path, &raw)?;
    #[cfg(unix)]
    fs::File::open(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?.sync_all()?;
    Ok(())
}

fn known_floor(root: &Path) -> io::Result<u64> {
    let mut floor = 0;
    for (name, suffix) in [("sessions", ".noindex"), ("agent-launches", ".json")] {
        let entries = match fs::read_dir(root.join(name)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(number) = name.to_str().and_then(|n| n.strip_suffix(suffix))
                && !number.is_empty()
                && number.bytes().all(|byte| byte.is_ascii_digit())
            {
                floor = floor.max(
                    number
                        .parse::<u64>()
                        .map_err(|_| io::ErrorKind::InvalidData)?,
                );
            }
        }
    }
    let path = root.join("session-creators");
    let mut raw = Vec::new();
    match fs::File::open(path) {
        Ok(file) => {
            file.take(1_048_577).read_to_end(&mut raw)?;
            if raw.len() > 1_048_576 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let creators: Vec<(SessionId, SessionId)> =
                serde_json::from_slice(&raw).map_err(|_| io::ErrorKind::InvalidData)?;
            for (session, parent) in creators {
                floor = floor.max(session.0).max(parent.0);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Account aliases outlive recordings. Reusing one would pause or redirect a new shell.
    Ok(floor.max(handoffs::known_floor(root)?))
}

#[cfg(test)]
mod tests;
