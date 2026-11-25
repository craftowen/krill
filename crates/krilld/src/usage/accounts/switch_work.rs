//! Manual account switch work queue shared between IPC handlers and the session worker.

use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex};

use krill_proto::usage::ProviderKind;

#[derive(Default)]
pub(crate) struct SwitchWork {
    pub pending: Mutex<BTreeMap<SwitchScope, SwitchTarget>>,
    pub wake: Condvar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SwitchScope {
    Provider(ProviderKind),
    Session(krill_proto::SessionId, ProviderKind),
}

pub(crate) struct SwitchTarget {
    pub account_id: String,
    pub prepared: bool,
    pub not_before: Option<std::time::Instant>,
    /// The user chose to restart this session even while its agent is working.
    pub interrupt_busy: bool,
}
