//! Wake the session worker whenever an automatic account selection is committed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};

use krill_proto::usage::{AutoSwitchEvent, ProviderKind};

use crate::session::lock;

#[derive(Default)]
pub(crate) struct SwitchWork {
    pub pending: Mutex<BTreeMap<ProviderKind, SwitchTarget>>,
    pub wake: Condvar,
}

pub(crate) struct SwitchTarget {
    pub account_id: String,
    pub prepared: bool,
    pub not_before: Option<std::time::Instant>,
}

static WORKERS: Mutex<BTreeMap<PathBuf, Weak<SwitchWork>>> = Mutex::new(BTreeMap::new());

pub(crate) fn subscribe(home: &Path, state: Option<&Path>) -> Arc<SwitchWork> {
    let mut workers = lock(&WORKERS);
    workers.retain(|_, worker| worker.strong_count() > 0);
    let key = super::config_path(home, state);
    let worker = Arc::new(SwitchWork::default());
    workers.insert(key, Arc::downgrade(&worker));
    worker
}

pub(super) fn notify(home: &Path, state: Option<&Path>, events: &[AutoSwitchEvent]) {
    if events.is_empty() {
        return;
    }
    let worker = lock(&WORKERS)
        .get(&super::config_path(home, state))
        .and_then(Weak::upgrade);
    if let Some(worker) = worker {
        let mut pending = lock(&worker.pending);
        for event in events {
            pending.insert(
                event.provider,
                SwitchTarget {
                    account_id: event.to_id.clone(),
                    prepared: false,
                    not_before: None,
                },
            );
        }
        worker.wake.notify_one();
    }
}
