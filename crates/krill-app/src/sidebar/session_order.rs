//! Session rows follow persisted tab order, independently for each host.

use std::borrow::Cow;
use std::collections::BTreeMap;

use gpui::Context;
use krill_proto::SessionId;

use super::{HostId, Sidebar, Snapshot};

impl Sidebar {
    /// All tab leaves in structural order, including inactive workspaces and splits.
    /// Repeating the same projection must not trigger an observer/rebuild loop.
    pub fn set_session_order(
        &mut self,
        sessions: Vec<(HostId, SessionId)>,
        cx: &mut Context<Self>,
    ) {
        let mut order: BTreeMap<HostId, BTreeMap<SessionId, usize>> = BTreeMap::new();
        for (rank, (host, session)) in sessions.into_iter().enumerate() {
            order
                .entry(host)
                .or_default()
                .entry(session)
                .or_insert(rank);
        }
        if self.session_order != order {
            self.session_order = order;
            self.rebuild(cx);
        }
    }

    pub(super) fn ordered_snapshot<'a>(
        &self,
        host: &HostId,
        snapshot: &'a Snapshot,
    ) -> Cow<'a, Snapshot> {
        // Preserve replacement filtering and the account-handoff fallback row.
        let mut snapshot = self.profiles.snapshot(host, snapshot);
        if let Some(order) = self.session_order.get(host) {
            let rank = |session: &super::Session| {
                self.profiles
                    .session_rank(host, session.id, order)
                    .unwrap_or(usize::MAX)
            };
            if !snapshot
                .sessions
                .windows(2)
                .all(|pair| rank(&pair[0]) <= rank(&pair[1]))
            {
                snapshot.to_mut().sessions.sort_by_key(rank);
            }
        }
        // Stable sorting keeps unseen/detached sessions in their existing order.
        snapshot
    }
}

#[cfg(all(test, unix))]
mod tests;
