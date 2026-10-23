//! Read-only snapshots; ACK/mutations always use the authoritative durable store.
use super::{Path, io};
use crate::agent::control::cache::{Stamp, stamp};
use krill_proto::orchestration::{ORCH_FILE, OrchestrationStore};

#[derive(Default)]
pub(super) struct Cache {
    stamp: Option<Stamp>,
    store: Option<OrchestrationStore>,
}

impl Cache {
    pub fn load(&mut self, dir: &Path) -> io::Result<OrchestrationStore> {
        let path = dir.join(ORCH_FILE);
        let before = stamp(&path);
        if before.is_some()
            && self.stamp == before
            && let Some(store) = &self.store
        {
            return Ok(store.clone());
        }
        let store = OrchestrationStore::load(dir)?;
        let after = stamp(&path);
        if before.is_some() && before == after {
            self.stamp = after;
            self.store = Some(store.clone());
        } else {
            self.stamp = None;
            self.store = None;
        }
        Ok(store)
    }
}
