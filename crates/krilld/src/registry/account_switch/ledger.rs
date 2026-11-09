use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::ops::Index;
use std::path::Path;

use krill_proto::SessionId;
use krill_proto::usage::{HandoffOutcome, SessionHandoff};

use super::FILE;

#[derive(Debug, Default)]
pub(crate) struct HandoffLedger {
    entries: BTreeMap<SessionId, SessionHandoff>,
    retained_by_replacement: BTreeMap<SessionId, SessionId>,
}

pub(super) fn is_prunable_when_ended(
    handoff: &SessionHandoff,
    is_ended: impl Fn(SessionId) -> bool,
) -> bool {
    match handoff.outcome() {
        HandoffOutcome::RetryableFailure { .. } => is_ended(handoff.from),
        HandoffOutcome::RetainedReplacement {
            replacement: Some(rep),
            ..
        } => is_ended(handoff.from) && is_ended(rep),
        HandoffOutcome::Pending
        | HandoffOutcome::Completed { .. }
        | HandoffOutcome::RetainedReplacement {
            replacement: None, ..
        } => false,
    }
}

impl HandoffLedger {
    pub(super) fn load(dir: Option<&Path>) -> Self {
        let mut ledger = Self::default();
        let items = dir
            .and_then(|d| std::fs::read(d.join(FILE)).ok())
            .and_then(|raw| serde_json::from_slice::<Vec<SessionHandoff>>(&raw).ok())
            .unwrap_or_default();
        for handoff in items {
            ledger.insert(handoff.from, handoff);
        }
        ledger
    }

    pub(crate) fn insert(
        &mut self,
        from: SessionId,
        handoff: SessionHandoff,
    ) -> Option<SessionHandoff> {
        let previous = self.remove(&from);
        if let HandoffOutcome::RetainedReplacement {
            replacement: Some(rep),
            ..
        } = handoff.outcome()
        {
            self.retained_by_replacement.insert(rep, from);
        }
        self.entries.insert(from, handoff);
        previous
    }

    pub(crate) fn remove<Q>(&mut self, from: &Q) -> Option<SessionHandoff>
    where
        SessionId: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let (removed_from, removed) = self.entries.remove_entry(from)?;
        if let HandoffOutcome::RetainedReplacement {
            replacement: Some(rep),
            ..
        } = removed.outcome()
            && self.retained_by_replacement.get::<SessionId>(&rep) == Some(&removed_from)
        {
            self.retained_by_replacement.remove::<SessionId>(&rep);
        }
        Some(removed)
    }

    pub(super) fn retarget_completed_aliases(&mut self, old: SessionId, new: SessionId) {
        for handoff in self.entries.values_mut() {
            if handoff.to == Some(old) {
                handoff.to = Some(new);
            }
        }
    }

    pub(crate) fn get<Q>(&self, from: &Q) -> Option<&SessionHandoff>
    where
        SessionId: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.entries.get(from)
    }

    pub(crate) fn contains_key<Q>(&self, from: &Q) -> bool
    where
        SessionId: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.entries.contains_key(from)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(all(test, unix))]
    pub(super) fn keys(&self) -> std::collections::btree_map::Keys<'_, SessionId, SessionHandoff> {
        self.entries.keys()
    }

    pub(crate) fn iter(&self) -> std::collections::btree_map::Iter<'_, SessionId, SessionHandoff> {
        self.entries.iter()
    }

    pub(crate) fn values(
        &self,
    ) -> std::collections::btree_map::Values<'_, SessionId, SessionHandoff> {
        self.entries.values()
    }

    pub(crate) fn has_retained_replacement(&self, session: SessionId) -> bool {
        self.retained_by_replacement.contains_key(&session)
    }

    pub(crate) fn retained_from(&self, replacement: SessionId) -> Option<SessionId> {
        self.retained_by_replacement.get(&replacement).copied()
    }

    pub(super) fn acknowledge_replacement(
        &mut self,
        replacement: SessionId,
        is_original_ended: impl Fn(SessionId) -> bool,
    ) -> Option<(SessionId, SessionHandoff)> {
        let from = self.retained_from(replacement)?;
        if !is_original_ended(from) {
            return None;
        }
        let removed = self.remove(&from)?;
        Some((from, removed))
    }

    pub(super) fn on_session_removed(
        &mut self,
        removed: SessionId,
        is_ended: impl Fn(SessionId) -> bool,
    ) -> bool {
        let mut to_remove = Vec::new();
        if let Some(handoff) = self.entries.get(&removed)
            && is_prunable_when_ended(handoff, &is_ended)
        {
            to_remove.push(removed);
        }
        if let Some(from) = self.retained_from(removed)
            && from != removed
            && let Some(handoff) = self.entries.get(&from)
            && is_prunable_when_ended(handoff, &is_ended)
        {
            to_remove.push(from);
        }
        let mut changed = false;
        for from in to_remove {
            changed |= self.remove(&from).is_some();
        }
        changed
    }

    pub(super) fn prune_dead(&mut self, is_ended: impl Fn(SessionId) -> bool) -> bool {
        let dead: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(&from, handoff)| {
                is_prunable_when_ended(handoff, &is_ended).then_some(from)
            })
            .collect();
        let mut changed = false;
        for from in dead {
            changed |= self.remove(&from).is_some();
        }
        changed
    }
}

impl Index<&SessionId> for HandoffLedger {
    type Output = SessionHandoff;

    fn index(&self, index: &SessionId) -> &Self::Output {
        self.entries.index(index)
    }
}
