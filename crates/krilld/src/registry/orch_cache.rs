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

#[cfg(test)]
mod tests {
    use super::{Cache, ORCH_FILE, OrchestrationStore};
    #[test]
    fn snapshots_see_external_atomic_replacement_and_deletion() {
        let dir = std::env::temp_dir().join(format!("orch-revision-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cache = Cache::default();
        let mut store = OrchestrationStore::default();
        store.create_run("first", None).unwrap();
        store.save(&dir).unwrap();
        assert_eq!(cache.load(&dir).unwrap().runs.len(), 1);
        store.create_run("other", None).unwrap();
        store.save(&dir).unwrap();
        assert_eq!(cache.load(&dir).unwrap().runs.len(), 2);
        std::fs::remove_file(dir.join(ORCH_FILE)).unwrap();
        assert!(cache.load(&dir).unwrap().runs.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
