//! Bounded local suppression of repeated metadata fingerprints across uploads.
//! This is admission control, not an occurrence counter or an installation ID.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Consent, Event, Kind, consent::write_atomic, file::read_bounded};

pub(super) const FILE: &str = "capture.json";
const WINDOW_MS: u64 = 3_600_000;
const MAX_KEYS: usize = 128;
const MAX_BYTES: usize = 16_384;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Gate {
    generation: String,
    entries: Vec<Stamp>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stamp {
    key: String,
    at_ms: u64,
}

/// Called only under queue.lock. Distinct native/uncertain exits keep their
/// incident IDs; repeated ordinary errors, panics and stalls get one slot/hour.
pub(super) fn admit(dir: &Path, consent: &Consent, event: &Event, now: u64) -> io::Result<bool> {
    if matches!(event.kind, Kind::NativeCrash | Kind::AbnormalExit) {
        return Ok(true);
    }
    let mut gate = match read_bounded(&dir.join(FILE), MAX_BYTES) {
        Ok(Some(raw)) => serde_json::from_slice::<Gate>(&raw).unwrap_or_default(),
        Ok(None) => Gate::default(),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            ) =>
        {
            Gate::default()
        }
        Err(e) => return Err(e),
    };
    if gate.generation != consent.generation || gate.entries.len() > MAX_KEYS {
        gate = Gate {
            generation: consent.generation.clone(),
            entries: Vec::new(),
        };
    }
    gate.entries.retain(|stamp| {
        super::types::is_hex32(&stamp.key)
            && stamp.at_ms <= now.saturating_add(WINDOW_MS)
            && now.saturating_sub(stamp.at_ms) < WINDOW_MS
    });
    let bytes = serde_json::to_vec(&(
        event.process,
        event.error_code,
        &event.source_loc,
        event.hang_bucket_ms,
        &event.release,
        &event.build_id,
    ))
    .map_err(io::Error::other)?;
    let key = super::event_id("krill.error-reporting.capture.v1", &bytes);
    if gate.entries.iter().any(|stamp| stamp.key == key) || gate.entries.len() >= MAX_KEYS {
        return Ok(false);
    }
    gate.entries.push(Stamp { key, at_ms: now });
    let bytes = serde_json::to_vec(&gate).map_err(io::Error::other)?;
    if bytes.len() > MAX_BYTES {
        return Ok(false);
    }
    write_atomic(dir, FILE, &bytes)?;
    Ok(true)
}
